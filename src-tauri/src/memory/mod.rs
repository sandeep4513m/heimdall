/// memory — Phase 5 Memory System
///
/// Module structure:
///   mod.rs          — public API, MemoryEngine struct (this file)
///   extraction.rs   — fact extraction + episode creation
///   dedup.rs        — deduplication + conflict detection
///   injection.rs    — fact/episode injection pipeline

pub mod dedup;
pub mod extraction;
pub mod injection;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::model_registry::ModelRegistry;
use crate::models::{CandidateFact, ExtractionResult, TierConfig};
use crate::ollama_client::OllamaClient;

/// The memory engine — owns references to all subsystems needed for
/// extraction, deduplication, and injection.
pub struct MemoryEngine {
    pub db: SqlitePool,
    pub ollama: OllamaClient,
    pub tier_config: TierConfig,
    pub vectors_dir: PathBuf,
    pub registry: Arc<ModelRegistry>,
    /// Serialises `on_conversation_end` so two rapid `newChat`/`switchConversation`
    /// calls cannot run extraction concurrently. Concurrent extraction would race
    /// on the dedup snapshot (causing duplicate fact rows) and on the writable
    /// `_memories.usearch` handle (only one in-flight writer is safe).
    extraction_lock: Arc<Mutex<()>>,
}

