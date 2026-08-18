/// lib.rs — Heimdall Tauri application entry point
///
/// Declares all modules, defines shared AppState, registers Tauri commands,
/// and wires up the async runtime. All heavy logic lives in the modules.

pub mod adaptive_config;
pub mod catalog;
pub mod db;
pub mod governor;
pub mod memory;
pub mod model_registry;
pub mod models;
pub mod ollama_client;
pub mod rag_engine;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use adaptive_config::AppConfig;
use crate::catalog::ModelCatalog;
use crate::models::{HardwareTier, TierConfig};
use memory::MemoryEngine;
use model_registry::ModelRegistry;
use models::{ContextHint, ExtractionResult, HardwareInfo, MemoryFact, MemorySettings, ModelCapabilities, OllamaHealth, OllamaModel, ModelInfo, OllamaChatMessage, OllamaOptions};
use ollama_client::OllamaClient;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tokio::sync::mpsc;
use crate::rag_engine::RagEngine;
use crate::rag_engine::ingestion::{IngestionRequest, spawn_ingestion_worker};

// ---------------------------------------------------------------------------
// Frontend-facing types (not DB rows, not Ollama types)
// ---------------------------------------------------------------------------

/// User preferences returned to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UserPreferences {
    pub default_chat_model: Option<String>,
    pub default_vision_model: Option<String>,
    pub ollama_url: String,
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

/// Shared state injected into every Tauri command via `tauri::State`.
///
/// Wrapped in Arc<Mutex<_>> where mutation is needed. Read-only fields
/// are not wrapped.
pub struct AppState {
    pub db: SqlitePool,
    pub ollama: OllamaClient,
    pub hardware: HardwareInfo,
    pub tier_config: TierConfig,
    pub config: Arc<Mutex<AppConfig>>,
    pub registry: Arc<ModelRegistry>,
    /// Active streaming conversations. Each entry is a CancellationToken
    /// that, when cancelled, causes the corresponding `chat_stream` call to
    /// break its loop and return partial content.
    ///
    /// Entries are inserted at the start of `chat_stream` and removed on
    /// completion (success, error, or cancel). The HashMap key is the
    /// conversation_id string.
    pub active_streams: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub active_ingestions: Arc<Mutex<HashMap<String, Arc<Mutex<bool>>>>>,
    pub rag_engine: Arc<RagEngine>,
    pub ingestion_tx: mpsc::Sender<IngestionRequest>,
    pub memory_engine: Arc<MemoryEngine>,

    // ── Governor shared state ───────────────────────────────────────────
    /// Last-token timestamp per model name. Updated via `try_lock` from
    /// `chat_stream`. `std::sync::Mutex` is used — not `tokio::sync::Mutex` —
    /// because the chat-stream hot path must never `.await` while holding it.
    pub model_last_used:
        Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    /// Maps `conversation_id -> model_name` for in-flight chat streams.
    /// `std::sync::Mutex` so the `Drop` guard installed on the streaming
    /// path is synchronous — async `Drop` is not yet stable in Rust.
    pub active_stream_models:
        Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// `Some(name)` when the next `chat_stream` call for `name` should
    /// emit a `governor://embedding_swap { phase: ReloadingChat }` event
    /// before issuing `/api/chat`. Set by the ingestion worker after a
    /// Tier 1 embedding swap.
    pub chat_reload_pending: Arc<std::sync::Mutex<Option<String>>>,
    /// Set on the rising edge of `Critical`; the ingestion worker checks
    /// this at the top of its dequeue loop and sleeps 1s rather than
    /// pulling the next job. Cleared on the falling edge.
    pub ingestion_paused: Arc<AtomicBool>,
    /// The Governor itself. Constructed in `bootstrap()` after every
    /// other field. Polling runs on Tokio and cancels on window close.
    pub governor: Arc<governor::Governor>,
    /// Sole termination signal for the Governor's polling task.
    pub governor_cancel: CancellationToken,
    /// Curated catalog of well-known Ollama models, parsed once at
    /// `bootstrap()` from the bundled `resources/model_catalog.json`.
    /// Read by backend Tauri commands (`models_tab_list`) to compute
    /// hardware-aware recommendations.
    pub model_catalog: Arc<ModelCatalog>,
}

// ---------------------------------------------------------------------------
// StreamGuard — synchronous Drop guarantee for `active_stream_models`
// ---------------------------------------------------------------------------

/// RAII guard that removes a `conversation_id -> model_name` entry from
/// `active_stream_models` on drop.
///
/// Why an explicit guard rather than manual `.remove()` calls? The
/// chat_stream Tauri command has multiple termination paths — success,
/// error from the registry, error from the underlying stream, and a
/// panic anywhere in between. A guard makes removal automatic across
/// every one of them and guarantees cleanup on completion, error,
/// cancellation, or panic.
/// something we have to remember to type at every `return`.
///
/// The map is wrapped in `std::sync::Mutex` (not `tokio::sync::Mutex`)
/// precisely so this `Drop` impl can be synchronous — async `Drop` is
/// not yet stable in Rust, and `parking_lot` is not in our dependency
/// tree. Lock contention here is only with the Governor's polling-loop
/// `try_lock` reads, which give up on contention; the guard's `lock()`
/// will not deadlock.
struct StreamGuard {
    map: Arc<std::sync::Mutex<HashMap<String, String>>>,
    conversation_id: String,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        // Best-effort removal. If the mutex is poisoned (a panic with
        // the lock held — extremely unlikely, but possible) we still
        // reach in via `into_inner()` so the entry doesn't leak. On
        // contention `lock()` will block briefly; this is acceptable
        // here because the guard runs from a single chat_stream
        // termination, not the polling hot path.
        let mut map = match self.map.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.remove(&self.conversation_id);
    }
}

// ---------------------------------------------------------------------------
// chat_reload_pending consume helper
// ---------------------------------------------------------------------------

/// Consume a pending chat-reload signal.
///
/// The ingestion worker sets `*map = Some(chat_model)` after a Tier 1
/// embedding swap force-unloads the chat model. On the next
/// `chat_stream` turn for that same model, this helper clears the field
/// and returns `Some(model)` so the caller can emit a transparent
/// `governor://embedding_swap { phase: ReloadingChat }` event.
///
/// Behaviour table:
/// - `map == Some(model)` → set `*map = None`, return `Some(model.into())`.
/// - `map == Some(other)` → leave untouched, return `None`.
/// - `map == None`        → leave untouched, return `None`.
///
/// Extracted as a free function so the three branches can be unit-tested
/// without spinning up a Tauri app or an `AppState`.
fn consume_chat_reload_pending(
    map: &mut Option<String>,
    model: &str,
) -> Option<String> {
    if map.as_deref() == Some(model) {
        *map = None;
        Some(model.to_string())
    } else {
        None
    }
}



/// Check whether Ollama is running and return its version.
#[tauri::command]
async fn check_ollama_health(
    state: tauri::State<'_, AppState>,
) -> Result<OllamaHealth, String> {
    Ok(state.ollama.check_health().await)
}

