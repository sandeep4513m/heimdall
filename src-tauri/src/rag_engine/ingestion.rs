/// ingestion.rs — IngestionWorker: embed, store, and track document ingestion
///
/// Provides a single global background task that drains a FIFO mpsc queue of
/// IngestionRequests. One job runs at a time; parallelism is at the source
/// granularity only (future; Phase 4 is strictly sequential).
///
/// ## Lifecycle per job
/// 1. Dequeue request → check `ingestion_paused` flag (Phase 6 Governor)
/// 2. Tier 1: `Governor::evaluate_embedding_fit` decides whether to
///    unload the chat model first; emits `governor://embedding_swap`
/// 3. For each source in the request:
///    a. dispatch_loader(source) → Box<dyn Loader>
///    b. loader.load(source) → Vec<LoadedContent>
///    c. chunk_text(content) → Vec<Chunk>
///    d. For each chunk (batched):
///       - OllamaClient::embed → Vec<f32>
///       - VectorIndex::add → usearch key
///       - db::insert_rag_chunk
/// 4. Flush SQLite + usearch on each batch
/// 5. Force-unload the embedding model and emit `UnloadingEmbedding`
/// 6. Emit rag://ingestion-progress every 100ms (throttled)
/// 7. Emit rag://ingestion-complete on finish
///
/// ## Batch sizes
/// - Tier 1: 4 chunks/batch
/// - Tier 2: 8 chunks/batch
/// - Tier 3: 16 chunks/batch
///
/// ## Error handling
/// - Per-source errors are logged; the worker continues with remaining sources.
/// - Per-chunk embed errors are logged; the worker continues.
/// - Cancel: the cancel flag is checked after each chunk; status → 'cancelled'.
/// - Resume: chunks before `resume_from` index are skipped.
///
/// ## Events
/// - `rag://ingestion-progress`: { job_id, chunks_done, chunks_total, status, current_file }
/// - `rag://ingestion-complete`: { job_id, success, error }
///
/// ## Startup
/// Any job with status='running' is marked 'interrupted' at startup.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::AppHandle;
use tauri::Emitter;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::db;
use crate::governor::Governor;
use crate::models::{
    EmbeddingFitDecision, EmbeddingSwapEvent, EmbeddingSwapPhase, HardwareTier,
    TierConfig,
};
use crate::ollama_client::OllamaClient;

use super::{
    chunker::{chunk_text, ChunkerConfig},
    index::VectorIndex,
    loaders::{dispatch_source, folder::FolderLoader, LoadedContent, SourceKind},
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A request to ingest one or more sources into a named collection.
#[derive(Debug, Clone)]
pub struct IngestionRequest {
    /// Job id (matches ingestion_jobs.id).
    pub job_id: String,
    /// Target collection id.
    pub collection: String,
    /// Source paths or URLs to ingest.
    pub sources: Vec<String>,
    /// Optional resume offset: number of chunks already done.
    /// When set, the worker skips the first `resume_from` chunks.
    pub resume_from: Option<i64>,
    /// Cancellation flag. Shared with the caller via `Arc<Mutex<bool>>`.
    pub cancel_flag: Arc<Mutex<bool>>,
    /// Hint for the worker's adaptive embedding-fit decision on Tier 1
    /// hardware. Should be the user's currently active chat model name.
    /// When None, the worker proceeds without a chat-side unload (no-op
    /// rather than 404).
    pub chat_model_hint: Option<String>,
}

/// Progress event payload (rag://ingestion-progress).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionProgressEvent {
    pub job_id: String,
    pub chunks_done: i64,
    pub chunks_total: i64,
    pub status: String,
    pub current_file: String,
}

/// Completion event payload (rag://ingestion-complete).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionCompleteEvent {
    pub job_id: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Spawn the global ingestion worker task.
