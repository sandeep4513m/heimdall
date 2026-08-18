/// dedup.rs — Deduplication and conflict detection for memory facts
///
/// Uses embedding similarity to detect duplicates and the extraction model
/// to classify conflicts between facts.

use std::sync::Arc;

use anyhow::Result;

use crate::model_registry::ModelRegistry;
use crate::models::{MemoryFact, OllamaChatMessage, TierConfig};
use crate::ollama_client::OllamaClient;

/// Deduplication status for a candidate fact.
#[derive(Debug, Clone, PartialEq)]
pub enum DedupStatus {
    /// < 0.7 similarity — fresh fact
    New,
    /// 0.7-0.9 similarity — offer to update existing
    PossibleUpdate,
    /// > 0.9 similarity — discard as duplicate
    Duplicate,
}

impl DedupStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DedupStatus::New => "new",
            DedupStatus::PossibleUpdate => "possible_update",
            DedupStatus::Duplicate => "duplicate",
        }
    }
}

/// Result of a deduplication check.
pub struct DedupResult {
    pub status: DedupStatus,
    pub closest_fact_id: Option<String>,
    pub similarity_score: f32,
    /// Top-K (id, score) pairs above the broad-conflict floor (0.5).
    /// Used by the conflict pipeline to scan recent-but-not-similar facts
    /// for outright contradictions even when the candidate is classified
    /// as `New` by the per-fact dedup gate.
    pub conflict_candidates: Vec<(String, f32)>,
}

/// Lower bound for the broad conflict scan. Facts below this similarity
/// are too unrelated to plausibly contradict the candidate.
pub const CONFLICT_SCAN_FLOOR: f32 = 0.5;
/// Maximum number of broad-conflict candidates returned per dedup check.
pub const CONFLICT_SCAN_TOP_K: usize = 3;

/// Classify a similarity score into a DedupStatus.
pub fn classify_score(score: f32) -> DedupStatus {
    if score > 0.9 {
        DedupStatus::Duplicate
    } else if score >= 0.7 {
        DedupStatus::PossibleUpdate
    } else {
        DedupStatus::New
    }
}

/// Check a candidate fact against existing confirmed facts using embedding similarity.
///
/// If the embedding model is unavailable (e.g., Tier 1 RAM constraints),
/// returns DedupStatus::New to allow the fact through without dedup.
pub async fn check_deduplication(
    ollama: &OllamaClient,
    tier_config: &TierConfig,
    candidate: &str,
    existing_facts: &[MemoryFact],
) -> Result<DedupResult> {
    if existing_facts.is_empty() {
        return Ok(DedupResult {
            status: DedupStatus::New,
            closest_fact_id: None,
            similarity_score: 0.0,
            conflict_candidates: Vec::new(),
        });
    }

    // Embed the candidate
    let embedding_model = &tier_config.embedding_model;
    let candidate_embedding = match ollama.embed(embedding_model, candidate).await {
        Ok(emb) => emb,
        Err(e) => {
            tracing::warn!("Dedup: embedding model unavailable, skipping dedup: {}", e);
            return Ok(DedupResult {
                status: DedupStatus::New,
                closest_fact_id: None,
                similarity_score: 0.0,
                conflict_candidates: Vec::new(),
            });
        }
    };

    // Compute similarity for every existing fact, keep all (id, score) pairs.
    let mut scored: Vec<(String, f32)> = Vec::with_capacity(existing_facts.len());
    for fact in existing_facts {
        let fact_embedding = match ollama.embed(embedding_model, &fact.fact).await {
            Ok(emb) => emb,
            Err(_) => continue, // Skip facts we can't embed
        };
        let score = cosine_similarity(&candidate_embedding, &fact_embedding);
        scored.push((fact.id.clone(), score));
    }

    // Best score → dedup classification
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let (best_fact_id, best_score) = scored
        .first()
        .map(|(id, score)| (Some(id.clone()), *score))
        .unwrap_or((None, 0.0));

    let status = classify_score(best_score);

    // Conflict-scan candidates: top-K above the floor.
    // Always include the closest fact if it cleared the floor — even when
    // already used as the dedup anchor, the conflict pipeline may still
    // want to confirm it's a contradiction rather than a duplicate.
    let conflict_candidates: Vec<(String, f32)> = scored
        .iter()
        .filter(|(_, s)| *s >= CONFLICT_SCAN_FLOOR)
        .take(CONFLICT_SCAN_TOP_K)
        .cloned()
        .collect();

    Ok(DedupResult {
        status,
        closest_fact_id: best_fact_id,
        similarity_score: best_score,
        conflict_candidates,
    })
}

/// Compute cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// Use the extraction model to classify whether a candidate fact contradicts an existing fact.
/// Returns true if the facts conflict (contradict each other on the same topic).
pub async fn detect_conflict(
    ollama: &OllamaClient,
    registry: &Arc<ModelRegistry>,
    tier_config: &TierConfig,
    candidate: &str,
    existing: &str,
    loaded_chat_model: Option<&str>,
) -> Result<bool> {
    let model = super::extraction::select_extraction_model(
        registry,
        tier_config.tier,
        loaded_chat_model,
    )
    .await?;

    let prompt = vec![
        OllamaChatMessage {
            role: "system".to_string(),
            content: "You are a fact conflict detector. Given two facts about a user, determine if they CONTRADICT each other (cannot both be true simultaneously). Output ONLY 'yes' or 'no'.".to_string(),
            images: None,
            thinking: None,
        },
        OllamaChatMessage {
            role: "user".to_string(),
            content: format!(
                "Fact A: \"{}\"\nFact B: \"{}\"\n\nDo these facts contradict each other?",
                existing, candidate
            ),
            images: None,
            thinking: None,
        },
    ];

    let response = ollama.generate_completion(&model, prompt, None).await?;
    let answer = response.trim().to_lowercase();

    Ok(answer.starts_with("yes"))
}
