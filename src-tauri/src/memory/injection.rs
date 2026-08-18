/// injection.rs — Memory injection pipeline
///
/// Builds the memory context prefix (facts + episodes) for injection into
/// the chat system prompt. Facts are always available (no embedding needed).
/// Episodes require semantic search via the embedding model and are only
/// retrieved on the first message of a conversation.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use sqlx::SqlitePool;

use crate::model_registry::ModelRegistry;
use crate::models::{HardwareTier, MemoryEpisode, MemoryFact, TierConfig};
use crate::ollama_client::OllamaClient;

/// Episode similarity threshold.
///
/// Lower than the RAG retrieval threshold (0.7) because episode summaries
/// are short, conversational, and rarely keyword-overlap with a fresh user
/// query. With ~50 episodes total, a stricter threshold filters out most
/// genuinely-related history. False positives are also less costly here:
/// an off-topic episode injection wastes a few hundred tokens at worst.
const EPISODE_SIMILARITY_THRESHOLD: f32 = 0.6;

/// Compute the per-section memory token budget based on the model's
/// declared context window.
///
/// Heuristic: spend ~8% of the model's `num_ctx` on memory facts, with
/// a floor and ceiling so very small models still get useful memory and
/// very large models don't waste tokens on hundreds of stale facts.
///
/// - facts: floor 200, ceiling 1500 tokens
/// - episodes: floor 240, ceiling 2000 tokens
///
/// The same `num_ctx` is reused for both calls; the caller picks the kind.
pub fn adaptive_facts_budget(num_ctx: u32) -> u32 {
    let target = (num_ctx as f32 * 0.08) as u32;
    target.clamp(200, 1500)
}

pub fn adaptive_episodes_budget(num_ctx: u32) -> u32 {
    let target = (num_ctx as f32 * 0.08) as u32;
    target.clamp(240, 2000)
}

/// Build the facts portion of the memory context.
/// Returns formatted text within the 500-token budget.
/// Facts are ordered by most recently confirmed first (created_at DESC).
pub fn build_facts_context(facts: &[MemoryFact], token_budget: u32) -> String {
    if facts.is_empty() {
        return String::new();
    }

    // Sort by created_at descending (most recent first)
    let mut sorted: Vec<&MemoryFact> = facts.iter().collect();
    sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let mut result = String::from("## What you know about the user:\n");
    let mut used_tokens: u32 = 10; // header overhead estimate

    for fact in sorted {
        // Rough token estimate: ~4 chars per token for English text
        let fact_tokens = (fact.fact.len() as u32 / 4).max(1) + 2; // +2 for "- " prefix and newline
        if used_tokens + fact_tokens > token_budget {
            break;
        }
        result.push_str("- ");
        result.push_str(&fact.fact);
        result.push('\n');
        used_tokens += fact_tokens;
    }

    result
}

/// Retrieve relevant episodes for the first message of a conversation.
/// Searches _memories usearch collection, filters by decay threshold and restore flag,
/// returns formatted text within the supplied token budget.
///
/// On Tier 1, if the embedding model cannot be loaded, returns Ok("") gracefully.
pub async fn retrieve_episodes(
    ollama: &OllamaClient,
    registry: &Arc<ModelRegistry>,
    tier_config: &TierConfig,
    vectors_dir: &PathBuf,
    db: &SqlitePool,
    user_message: &str,
    decay_threshold_days: u32,
    token_budget: u32,
) -> Result<String> {
    // Get active episodes from DB (filtered by decay + restore)
    let episodes = crate::db::get_active_episodes(db, decay_threshold_days).await?;
    if episodes.is_empty() {
        return Ok(String::new());
    }

    // Try to embed the user message for semantic search
    let embedding_model = &tier_config.embedding_model;
    let query_embedding = match ollama.embed(embedding_model, user_message).await {
        Ok(emb) => emb,
        Err(e) => {
            // On Tier 1, graceful degradation — skip episodes
            if tier_config.tier == HardwareTier::Minimal {
                tracing::info!(
                    "Memory: skipping episode retrieval on Tier 1 — embedding model unavailable: {}",
                    e
                );
                return Ok(String::new());
            }
            return Err(e.into());
        }
    };

    // Open the _memories usearch index
    let index_path = vectors_dir.join("_memories.usearch");
    if !index_path.exists() {
        return Ok(String::new());
    }

    let index = crate::rag_engine::index::VectorIndex::open(
        &index_path,
        768,
        tier_config.quantization,
        tier_config.index_mmap,
    )?;
    let results = index.search(&query_embedding, 5)?; // get top 5, filter to top 3 after threshold

    // Match results to episodes and filter by similarity threshold
    let mut matched: Vec<(&MemoryEpisode, f32)> = Vec::new();
    for (key, score) in &results {
        if *score < EPISODE_SIMILARITY_THRESHOLD {
            continue;
        }
        if let Some(ep) = episodes.iter().find(|e| e.vector_id == Some(*key as i64)) {
            matched.push((ep, *score));
        }
    }

    // Sort by score descending, take top 3
    matched.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    matched.truncate(3);

    if matched.is_empty() {
        return Ok(String::new());
    }

    // Format within budget
    let mut result = String::from("## Relevant past conversations:\n");
    let mut used_tokens: u32 = 10;

    for (episode, _score) in &matched {
        let ep_tokens = (episode.summary.len() as u32 / 4).max(1) + 2;
        if used_tokens + ep_tokens > token_budget {
            break;
        }
        result.push_str("- ");
        result.push_str(&episode.summary);
        result.push('\n');
        used_tokens += ep_tokens;
    }

    Ok(result)
}
