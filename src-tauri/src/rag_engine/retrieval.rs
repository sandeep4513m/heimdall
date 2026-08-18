/// retrieval.rs — RAG retrieval pipeline
///
/// Implements the fan-out retrieval pipeline:
///   1. Embed the user query via OllamaClient::embed
///   2. Validate the embedding model has `embedding` capability
///   3. For each collection: open VectorIndex, search top_k → (usearch_key, score)
///   4. Merge results across collections, deduplicate by usearch key per collection
///   5. Filter scores below 0.7
///   6. Tie-break: ascending chunk.id lexicographic order (determinism across repeated calls)
///   7. Sort by score DESC, take top_k
///   8. Fetch full chunk text from SQLite via vector_id lookup
///
/// Collection searches run in parallel (tokio::spawn fan-out). Missing
/// collections (no index file) log a warning and are skipped without failing.
/// Zero results is not an error.

use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::db;
use crate::model_registry::ModelRegistry;
use crate::models::{RagChunk, TierConfig};
use crate::ollama_client::OllamaClient;

use super::{index::VectorIndex, RagError};

/// Minimum cosine similarity score to include a chunk in results.
const MIN_SCORE_THRESHOLD: f32 = 0.7;

/// A retrieved chunk with its similarity score.
#[derive(Debug, Clone)]
pub struct RetrievedChunk {
    pub chunk: RagChunk,
    pub score: f32,
}