///
/// Returns a sender that callers use to enqueue `IngestionRequest`s.
/// The worker runs for the lifetime of the application.
///
/// On startup, this function marks any `status='running'` rows as
/// `'interrupted'` — those were orphaned by a previous crash or restart.
///
/// # Arguments
/// * `app` — Tauri AppHandle for emitting Tauri events
/// * `db` — SQLite pool
/// * `ollama` — Ollama client for embedding
/// * `tier_config` — Active tier configuration
/// * `vectors_dir` — Directory for .usearch index files
/// * `ingestion_paused` — Atomic flag set by the Governor on Critical
///   state edge transitions (Req 9.3). The worker checks this BEFORE
///   each dequeue and sleeps 1 s rather than pulling the next job.
/// * `governor` — Shared Governor handle for `can_load_embedding`
///   decisions (Run 3 / Task 14.2).
/// * `chat_reload_pending` — Shared field that the worker sets to
///   `Some(name)` after force-unloading the chat model on a Tier 1
///   embedding swap (Req 10.6). The next chat_stream call clears it
///   and emits `governor://embedding_swap { phase: ReloadingChat }`.
pub async fn spawn_ingestion_worker(
    app: AppHandle,
    db: SqlitePool,
    ollama: OllamaClient,
    tier_config: TierConfig,
    vectors_dir: PathBuf,
    ingestion_paused: Arc<AtomicBool>,
    governor: Arc<Governor>,
    chat_reload_pending: Arc<std::sync::Mutex<Option<String>>>,
) -> mpsc::Sender<IngestionRequest> {
    // Mark any orphaned 'running' jobs as 'interrupted' before starting.
    if let Err(e) = mark_interrupted_jobs(&db).await {
        warn!(error = %e, "ingestion_worker: failed to mark interrupted jobs at startup");
    }

    let (tx, mut rx) = mpsc::channel::<IngestionRequest>(100);

    tokio::spawn(async move {
        info!("ingestion_worker: started");

        // Restructured loop (Run 3 / Task 13.2): we check `ingestion_paused`
        // BEFORE dequeuing so the queue is not drained while the Governor
        // is in the Critical edge-transition recovery window. Once a
        // request is past the dequeue point we never cancel it (Req 9.5).
        loop {
            // Pause check (Req 9.3): re-check every 1000 ms.
            if ingestion_paused.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                continue;
            }
            let request = match rx.recv().await {
                Some(r) => r,
                None => {
                    info!("ingestion_worker: sender dropped, shutting down");
                    return;
                }
            };

            // Tier 1 adaptive embedding orchestration (Run 3 / Task 14.2).
            // Replaces the old Phase 4 stop-gap unload helper — pressure
            // response is the Governor's job now.
            if tier_config.tier == HardwareTier::Minimal {
                let chat_model = request.chat_model_hint.clone();
                let decision = governor
                    .evaluate_embedding_fit(chat_model.as_deref())
                    .await;
                match decision {
                    EmbeddingFitDecision::FitsAlongside => {
                        // Both fit within the safe headroom — proceed
                        // with the chat model still loaded.
                    }
                    EmbeddingFitDecision::RequiresChatUnload => {
                        if let Some(name) = chat_model.as_deref() {
                            match ollama.force_unload(name).await {
                                Ok(()) => {
                                    if let Ok(mut p) = chat_reload_pending.lock() {
                                        *p = Some(name.to_string());
                                    }
                                    let _ = app.emit(
                                        "governor://embedding_swap",
                                        &EmbeddingSwapEvent {
                                            phase: EmbeddingSwapPhase::UnloadingChat,
                                            chat_model: Some(name.to_string()),
                                        },
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        model = %name,
                                        error = %e,
                                        "ingestion: chat unload failed"
                                    );
                                }
                            }
                        }
                    }
                    EmbeddingFitDecision::InsufficientEvenAlone => {
                        // Embedding alone exceeds safe headroom — fail
                        // the job with a user-visible error and skip to
                        // the next dequeue (Req 10.8).
                        let msg = format!(
                            "Embedding model alone exceeds the configured safe RAM headroom. \
                             Available: {} MB. Free RAM and retry.",
                            governor.last_available_mb()
                        );
                        if let Err(e) =
                            db::fail_ingestion_job(&db, &request.job_id, &msg).await
                        {
                            warn!(
                                job_id = %request.job_id,
                                error = %e,
                                "ingestion: failed to mark job as failed"
                            );
                        }
                        emit_complete(&app, &request.job_id, false, Some(&msg));
                        continue;
                    }
                }
            }

            process_job(
                &app,
                &db,
                &ollama,
                &tier_config,
                &vectors_dir,
                request,
            )
            .await;
        }
    });

    tx
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Batch size for SQLite + usearch flush, keyed on hardware tier.
fn batch_size(tier: &HardwareTier) -> usize {
    match tier {
        HardwareTier::Minimal => 4,
        HardwareTier::Standard => 8,
        HardwareTier::Full => 16,
    }
}