impl MemoryEngine {
    pub fn new(
        db: SqlitePool,
        ollama: OllamaClient,
        tier_config: TierConfig,
        vectors_dir: PathBuf,
        registry: Arc<ModelRegistry>,
    ) -> Self {
        Self {
            db,
            ollama,
            tier_config,
            vectors_dir,
            registry,
            extraction_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Get the count of confirmed memory facts.
    pub async fn confirmed_fact_count(&self) -> Result<u64> {
        crate::db::get_confirmed_fact_count(&self.db).await
    }

    /// Build the full memory context prefix for injection into chat.
    /// Returns formatted system prompt text (facts + episodes).
    /// Returns empty string if memory is globally disabled.
    ///
    /// `num_ctx` drives the adaptive token budget: facts get
    /// `adaptive_facts_budget(num_ctx)` and episodes get
    /// `adaptive_episodes_budget(num_ctx)`.
    pub async fn build_injection_context(
        &self,
        conversation_id: &str,
        user_message: &str,
        is_first_message: bool,
        num_ctx: u32,
    ) -> Result<String> {
        // Check global enabled
        let global_enabled = crate::db::get_memory_setting(&self.db, "global_enabled")
            .await?
            .map(|v| v == "true")
            .unwrap_or(true);

        if !global_enabled {
            return Ok(String::new());
        }

        // Check per-conversation enabled
        let conv_enabled =
            crate::db::get_conversation_memory_enabled(&self.db, conversation_id).await?;
        if !conv_enabled {
            return Ok(String::new());
        }

        // Build facts context (always available, no embedding needed)
        let facts = crate::db::get_confirmed_memory_facts(&self.db).await?;
        let facts_budget = injection::adaptive_facts_budget(num_ctx);
        let facts_context = injection::build_facts_context(&facts, facts_budget);

        // Build episodes context (only on first message, requires embedding)
        let episodes_context = if is_first_message {
            let decay_days = crate::db::get_memory_setting(&self.db, "decay_threshold_days")
                .await?
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(90);

            let episodes_budget = injection::adaptive_episodes_budget(num_ctx);

            injection::retrieve_episodes(
                &self.ollama,
                &self.registry,
                &self.tier_config,
                &self.vectors_dir,
                &self.db,
                user_message,
                decay_days,
                episodes_budget,
            )
            .await
            .unwrap_or_default()
        } else {
            String::new()
        };

        // Combine
        let mut context = String::new();
        if !facts_context.is_empty() {
            context.push_str(&facts_context);
        }
        if !episodes_context.is_empty() {
            if !context.is_empty() {
                context.push('\n');
            }
            context.push_str(&episodes_context);
        }

        Ok(context)
    }

    /// Trigger extraction for a completed conversation.
    /// Called when user navigates away or starts a new chat.
    /// No-op if conversation has < 4 user messages or memory is disabled.
    pub async fn on_conversation_end(
        &self,
        conversation_id: &str,
        loaded_chat_model: Option<&str>,
    ) -> Result<ExtractionResult> {
        // Serialise concurrent extraction. Two rapid `switchConversation`
        // calls must not race on dedup snapshots or the writable episode
        // index handle.
        let _extraction_guard = self.extraction_lock.lock().await;

        // Check if memory is enabled for this conversation
        let conv_enabled =
            crate::db::get_conversation_memory_enabled(&self.db, conversation_id).await?;
        if !conv_enabled {
            return Ok(ExtractionResult {
                facts_extracted: vec![],
                episode_created: false,
                skipped_reason: Some("Memory disabled for this conversation".to_string()),
                extraction_error: None,
                episode_error: None,
            });
        }

        // Check global enabled
        let global_enabled = crate::db::get_memory_setting(&self.db, "global_enabled")
            .await?
            .map(|v| v == "true")
            .unwrap_or(true);
        if !global_enabled {
            return Ok(ExtractionResult {
                facts_extracted: vec![],
                episode_created: false,
                skipped_reason: Some("Memory system globally disabled".to_string()),
                extraction_error: None,
                episode_error: None,
            });
        }

        // Get messages and check threshold
        let messages = crate::db::get_messages(&self.db, conversation_id).await?;
        let user_msg_count = messages.iter().filter(|m| m.role == "user").count();

        if user_msg_count < 4 {
            return Ok(ExtractionResult {
                facts_extracted: vec![],
                episode_created: false,
                skipped_reason: Some(format!(
                    "Only {} user messages (need 4+)",
                    user_msg_count
                )),
                extraction_error: None,
                episode_error: None,
            });
        }

        // Derive the best available model hint for extraction.
        // Priority: caller-supplied hint > model stored on the conversation row > None.
        // The conversation row's `model` column is set when the conversation is created
        // and reflects what the user was actually chatting with.
        let effective_model_hint: Option<String> = if loaded_chat_model.is_some() {
            loaded_chat_model.map(str::to_string)
        } else {
            crate::db::get_conversation_model(&self.db, conversation_id)
                .await
                .ok()
                .flatten()
        };

        let batch_id = uuid::Uuid::new_v4().to_string();
        let mut candidate_facts = Vec::new();

        // --- Fact Extraction ---
        let mut extraction_error: Option<String> = None;
        let raw_facts = match extraction::extract_facts(
            &self.ollama,
            &self.registry,
            &self.tier_config,
            &messages,
            effective_model_hint.as_deref(),
        )
        .await
        {
            Ok(facts) => facts,
            Err(e) => {
                let msg = format!("Fact extraction failed: {}", e);
                tracing::warn!("{}", msg);
                extraction_error = Some(msg);
                vec![]
            }
        };

        // --- Deduplication & Conflict Detection ---
        let existing_facts = crate::db::get_confirmed_memory_facts(&self.db).await?;

        for fact_text in &raw_facts {
            // Check dedup
            let dedup_result = dedup::check_deduplication(
                &self.ollama,
                &self.tier_config,
                fact_text,
                &existing_facts,
            )
            .await
            .unwrap_or(dedup::DedupResult {
                status: dedup::DedupStatus::New,
                closest_fact_id: None,
                similarity_score: 0.0,
                conflict_candidates: Vec::new(),
            });

            // Skip duplicates
            if dedup_result.status == dedup::DedupStatus::Duplicate {
                continue;
            }

            // Conflict scan — broaden beyond just the dedup anchor.
            //
            // Trust killer: if a fresh fact ("User is on Heimdall v1.0") and a
            // stale fact ("User is on Heimdall v0.5") have similarity below
            // the PossibleUpdate threshold (0.7), the original code stored both
            // and the model later saw contradictions. Now we scan every
            // candidate above 0.5 and short-circuit on the first contradiction.
            let mut conflict_with: Option<String> = None;
            for (candidate_id, _score) in &dedup_result.conflict_candidates {
                let existing = match existing_facts.iter().find(|f| f.id == *candidate_id) {
                    Some(f) => f,
                    None => continue,
                };
                let is_conflict = dedup::detect_conflict(
                    &self.ollama,
                    &self.registry,
                    &self.tier_config,
                    fact_text,
                    &existing.fact,
                    effective_model_hint.as_deref(),
                )
                .await
                .unwrap_or(false);
                if is_conflict {
                    conflict_with = Some(candidate_id.clone());
                    break;
                }
            }

            // Store the candidate fact
            let stored = crate::db::insert_memory_fact(
                &self.db,
                fact_text,
                Some(conversation_id),
                Some(dedup_result.status.as_str()),
                conflict_with.as_deref(),
                dedup_result.closest_fact_id.as_deref(),
                Some(&batch_id),
            )
            .await?;

            candidate_facts.push(CandidateFact {
                id: stored.id,
                text: fact_text.clone(),
                dedup_status: dedup_result.status.as_str().to_string(),
                conflict_with,
            });
        }

        // --- Episode Creation ---
        let mut episode_error: Option<String> = None;
        let episode_created = match extraction::generate_episode_summary(
            &self.ollama,
            &self.registry,
            &self.tier_config,
            &messages,
            effective_model_hint.as_deref(),
        )
        .await
        {
            Ok(summary) => match self.store_episode(&summary, conversation_id).await {
                Ok(_) => true,
                Err(e) => {
                    let msg = format!("Episode storage failed: {}", e);
                    tracing::warn!("{}", msg);
                    episode_error = Some(msg);
                    false
                }
            },
            Err(e) => {
                let msg = format!("Episode summary generation failed: {}", e);
                tracing::warn!("{}", msg);
                episode_error = Some(msg);
                false
            }
        };

        Ok(ExtractionResult {
            facts_extracted: candidate_facts,
            episode_created,
            skipped_reason: None,
            extraction_error,
            episode_error,
        })
    }

    /// Store an episode summary as a vector in the _memories collection.
    async fn store_episode(&self, summary: &str, conversation_id: &str) -> Result<()> {
        // Embed the summary
        let embedding = self
            .ollama
            .embed(&self.tier_config.embedding_model, summary)
            .await?;

        // Determine dimensions from the embedding itself
        let dims = embedding.len();

        // Open or create the _memories index — writable mode (never mmap).
        let index_path = self.vectors_dir.join("_memories.usearch");
        let index = crate::rag_engine::index::VectorIndex::open_writable(
            &index_path,
            dims,
            self.tier_config.quantization,
        )?;

        // Add the vector
        let vector_id = index.add(&embedding)?;
        index.save()?;

        // Store metadata in SQLite
        crate::db::insert_memory_episode(
            &self.db,
            summary,
            Some(conversation_id),
            Some(vector_id as i64),
        )
        .await?;

        Ok(())
    }
}