/// Retrieve the top-k most relevant chunks for a natural language `query`.
///
/// # Arguments
/// * `db` — SQLite pool for fetching chunk text after vector search
/// * `ollama` — Ollama client for embedding the query
/// * `registry` — Model registry for validating the embedding model capability
/// * `tier_config` — Active tier config (embedding model, dimensions, top_k)
/// * `vectors_dir` — Directory containing `<collection_id>.usearch` files
/// * `query` — The natural language query string to embed
/// * `collections` — Collection IDs to search (empty → return empty vec)
/// * `top_k` — Maximum number of chunks to return across all collections
///
/// # Returns
/// Sorted by score DESC. Scores below `MIN_SCORE_THRESHOLD` (0.7) are filtered.
/// Returns `Ok(vec![])` when no chunks pass the threshold — not an error.
pub async fn retrieve(
    db: &SqlitePool,
    ollama: &OllamaClient,
    registry: &Arc<ModelRegistry>,
    tier_config: &TierConfig,
    vectors_dir: &PathBuf,
    query: &str,
    collections: &[String],
    top_k: usize,
) -> Result<Vec<RetrievedChunk>, RagError> {
    if collections.is_empty() {
        return Ok(vec![]);
    }

    let embedding_model = &tier_config.embedding_model;

    // Validate that the configured embedding model has embedding capability.
    // Warns and proceeds on registry miss rather than blocking retrieval.
    match registry.get_capabilities(embedding_model).await {
        Ok(caps) if !caps.embedding => {
            return Err(RagError::EmbeddingFailed(format!(
                "Model '{}' does not have embedding capability",
                embedding_model
            )));
        }
        Err(e) => {
            warn!(
                model = %embedding_model,
                error = %e,
                "retrieve: could not validate embedding capability, proceeding anyway"
            );
        }
        _ => {} // embedding = true, good
    }

    // Embed the query.
    let query_vec = ollama
        .embed(embedding_model, query)
        .await
        .map_err(|e| RagError::EmbeddingFailed(e.to_string()))?;

    info!(
        embedding_model = %embedding_model,
        collections = ?collections,
        top_k,
        "retrieve: embedded query ({} dims), searching {} collection(s)",
        query_vec.len(),
        collections.len()
    );

    let dims = query_vec.len();
    let quantization = tier_config.quantization;
    let mmap = tier_config.index_mmap;

    // Fan-out: search each collection in parallel via tokio::spawn.
    let mut join_handles = Vec::with_capacity(collections.len());

    for coll_id in collections.iter().cloned() {
        let qv = query_vec.clone();
        let vdir = vectors_dir.clone();
        let top_k_search = top_k;
        // Clone before moving into the spawn closure so we can push the
        // coll_id into join_handles without a borrow-after-move error.
        let coll_id_for_handle = coll_id.clone();

        let handle = tokio::spawn(async move {
            search_collection(
                &coll_id,
                &qv,
                &vdir,
                top_k_search,
                dims,
                quantization,
                mmap,
            )
            .await
        });
        join_handles.push((coll_id_for_handle, handle));
    }

    // Collect search results: Vec<(collection_id, usearch_key, score)>.
    let mut all_hits: Vec<(String, u64, f32)> = Vec::new();

    for (coll_id, handle) in join_handles {
        match handle.await {
            Ok(Ok(hits)) => {
                for (key, score) in hits {
                    all_hits.push((coll_id.clone(), key, score));
                }
            }
            Ok(Err(e)) => {
                warn!(
                    collection = %coll_id,
                    error = %e,
                    "retrieve: collection search failed, skipping"
                );
            }
            Err(join_err) => {
                warn!(
                    collection = %coll_id,
                    error = %join_err,
                    "retrieve: collection search task panicked, skipping"
                );
            }
        }
    }

    if all_hits.is_empty() {
        return Ok(vec![]);
    }

    // Apply threshold filter before DB lookup to avoid unnecessary queries.
    let filtered: Vec<(String, u64, f32)> = all_hits
        .into_iter()
        .filter(|(_, _, score)| *score >= MIN_SCORE_THRESHOLD)
        .collect();

    if filtered.is_empty() {
        info!("retrieve: 0 chunks passed score threshold ({MIN_SCORE_THRESHOLD})");
        return Ok(vec![]);
    }

    // Group by collection so we can do per-collection DB lookups.
    let mut by_collection: std::collections::HashMap<String, Vec<(u64, f32)>> =
        std::collections::HashMap::new();
    for (coll_id, key, score) in &filtered {
        by_collection
            .entry(coll_id.clone())
            .or_default()
            .push((*key, *score));
    }

    // Fetch chunk rows from SQLite, one query per collection.
    let mut candidate_chunks: Vec<(RagChunk, f32)> = Vec::new();

    for (coll_id, hits) in &by_collection {
        let vector_ids: Vec<i64> = hits.iter().map(|(k, _)| *k as i64).collect();
        let score_map: std::collections::HashMap<i64, f32> = hits
            .iter()
            .map(|(k, s)| (*k as i64, *s))
            .collect();

        match db::get_rag_chunks_by_vector_ids(db, &vector_ids, coll_id).await {
            Ok(chunks) => {
                for chunk in chunks {
                    let score = chunk
                        .vector_id
                        .and_then(|vid| score_map.get(&vid).copied())
                        .unwrap_or(0.0);
                    candidate_chunks.push((chunk, score));
                }
            }
            Err(e) => {
                warn!(
                    collection = %coll_id,
                    error = %e,
                    "retrieve: DB lookup failed for collection, skipping"
                );
            }
        }
    }

    if candidate_chunks.is_empty() {
        return Ok(vec![]);
    }

    // Sort: score DESC, tie-break by chunk.id ASC (lexicographic, for P6 determinism).
    candidate_chunks.sort_by(|(chunk_a, score_a), (chunk_b, score_b)| {
        score_b
            .partial_cmp(score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| chunk_a.id.cmp(&chunk_b.id))
    });

    // Deduplicate by chunk.id (same chunk could theoretically appear via different paths).
    let mut seen_ids = std::collections::HashSet::new();
    candidate_chunks.retain(|(chunk, _)| seen_ids.insert(chunk.id.clone()));

    // Take top_k.
    candidate_chunks.truncate(top_k);

    let results: Vec<RetrievedChunk> = candidate_chunks
        .into_iter()
        .map(|(chunk, score)| RetrievedChunk { chunk, score })
        .collect();

    info!(
        "retrieve: returning {} chunks (threshold={}, top_k={})",
        results.len(),
        MIN_SCORE_THRESHOLD,
        top_k
    );

    Ok(results)
}

/// Search a single collection's vector index.
///
/// Returns `(usearch_key, score)` pairs sorted by score DESC.
/// Missing index files log a warning and return an empty vec.
async fn search_collection(
    collection_id: &str,
    query_vec: &[f32],
    vectors_dir: &PathBuf,
    top_k: usize,
    dims: usize,
    quantization: crate::models::ScalarKind,
    mmap: bool,
) -> Result<Vec<(u64, f32)>, RagError> {
    let index_path = vectors_dir.join(format!("{}.usearch", collection_id));

    if !index_path.exists() {
        warn!(
            collection = %collection_id,
            path = %index_path.display(),
            "retrieve: index file not found, skipping collection"
        );
        return Ok(vec![]);
    }

    let index = VectorIndex::open(&index_path, dims, quantization, mmap)?;

    if index.is_empty() {
        return Ok(vec![]);
    }

    index.search(query_vec, top_k)
}