/// Mark all rows with status='running' as 'interrupted'.
async fn mark_interrupted_jobs(db: &SqlitePool) -> Result<(), anyhow::Error> {
    sqlx::query(
        "UPDATE ingestion_jobs SET status = 'interrupted' WHERE status = 'running';"
    )
    .execute(db)
    .await
    .map_err(|e| anyhow::anyhow!("Failed to mark interrupted jobs: {}", e))?;
    Ok(())
}

/// Update the ingestion job status to 'cancelled' in SQLite.
async fn mark_cancelled(db: &SqlitePool, job_id: &str) {
    let _ = sqlx::query(
        "UPDATE ingestion_jobs SET status = 'cancelled', completed_at = ? WHERE id = ?;"
    )
    .bind(chrono::Utc::now().timestamp())
    .bind(job_id)
    .execute(db)
    .await;
}

/// Emit a progress event (throttled to 100ms intervals).
fn maybe_emit_progress(
    app: &AppHandle,
    last_emit: &mut Instant,
    job_id: &str,
    chunks_done: i64,
    chunks_total: i64,
    current_file: &str,
) {
    let now = Instant::now();
    if now.duration_since(*last_emit) >= Duration::from_millis(100) {
        *last_emit = now;
        let evt = IngestionProgressEvent {
            job_id: job_id.to_string(),
            chunks_done,
            chunks_total,
            status: "running".to_string(),
            current_file: current_file.to_string(),
        };
        if let Err(e) = app.emit("rag://ingestion-progress", &evt) {
            warn!(error = %e, "ingestion_worker: failed to emit progress event");
        }
    }
}

