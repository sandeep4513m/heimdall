/// rag_engine — Phase 4 RAG Engine
///
/// Module structure:
///   mod.rs        — public API (this file, grows in task 15)
///   index.rs      — usearch VectorIndex wrapper
///   chunker.rs    — text chunker (task 3)
///   loaders/      — document loaders (tasks 4-8)
///   ingestion.rs  — IngestionWorker (task 9)
///   retrieval.rs  — retrieval pipeline (task 11)
///
/// Phase 6 / Task 26.1: the Phase 4 stop-gap helper module that lived
/// here was deleted along with its callers. Pressure-aware unloading
/// is now the Governor's job. See `_design/HEIMDALL_ARCHITECTURE.md`
/// Appendix A.

pub mod chunker;
pub mod index;
pub mod ingestion;
pub mod loaders;
pub mod retrieval;

/// Errors that can occur in the RAG engine.
#[derive(Debug, thiserror::Error)]
pub enum RagError {
    #[error("Collection not found: {0}")]
    CollectionNotFound(String),

    #[error("Collection unavailable (index corrupted or dimension mismatch): {0}")]
    CollectionUnavailable(String),

    #[error("Invalid collection name: {0}")]
    InvalidCollectionName(String),

    #[error("A collection named '{0}' already exists")]
    CollectionAlreadyExists(String),

    #[error("Tier vector cap reached: {current}/{max}")]
    VectorCapReached { current: u64, max: u64 },

    #[error("Embedding failed: {0}")]
    EmbeddingFailed(String),

    #[error("Loader error: {0}")]
    LoaderError(String),

    #[error("Index error: {0}")]
    IndexError(String),