/// List all locally available Ollama models.
#[tauri::command]
async fn list_models(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<OllamaModel>, String> {
    let models = state
        .registry
        .list_with_capabilities()
        .await
        .map_err(|e| e.to_string())?;

    // Collect names of entries whose capabilities are not yet known and
    // trigger background warm-up so they resolve before the next call.
    let uncached: Vec<String> = models
        .iter()
        .filter(|m| m.capabilities.is_none())
        .map(|m| m.name.clone())
        .collect();

    if !uncached.is_empty() {
        state.registry.warm_up(uncached);
    }

    Ok(models)
}

/// Get detailed information about a specific model.
#[tauri::command]
#[allow(deprecated)]
async fn get_model_info(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<ModelInfo, String> {
    state
        .ollama
        .get_model_info(&model_name)
        .await
        .map_err(|e| e.to_string())
}

/// Stream a chat completion. Tokens are emitted as `chat://token` events.
///
/// Returns the full assembled response text when streaming is complete.
/// Persists both the user message and assistant response to SQLite.
///
/// `context` is reserved for Phase 4 RAG retrieval and Phase 5 memory
/// injection. Today it's accepted and ignored.
#[tauri::command]
async fn chat_stream(
    app: tauri::AppHandle,
    conversation_id: String,
    model: String,
    messages: Vec<OllamaChatMessage>,
    options: Option<OllamaOptions>,
    context: Option<ContextHint>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    // FK guard: ensure the conversation row exists before any insert.
    // Frontend usually creates it via `create_conversation` first, but a
    // race or stale id from before a delete would otherwise hit a foreign
    // key violation. Idempotent: creates the row with the requested id only
    // if it's missing.
    let exists = db::conversation_exists(&state.db, &conversation_id)
        .await
        .map_err(|e| e.to_string())?;
    if !exists {
        db::create_conversation_with_id(&state.db, &conversation_id, &model)
            .await
            .map_err(|e| e.to_string())?;
    }

    // ── Consume chat_reload_pending ────────────────────────────────────
    //
    // The ingestion worker sets `chat_reload_pending = Some(model)`
    // immediately after force-unloading the chat model on a Tier 1
    // embedding swap. The next time the user issues a
    // chat turn against that same model, we emit a transparent
    // `governor://embedding_swap { phase: ReloadingChat }` event so the
    // frontend can render a brief "model reloading" indicator. The
    // pending field is cleared whether or not it matches — a stale entry
    // (e.g. user switched models in between) would otherwise sit forever.
    {
        let mut pending = state
            .chat_reload_pending
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let consumed = consume_chat_reload_pending(&mut pending, &model);
        // Drop the std::sync::Mutex guard before doing the emit so
        // we never hold a synchronous lock across an `.await`-style
        // boundary. Tauri's `emit` is synchronous but we keep the
        // discipline anyway.
        drop(pending);
        if let Some(reloaded_model) = consumed {
            use tauri::Emitter;
            let _ = app.emit(
                "governor://embedding_swap",
                &crate::models::EmbeddingSwapEvent {
                    phase: crate::models::EmbeddingSwapPhase::ReloadingChat,
                    chat_model: Some(reloaded_model),
                },
            );
        }
    }

    // Read thinking support from the registry (task 3.5). On error,
    // fall back to false (safe default — better to skip thinking than
    // crash with HTTP 400).
    let supports_thinking = match state.registry.get_capabilities(&model).await {
        Ok(caps) => caps.thinking,
        Err(e) => {
            warn!(
                model = %model,
                error = %e,
                "chat_stream: failed to read capabilities from registry, defaulting supports_thinking=false",
            );
            false
        }
    };

    // Register a fresh CancellationToken for this stream so the frontend
    // can abort via `cancel_chat_stream`. Removed in the finally block below.
    let cancel_token = CancellationToken::new();
    {
        let mut streams = state.active_streams.lock().await;
        streams.insert(conversation_id.clone(), cancel_token.clone());
    }

    // ── Register active stream model ───────────────────────────────────
    //
    // Insert (conversation_id, model_name) into `active_stream_models`
    // BEFORE forwarding the first chunk. The Governor's candidate selector
    // reads this map to enforce the streaming guard. Removal happens via
    // the `StreamGuard` Drop impl (below) so completion, error, cancellation,
    // AND panic all land in the same removal path.
    {
        let mut s = state
            .active_stream_models
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        s.insert(conversation_id.clone(), model.clone());
    }
    // The guard MUST live for the entire chat_stream lifetime — even
    // through early returns from the FK guard, the registry call, and
    // the pre-stream message assembly. We construct it now and drop it
    // implicitly when chat_stream returns.
    let _stream_guard = StreamGuard {
        map: state.active_stream_models.clone(),
        conversation_id: conversation_id.clone(),
    };

    // Persist the user message (last in the list) before streaming
    if let Some(user_msg) = messages.last() {
        if user_msg.role == "user" {
            // Serialize images to JSON if present. We surface the error
            // rather than silently storing "" — protects against a future
            // schema change making `images` non-nullable.
            let images_json = match user_msg.images.as_ref() {
                Some(imgs) => Some(
                    serde_json::to_string(imgs).map_err(|e| {
                        format!("Failed to serialize images: {}", e)
                    })?,
                ),
                None => None,
            };

            let input_type = if user_msg.images.is_some() { "image" } else { "text" };

            db::insert_message(
                &state.db,
                &conversation_id,
                "user",
                &user_msg.content,
                input_type,
                None,
                images_json.as_deref(),
                None, // users don't have thinking blocks
            )
            .await
            .map_err(|e| e.to_string())?;
        }
    }

    let mut messages_to_send = messages.clone();
    if let Some(ref ctx) = context {
        if let Some(ref collections) = ctx.rag_collections {
            if !collections.is_empty() {
                // Frontend may send either display names (from the picker)
                // or already-slugged ids (from `get_active_collections`).
                // Normalize to ids before retrieval.
                let collection_ids: Vec<String> = collections
                    .iter()
                    .filter_map(|name| crate::rag_engine::slug_id(name).ok())
                    .collect();

                if !collection_ids.is_empty() {
                    if let Some(user_msg) = messages.last() {
                        if user_msg.role == "user" && !user_msg.content.is_empty() {
                            let retrieved = crate::rag_engine::retrieval::retrieve(
                                &state.db,
                                &state.ollama,
                                &state.registry,
                                &state.tier_config,
                                &state.rag_engine.vectors_dir,
                                &user_msg.content,
                                &collection_ids,
                                state.tier_config.rag_top_k as usize,
                            ).await.map_err(|e| e.to_string())?;

                            if !retrieved.is_empty() {
                                let mut system_prompt = String::from("Use the following retrieved context to answer the user's question. If the context is irrelevant, ignore it and answer from general knowledge.\n\n");
                                let budget = 1228; // ~30% of 4096
                                let mut used_tokens = 0;
                                let mut chunks_kept = 0;

                                for r in retrieved.iter() {
                                    let chunk_tokens = r.chunk.token_count as u32;
                                    if used_tokens + chunk_tokens > budget {
                                        break;
                                    }
                                    system_prompt.push_str(&format!("--- Source: {} (chunk {}) ---\n{}\n\n", r.chunk.source_path, r.chunk.chunk_index, r.chunk.content));
                                    used_tokens += chunk_tokens;
                                    chunks_kept += 1;
                                }

                                tracing::info!("RAG Retrieval: retrieved {} chunks, kept {} chunks ({} tokens / {} budget)", retrieved.len(), chunks_kept, used_tokens, budget);

                                let system_msg = OllamaChatMessage {
                                    role: "system".to_string(),
                                    content: system_prompt,
                                    images: None,
                                    thinking: None,
                                };
                                messages_to_send.insert(0, system_msg);
                            }
                        }
                    }
                }
            }
        }
    }

    // --- Phase 5: Memory injection ---
    {
        let memory_enabled = context
            .as_ref()
            .and_then(|ctx| ctx.memory_enabled)
            .unwrap_or(true);

        if memory_enabled {
            // Determine if this is the first message in the conversation
            let is_first_message = messages.len() <= 1;

            if let Some(user_msg) = messages.last() {
                if user_msg.role == "user" {
                    // Resolve num_ctx for the active model. Falls back to a
                    // sensible 4096 when the user hasn't set a per-model
                    // override (the default in the future Models tab).
                    let num_ctx = state
                        .registry
                        .get_settings(&model)
                        .await
                        .ok()
                        .and_then(|s| s.num_ctx)
                        .unwrap_or(4096);

                    let memory_context = state.memory_engine
                        .build_injection_context(&conversation_id, &user_msg.content, is_first_message, num_ctx)
                        .await
                        .unwrap_or_default();

                    if !memory_context.is_empty() {
                        // Emit a memory transparency event so the frontend can
                        // show "Memory used" — exact text injected for this turn.
                        use tauri::Emitter;
                        let payload = serde_json::json!({
                            "conversation_id": conversation_id,
                            "memory_text": memory_context,
                            "num_ctx": num_ctx,
                        });
                        let _ = app.emit("chat://memory_used", payload);

                        let memory_msg = OllamaChatMessage {
                            role: "system".to_string(),
                            content: memory_context,
                            images: None,
                            thinking: None,
                        };
                        // Insert memory context BEFORE RAG context (position 0)
                        messages_to_send.insert(0, memory_msg);
                    }
                }
            }
        }
    }

    // ── On token closure ───────────────────────────────────────────────
    //
    // Builds a per-token closure that captures the `model_last_used`
    // Arc and the model name. On every successful answer chunk inside
    // `OllamaClient::chat_stream` the closure runs synchronously and updates
    // the timestamp via `try_lock`. On contention the update is dropped silently —
    // the polling loop tolerates one tick of stale idle data, but the chat-stream
    // hot path must NEVER block. The closure body never
    // touches `state.active_stream_models` or any tokio Mutex; it only
    // touches the std::sync::Mutex over the timestamp map.
    let on_token: Option<Box<dyn FnMut(&str) + Send>> = {
        let model_lu = state.model_last_used.clone();
        let model_name = model.clone();
        Some(Box::new(move |_token: &str| {
            if let Ok(mut map) = model_lu.try_lock() {
                map.insert(model_name.clone(), std::time::Instant::now());
            }
            // Contention path: drop the update silently.
        }))
    };

    let stream_result = state
        .ollama
        .chat_stream(
            &app,
            &conversation_id,
            &model,
            messages_to_send,
            options,
            supports_thinking,
            cancel_token,
            on_token,
        )
        .await;

    // Deregister the cancel token regardless of outcome.
    {
        let mut streams = state.active_streams.lock().await;
        streams.remove(&conversation_id);
    }

    let (full_text, think_text, tokens_used) = match stream_result {
        Ok(t) => t,
        Err(e) => {
            // Stream failed (or was cancelled with no content). The user
            // message is already persisted but we do NOT persist a placeholder
            // assistant message — the frontend shows a transient error banner
            // which is enough. Persisting errors would pollute conversation
            // history and get sent back to the model on the next turn.
            return Err(e.to_string());
        }
    };

    // Guard against empty responses (model OOM, clean exit with no tokens,
    // or cancelled before any content arrived).
    if full_text.trim().is_empty() {
        return Err(
            "Model returned an empty response. The model may have crashed, run out of memory, or the stream was cancelled before any content arrived."
                .to_string(),
        );
    }

    // Persist the assistant message to the database
    // thinking is stored as None if empty (no think block was present)
    let thinking_to_store = if think_text.is_empty() { None } else { Some(think_text.as_str()) };

    db::insert_message(
        &state.db,
        &conversation_id,
        "assistant",
        &full_text,
        "text",
        tokens_used.map(|t| t as i64),
        None,
        thinking_to_store,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(full_text)
}

/// Pull a model from the Ollama registry.
///
/// Progress is emitted as `model://pull-progress` events.
#[tauri::command]
async fn pull_model(
    app: tauri::AppHandle,
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state
        .ollama
        .pull_model(&app, &model_name)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a locally stored model.
#[tauri::command]
async fn delete_model(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state
        .ollama
        .delete_model(&model_name)
        .await
        .map_err(|e| e.to_string())
}

/// Return the detected hardware info and current tier config.
#[tauri::command]
async fn get_hardware_info(
    state: tauri::State<'_, AppState>,
) -> Result<HardwareInfo, String> {
    Ok(state.hardware.clone())
}

/// Create a new conversation and return its metadata.
#[tauri::command]
async fn create_conversation(
    model: String,
    state: tauri::State<'_, AppState>,
) -> Result<models::Conversation, String> {
    db::create_conversation(&state.db, &model)
        .await
        .map_err(|e| e.to_string())
}

/// List all conversations ordered by most recently updated.
#[tauri::command]
async fn list_conversations(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<models::Conversation>, String> {
    db::list_conversations(&state.db)
        .await
        .map_err(|e| e.to_string())
}

/// Fetch all messages for a given conversation in chronological order.
#[tauri::command]
async fn get_messages(
    conversation_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<models::Message>, String> {
    db::get_messages(&state.db, &conversation_id)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a conversation and all its messages.
#[tauri::command]
async fn delete_conversation(
    conversation_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    db::delete_conversation(&state.db, &conversation_id)
        .await
        .map_err(|e| e.to_string())
}

/// Update the title of a conversation.
#[tauri::command]
async fn update_conversation_title(
    conversation_id: String,
    title: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    db::update_conversation_title(&state.db, &conversation_id, &title)
        .await
        .map_err(|e| e.to_string())
}

/// Return user preferences relevant to the frontend (default model, etc).
#[tauri::command]
async fn get_user_preferences(
    state: tauri::State<'_, AppState>,
) -> Result<UserPreferences, String> {
    let config = state.config.lock().await;
    Ok(UserPreferences {
        default_chat_model: config.default_chat_model.clone(),
        default_vision_model: config.default_vision_model.clone(),
        ollama_url: config.ollama_url.clone(),
    })
}

/// Persist the user's default chat model selection to config.toml.
#[tauri::command]
async fn set_default_model(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Mutate inside the lock, snapshot, then release before async file IO.
    // Keeps concurrent `get_user_preferences` calls from waiting for fsync.
    let snapshot = {
        let mut config = state.config.lock().await;
        config.default_chat_model = Some(model_name);
        config.clone()
    };
    // lock dropped here

    let config_path = adaptive_config::config_path().map_err(|e| e.to_string())?;
    adaptive_config::write_config(&config_path, &snapshot)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Cancel an in-progress streaming chat turn.
///
/// Looks up the CancellationToken for `conversation_id` and calls `.cancel()`
/// on it. The `chat_stream` loop breaks within one chunk boundary (effectively
/// within 200 ms). Whatever content arrived before the cancel is persisted
/// to SQLite as a partial assistant message.
///
/// No-op if `conversation_id` is not currently streaming.
#[tauri::command]
async fn cancel_chat_stream(
    conversation_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let streams = state.active_streams.lock().await;
    if let Some(token) = streams.get(&conversation_id) {
        token.cancel();
        info!(conversation_id = %conversation_id, "cancel_chat_stream: token cancelled");
    } else {
        info!(conversation_id = %conversation_id, "cancel_chat_stream: no active stream, no-op");
    }
    Ok(())
}

/// Return the active per-tier configuration (chunk size, embedding model,
/// auto-unload threshold, RAG top-k, etc).
///
/// Used by the future Phase 4 Knowledge Base UI and Phase 6 Governor panel
/// to show what tier the app is running as and what settings derive from it.
#[tauri::command]
async fn get_tier_config(
    state: tauri::State<'_, AppState>,
) -> Result<TierConfig, String> {
    Ok(state.tier_config.clone())
}

/// Return the registry's capabilities for a single model.
///
/// Delegates to `ModelRegistry::get_capabilities`, dereferences the `Arc`
/// to a clone, and maps errors via `e.to_string()`.
#[tauri::command]
async fn get_model_capabilities(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<ModelCapabilities, String> {
    let arc = state
        .registry
        .get_capabilities(&model_name)
        .await
        .map_err(|e| e.to_string())?;
    Ok((*arc).clone())
}

/// Force re-detection of a model's capabilities.
///
/// Delegates to `ModelRegistry::refresh`, dereferences the `Arc` to a
/// clone, and maps errors via `e.to_string()`.
#[tauri::command]
async fn refresh_model_capabilities(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<ModelCapabilities, String> {
    let arc = state
        .registry
        .refresh(&model_name)
        .await
        .map_err(|e| e.to_string())?;
    Ok((*arc).clone())
}

// ---------------------------------------------------------------------------
// RAG Engine commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn rag_list_collections(state: tauri::State<'_, AppState>) -> Result<Vec<crate::models::Collection>, String> {
    state.rag_engine.list_collections().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn rag_create_collection(state: tauri::State<'_, AppState>, name: String) -> Result<crate::models::Collection, String> {
    state.rag_engine.create_collection(&name).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn rag_rename_collection(state: tauri::State<'_, AppState>, old_name: String, new_name: String) -> Result<crate::models::Collection, String> {
    state.rag_engine.rename_collection(&old_name, &new_name).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn rag_delete_collection(state: tauri::State<'_, AppState>, name: String) -> Result<(), String> {
    state.rag_engine.delete_collection(&name).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn rag_collection_stats(state: tauri::State<'_, AppState>, name: String) -> Result<crate::models::CollectionStats, String> {
    state.rag_engine.collection_stats(&name).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn rag_search_preview(state: tauri::State<'_, AppState>, name: String, query: String, k: usize) -> Result<Vec<crate::models::ChunkPreview>, String> {
    state.rag_engine.search_preview(&state.registry, &name, &query, k).await.map_err(|e| e.to_string())
}

/// Delete a single source (file path or URL) from a collection.
///
/// Removes all chunks for that source from SQLite, removes their vectors
/// from the usearch index, and deletes the associated ingestion job rows.
/// Returns the number of chunks removed.
#[tauri::command]
async fn rag_delete_source(
    state: tauri::State<'_, AppState>,
    collection: String,
    source_path: String,
) -> Result<u64, String> {
    state
        .rag_engine
        .delete_source(&collection, &source_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rag_ingest_paths(state: tauri::State<'_, AppState>, collection: String, paths: Vec<String>) -> Result<String, String> {
    // Slug the display name to the collection id used by the worker, so the
    // ingestion job's `collection` column matches `collections.id` and
    // retrieval/stats can find it.
    let collection_id = crate::rag_engine::slug_id(&collection).map_err(|e| e.to_string())?;

    // Friendly source label for the jobs list. Single source → its own
    // path. Multiple sources → a summary like "3 files: a.pdf, b.pdf, …"
    let source_label = match paths.len() {
        0 => String::new(),
        1 => paths[0].clone(),
        n => {
            let preview: Vec<String> = paths
                .iter()
                .take(2)
                .map(|p| {
                    std::path::Path::new(p)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| p.clone())
                })
                .collect();
            if n <= 2 {
                format!("{} files: {}", n, preview.join(", "))
            } else {
                format!("{} files: {}, …", n, preview.join(", "))
            }
        }
    };

    let job = crate::db::create_ingestion_job(&state.db, &source_label, &collection_id).await.map_err(|e| e.to_string())?;
    let job_id = job.id;

    let cancel_flag = Arc::new(tokio::sync::Mutex::new(false));
    {
        let mut cancels = state.active_ingestions.lock().await;
        cancels.insert(job_id.clone(), cancel_flag.clone());
    }

    let chat_model_hint = state.config.lock().await.default_chat_model.clone();

    let req = IngestionRequest {
        job_id: job_id.clone(),
        collection: collection_id,
        sources: paths,
        resume_from: None,
        cancel_flag,
        chat_model_hint,
    };
    state.ingestion_tx.send(req).await.map_err(|e| e.to_string())?;
    Ok(job_id)
}

#[tauri::command]
async fn rag_ingest_url(state: tauri::State<'_, AppState>, collection: String, url: String) -> Result<String, String> {
    let collection_id = crate::rag_engine::slug_id(&collection).map_err(|e| e.to_string())?;
    let job = crate::db::create_ingestion_job(&state.db, &url, &collection_id).await.map_err(|e| e.to_string())?;
    let job_id = job.id;

    let cancel_flag = Arc::new(tokio::sync::Mutex::new(false));
    {
        let mut cancels = state.active_ingestions.lock().await;
        cancels.insert(job_id.clone(), cancel_flag.clone());
    }

    let chat_model_hint = state.config.lock().await.default_chat_model.clone();

    let req = IngestionRequest {
        job_id: job_id.clone(),
        collection: collection_id,
        sources: vec![url],
        resume_from: None,
        cancel_flag,
        chat_model_hint,
    };
    state.ingestion_tx.send(req).await.map_err(|e| e.to_string())?;
    Ok(job_id)
}

#[tauri::command]
async fn rag_cancel_ingestion(state: tauri::State<'_, AppState>, job_id: String) -> Result<(), String> {
    {
        let cancels = state.active_ingestions.lock().await;
        if let Some(flag) = cancels.get(&job_id) {
            let mut f = flag.lock().await;
            *f = true;
        }
    }
    sqlx::query("UPDATE ingestion_jobs SET status = 'cancelled' WHERE id = ?")
        .bind(&job_id)
        .execute(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn rag_resume_ingestion(state: tauri::State<'_, AppState>, job_id: String) -> Result<(), String> {
    let job = sqlx::query_as::<_, crate::models::IngestionJob>("SELECT id, source_path, collection, status, chunks_total, chunks_done, error, created_at, completed_at FROM ingestion_jobs WHERE id = ?")
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(job) = job {
        if job.status.as_deref() == Some("running") { return Ok(()); }

        // Multi-file jobs store a summary label in `source_path`, not a
        // real path — they cannot be resumed today (we'd be passing the
        // label "3 files: a.pdf, …" as a source string and the dispatcher
        // would mark it Unsupported). Surface a clear error rather than
        // silently re-running with a bogus source.
        let source = job.source_path.clone().unwrap_or_default();
        let looks_like_multi_label = source.contains("files:");
        if looks_like_multi_label {
            return Err(
                "Resume isn't supported for multi-file jobs yet. Re-add the files to the collection."
                    .to_string(),
            );
        }

        // Reuse the existing cancel flag if one is still registered.
        // Replacing it would orphan any external handle the caller is
        // holding, so cancel-after-resume could no-op.
        let cancel_flag = {
            let mut cancels = state.active_ingestions.lock().await;
            match cancels.get(&job_id) {
                Some(existing) => {
                    // Reset the flag in case it was previously cancelled —
                    // resume implies "not cancelled anymore".
                    let mut f = existing.lock().await;
                    *f = false;
                    existing.clone()
                }
                None => {
                    let new_flag = Arc::new(tokio::sync::Mutex::new(false));
                    cancels.insert(job_id.clone(), new_flag.clone());
                    new_flag
                }
            }
        };

        let chat_model_hint = state.config.lock().await.default_chat_model.clone();

        let req = IngestionRequest {
            job_id: job_id.clone(),
            collection: job.collection.unwrap_or_default(),
            sources: vec![source],
            resume_from: Some(job.chunks_done),
            cancel_flag,
            chat_model_hint,
        };
        sqlx::query("UPDATE ingestion_jobs SET status = 'running' WHERE id = ?")
            .bind(&job_id)
            .execute(&state.db)
            .await
            .map_err(|e| e.to_string())?;

        state.ingestion_tx.send(req).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn rag_list_ingestion_jobs(state: tauri::State<'_, AppState>, collection: Option<String>) -> Result<Vec<crate::models::IngestionJob>, String> {
    let jobs = if let Some(c) = collection {
        // Slug at the boundary so the filter matches what's stored in DB.
        let collection_id = crate::rag_engine::slug_id(&c).map_err(|e| e.to_string())?;
        sqlx::query_as::<_, crate::models::IngestionJob>("SELECT id, source_path, collection, status, chunks_total, chunks_done, error, created_at, completed_at FROM ingestion_jobs WHERE collection = ? ORDER BY created_at DESC")
            .bind(&collection_id)
            .fetch_all(&state.db)
            .await
            .map_err(|e| e.to_string())?
    } else {
        sqlx::query_as::<_, crate::models::IngestionJob>("SELECT id, source_path, collection, status, chunks_total, chunks_done, error, created_at, completed_at FROM ingestion_jobs ORDER BY created_at DESC")
            .fetch_all(&state.db)
            .await
            .map_err(|e| e.to_string())?
    };
    Ok(jobs)
}

/// Persist the active RAG collections for a conversation.
///
/// Accepts an array of display names from the UI and stores collection ids
/// (slugs) so retrieval can look up `<id>.usearch` files directly.
#[tauri::command]
async fn set_active_collections(state: tauri::State<'_, AppState>, conversation_id: String, collections: Vec<String>) -> Result<(), String> {
    let ids: Result<Vec<String>, _> = collections
        .iter()
        .map(|name| crate::rag_engine::slug_id(name))
        .collect();
    let ids = ids.map_err(|e| e.to_string())?;
    let json_str = serde_json::to_string(&ids).map_err(|e| e.to_string())?;
    sqlx::query("UPDATE conversations SET active_rag_collections = ? WHERE id = ?")
        .bind(json_str)
        .bind(&conversation_id)
        .execute(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Return the active RAG collections for a conversation as display names.
///
/// Stored values are collection ids; this resolves them back through the
/// collections table so the UI's pill row shows what the user typed.
#[tauri::command]
async fn get_active_collections(state: tauri::State<'_, AppState>, conversation_id: String) -> Result<Vec<String>, String> {
    let row = sqlx::query("SELECT active_rag_collections FROM conversations WHERE id = ?")
        .bind(&conversation_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| e.to_string())?;

    let stored_ids: Vec<String> = if let Some(row) = row {
        use sqlx::Row;
        match row.try_get::<Option<String>, _>("active_rag_collections") {
            Ok(Some(json_str)) => serde_json::from_str::<Vec<String>>(&json_str).unwrap_or_default(),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };

    if stored_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Translate ids back to display names. Drop ids that no longer exist
    // (collection was deleted) — they would never retrieve anything anyway.
    let placeholders = stored_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let query_str = format!(
        "SELECT id, display_name FROM collections WHERE id IN ({})",
        placeholders
    );
    let mut q = sqlx::query(&query_str);
    for id in &stored_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(&state.db).await.map_err(|e| e.to_string())?;

    use sqlx::Row;
    let mut id_to_name = std::collections::HashMap::new();
    for r in rows {
        let id: String = r.try_get("id").map_err(|e| e.to_string())?;
        let display_name: String = r.try_get("display_name").map_err(|e| e.to_string())?;
        id_to_name.insert(id, display_name);
    }

    // Preserve the stored order.
    let names: Vec<String> = stored_ids
        .into_iter()
        .filter_map(|id| id_to_name.remove(&id))
        .collect();
    Ok(names)
}

// ---------------------------------------------------------------------------
// Phase 5: Memory commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn memory_extract(
    app: tauri::AppHandle,
    conversation_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ExtractionResult, String> {
    // Get the default chat model as the "loaded" hint
    let loaded_model = state.config.lock().await.default_chat_model.clone();
    let result = state.memory_engine
        .on_conversation_end(&conversation_id, loaded_model.as_deref())
        .await
        .map_err(|e| e.to_string())?;

    // Emit event so the frontend can react (e.g., show review banner)
    {
        use tauri::Emitter;
        let payload = serde_json::json!({
            "conversation_id": conversation_id,
            "facts_count": result.facts_extracted.len(),
            "episode_created": result.episode_created,
            "extraction_error": result.extraction_error,
            "episode_error": result.episode_error,
            "skipped_reason": result.skipped_reason,
        });
        let _ = app.emit("memory://extraction_complete", payload);
    }

    Ok(result)
}

#[tauri::command]
async fn memory_list_facts(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<MemoryFact>, String> {
    crate::db::list_all_memory_facts(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_confirm_fact(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Check hard cap before confirming
    let count = crate::db::get_confirmed_fact_count(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    if count >= 200 {
        return Err("Memory is full (200 confirmed facts). Please delete some facts before confirming new ones.".to_string());
    }
    crate::db::confirm_memory_fact(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_confirm_all(
    ids: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let count = crate::db::get_confirmed_fact_count(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    let remaining = 200u64.saturating_sub(count);
    if remaining == 0 {
        return Err("Memory is full (200 confirmed facts). Please delete some facts before confirming new ones.".to_string());
    }
    // Only confirm up to remaining capacity
    let to_confirm = ids.into_iter().take(remaining as usize).collect::<Vec<_>>();
    for id in &to_confirm {
        crate::db::confirm_memory_fact(&state.db, id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn memory_reject_fact(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::delete_memory_fact(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_reject_all(
    ids: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    for id in &ids {
        crate::db::delete_memory_fact(&state.db, id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn memory_edit_fact(
    id: String,
    text: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::update_memory_fact_text(&state.db, &id, &text)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_delete_fact(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::delete_memory_fact(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_delete_all_facts(
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::delete_all_memory_facts(&state.db)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_delete_all_episodes(
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::delete_all_memory_episodes(&state.db)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())?;
    // Also delete the _memories usearch index file
    let vectors_dir = crate::adaptive_config::vectors_dir().map_err(|e| e.to_string())?;
    let index_path = vectors_dir.join("_memories.usearch");
    if index_path.exists() {
        tokio::fs::remove_file(&index_path).await.ok();
    }
    Ok(())
}

#[tauri::command]
async fn memory_get_episode_count(
    state: tauri::State<'_, AppState>,
) -> Result<u64, String> {
    crate::db::get_memory_episode_count(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_get_settings(
    state: tauri::State<'_, AppState>,
) -> Result<MemorySettings, String> {
    let global_enabled = crate::db::get_memory_setting(&state.db, "global_enabled")
        .await
        .map_err(|e| e.to_string())?
        .map(|v| v == "true")
        .unwrap_or(true);
    let decay_threshold_days = crate::db::get_memory_setting(&state.db, "decay_threshold_days")
        .await
        .map_err(|e| e.to_string())?
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(90);
    let fact_count = crate::db::get_confirmed_fact_count(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    let episode_count = crate::db::get_memory_episode_count(&state.db)
        .await
        .map_err(|e| e.to_string())?;

    Ok(MemorySettings {
        global_enabled,
        decay_threshold_days,
        fact_count,
        episode_count,
    })
}

#[tauri::command]
async fn memory_update_settings(
    settings: MemorySettings,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::set_memory_setting(
        &state.db,
        "global_enabled",
        if settings.global_enabled { "true" } else { "false" },
    )
    .await
    .map_err(|e| e.to_string())?;
    crate::db::set_memory_setting(
        &state.db,
        "decay_threshold_days",
        &settings.decay_threshold_days.to_string(),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn memory_export_facts(
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let facts = crate::db::get_confirmed_memory_facts(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    let export: Vec<&str> = facts.iter().map(|f| f.fact.as_str()).collect();
    serde_json::to_string_pretty(&export).map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_set_conversation_memory(
    conv_id: String,
    enabled: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    crate::db::set_conversation_memory_enabled(&state.db, &conv_id, enabled)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn memory_get_conversation_memory(
    conv_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    crate::db::get_conversation_memory_enabled(&state.db, &conv_id)
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Governor / Models tab Tauri commands
// ---------------------------------------------------------------------------

/// Manually unload one model. Idempotent on a model that's not currently
/// loaded (Req 15.7). When `force` is true, any chat streams emitting
/// this model are cancelled before the unload is sent.
#[tauri::command]
async fn governor_unload_model(
    name: String,
    force: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Idempotency: nothing to do if the model isn't loaded.
    let loaded = state.governor.last_loaded_snapshot();
    if !loaded.iter().any(|m| m.name == name) {
        return Ok(());
    }

    // Streaming-guard: refuse without force.
    let streaming_models: HashSet<String> = state
        .active_stream_models
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    if streaming_models.contains(&name) && !force {
        return Err("currently_streaming".to_string());
    }

    if force {
        // Cancel every stream whose model matches `name`. We snapshot
        // the conv_id → model map first (synchronous lock) so we can
        // hold the active_streams tokio Mutex for the cancel pass
        // without holding both locks simultaneously.
        let conv_to_model: HashMap<String, String> = {
            let g = state
                .active_stream_models
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            g.clone()
        };
        let streams = state.active_streams.lock().await;
        for (conv, model_name) in conv_to_model.iter() {
            if model_name == &name {
                if let Some(token) = streams.get(conv) {
                    token.cancel();
                }
            }
        }
    }

    state
        .ollama
        .force_unload(&name)
        .await
        .map_err(|e| e.to_string())
}

/// Live-update the per-tier governor thresholds for the active session.
///
/// Validation: requires `warn_mb > unload_mb > critical_mb > 0` (Req 6.8).
/// The change applies only when `tier` matches the running tier — the
/// command is idempotent across other tiers' fields.
///
/// **Persistence note:** `AppConfig` does not currently carry per-tier
/// override fields, so the threshold change lasts for the session only
/// (until restart). Adding TOML persistence is a v1.1 candidate.
#[tauri::command]
async fn governor_set_thresholds(
    tier: HardwareTier,
    warn_mb: u64,
    unload_mb: u64,
    critical_mb: u64,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if !(warn_mb > unload_mb && unload_mb > critical_mb && critical_mb > 0) {
        return Err(
            "invalid thresholds: require warn > unload > critical > 0".to_string(),
        );
    }

    // Update the live tier_config the Governor reads on every tick.
    // Other tiers' fields are not touched; calling this for a tier that
    // is not currently active is a no-op against the live values (the
    // session-time persistence is per-tier, but `TierConfig` carries
    // only the active tier's numbers — see adaptive_config::build_tier_config).
    let mut tc = state.governor.tier_config.write().await;
    if tc.tier == tier {
        tc.governor_warn_mb = warn_mb;
        tc.governor_unload_mb = unload_mb;
        tc.governor_critical_mb = critical_mb;
    }
    Ok(())
}

/// Persist a per-model auto-unload toggle to `config.toml`.
#[tauri::command]
async fn governor_set_auto_unload_for_model(
    name: String,
    enabled: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let config_path = adaptive_config::config_path().map_err(|e| e.to_string())?;
    let snapshot = {
        let mut config = state.config.lock().await;
        config.auto_unload_per_model.insert(name, enabled);
        config.clone()
    };
    adaptive_config::write_config(&config_path, &snapshot)
        .await
        .map_err(|e| e.to_string())
}

/// Persist the global auto-unload master switch to `config.toml`.
#[tauri::command]
async fn governor_set_auto_unload_global(
    enabled: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let config_path = adaptive_config::config_path().map_err(|e| e.to_string())?;
    let snapshot = {
        let mut config = state.config.lock().await;
        config.auto_unload_enabled = Some(enabled);
        config.clone()
    };
    adaptive_config::write_config(&config_path, &snapshot)
        .await
        .map_err(|e| e.to_string())
}

/// Persist the user's tier override to `config.toml`. Takes effect on
/// next launch — the tier-specific defaults that derive from it live
/// across `TierConfig`, `AppState.hardware`, and the Governor's polling
/// thresholds. The Run 4 / 5 frontend surfaces a "restart recommended"
/// hint when the user changes this.
#[tauri::command]
async fn set_tier_override(
    tier: Option<HardwareTier>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let config_path = adaptive_config::config_path().map_err(|e| e.to_string())?;
    let snapshot = {
        let mut config = state.config.lock().await;
        config.tier_override = tier;
        config.clone()
    };
    adaptive_config::write_config(&config_path, &snapshot)
        .await
        .map_err(|e| e.to_string())
}

/// **Legendary feature (Task 28.1) — gated, default OFF.**
///
/// Predictive ingestion-pressure preview: given an estimated chunk count,
/// asks the Governor whether the embedding model can load alongside the
/// current default chat model and returns a traffic-light
/// `IngestionFitPreview` the Knowledge panel can render *before* the user
/// commits to an ingestion.
///
/// Gating: the feature is behind `AppConfig.legendary_predictive_preview`
/// which defaults to `Some(false)`. When the flag is off (or `None`) the
/// command returns a `status: "disabled"` payload with all MB fields `0`
/// and never touches the Governor — cheap and side-effect free.
///
/// `estimated_chunks` is accepted for forward-compatibility (a future
/// revision can refine the embedding-size estimate from the chunk count);
/// in this release the decision is driven by the Governor's cached
/// loaded-model snapshot and available-RAM reading, matching
/// `can_load_embedding` exactly (Req 10.1, 10.2).
#[tauri::command]
async fn governor_preview_ingestion(
    estimated_chunks: u64,
    state: tauri::State<'_, AppState>,
) -> Result<crate::models::IngestionFitPreview, String> {
    // `estimated_chunks` is reserved for a future size-refinement pass;
    // bind it so the parameter is documented and not dropped silently.
    let _ = estimated_chunks;

    // Feature gate: off by default. Treat `None` as off too.
    let enabled = {
        let cfg = state.config.lock().await;
        cfg.legendary_predictive_preview.unwrap_or(false)
    };
    if !enabled {
        return Ok(crate::models::IngestionFitPreview {
            status: "disabled".to_string(),
            embedding_mb: 0,
            chat_mb: 0,
            available_mb: 0,
            budget_mb: 0,
        });
    }

    let default_chat_model = {
        let cfg = state.config.lock().await;
        cfg.default_chat_model.clone()
    };

    Ok(state
        .governor
        .preview_embedding_fit(default_chat_model.as_deref())
        .await)
}

/// Persist the user's default vision model to `config.toml`.
#[tauri::command]
async fn set_default_vision_model(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let snapshot = {
        let mut config = state.config.lock().await;
        config.default_vision_model = Some(model_name);
        config.clone()
    };
    let config_path = adaptive_config::config_path().map_err(|e| e.to_string())?;
    adaptive_config::write_config(&config_path, &snapshot)
        .await
        .map_err(|e| e.to_string())
}

/// Persist the user's default embedding model to `config.toml` via the
/// existing `embedding_model` field on `AppConfig`. The session's
/// `TierConfig.embedding_model` is left unchanged — it picks up the new
/// value on next launch (rebuilding `TierConfig` mid-session would
/// invalidate cached vector indexes for any in-flight ingestion).
#[tauri::command]
async fn set_default_embedding_model(
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let snapshot = {
        let mut config = state.config.lock().await;
        config.embedding_model = model_name;
        config.clone()
    };
    let config_path = adaptive_config::config_path().map_err(|e| e.to_string())?;
    adaptive_config::write_config(&config_path, &snapshot)
        .await
        .map_err(|e| e.to_string())
}

/// Return the bundled curated model catalog. Frontend `PullPanel`
/// reads this once on mount and filters the entries client-side by
/// capability + the user's effective tier (Req 14.1, 14.2).
///
/// The catalog is parsed once at bootstrap and cached as
/// `Arc<ModelCatalog>` on `AppState`, so each call is a cheap clone of
/// the inner `Vec<CatalogEntry>` — no disk I/O.
#[tauri::command]
async fn models_catalog_list(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<crate::catalog::CatalogEntry>, String> {
    Ok(state.model_catalog.entries.clone())
}

/// One-row-per-model payload for the Models tab. Combines registry
/// capabilities, the most recent loaded snapshot from the polling loop,
/// the per-model last-used timestamp from this session, and the
/// hardware-aware recommendation.
#[tauri::command]
async fn models_tab_list(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<crate::models::ModelsTabRow>, String> {
    let models = state
        .registry
        .list_with_capabilities()
        .await
        .map_err(|e| e.to_string())?;

    let loaded = state.governor.last_loaded_snapshot();
    let loaded_names: HashSet<String> = loaded.iter().map(|m| m.name.clone()).collect();

    // Snapshot the model_last_used map under the synchronous lock so we
    // can iterate it cheaply per row. The map is small (one entry per
    // streamed model in this session), so cloning is fine.
    let model_lu: HashMap<String, std::time::Instant> = {
        let g = state
            .model_last_used
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        g.clone()
    };
    // Take `now` in two forms: `Instant::now()` for elapsed math, plus
    // the matching wall-clock so we can convert each `Instant` into a
    // Unix epoch second the frontend can format.
    let inst_now = std::time::Instant::now();
    let unix_now = chrono::Utc::now().timestamp();

    let rows: Vec<crate::models::ModelsTabRow> = models
        .into_iter()
        .map(|m| {
            let recommendation = crate::catalog::compute_recommendation(
                m.size / (1024 * 1024),
                &state.tier_config,
                &state.hardware,
            );
            let last_used_unix = model_lu.get(&m.name).map(|inst| {
                // Convert Instant → wall-clock by subtracting elapsed
                // from `now`. Saturating so a future `Instant` (rare:
                // can happen with monotonic clock weirdness) doesn't
                // underflow.
                let elapsed = inst_now.saturating_duration_since(*inst);
                unix_now.saturating_sub(elapsed.as_secs() as i64)
            });
            crate::models::ModelsTabRow {
                name: m.name.clone(),
                size: m.size,
                digest: m.digest,
                modified_at: m.modified_at,
                capabilities: m.capabilities,
                last_used_unix,
                currently_loaded: loaded_names.contains(&m.name),
                recommendation,
            }
        })
        .collect();
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Application bootstrap
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise tracing — writes to both stdout and a daily-rolling file
    // at ~/.heimdall/logs/heimdall.log so .desktop launches have something
    // to debug from. The file appender holds a worker thread; we keep its
    // guard alive for the process lifetime.
    let _log_guard = init_logging();

    info!("Heimdall starting up");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // Bootstrap async initialisation on the Tokio runtime.
            // If it fails, propagate the error so Tauri refuses to start.
            // The user sees the panic message in stderr / journal.
            tauri::async_runtime::block_on(async move {
                bootstrap(app_handle).await
            })?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Phase 6: cancel the Governor's polling token on window
            // close so the loop exits before the tokio runtime drops
            // (Req 1.5). Best-effort — if AppState isn't registered yet
            // (very early shutdown) there is nothing to cancel.
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                use tauri::Manager;
                if let Some(state) = window.try_state::<AppState>() {
                    state.governor_cancel.cancel();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            check_ollama_health,
            list_models,
            get_model_info,
            get_model_capabilities,
            refresh_model_capabilities,
            chat_stream,
            cancel_chat_stream,
            pull_model,
            delete_model,
            get_hardware_info,
            create_conversation,
            list_conversations,
            get_messages,
            delete_conversation,
            update_conversation_title,
            get_user_preferences,
            set_default_model,
            get_tier_config,
            rag_list_collections,
            rag_create_collection,
            rag_rename_collection,
            rag_delete_collection,
            rag_collection_stats,
            rag_search_preview,
            rag_delete_source,
            rag_ingest_paths,
            rag_ingest_url,
            rag_cancel_ingestion,
            rag_resume_ingestion,
            rag_list_ingestion_jobs,
            set_active_collections,
            get_active_collections,
            // Phase 5: Memory commands
            memory_extract,
            memory_list_facts,
            memory_confirm_fact,
            memory_confirm_all,
            memory_reject_fact,
            memory_reject_all,
            memory_edit_fact,
            memory_delete_fact,
            memory_delete_all_facts,
            memory_delete_all_episodes,
            memory_get_episode_count,
            memory_get_settings,
            memory_update_settings,
            memory_export_facts,
            memory_set_conversation_memory,
            memory_get_conversation_memory,
            // Phase 6: Governor / Models tab commands
            governor_unload_model,
            governor_set_thresholds,
            governor_set_auto_unload_for_model,
            governor_set_auto_unload_global,
            governor_preview_ingestion,
            set_tier_override,
            set_default_vision_model,
            set_default_embedding_model,
            models_tab_list,
            models_catalog_list,
        ])
        .run(tauri::generate_context!())
        .expect("Heimdall runtime error — check ~/.heimdall/logs/ for details");
}

/// Initialise logging to both stdout and a daily-rolling file at
/// `~/.heimdall/logs/heimdall.log`.
///
/// Returns a guard that must be kept alive for the duration of the process
/// (dropping it shuts down the file writer worker thread). Falls back to
/// stdout-only if the log directory can't be resolved or created.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // Resolve ~/.heimdall/logs synchronously; this runs before tokio is up.
    let log_dir = adaptive_config::heimdall_dir()
        .ok()
        .map(|d| d.join("logs"));

    let (file_layer, guard) = match log_dir {
        Some(dir) => {
            // Create dir if missing — best-effort, fall through if it fails.
            let _ = std::fs::create_dir_all(&dir);
            let appender = tracing_appender::rolling::daily(&dir, "heimdall.log");
            let (nb, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(nb)
                .with_ansi(false);
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    let stdout_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer);

    // Attach the file layer if we got one.
    match file_layer {
        Some(layer) => {
            registry.with(layer).init();
        }
        None => {
            registry.init();
        }
    }

    guard
}

/// Async bootstrap: ensure dirs, load config, detect hardware, open DB.
///
/// Builds AppState and registers it with the Tauri app handle so every
/// command can access it via `tauri::State<AppState>`.
async fn bootstrap(app: tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::Manager;

    // 1. Ensure ~/.heimdall/ directory tree exists
    adaptive_config::ensure_dirs().await?;

    // 2. Load or create config.toml
    let config_path = adaptive_config::config_path()?;
    let config = adaptive_config::load_config(&config_path).await;

    // 3. Detect hardware and assign tier
    let hardware = adaptive_config::detect_hardware(&config);
    let tier_config = adaptive_config::build_tier_config(&hardware, &config);

    info!(
        "Hardware tier: {:?} | RAM: {} MB | Cores: {}",
        hardware.effective_tier, hardware.total_ram_mb, hardware.cpu_cores
    );

    // 4. Open SQLite connection pool
    let db_path = adaptive_config::db_path()?;
    let pool = db::init_pool(&db_path).await?;

    // 5. Build the Ollama client
    let ollama = OllamaClient::new(&config.ollama_url);

    // 6. Build the model registry, hydrate from SQLite, and warm up.
    let registry = Arc::new(ModelRegistry::new(pool.clone(), ollama.clone()));

    // Hydrate is non-fatal: log on error and continue (Requirement 1.4).
    if let Err(e) = registry.hydrate().await {
        warn!(
            error = %e,
            "bootstrap: registry hydrate failed — starting with empty cache",
        );
    }

    // Best-effort warm-up: list currently installed models and pre-populate
    // the registry cache in the background. The warm_up call returns
    // immediately and does not block bootstrap.
    let model_names: Vec<String> = registry
        .ollama
        .list_tags_raw()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    if !model_names.is_empty() {
        registry.warm_up(model_names);
    }

    // 7. Initialize RAG Engine
    let heimdall_dir = adaptive_config::heimdall_dir()?;
    let vectors_dir = heimdall_dir.join("vectors");
    let knowledge_dir = heimdall_dir.join("knowledge");
    std::fs::create_dir_all(&vectors_dir)?;
    std::fs::create_dir_all(&knowledge_dir)?;
    
    let rag_engine = Arc::new(RagEngine::new(
        pool.clone(),
        ollama.clone(),
        tier_config.clone(),
        vectors_dir.clone(),
        knowledge_dir,
    ));

    // ── Phase 6: build the Governor's shared-state Arcs before the
    //            ingestion worker so the worker can observe them.
    //
    // The original ordering spawned the worker first; Run 3 / Task 14.2
    // moves it after Governor construction because the worker now reads
    // `ingestion_paused`, calls into `Governor::evaluate_embedding_fit`,
    // and writes `chat_reload_pending`. We construct everything the
    // worker needs, then the Governor (which also wants these clones),
    // then finally spawn the worker so the closure can move the Arcs.
    let active_streams: Arc<Mutex<HashMap<String, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let active_ingestions: Arc<Mutex<HashMap<String, Arc<Mutex<bool>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let model_last_used: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    let active_stream_models: Arc<std::sync::Mutex<HashMap<String, String>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    let chat_reload_pending: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let ingestion_paused: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let governor_cancel = CancellationToken::new();

    // The Governor reads `tier_config` through an `Arc<RwLock<_>>` so
    // user threshold edits via `governor_set_thresholds` (Run 5) take
    // effect without restart. AppState keeps its plain `TierConfig`
    // clone for read-only consumers (existing commands like
    // `get_tier_config`).
    let tier_config_shared: Arc<tokio::sync::RwLock<TierConfig>> =
        Arc::new(tokio::sync::RwLock::new(tier_config.clone()));
    let config_shared: Arc<Mutex<AppConfig>> = Arc::new(Mutex::new(config));

    let governor = Arc::new(governor::Governor::new(
        ollama.clone(),
        pool.clone(),
        tier_config_shared,
        hardware.clone(),
        config_shared.clone(),
        model_last_used.clone(),
        active_streams.clone(),
        active_stream_models.clone(),
        active_ingestions.clone(),
        chat_reload_pending.clone(),
        ingestion_paused.clone(),
        app.clone(),
    ));

    // Phase 6 / Task 13.2 + 14.2: pass the new Arcs through so the
    // worker can pause on Critical, route through `can_load_embedding`,
    // and signal `chat_reload_pending` after a Tier 1 swap.
    let ingestion_tx = spawn_ingestion_worker(
        app.clone(),
        pool.clone(),
        ollama.clone(),
        tier_config.clone(),
        vectors_dir.clone(),
        ingestion_paused.clone(),
        governor.clone(),
        chat_reload_pending.clone(),
    ).await;

    // 8. Register AppState
    let memory_engine = Arc::new(MemoryEngine::new(
        pool.clone(),
        ollama.clone(),
        tier_config.clone(),
        vectors_dir,
        registry.clone(),
    ));

    // ── Phase 6 / Task 16.1 — load the bundled model catalog ────────────
    //
    // Bundled at compile time via `include_str!` so the file path is
    // unambiguous regardless of how Heimdall was packaged (cargo run
    // from the source tree, AppImage, system install). The JSON is
    // small (~600 bytes) and parsed once; subsequent reads come from
    // the `Arc<ModelCatalog>` clone on `AppState`.
    const MODEL_CATALOG_JSON: &str =
        include_str!("../resources/model_catalog.json");
    let model_catalog: Arc<ModelCatalog> = Arc::new(
        serde_json::from_str(MODEL_CATALOG_JSON)
            .expect("model_catalog.json failed to parse — check resources/model_catalog.json"),
    );

    app.manage(AppState {
        db: pool,
        ollama,
        hardware,
        tier_config,
        config: config_shared,
        registry,
        active_streams,
        active_ingestions,
        rag_engine,
        ingestion_tx,
        memory_engine,
        // Phase 6:
        model_last_used,
        active_stream_models,
        chat_reload_pending,
        ingestion_paused,
        governor,
        governor_cancel,
        model_catalog,
    });

    // ── Phase 6 / Run 2 — spawn the Governor polling task ──────────────
    //
    // After AppState is registered (Req 1.1) we clone the Arc and the
    // cancellation token off the managed state and hand them to a
    // detached tokio task that drives the 2-second polling loop until
    // the token is cancelled. The task exits cleanly within 2200 ms of
    // cancellation (Req 1.4) — see governor.rs module docs for the
    // worst-case bound.
    {
        let governor_for_task = {
            let state: tauri::State<AppState> = app.state();
            state.governor.clone()
        };
        let cancel_for_task = {
            let state: tauri::State<AppState> = app.state();
            state.governor_cancel.clone()
        };
        tokio::spawn(async move {
            governor_for_task.run(cancel_for_task).await;
        });
    }

    // 9. Enforce minimum window size programmatically (config hint is not
    //    always respected by Linux window managers with decorations: false)
    if let Some(window) = app.get_webview_window("main") {
        use tauri::LogicalSize;
        let _ = window.set_min_size(Some(LogicalSize::new(800_f64, 500_f64)));
        // If the current size is below minimum, restore it
        if let Ok(size) = window.inner_size() {
            let scale = window.scale_factor().unwrap_or(1.0);
            let logical_h = size.height as f64 / scale;
            if logical_h < 500.0 {
                let _ = window.set_size(LogicalSize::new(1100_f64, 700_f64));
            }
        }
    }

    info!("Heimdall bootstrap complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 6 unit tests — StreamGuard Drop (Task 10.3) and the
// chat_reload_pending consume path (Task 15.2). These live inside lib.rs
// because both items are private to the crate.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod phase6_lib_tests {
    use super::{consume_chat_reload_pending, StreamGuard};
    use std::collections::HashMap;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Arc;

    // ── Task 10.3 — StreamGuard removes its entry on every termination
    //    path: completion, error, drop, and panic. ─────────────────────

    type StreamMap = Arc<std::sync::Mutex<HashMap<String, String>>>;

    fn fresh_map_with(conv: &str, model: &str) -> StreamMap {
        let mut m = HashMap::new();
        m.insert(conv.to_string(), model.to_string());
        Arc::new(std::sync::Mutex::new(m))
    }

    fn contains(map: &StreamMap, conv: &str) -> bool {
        map.lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(conv)
    }

    #[test]
    fn stream_guard_removes_entry_on_normal_completion() {
        let map = fresh_map_with("conv-1", "gemma3");
        {
            let _guard = StreamGuard {
                map: map.clone(),
                conversation_id: "conv-1".to_string(),
            };
            assert!(contains(&map, "conv-1"), "entry present while guard alive");
            // Normal completion path: guard drops at end of scope.
        }
        assert!(
            !contains(&map, "conv-1"),
            "StreamGuard must remove its entry on normal completion"
        );
    }

    #[test]
    fn stream_guard_removes_entry_on_early_error_return() {
        // Simulate a function that constructs the guard then returns Err
        // early — the guard's Drop must still fire.
        let map = fresh_map_with("conv-2", "llama3");
        fn errors_out(map: StreamMap) -> Result<(), String> {
            let _guard = StreamGuard {
                map,
                conversation_id: "conv-2".to_string(),
            };
            Err("simulated stream error".to_string())
        }
        let res = errors_out(map.clone());
        assert!(res.is_err());
        assert!(
            !contains(&map, "conv-2"),
            "StreamGuard must remove its entry on an error return path"
        );
    }

    #[test]
    fn stream_guard_removes_entry_on_explicit_drop() {
        let map = fresh_map_with("conv-3", "qwen3");
        let guard = StreamGuard {
            map: map.clone(),
            conversation_id: "conv-3".to_string(),
        };
        assert!(contains(&map, "conv-3"));
        drop(guard);
        assert!(
            !contains(&map, "conv-3"),
            "StreamGuard must remove its entry on explicit drop"
        );
    }

    #[test]
    fn stream_guard_removes_entry_on_panic() {
        // A panic between guard construction and the natural end of scope
        // must still unwind through the guard's Drop (Req 7.5).
        let map = fresh_map_with("conv-4", "phi4");
        let map_for_closure = map.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = StreamGuard {
                map: map_for_closure,
                conversation_id: "conv-4".to_string(),
            };
            panic!("simulated panic mid-stream");
        }));
        assert!(result.is_err(), "closure should have panicked");
        assert!(
            !contains(&map, "conv-4"),
            "StreamGuard must remove its entry even when the stream panics"
        );
    }

    #[test]
    fn stream_guard_only_removes_its_own_conversation() {
        // Two concurrent streams; dropping one guard must not disturb the
        // other's entry.
        let mut m = HashMap::new();
        m.insert("conv-a".to_string(), "gemma3".to_string());
        m.insert("conv-b".to_string(), "llama3".to_string());
        let map: StreamMap = Arc::new(std::sync::Mutex::new(m));
        {
            let _guard = StreamGuard {
                map: map.clone(),
                conversation_id: "conv-a".to_string(),
            };
        }
        assert!(!contains(&map, "conv-a"), "conv-a removed");
        assert!(contains(&map, "conv-b"), "conv-b untouched");
    }

    // ── Task 15.2 — consume_chat_reload_pending: 3 cases. ──────────────

    #[test]
    fn consume_chat_reload_pending_matches_emits_and_clears() {
        let mut map = Some("gemma3".to_string());
        let consumed = consume_chat_reload_pending(&mut map, "gemma3");
        assert_eq!(consumed.as_deref(), Some("gemma3"));
        assert!(map.is_none(), "pending must be cleared after a match");
    }

    #[test]
    fn consume_chat_reload_pending_mismatch_no_emit() {
        let mut map = Some("gemma3".to_string());
        let consumed = consume_chat_reload_pending(&mut map, "llama3");
        assert!(consumed.is_none(), "mismatch must not emit");
        assert_eq!(
            map.as_deref(),
            Some("gemma3"),
            "mismatch must leave the pending entry untouched"
        );
    }

    #[test]
    fn consume_chat_reload_pending_none_no_emit() {
        let mut map: Option<String> = None;
        let consumed = consume_chat_reload_pending(&mut map, "gemma3");
        assert!(consumed.is_none(), "None must not emit");
        assert!(map.is_none(), "None stays None");
    }
}