/// Process a single ingestion job end-to-end.
async fn process_job(
    app: &AppHandle,
    db: &SqlitePool,
    ollama: &OllamaClient,
    tier_config: &TierConfig,
    vectors_dir: &PathBuf,
    request: IngestionRequest,
) {
    let job_id = &request.job_id;
    let collection = &request.collection;
    let resume_from = request.resume_from.unwrap_or(0);

    info!(
        job_id = %job_id,
        collection = %collection,
        sources = request.sources.len(),
        resume_from,
        "ingestion_worker: starting job"
    );

    // Open (or create) the vector index for this collection.
    // nomic-embed-text produces 768-dim vectors. Writable mode — never mmap.
    let index_path = vectors_dir.join(format!("{}.usearch", collection));
    let vector_index = match VectorIndex::open_writable(
        &index_path,
        768,
        tier_config.quantization,
    ) {
        Ok(idx) => Arc::new(Mutex::new(idx)),
        Err(e) => {
            warn!(
                job_id = %job_id,
                error = %e,
                "ingestion_worker: failed to open vector index"
            );
            emit_complete(app, job_id, false, Some(&e.to_string()));
            if let Err(db_err) = db::fail_ingestion_job(db, job_id, &e.to_string()).await {
                warn!(error = %db_err, "ingestion_worker: also failed to mark job as failed");
            }
            return;
        }
    };

    let chunker_cfg = ChunkerConfig {
        chunk_size_tokens: tier_config.chunk_size_tokens,
        chunk_overlap_tokens: tier_config.chunk_overlap_tokens,
        tokenizer: "cl100k_base".to_string(),
    };

    let flush_batch = batch_size(&tier_config.tier);
    let mut global_chunk_index: i64 = 0;
    let mut chunks_done: i64 = 0;
    let mut last_emit = Instant::now() - Duration::from_secs(1); // ensure first emit fires

    // ── Main source loop ──────────────────────────────────────────────────
    let job_error: Option<String> = None;

    'sources: for source in &request.sources {
        // Check cancel before each source.
        if *request.cancel_flag.lock().await {
            info!(job_id = %job_id, "ingestion_worker: cancelled before source {}", source);
            mark_cancelled(db, job_id).await;
            emit_complete(app, job_id, false, Some("Cancelled by user"));
            return;
        }

        // Get the loader for this source. URLs route through UrlLoader,
        // directories walk via FolderLoader, files dispatch by extension.
        let loaded_items: Vec<LoadedContent> = match dispatch_source(source) {
            SourceKind::Url(loader) | SourceKind::File(loader) => {
                match loader.load(source).await {
                    Ok(items) => items,
                    Err(e) => {
                        warn!(
                            job_id = %job_id,
                            source = %source,
                            error = %e,
                            "ingestion_worker: loader failed, skipping source"
                        );
                        continue 'sources;
                    }
                }
            }
            SourceKind::Folder => {
                match FolderLoader.load_folder(source).await {
                    Ok((items, errs)) => {
                        if !errs.is_empty() {
                            warn!(
                                job_id = %job_id,
                                source = %source,
                                errors = errs.len(),
                                "ingestion_worker: folder walk had {} per-file errors (continuing with successful files)",
                                errs.len()
                            );
                        }
                        items
                    }
                    Err(e) => {
                        warn!(
                            job_id = %job_id,
                            source = %source,
                            error = %e,
                            "ingestion_worker: folder walk failed, skipping source"
                        );
                        continue 'sources;
                    }
                }
            }
            SourceKind::Unsupported => {
                warn!(
                    job_id = %job_id,
                    source = %source,
                    "ingestion_worker: unsupported source kind (not a URL, not a directory, no recognised file extension), skipping"
                );
                continue 'sources;
            }
        };

        // Process each loaded content item (e.g. each page of a PDF).
        for loaded in loaded_items {
            // Chunk the loaded content.
            let chunks = chunk_text(&loaded.text, &chunker_cfg);
            let chunks_in_source = chunks.len() as i64;

            for chunk in chunks {
                let absolute_chunk_idx = global_chunk_index;
                global_chunk_index += 1;

                // Resume: skip chunks already processed.
                if absolute_chunk_idx < resume_from {
                    chunks_done = absolute_chunk_idx + 1;
                    continue;
                }

                // Check cancel before each chunk.
                if *request.cancel_flag.lock().await {
                    info!(job_id = %job_id, "ingestion_worker: cancelled mid-source");
                    mark_cancelled(db, job_id).await;
                    emit_complete(app, job_id, false, Some("Cancelled by user"));
                    return;
                }

                // Tier 1: the per-chunk Phase 4 stop-gap memory check
                // has been removed (Run 3 / Task 14.2). The Governor's
                // polling loop is the new pressure detector — if RAM
                // goes critical mid-ingestion the Governor sets
                // `ingestion_paused = true` and we sleep at the top of
                // the dequeue loop. Within a single job we proceed
                // straight through; cancellation is the only mid-job
                // exit (Req 10.4).

                // Embed the chunk.
                let embedding =
                    match ollama.embed(&tier_config.embedding_model, &chunk.content).await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(
                                job_id = %job_id,
                                chunk_idx = absolute_chunk_idx,
                                error = %e,
                                "ingestion_worker: embed failed, skipping chunk"
                            );
                            continue;
                        }
                    };

                // Add to vector index.
                let vector_id: Option<i64> = {
                    let idx = vector_index.lock().await;
                    match idx.add(&embedding) {
                        Ok(vid) => Some(vid as i64),
                        Err(e) => {
                            warn!(
                                job_id = %job_id,
                                error = %e,
                                "ingestion_worker: vector add failed, skipping chunk"
                            );
                            continue;
                        }
                    }
                };

                // Insert chunk into SQLite.
                if let Err(e) = db::insert_rag_chunk(
                    db,
                    collection,
                    source,
                    absolute_chunk_idx,
                    &chunk.content,
                    chunk.token_count as i64,
                    vector_id,
                )
                .await
                {
                    warn!(
                        job_id = %job_id,
                        error = %e,
                        "ingestion_worker: db insert failed, skipping chunk"
                    );
                    continue;
                }

                chunks_done += 1;

                // Flush on batch boundary: save usearch index to disk.
                if chunks_done % flush_batch as i64 == 0 {
                    let idx = vector_index.lock().await;
                    if let Err(e) = idx.save() {
                        warn!(
                            job_id = %job_id,
                            error = %e,
                            "ingestion_worker: usearch save failed during batch flush"
                        );
                    }

                    // Update DB progress.
                    let total_estimate = global_chunk_index.max(chunks_done);
                    let _ = db::update_ingestion_progress(db, job_id, total_estimate, chunks_done)
                        .await;
                }

                // Throttled progress event.
                maybe_emit_progress(
                    app,
                    &mut last_emit,
                    job_id,
                    chunks_done,
                    chunks_in_source, // best estimate — we don't know total upfront
                    source,
                );
            }
        }
    }

    // Final flush of the vector index.
    {
        let idx = vector_index.lock().await;
        if let Err(e) = idx.save() {
            warn!(
                job_id = %job_id,
                error = %e,
                "ingestion_worker: final usearch save failed"
            );
        }
    }

    // Phase 6 / Task 14.2 (Req 10.5, 10.9): force-unload the embedding
    // model now that ingestion has finished, freeing RAM for the next
    // chat turn. Best-effort — a failure here just leaves the embedding
    // model warm for `keep_alive` (Ollama's default 5 minutes), which
    // is harmless. The frontend uses `UnloadingEmbedding` to clear any
    // "swap in progress" indicator.
    if let Err(e) = ollama.force_unload(&tier_config.embedding_model).await {
        warn!(
            model = %tier_config.embedding_model,
            error = %e,
            "ingestion_worker: post-job embedding unload failed (best-effort)"
        );
    }
    let _ = app.emit(
        "governor://embedding_swap",
        &EmbeddingSwapEvent {
            phase: EmbeddingSwapPhase::UnloadingEmbedding,
            chat_model: None,
        },
    );

    // Emit a final progress event with correct totals.
    let _ = app.emit(
        "rag://ingestion-progress",
        &IngestionProgressEvent {
            job_id: job_id.to_string(),
            chunks_done,
            chunks_total: chunks_done, // chunks_done is the true total at this point
            status: if job_error.is_some() {
                "failed".to_string()
            } else {
                "done".to_string()
            },
            current_file: String::new(),
        },
    );

    match job_error {
        Some(ref err) => {
            let _ = db::fail_ingestion_job(db, job_id, err).await;
            emit_complete(app, job_id, false, Some(err));
        }
        None => {
            let _ = db::complete_ingestion_job(db, job_id).await;
            emit_complete(app, job_id, true, None);
            info!(job_id = %job_id, chunks_done, "ingestion_worker: job complete");
        }
    }
}

/// Emit a rag://ingestion-complete event.
fn emit_complete(app: &AppHandle, job_id: &str, success: bool, error: Option<&str>) {
    let evt = IngestionCompleteEvent {
        job_id: job_id.to_string(),
        success,
        error: error.map(str::to_string),
    };
    if let Err(e) = app.emit("rag://ingestion-complete", &evt) {
        warn!(error = %e, "ingestion_worker: failed to emit complete event");
    }
}