    #[error("Database error: {0}")]
    DbError(#[from] anyhow::Error),

    #[error("SQLx error: {0}")]
    SqlxError(#[from] sqlx::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

use std::path::PathBuf;
use sqlx::SqlitePool;
use regex::Regex;
use sqlx::Row;

use crate::models::{TierConfig, Collection, CollectionStats, ChunkPreview};
use crate::ollama_client::OllamaClient;

/// Validate a user-facing collection display name and return its slugified id.
///
/// Rules:
/// - 1–64 chars, only `[A-Za-z0-9_ -]` allowed.
/// - lower-cased, spaces and underscores collapsed to `-`, non-alphanumeric
///   stripped except `-`.
///
/// This is the single source of truth for the display-name → id mapping.
/// Tauri commands call this at the IPC boundary so the DB consistently
/// stores ids (slugs) while the UI keeps speaking display names.
pub fn slug_id(name: &str) -> Result<String, RagError> {
    let re = Regex::new(r"^[A-Za-z0-9_ -]{1,64}$").unwrap();
    if !re.is_match(name) {
        return Err(RagError::InvalidCollectionName(name.to_string()));
    }
    let slug = name.to_lowercase().replace(' ', "-").replace('_', "-");
    // keep only alphanumeric and hyphens
    let slug = slug
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>();
    Ok(slug)
}

pub struct RagEngine {
    pub db: SqlitePool,
    pub ollama: OllamaClient,
    pub tier_config: TierConfig,
    pub vectors_dir: PathBuf,
    pub knowledge_dir: PathBuf,
}

impl RagEngine {
    pub fn new(
        db: SqlitePool,
        ollama: OllamaClient,
        tier_config: TierConfig,
        vectors_dir: PathBuf,
        knowledge_dir: PathBuf,
    ) -> Self {
        Self {
            db,
            ollama,
            tier_config,
            vectors_dir,
            knowledge_dir,
        }
    }

    /// Validates collection name (`^[A-Za-z0-9_ -]{1,64}$`) and returns a slugified id.
    fn generate_id(name: &str) -> Result<String, RagError> {
        slug_id(name)
    }

    pub async fn create_collection(&self, name: &str) -> Result<Collection, RagError> {
        let id = Self::generate_id(name)?;
        let now = chrono::Utc::now().timestamp();

        // Pre-check for collisions on either id (slug) or display_name so
        // the user gets a clean "already exists" message rather than a raw
        // "UNIQUE constraint failed" SQLx error string.
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM collections WHERE id = ? OR display_name = ? LIMIT 1;",
        )
        .bind(&id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?;
        if existing.is_some() {
            return Err(RagError::CollectionAlreadyExists(name.to_string()));
        }

        // 1. Insert into DB
        sqlx::query(
            "INSERT INTO collections (id, display_name, created_at, updated_at, last_ingested_at)
             VALUES (?, ?, ?, ?, NULL);"
        )
        .bind(&id)
        .bind(name)
        .bind(now)
        .bind(now)
        .execute(&self.db)
        .await?;

        // 2. Create vectors_dir/<id>.usearch (empty index) — writable.
        let index_path = self.vectors_dir.join(format!("{}.usearch", id));
        let index = index::VectorIndex::open_writable(
            &index_path,
            768,
            self.tier_config.quantization,
        )?;
        index.save()?;

        // 3. Create knowledge_dir/<id>/ dir
        let coll_knowledge_dir = self.knowledge_dir.join(&id);
        std::fs::create_dir_all(&coll_knowledge_dir)?;

        Ok(Collection {
            id,
            display_name: name.to_string(),
            created_at: now,
            updated_at: now,
            last_ingested_at: None,
        })
    }

    pub async fn delete_collection(&self, name: &str) -> Result<(), RagError> {
        let id = Self::generate_id(name)?;
        
        // 1. Delete collections row
        sqlx::query("DELETE FROM collections WHERE id = ?")
            .bind(&id)
            .execute(&self.db)
            .await?;

        // 2. Delete rag_chunks rows
        crate::db::delete_rag_chunks_for_collection(&self.db, &id).await?;

        // 3. Delete .usearch file
        let index_path = self.vectors_dir.join(format!("{}.usearch", id));
        let _ = std::fs::remove_file(index_path);

        // 4. Delete knowledge dir
        let coll_knowledge_dir = self.knowledge_dir.join(&id);
        let _ = std::fs::remove_dir_all(coll_knowledge_dir);

        // 5. Remove from conversations.active_rag_collections
        let convs = sqlx::query("SELECT id, active_rag_collections FROM conversations WHERE active_rag_collections IS NOT NULL")
            .fetch_all(&self.db)
            .await?;
            
        for conv in convs {
            let conv_id: String = conv.try_get("id")?;
            let json_str: Option<String> = conv.try_get("active_rag_collections")?;
            if let Some(json_str) = json_str {
                if let Ok(mut active_cols) = serde_json::from_str::<Vec<String>>(&json_str) {
                    if active_cols.contains(&id) {
                        active_cols.retain(|c| c != &id);
                        let new_json = serde_json::to_string(&active_cols).unwrap();
                        sqlx::query("UPDATE conversations SET active_rag_collections = ? WHERE id = ?")
                            .bind(new_json)
                            .bind(conv_id)
                            .execute(&self.db)
                            .await?;
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn rename_collection(&self, old_name: &str, new_name: &str) -> Result<Collection, RagError> {
        let old_id = Self::generate_id(old_name)?;
        let new_id = Self::generate_id(new_name)?;
        let now = chrono::Utc::now().timestamp();

        let mut tx = self.db.begin().await?;

        // 1. Update collections.display_name and id
        sqlx::query(
            "UPDATE collections SET id = ?, display_name = ?, updated_at = ? WHERE id = ?"
        )
        .bind(&new_id)
        .bind(new_name)
        .bind(now)
        .bind(&old_id)
        .execute(&mut *tx)
        .await?;

        // 2. Update rag_chunks.collection
        sqlx::query("UPDATE rag_chunks SET collection = ? WHERE collection = ?")
            .bind(&new_id)
            .bind(&old_id)
            .execute(&mut *tx)
            .await?;
            
        // Update active_rag_collections in conversations
        let convs = sqlx::query("SELECT id, active_rag_collections FROM conversations WHERE active_rag_collections IS NOT NULL")
            .fetch_all(&mut *tx)
            .await?;
            
        for conv in convs {
            let conv_id: String = conv.try_get("id")?;
            let json_str: Option<String> = conv.try_get("active_rag_collections")?;
            if let Some(json_str) = json_str {
                if let Ok(mut active_cols) = serde_json::from_str::<Vec<String>>(&json_str) {
                    let mut changed = false;
                    for col in active_cols.iter_mut() {
                        if *col == old_id {
                            *col = new_id.clone();
                            changed = true;
                        }
                    }
                    if changed {
                        let new_json = serde_json::to_string(&active_cols).unwrap();
                        sqlx::query("UPDATE conversations SET active_rag_collections = ? WHERE id = ?")
                            .bind(new_json)
                            .bind(conv_id)
                            .execute(&mut *tx)
                            .await?;
                    }
                }
            }
        }

        tx.commit().await?;

        // 3. Rename .usearch file
        let old_index_path = self.vectors_dir.join(format!("{}.usearch", old_id));
        let new_index_path = self.vectors_dir.join(format!("{}.usearch", new_id));
        if old_index_path.exists() {
            std::fs::rename(old_index_path, new_index_path)?;
        }

        // 4. Rename knowledge dir
        let old_coll_knowledge_dir = self.knowledge_dir.join(&old_id);
        let new_coll_knowledge_dir = self.knowledge_dir.join(&new_id);
        if old_coll_knowledge_dir.exists() {
            std::fs::rename(old_coll_knowledge_dir, new_coll_knowledge_dir)?;
        }

        // Fetch the updated collection
        let c = sqlx::query_as::<_, Collection>(
            "SELECT id, display_name, created_at, updated_at, last_ingested_at FROM collections WHERE id = ?"
        )
        .bind(new_id)
        .fetch_one(&self.db)
        .await?;

        Ok(c)
    }

    pub async fn list_collections(&self) -> Result<Vec<Collection>, RagError> {
        let cols = sqlx::query_as::<_, Collection>(
            "SELECT id, display_name, created_at, updated_at, last_ingested_at FROM collections ORDER BY display_name ASC"
        )
        .fetch_all(&self.db)
        .await?;
        
        Ok(cols)
    }

    pub async fn collection_stats(&self, name: &str) -> Result<CollectionStats, RagError> {
        let id = Self::generate_id(name)?;

        // Count chunks and distinct sources
        let row = sqlx::query(
            "SELECT COUNT(id) as chunks_count, COUNT(DISTINCT source_path) as sources_count FROM rag_chunks WHERE collection = ?"
        )
        .bind(&id)
        .fetch_one(&self.db)
        .await?;

        let chunks_count: i64 = row.try_get("chunks_count")?;
        let sources_count: i64 = row.try_get("sources_count")?;
        let chunks_count = chunks_count as u64;
        let sources_count = sources_count as u64;

        // Fetch the actual display_name and last_ingested_at so the response
        // is correct even when callers pass a slug rather than the original
        // typed name.
        let coll_row = sqlx::query(
            "SELECT display_name, last_ingested_at FROM collections WHERE id = ?",
        )
        .bind(&id)
        .fetch_optional(&self.db)
        .await?;

        let (display_name, last_ingested_at) = if let Some(r) = coll_row {
            (
                r.try_get::<String, _>("display_name")?,
                r.try_get::<Option<i64>, _>("last_ingested_at")?,
            )
        } else {
            // Caller asked for stats on a non-existent collection — return
            // zeros rather than failing. UI handles this gracefully.
            (name.to_string(), None)
        };

        let index_path = self.vectors_dir.join(format!("{}.usearch", id));
        let index_size_bytes = if index_path.exists() {
            std::fs::metadata(index_path)?.len()
        } else {
            0
        };

        Ok(CollectionStats {
            display_name,
            chunks: chunks_count,
            sources: sources_count,
            last_updated: last_ingested_at,
            vector_bytes: index_size_bytes,
        })
    }

    /// Delete a single source (file or URL) from a collection.
    ///
    /// Removes all `rag_chunks` rows for this source, removes their vectors
    /// from the usearch index, deletes the associated `ingestion_jobs` rows,
    /// and saves the updated index to disk.
    ///
    /// usearch `remove(key)` is O(1) — no full index rebuild required.
    /// The index file size does not shrink (deleted slots are tombstoned),
    /// but the vectors are excluded from all future searches.
    ///
    /// Returns `Ok(chunks_removed)` so the caller can log / surface the count.
    pub async fn delete_source(
        &self,
        collection_display_name: &str,
        source_path: &str,
    ) -> Result<u64, RagError> {
        let id = Self::generate_id(collection_display_name)?;

        // 1. Fetch the vector_ids for all chunks belonging to this source.
        let rows = sqlx::query(
            "SELECT vector_id FROM rag_chunks WHERE collection = ? AND source_path = ?;",
        )
        .bind(&id)
        .bind(source_path)
        .fetch_all(&self.db)
        .await?;

        let vector_ids: Vec<i64> = rows
            .iter()
            .filter_map(|r| {
                use sqlx::Row;
                r.try_get::<Option<i64>, _>("vector_id").ok().flatten()
            })
            .collect();

        let chunks_count = rows.len() as u64;

        // 2. Delete the chunk rows from SQLite.
        sqlx::query(
            "DELETE FROM rag_chunks WHERE collection = ? AND source_path = ?;",
        )
        .bind(&id)
        .bind(source_path)
        .execute(&self.db)
        .await?;

        // 3. Remove the vectors from the usearch index.
        //    Open the index, remove each key, save.
        //    If the index file doesn't exist (e.g. collection was just created
        //    and never had a successful embed), skip silently.
        if !vector_ids.is_empty() {
            let index_path = self.vectors_dir.join(format!("{}.usearch", id));
            if index_path.exists() {
                let index = index::VectorIndex::open_writable(
                    &index_path,
                    768,
                    self.tier_config.quantization,
                )?;
                for vid in &vector_ids {
                    // remove() is idempotent — missing keys are silently ignored.
                    let _ = index.remove(*vid as u64);
                }
                index.save()?;
            }
        }

        // 4. Delete the ingestion_jobs rows for this source in this collection.
        //    A source may have been ingested multiple times (e.g. re-ingested
        //    after a failed run), so we delete all matching rows.
        sqlx::query(
            "DELETE FROM ingestion_jobs WHERE collection = ? AND source_path = ?;",
        )
        .bind(&id)
        .bind(source_path)
        .execute(&self.db)
        .await?;

        // 5. Touch the collection's updated_at so stats reflect the change.
        let now = chrono::Utc::now().timestamp();
        let _ = sqlx::query(
            "UPDATE collections SET updated_at = ? WHERE id = ?;",
        )
        .bind(now)
        .bind(&id)
        .execute(&self.db)
        .await;

        Ok(chunks_count)
    }

    pub async fn search_preview(
        &self,
        registry: &std::sync::Arc<crate::model_registry::ModelRegistry>,
        name: &str,
        query: &str,
        k: usize,
    ) -> Result<Vec<ChunkPreview>, RagError> {
        let id = Self::generate_id(name)?;

        // Reuse the application-wide registry instead of constructing a
        // fresh one — that would cost an /api/show round trip per preview
        // and lose any cached capability data.
        let retrieved = retrieval::retrieve(
            &self.db,
            &self.ollama,
            registry,
            &self.tier_config,
            &self.vectors_dir,
            query,
            &[id.clone()],
            k,
        )
        .await?;

        let mut previews = Vec::new();
        for r in retrieved {
            previews.push(ChunkPreview {
                chunk_id: r.chunk.id,
                source_path: r.chunk.source_path,
                content: r.chunk.content,
                chunk_index: r.chunk.chunk_index,
                score: r.score,
            });
        }

        Ok(previews)
    }
}
