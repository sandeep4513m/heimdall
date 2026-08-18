/// ollama_client.rs — All communication with the local Ollama instance
///
/// This module owns every HTTP call to Ollama. No other module makes raw
/// HTTP requests to Ollama. All types are defined in models.rs.
///
/// Streaming chat completions are emitted as Tauri events so the frontend
/// receives tokens in real time without polling.
///
/// Ollama base URL defaults to http://localhost:11434 and is configurable
/// via the app config.

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::models::{
    ModelCapability, ModelInfo, OllamaChatMessage, OllamaChatRequest,
    OllamaHealth, OllamaModel, OllamaOptions, PullProgressEvent, RunningModel,
    StreamTokenEvent, ThinkingEvent,
};

// ---------------------------------------------------------------------------
// Client struct
// ---------------------------------------------------------------------------

/// Stateless HTTP client wrapper for the Ollama API.
///
/// Cheap to clone — the inner `reqwest::Client` uses an `Arc` internally.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    client: Client,
    base_url: String,
}

impl OllamaClient {
    /// Create a new client pointing at the given base URL.
    ///
    /// `base_url` should be `"http://localhost:11434"` in production.
    pub fn new(base_url: impl Into<String>) -> Self {
        // ── Timeout strategy (industry standard for LLM streaming) ────────
        //
        // LLM inference is unbounded in duration: a thinking model on slow
        // hardware can reason for 10+ minutes before emitting the first
        // answer token. A fixed total-request timeout kills these streams.
        //
        // Instead we use:
        //   • connect_timeout — fail fast if Ollama isn't reachable (10s)
        //   • read_timeout    — kill the stream only if Ollama goes
        //                       completely silent for 5 minutes (indicates
        //                       a hang, OOM crash, or network drop)
        //   • NO total timeout — the stream lives as long as tokens flow
        //
        // This mirrors how OpenAI/Anthropic SDKs, LangChain, and llama.cpp
        // clients handle streaming: no wall-clock cap, only idle detection.
        // Works on all hardware tiers — a Raspberry Pi generating 0.5 tok/s
        // still sends bytes within the idle window.
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(300))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            base_url: base_url.into(),
        }
    }

    /// Return the base URL this client is pointed at.
    ///
    /// Exposed so callers that need to construct an Ollama URL outside
    /// this client (e.g. status pings from a separate task) don't have
    /// to duplicate the URL string.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

impl OllamaClient {
    /// Ping Ollama and return whether it is reachable plus its version string.
    ///
    /// This is a lightweight GET / — Ollama returns a plain text "Ollama is running".
    /// We also try /api/version to get the version number.
    #[instrument(skip(self))]
    pub async fn check_health(&self) -> OllamaHealth {
        // First check if the server is up at all
        let root_ok = self
            .client
            .get(format!("{}/", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);

        if !root_ok {
            return OllamaHealth {
                online: false,
                version: None,
            };
        }

        // Try to get the version
        let version = self.fetch_version().await.ok();

        OllamaHealth {
            online: true,
            version,
        }
    }

    async fn fetch_version(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct VersionResponse {
            version: String,
        }

        let resp: VersionResponse = self
            .client
            .get(format!("{}/api/version", self.base_url))
            .send()
            .await
            .context("GET /api/version failed")?
            .json()
            .await
            .context("Failed to parse version response")?;

        Ok(resp.version)
    }
}

// ---------------------------------------------------------------------------
// Model listing
// ---------------------------------------------------------------------------

/// Raw shape returned by GET /api/tags.
///
/// `pub` so the model-intelligence-registry (`model_registry.rs`) can
/// consume the unmodified `/api/tags` payload via `list_tags_raw` without
/// the legacy single-enum capability synthesis baked into
/// `OllamaClient::list_models`. The registry uses these entries directly
/// to compare cached digests against live ones during
/// `list_with_capabilities`.
#[derive(Debug, Clone, Deserialize)]
pub struct RawTagEntry {
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub modified_at: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<RawTagEntry>,
}

impl OllamaClient {
    /// Fetch the raw `/api/tags` response for the registry path with no
    /// capability synthesis applied.
    ///
    /// This is the entry point used by `ModelRegistry::list_with_capabilities`:
    /// the registry owns digest comparison and capability resolution against
    /// its own cache and SQLite tables, and only needs the unmodified
    /// `(name, size, digest, modified_at)` tuples from Ollama. The legacy
    /// `OllamaClient::list_models` path remains in place for one release per
    /// the migration plan and bakes in the single-enum
    /// `detect_capability_from_name` heuristic; do not use it from new code.
    ///
    /// Per Requirement 13.6, a per-request 5-second timeout is applied and
    /// the call does **not** retry. Timeouts and HTTP / JSON failures
    /// propagate to the caller as `Err`; the registry treats those errors
    /// as "Ollama unreachable" and serves cached entries instead
    /// (Requirement 13.1).
    #[instrument(skip(self))]
    pub async fn list_tags_raw(&self) -> Result<Vec<RawTagEntry>> {
        const TAGS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        let request = self.client.get(format!("{}/api/tags", self.base_url));

        let resp = match tokio::time::timeout(TAGS_TIMEOUT, request.send()).await {
            Ok(send_result) => send_result.context("GET /api/tags failed")?,
            Err(_) => {
                return Err(anyhow!(
                    "GET /api/tags timed out after {}s",
                    TAGS_TIMEOUT.as_secs()
                ));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Ollama /api/tags returned HTTP {}: {}",
                status,
                body
            ));
        }

        let parsed: TagsResponse = match tokio::time::timeout(TAGS_TIMEOUT, resp.json()).await {
            Ok(json_result) => json_result.context("Failed to parse /api/tags response")?,
            Err(_) => {
                return Err(anyhow!(
                    "Reading /api/tags body timed out after {}s",
                    TAGS_TIMEOUT.as_secs()
                ));
            }
        };

        debug!("/api/tags returned {} models", parsed.models.len());
        Ok(parsed.models)
    }

    /// List all locally available models with detected capabilities.
    #[allow(deprecated)]
    #[instrument(skip(self))]
    pub async fn list_models(&self) -> Result<Vec<OllamaModel>> {
        let resp: TagsResponse = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .context("GET /api/tags failed")?
            .json()
            .await
            .context("Failed to parse /api/tags response")?;

        let mut models = Vec::with_capacity(resp.models.len());
        for raw in resp.models {
            let capability = self.detect_capability_from_name(&raw.name);
            // `OllamaModel::capability` is `#[deprecated]` for one release per the
            // model-intelligence-registry migration plan (step 1). This call site is
            // the legacy `OllamaClient::list_models` path; task 3.4 replaces it with
            // `ModelRegistry::list_with_capabilities`, which populates `capabilities`
            // from the registry cache. Until then we suppress the deprecation warning
            // here only — every other reader (chat, frontend) reads `capabilities`.
            #[allow(deprecated)]
            models.push(OllamaModel {
                name: raw.name,
                size: raw.size,
                digest: raw.digest,
                modified_at: raw.modified_at,
                capabilities: None,
                capability,
            });
        }

        info!("Listed {} models", models.len());
        Ok(models)
    }

    /// Heuristic capability detection from the model name.
    ///
    /// Ollama's /api/show gives richer data but requires a separate call per
    /// model. For the list view we use name-based heuristics and upgrade to
    /// the full info only when the user selects a model.
    ///
    /// **Order matters.** Substring checks run top-to-bottom; the first match
    /// wins. We check more specific categories first (Embedding, Audio,
    /// Vision) before more general ones (Thinking) so that hybrid names like
    /// a hypothetical `gemma3-vision` are classified as Vision rather than
    /// Thinking. Substring matching is inherently a heuristic — for accurate
    /// classification of selected models, `detect_capability_from_template`
    /// upgrades the answer using the model's actual template.
    #[deprecated(note = "Use ModelRegistry::get_capabilities instead. Removed next release.")]
    fn detect_capability_from_name(&self, name: &str) -> ModelCapability {
        let lower = name.to_lowercase();

        // Embedding models
        if lower.contains("embed")
            || lower.contains("nomic")
            || lower.contains("minilm")
            || lower.contains("mxbai-embed")
            || lower.contains("snowflake")
            || lower.contains("bge-")
            || lower.contains("all-minilm")
        {
            return ModelCapability::Embedding;
        }

        // Audio models
        if lower.contains("whisper") {
            return ModelCapability::Audio;
        }

        // Vision models — checked BEFORE Thinking so a hybrid model
        // (e.g. hypothetical `gemma3-vision`) is correctly classified as
        // Vision rather than Thinking.
        if lower.contains("llava")
            || lower.contains("vision")
            || lower.contains("bakllava")
            || lower.contains("moondream")
            || lower.contains("minicpm-v")
            || lower.contains("cogvlm")
            || lower.contains("internvl")
            || lower.contains("qwen-vl")
            || lower.contains("qwen2-vl")
            || lower.contains("phi-3-vision")
            || lower.contains("phi3-vision")
        {
            return ModelCapability::Vision;
        }

        // Thinking models (expose native thinking or <think> reasoning blocks)
        // NOTE: Gemma 3 does NOT support thinking — only Gemma 4+ does.
        // Sending think:true to Gemma 3 causes Ollama HTTP 400.
        if lower.contains("deepseek-r1")
            || lower.contains("deepseek-r2")
            || lower.contains("qwen3")
            || lower.contains("qwq")
            || lower.contains("gemma4")
            || lower.contains("gemma-4")
        {
            return ModelCapability::Thinking;
        }

        ModelCapability::TextOnly
    }
}

// ---------------------------------------------------------------------------
// Model info
// ---------------------------------------------------------------------------

/// Raw shape returned by POST /api/show.
///
/// Public so the model-intelligence-registry (`model_registry.rs`) can
/// consume the unmodified Ollama response and run its own three-layer
/// detection without the legacy single-enum capability synthesis baked
/// into `OllamaClient::get_model_info`. All fields are optional with
/// `#[serde(default)]` because older Ollama versions omit `capabilities`
/// entirely and some endpoints return partial responses.
#[derive(Debug, Clone, Deserialize)]
pub struct ShowResponseRaw {
    #[serde(default)]
    pub modelfile: Option<String>,
    #[serde(default)]
    pub details: Option<ShowDetails>,
    #[serde(default)]
    pub template: Option<String>,
    /// Ollama reports model capabilities here since ~v0.5.
    /// Possible values: "completion", "vision", "tools", "thinking",
    /// "embedding". Absent (`None`) on older Ollama versions.
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
}

/// Sub-object inside `ShowResponseRaw.details` carrying model metadata.
/// Public for the same reason as `ShowResponseRaw`.
#[derive(Debug, Clone, Deserialize)]
pub struct ShowDetails {
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
}

// ---------------------------------------------------------------------------
// Raw /api/show accessor (consumed by model_registry.rs)
// ---------------------------------------------------------------------------

impl OllamaClient {
    /// Fetch the raw `/api/show` response for a model with no capability
    /// synthesis applied.
    ///
    /// This is the entry point used by `ModelRegistry`: the registry owns
    /// the three-layer detection chain (api_show → template → heuristic)
    /// and needs the unmodified Ollama payload — including the verbatim
    /// `capabilities` array for `raw_capabilities` storage and the
    /// `template` string for layer 2 fallback. The legacy
    /// `OllamaClient::get_model_info` path remains in place for one
    /// release per the migration plan and bakes in single-enum capability
    /// synthesis; do not use it from new code.
    ///
    /// Per Requirement 13.6, a per-request 5-second timeout is applied
    /// and the call does **not** retry. Timeouts and HTTP / JSON failures
    /// propagate to the caller as `Err`; the registry handles fallback to
    /// template inspection and the name heuristic at a higher level.
    #[instrument(skip(self), fields(model = %model_name))]
    pub async fn show(&self, model_name: &str) -> Result<ShowResponseRaw> {
        const SHOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        let body = serde_json::json!({ "name": model_name });
        let request = self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&body);

        let resp = match tokio::time::timeout(SHOW_TIMEOUT, request.send()).await {
            Ok(send_result) => send_result.with_context(|| {
                format!("POST /api/show failed for model '{}'", model_name)
            })?,
            Err(_) => {
                return Err(anyhow!(
                    "POST /api/show timed out after {}s for model '{}'",
                    SHOW_TIMEOUT.as_secs(),
                    model_name
                ));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Ollama /api/show returned HTTP {} for model '{}': {}",
                status,
                model_name,
                body
            ));
        }

        let parsed: ShowResponseRaw = match tokio::time::timeout(SHOW_TIMEOUT, resp.json()).await {
            Ok(json_result) => json_result.with_context(|| {
                format!("Failed to parse /api/show response for model '{}'", model_name)
            })?,
            Err(_) => {
                return Err(anyhow!(
                    "Reading /api/show body timed out after {}s for model '{}'",
                    SHOW_TIMEOUT.as_secs(),
                    model_name
                ));
            }
        };

        debug!(
            "/api/show for '{}': capabilities={:?}, has_template={}",
            model_name,
            parsed.capabilities,
            parsed.template.is_some()
        );
        Ok(parsed)
    }
}

impl OllamaClient {
    /// Fetch detailed information about a specific model.
    ///
    /// Uses /api/show which returns the modelfile, template, details, and
    /// (on modern Ollama) a `capabilities` array. Capability detection
    /// follows a three-layer priority:
    ///   1. capabilities array from /api/show (authoritative, dynamic)
    ///   2. template inspection ({{ .Images }}, {{ .Think }})
    ///   3. name-based heuristic (fallback for old Ollama)
    #[deprecated(note = "Use ModelRegistry::get_capabilities instead. Removed next release.")]
    #[allow(deprecated)]
    #[instrument(skip(self))]
    pub async fn get_model_info(&self, model_name: &str) -> Result<ModelInfo> {
        let body = serde_json::json!({ "name": model_name });

        let resp: ShowResponseRaw = self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST /api/show failed for model '{}'", model_name))?
            .json()
            .await
            .context("Failed to parse /api/show response")?;

        let details = resp.details.unwrap_or(ShowDetails {
            family: None,
            parameter_size: None,
            quantization_level: None,
        });

        let family = details.family.unwrap_or_default();
        let raw_caps = resp.capabilities.clone().unwrap_or_default();

        // Three-layer capability detection:
        // 1. Use Ollama's capabilities array if present (most reliable)
        // 2. Fall back to template inspection
        // 3. Fall back to name heuristic
        let capability = if !raw_caps.is_empty() {
            self.capability_from_ollama_array(&raw_caps)
        } else {
            self.detect_capability_from_template(
                model_name,
                resp.template.as_deref(),
                &family,
            )
        };

        info!(
            "Model '{}': capabilities={:?}, detected={:?}",
            model_name, raw_caps, capability
        );

        Ok(ModelInfo {
            name: model_name.to_string(),
            family,
            parameter_size: details.parameter_size.unwrap_or_default(),
            quantization_level: details.quantization_level.unwrap_or_default(),
            capability,
            template: resp.template,
            capabilities: raw_caps,
        })
    }

    /// Map Ollama's raw capabilities array to our ModelCapability enum.
    ///
    /// Priority: Embedding > Vision > Thinking > TextOnly.
    /// We check the most restrictive categories first.
    #[deprecated(note = "Use ModelRegistry::get_capabilities instead. Removed next release.")]
    fn capability_from_ollama_array(&self, caps: &[String]) -> ModelCapability {
        // Embedding models typically don't have "completion"
        // but let's check for explicit embedding signals
        if caps.iter().any(|c| c == "embedding") {
            return ModelCapability::Embedding;
        }
        if caps.iter().any(|c| c == "vision") {
            return ModelCapability::Vision;
        }
        if caps.iter().any(|c| c == "thinking") {
            return ModelCapability::Thinking;
        }
        ModelCapability::TextOnly
    }

    /// Query whether a model supports thinking, using /api/show.
    ///
    /// Returns true if the model's capabilities include "thinking".
    /// Falls back to template inspection, then name heuristic if /api/show
    /// doesn't return capabilities (old Ollama).
    /// Returns false on any error (fail-safe — better to skip thinking
    /// than to crash with HTTP 400).
    ///
    /// NOTE: Kept for migration step 1 backward compatibility. The streaming
    /// hot path now receives `supports_thinking` from the registry via the
    /// caller. This method will be removed in the next release.
    #[deprecated(note = "Use ModelRegistry::get_capabilities instead. Removed next release.")]
    #[allow(dead_code)]
    #[allow(deprecated)]
    async fn model_supports_thinking(&self, model: &str) -> bool {
        let body = serde_json::json!({ "name": model });

        let resp = match self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to query /api/show for '{}': {} — falling back to name heuristic", model, e);
                return self.detect_capability_from_name(model) == ModelCapability::Thinking;
            }
        };

        let show: ShowResponseRaw = match resp.json().await {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to parse /api/show for '{}': {} — falling back to name heuristic", model, e);
                return self.detect_capability_from_name(model) == ModelCapability::Thinking;
            }
        };

        // Layer 1: capabilities array (authoritative)
        if let Some(ref caps) = show.capabilities {
            if !caps.is_empty() {
                let supports = caps.iter().any(|c| c == "thinking");
                debug!("Model '{}' capabilities={:?}, supports_thinking={}", model, caps, supports);
                return supports;
            }
        }

        // Layer 2: template inspection
        if let Some(ref tmpl) = show.template {
            if tmpl.contains(".Think") {
                debug!("Model '{}' template contains .Think — supports thinking", model);
                return true;
            }
        }

        // Layer 3: name heuristic (last resort)
        let fallback = self.detect_capability_from_name(model) == ModelCapability::Thinking;
        debug!("Model '{}' no capabilities/template signal — name heuristic={}", model, fallback);
        fallback
    }

    /// Derive capability from the model template and family string.
    ///
    /// The template often contains `{{ .Images }}` for vision models.
    /// We also fall back to name-based heuristics.
    #[deprecated(note = "Use ModelRegistry::get_capabilities instead. Removed next release.")]
    #[allow(deprecated)]
    fn detect_capability_from_template(
        &self,
        name: &str,
        template: Option<&str>,
        family: &str,
    ) -> ModelCapability {
        // Template-based detection is most reliable
        if let Some(tmpl) = template {
            if tmpl.contains("{{ .Images }}") || tmpl.contains("{{.Images}}") {
                return ModelCapability::Vision;
            }
        }

        // Family-based detection
        let family_lower = family.to_lowercase();
        if family_lower.contains("clip")
            || family_lower.contains("llava")
            || family_lower.contains("vision")
        {
            return ModelCapability::Vision;
        }
        if family_lower.contains("whisper") {
            return ModelCapability::Audio;
        }
        if family_lower.contains("bert")
            || family_lower.contains("nomic")
            || family_lower.contains("embed")
        {
            return ModelCapability::Embedding;
        }

        // Fall back to name heuristics
        self.detect_capability_from_name(name)
    }
}

// ---------------------------------------------------------------------------
// Streaming chat
// ---------------------------------------------------------------------------

impl OllamaClient {
    /// Stream a chat completion to the frontend via Tauri events.
    ///
    /// Emits `chat://token` events for each answer token fragment.
    /// Emits `chat://thinking` events for content inside <think>…</think> blocks.
    /// Emits a final `chat://token` event with `done: true` when complete.
    ///
    /// Thinking block detection is dynamic — triggered by <think> tags in the
    /// token stream. No model name hardcoding here; detection happens upstream
    /// in detect_capability_from_name() but the parser handles any model that
    /// emits <think> tags regardless of capability classification.
    ///
    /// Returns the full assembled answer text, the full thinking content, and
    /// total token count.
    ///
    /// `supports_thinking` is provided by the caller (typically from the
    /// model registry). The client no longer calls `model_supports_thinking()`
    /// internally on the streaming path.
    ///
    /// `cancel_token` is checked on every stream chunk. When cancelled, the
    /// loop breaks immediately and returns partial content. The caller
    /// (`chat_stream` command in lib.rs) persists the partial response.
    #[instrument(skip(self, app, messages, cancel_token, on_token))]
    pub async fn chat_stream(
        &self,
        app: &AppHandle,
        conversation_id: &str,
        model: &str,
        messages: Vec<OllamaChatMessage>,
        options: Option<OllamaOptions>,
        supports_thinking: bool,
        cancel_token: CancellationToken,
        on_token: Option<Box<dyn FnMut(&str) + Send>>,
    ) -> Result<(String, String, Option<u32>)> {
        // Make on_token mutable inside this function so the per-token
        // closure (set up by the caller in lib.rs::chat_stream — Task 10.2)
        // can update `model_last_used` on every successful answer chunk.
        // Phase 6 hooks live here (Req 7.2): only invoked on user-facing
        // answer tokens, never on thinking-block content.
        let mut on_token = on_token;
        // Pull keep_alive out of options for top-level placement on the
        // request; Ollama accepts it both at top-level and inside options
        // but top-level is the documented form.
        let keep_alive = options.as_ref().and_then(|o| o.keep_alive.clone());

        // ── Dynamic thinking detection ─────────────────────────────────────
        //
        // `supports_thinking` is now passed in by the caller (from the model
        // registry) rather than queried inline via `model_supports_thinking`.
        let mut think = if supports_thinking { Some(true) } else { None };

        info!("Model '{}': supports_thinking={}", model, supports_thinking);

        // ── Request + retry loop ──────────────────────────────────────────
        //
        // Safety net: if we guessed wrong and Ollama returns HTTP 400
        // complaining the model doesn't support thinking, we automatically
        // retry without think:true. The user never sees an error.
        let response = loop {
            let request = OllamaChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                stream: true,
                options: options.clone(),
                think,
                keep_alive: keep_alive.clone(),
                format: None,
            };

            let resp = self
                .client
                .post(format!("{}/api/chat", self.base_url))
                .json(&request)
                .send()
                .await
                .context("POST /api/chat failed — model may be loading or request timed out")?;

            if resp.status() == reqwest::StatusCode::BAD_REQUEST {
                let body = resp.text().await.unwrap_or_default();

                // If we sent think:true and Ollama rejected it, retry without.
                if think.is_some() && body.contains("does not support thinking") {
                    warn!(
                        "Model '{}' rejected think:true — retrying without. \
                         Capability detection will improve as Ollama adds more metadata.",
                        model
                    );
                    think = None;
                    continue;
                }

                return Err(anyhow!("Ollama returned HTTP 400: {}", body));
            }

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(anyhow!("Ollama returned HTTP {}: {}", status, body));
            }

            break resp;
        };

        let mut stream = response.bytes_stream();

        // ── Byte buffer for the NDJSON stream ─────────────────────────────────
        //
        // reqwest::bytes_stream() returns chunks at TCP boundaries, NOT at
        // UTF-8 character boundaries. A multi-byte char (CJK, emoji) split
        // across two chunks would crash str::from_utf8.
        //
        // Fix: accumulate raw bytes; split on `\n` (Ollama's NDJSON format
        // guarantees newline-terminated JSON objects); decode UTF-8 only on
        // each complete line, where the boundary is guaranteed safe.
        let mut byte_buf: Vec<u8> = Vec::with_capacity(8192);

        // ── Thinking block state ──────────────────────────────────────────────
        //
        // Ollama v0.9+ natively parses <think> tags on the server and streams
        // reasoning tokens in a dedicated `message.thinking` field. We read
        // this field directly (primary path). If the native field is never
        // populated — older Ollama or non-Ollama endpoints — we fall back to
        // the tag parser which scans `content` for <think>…</think> tags.
        let mut native_thinking_detected = false;
        let mut thinking_finished = false;
        let mut think_content = String::new();
        let mut answer_content = String::new();

        // Fallback tag parser state (only used if native field is absent)
        let mut in_think_block = false;
        let mut tag_buf = String::new();
        // Cap on the tag-parser fallback buffer. If a malformed model emits
        // `<think>` and never closes it, we'd grow tag_buf without bound.
        // 1 MB is generous for any well-behaved thinking response.
        const TAG_BUF_CAP: usize = 1_048_576;
        let mut tag_buf_overflow = false;

        let mut total_tokens: Option<u32> = None;

        // ── Cancellation + stream loop ────────────────────────────────────────
        //
        // `tokio::select!` races the next stream chunk against the cancel
        // signal. On cancel, we break the outer loop and return whatever
        // content has been assembled. The caller persists the partial response.
        'stream: loop {
            let chunk_result = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    debug!("chat_stream: cancel token fired, breaking stream loop");
                    break 'stream;
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(r) => r,
                        None => break 'stream,
                    }
                }
            };

            let chunk = chunk_result.context("Error reading stream chunk")?;
            byte_buf.extend_from_slice(&chunk);

            // Process every complete line in the buffer. Trailing partial
            // line stays in byte_buf for the next iteration.
            loop {
                let newline_pos = match byte_buf.iter().position(|&b| b == b'\n') {
                    Some(p) => p,
                    None => break,
                };

                // Drain the line including the newline.
                let line_bytes: Vec<u8> = byte_buf.drain(..=newline_pos).collect();
                let line = match std::str::from_utf8(&line_bytes) {
                    Ok(s) => s.trim(),
                    Err(e) => {
                        warn!("Skipping non-UTF-8 stream line: {}", e);
                        continue;
                    }
                };

                if line.is_empty() {
                    continue;
                }

                let parsed = match serde_json::from_str::<crate::models::OllamaChatChunk>(line) {
                    Ok(p) => p,
                    Err(e) => {
                        // Ollama occasionally emits non-JSON status lines
                        // during model loading; warn but don't abort.
                        warn!("Failed to parse stream line '{}': {}", line, e);
                        continue;
                    }
                };

                let content_token = parsed.message.content.clone();
                let think_token = parsed.message.thinking
                    .as_deref()
                    .unwrap_or("")
                    .to_string();

                if parsed.done {
                    total_tokens = parsed.eval_count;
                }

                // ── Primary path: native thinking field ──────────────
                if !think_token.is_empty() {
                    native_thinking_detected = true;
                    think_content.push_str(&think_token);

                    let evt = ThinkingEvent {
                        conversation_id: conversation_id.to_string(),
                        content: think_token,
                        done: false,
                    };
                    if let Err(e) = app.emit("chat://thinking", &evt) {
                        warn!("Failed to emit thinking event: {}", e);
                    }
                }

                if !content_token.is_empty() {
                    // State transition: first content token after thinking
                    if native_thinking_detected && !thinking_finished {
                        thinking_finished = true;
                        let done_evt = ThinkingEvent {
                            conversation_id: conversation_id.to_string(),
                            content: String::new(),
                            done: true,
                        };
                        if let Err(e) = app.emit("chat://thinking", &done_evt) {
                            warn!("Failed to emit thinking done event: {}", e);
                        }
                    }

                    if native_thinking_detected {
                        // Native path: content is already the clean answer
                        answer_content.push_str(&content_token);
                        // Phase 6 — Task 10.1: invoke the per-token hook on
                        // every non-error answer chunk BEFORE the frontend
                        // emit. Used by `chat_stream` (lib.rs) to refresh
                        // `model_last_used`. Thinking tokens are excluded
                        // (handled in the `!think_token.is_empty()` branch
                        // above, which deliberately does not call this).
                        if let Some(cb) = on_token.as_mut() {
                            cb(&content_token);
                        }
                        let evt = StreamTokenEvent {
                            conversation_id: conversation_id.to_string(),
                            token: content_token,
                            done: false,
                            tokens_used: None,
                        };
                        if let Err(e) = app.emit("chat://token", &evt) {
                            warn!("Failed to emit token event: {}", e);
                        }
                    } else if !tag_buf_overflow {
                        // ── Fallback: tag parser on content ───────────
                        // Only reached if Ollama never sent the native
                        // thinking field (older version or non-Ollama).
                        tag_buf.push_str(&content_token);

                        // Cap-check before processing. If the buffer has
                        // ballooned (model emitted `<think>` but never
                        // closed it), force-close the thinking block and
                        // treat all subsequent content as answer text.
                        if tag_buf.len() > TAG_BUF_CAP {
                            warn!(
                                "Tag-parser buffer exceeded {} bytes; force-closing thinking block",
                                TAG_BUF_CAP
                            );
                            tag_buf_overflow = true;
                            // If we were inside a think block, force-close it.
                            if in_think_block {
                                think_content.push_str(&tag_buf);
                                let done_evt = ThinkingEvent {
                                    conversation_id: conversation_id.to_string(),
                                    content: String::new(),
                                    done: true,
                                };
                                let _ = app.emit("chat://thinking", &done_evt);
                                in_think_block = false;
                            } else {
                                answer_content.push_str(&tag_buf);
                                if let Some(cb) = on_token.as_mut() {
                                    cb(&tag_buf);
                                }
                                let evt = StreamTokenEvent {
                                    conversation_id: conversation_id.to_string(),
                                    token: tag_buf.clone(),
                                    done: false,
                                    tokens_used: None,
                                };
                                let _ = app.emit("chat://token", &evt);
                            }
                            tag_buf.clear();
                        } else {
                            loop {
                                if in_think_block {
                                    if let Some(end_pos) = tag_buf.find("</think>") {
                                        let chunk_content = tag_buf[..end_pos].to_string();
                                        if !chunk_content.is_empty() {
                                            think_content.push_str(&chunk_content);
                                            let evt = ThinkingEvent {
                                                conversation_id: conversation_id.to_string(),
                                                content: chunk_content,
                                                done: false,
                                            };
                                            if let Err(e) = app.emit("chat://thinking", &evt) {
                                                warn!("Failed to emit thinking event: {}", e);
                                            }
                                        }
                                        let done_evt = ThinkingEvent {
                                            conversation_id: conversation_id.to_string(),
                                            content: String::new(),
                                            done: true,
                                        };
                                        if let Err(e) = app.emit("chat://thinking", &done_evt) {
                                            warn!("Failed to emit thinking done event: {}", e);
                                        }
                                        in_think_block = false;
                                        tag_buf = tag_buf[end_pos + 8..].to_string();
                                    } else if tag_buf.len() > 16 {
                                        let mut safe_len = tag_buf.len() - 8;
                                        while safe_len > 0 && !tag_buf.is_char_boundary(safe_len) {
                                            safe_len -= 1;
                                        }
                                        let chunk_content = tag_buf[..safe_len].to_string();
                                        think_content.push_str(&chunk_content);
                                        let evt = ThinkingEvent {
                                            conversation_id: conversation_id.to_string(),
                                            content: chunk_content,
                                            done: false,
                                        };
                                        if let Err(e) = app.emit("chat://thinking", &evt) {
                                            warn!("Failed to emit thinking event: {}", e);
                                        }
                                        tag_buf = tag_buf[safe_len..].to_string();
                                        break;
                                    } else {
                                        break;
                                    }
                                } else if let Some(start_pos) = tag_buf.find("<think>") {
                                    let pre = tag_buf[..start_pos].to_string();
                                    if !pre.is_empty() {
                                        answer_content.push_str(&pre);
                                        if let Some(cb) = on_token.as_mut() {
                                            cb(&pre);
                                        }
                                        let evt = StreamTokenEvent {
                                            conversation_id: conversation_id.to_string(),
                                            token: pre,
                                            done: false,
                                            tokens_used: None,
                                        };
                                        if let Err(e) = app.emit("chat://token", &evt) {
                                            warn!("Failed to emit token event: {}", e);
                                        }
                                    }
                                    in_think_block = true;
                                    tag_buf = tag_buf[start_pos + 7..].to_string();
                                } else if tag_buf.len() > 7 {
                                    let mut safe_len = tag_buf.len() - 7;
                                    while safe_len > 0 && !tag_buf.is_char_boundary(safe_len) {
                                        safe_len -= 1;
                                    }
                                    let answer_chunk = tag_buf[..safe_len].to_string();
                                    answer_content.push_str(&answer_chunk);
                                    if let Some(cb) = on_token.as_mut() {
                                        cb(&answer_chunk);
                                    }
                                    let evt = StreamTokenEvent {
                                        conversation_id: conversation_id.to_string(),
                                        token: answer_chunk,
                                        done: false,
                                        tokens_used: None,
                                    };
                                    if let Err(e) = app.emit("chat://token", &evt) {
                                        warn!("Failed to emit token event: {}", e);
                                    }
                                    tag_buf = tag_buf[safe_len..].to_string();
                                    break;
                                } else {
                                    break;
                                }
                            }
                        }
                    } else {
                        // Tag-buf overflowed earlier — pass content straight through
                        answer_content.push_str(&content_token);
                        if let Some(cb) = on_token.as_mut() {
                            cb(&content_token);
                        }
                        let evt = StreamTokenEvent {
                            conversation_id: conversation_id.to_string(),
                            token: content_token,
                            done: false,
                            tokens_used: None,
                        };
                        let _ = app.emit("chat://token", &evt);
                    }
                }

                if parsed.done {
                    // If native thinking was detected but never finished
                    // (edge case: stream ends while still thinking)
                    if native_thinking_detected && !thinking_finished {
                        let done_evt = ThinkingEvent {
                            conversation_id: conversation_id.to_string(),
                            content: String::new(),
                            done: true,
                        };
                        if let Err(e) = app.emit("chat://thinking", &done_evt) {
                            warn!("Failed to emit thinking done event: {}", e);
                        }
                    }

                    // Flush tag parser buffer (fallback path only)
                    // The tag parser holds back up to 7 bytes as lookahead
                    // for `<think>` detection. On stream end, these bytes
                    // must be emitted to the frontend AND added to
                    // answer_content. Without the emit, the last few
                    // characters of every non-thinking response are lost
                    // from the UI (though persisted to DB via answer_content).
                    if !native_thinking_detected && !tag_buf.is_empty() && !in_think_block {
                        answer_content.push_str(&tag_buf);
                        if let Some(cb) = on_token.as_mut() {
                            cb(&tag_buf);
                        }
                        let flush_evt = StreamTokenEvent {
                            conversation_id: conversation_id.to_string(),
                            token: tag_buf.clone(),
                            done: false,
                            tokens_used: None,
                        };
                        if let Err(e) = app.emit("chat://token", &flush_evt) {
                            warn!("Failed to emit tag-buf flush token event: {}", e);
                        }
                        tag_buf.clear();
                    }

                    // Emit final done event
                    let final_evt = StreamTokenEvent {
                        conversation_id: conversation_id.to_string(),
                        token: String::new(),
                        done: true,
                        tokens_used: total_tokens,
                    };
                    if let Err(e) = app.emit("chat://token", &final_evt) {
                        warn!("Failed to emit final token event: {}", e);
                    }

                    debug!("Stream complete. Tokens used: {:?}", total_tokens);
                    // Drain any remaining bytes (none expected, but be tidy)
                    byte_buf.clear();
                    break 'stream;
                }
            }
        } // end 'stream

        Ok((answer_content, think_content, total_tokens))
    }
}

// ---------------------------------------------------------------------------
// Model management
// ---------------------------------------------------------------------------

impl OllamaClient {
    /// Pull a model from the Ollama registry, streaming progress events.
    ///
    /// Emits `model://pull-progress` events with download progress.
    /// The frontend can display a progress bar using these events.
    #[instrument(skip(self, app))]
    pub async fn pull_model(&self, app: &AppHandle, model_name: &str) -> Result<()> {
        let body = serde_json::json!({
            "name": model_name,
            "stream": true
        });

        let response = self
            .client
            .post(format!("{}/api/pull", self.base_url))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST /api/pull failed for model '{}'", model_name))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Ollama pull returned HTTP {}: {}",
                status,
                body
            ));
        }

        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.context("Error reading pull stream")?;
            let text = std::str::from_utf8(&chunk).context("Pull stream chunk is not valid UTF-8")?;

            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Ollama pull progress shape: { "status": "...", "completed": N, "total": N }
                #[derive(Deserialize)]
                struct PullChunk {
                    status: String,
                    completed: Option<u64>,
                    total: Option<u64>,
                }

                match serde_json::from_str::<PullChunk>(line) {
                    Ok(parsed) => {
                        let event = PullProgressEvent {
                            model: model_name.to_string(),
                            status: parsed.status.clone(),
                            completed: parsed.completed,
                            total: parsed.total,
                        };

                        if let Err(e) = app.emit("model://pull-progress", &event) {
                            warn!("Failed to emit pull progress event: {}", e);
                        }

                        if parsed.status == "success" {
                            info!("Model '{}' pulled successfully", model_name);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse pull line '{}': {}", line, e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Delete a locally stored model.
    #[instrument(skip(self))]
    pub async fn delete_model(&self, model_name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": model_name });

        let response = self
            .client
            .delete(format!("{}/api/delete", self.base_url))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("DELETE /api/delete failed for model '{}'", model_name))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Ollama delete returned HTTP {}: {}",
                status,
                body
            ));
        }

        info!("Model '{}' deleted", model_name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Non-streaming completion (used by Memory extraction — Phase 5)
// ---------------------------------------------------------------------------

impl OllamaClient {
    /// Send a non-streaming chat request and return the full response content.
    ///
    /// Used by the memory extraction engine for fact extraction and episode
    /// summarization where streaming is unnecessary and we just need the
    /// final text output.
    ///
    /// `format` lets the caller force constrained-generation output. Pass
    /// `Some(json!("json"))` for any-valid-JSON mode, `Some(json!(<schema>))`
    /// for full JSON Schema constraint, or `None` for free-form text.
    #[instrument(skip(self, messages, format))]
    pub async fn generate_completion(
        &self,
        model: &str,
        messages: Vec<OllamaChatMessage>,
        format: Option<serde_json::Value>,
    ) -> Result<String> {
        let request = OllamaChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
            options: None,
            think: Some(false),
            keep_alive: None,
            format,
        };

        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request)
            .send()
            .await
            .context("POST /api/chat (non-streaming) failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama returned HTTP {}: {}", status, body));
        }

        // Non-streaming response is a single JSON object with message.content
        #[derive(Deserialize)]
        struct ChatResponse {
            message: ChatResponseMessage,
        }
        #[derive(Deserialize)]
        struct ChatResponseMessage {
            content: String,
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .context("Failed to parse non-streaming chat response")?;

        Ok(parsed.message.content)
    }
}

// ---------------------------------------------------------------------------
// Embedding (used by RAG engine — Phase 4)
// ---------------------------------------------------------------------------

impl OllamaClient {
    /// Generate an embedding vector for the given text.
    ///
    /// Uses the embedding model specified in the tier config.
    /// Returns a flat Vec<f32> ready for usearch insertion.
    #[instrument(skip(self, text))]
    pub async fn embed(&self, model: &str, text: &str) -> Result<Vec<f32>> {
        let body = serde_json::json!({
            "model": model,
            "prompt": text
        });

        #[derive(Deserialize)]
        struct EmbedResponse {
            embedding: Vec<f32>,
        }

        let resp: EmbedResponse = self
            .client
            .post(format!("{}/api/embeddings", self.base_url))
            .json(&body)
            .send()
            .await
            .context("POST /api/embeddings failed")?
            .json()
            .await
            .context("Failed to parse embeddings response")?;

        Ok(resp.embedding)
    }
}

// ---------------------------------------------------------------------------
// Phase 6: list_running (/api/ps) and force_unload helpers
// ---------------------------------------------------------------------------

/// Raw shape returned by `GET /api/ps`. Private — every caller goes
/// through `list_running()` and reads the mapped `RunningModel` form.
///
/// All fields are optional with `#[serde(default)]` so missing or unknown
/// fields never fail the decode. Ollama's response also carries `digest`,
/// `model`, `modified_at`, and `details` which we deliberately ignore.
#[derive(Debug, Deserialize)]
pub(crate) struct PsResponseRaw {
    #[serde(default)]
    pub(crate) models: Vec<PsEntryRaw>,
}

/// One entry inside `PsResponseRaw.models`. Bytes-as-reported-by-Ollama;
/// conversion to MiB happens in `map_ps_entries`.
#[derive(Debug, Deserialize)]
pub(crate) struct PsEntryRaw {
    #[serde(default)]
    pub(crate) name: String,
    /// Total bytes (RAM + VRAM). Present in modern Ollama; missing on
    /// some endpoints — defaults to `0` via `unwrap_or(0)` (Req 5.6).
    #[serde(default)]
    pub(crate) size: Option<u64>,
    /// VRAM bytes. Absent or zero means "no VRAM allocated", which we
    /// surface as `Option::None` (Req 5.1).
    #[serde(default)]
    pub(crate) size_vram: Option<u64>,
    /// RFC3339 string. Parsed via `chrono::DateTime::parse_from_rfc3339`;
    /// defaults to `0` on parse failure or absence.
    #[serde(default)]
    pub(crate) expires_at: Option<String>,
}

/// Map a parsed `/api/ps` response into the public `RunningModel` shape.
///
/// Extracted from `list_running` (Task 7.2 / 2.3) so the mapping rules
/// can be unit- and property-tested without an HTTP round-trip. The
/// transform is total and order-preserving — one `RunningModel` per
/// input entry, no dedup or aggregation (Req 5.5, 5.6, 5.7):
///   - `size_total_mb = size.unwrap_or(0) / (1024 * 1024)` (zero passes
///     through unchanged).
///   - `size_vram_mb = Some(v / (1024 * 1024))` when `size_vram` is
///     `Some(v)` with `v > 0`, else `None`.
///   - `expires_at` parsed RFC3339 → epoch seconds, `0` on failure.
///   - `name` truncated at 256 bytes on a char boundary.
///   - `idle_seconds = None` (the Governor fills it per tick).
pub(crate) fn map_ps_entries(parsed: PsResponseRaw) -> Vec<RunningModel> {
    parsed
        .models
        .into_iter()
        .map(|e| {
            let size_total_mb = e.size.unwrap_or(0) / (1024 * 1024);
            let size_vram_mb = match e.size_vram {
                Some(v) if v > 0 => Some(v / (1024 * 1024)),
                _ => None,
            };
            let expires_at = e
                .expires_at
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.timestamp())
                .unwrap_or(0);
            let name = truncate_name_at_256_bytes(e.name);
            RunningModel {
                name,
                size_vram_mb,
                size_total_mb,
                expires_at,
                idle_seconds: None,
            }
        })
        .collect()
}

/// Parse a raw `/api/ps` JSON body into `Vec<RunningModel>`.
///
/// Thin wrapper over `serde_json::from_str` + `map_ps_entries`, exposed
/// `pub` so the external-crate property test
/// `tests/property_p3_per_model_accounting.rs` and the integration test
/// `tests/integration_api_ps_shape.rs` can build fixtures from string
/// literals (the in-crate unit tests in Task 2.3 use it too). A JSON
/// decode error or schema mismatch surfaces as `Err` (Req 5.3).
pub fn parse_ps_json(body: &str) -> Result<Vec<RunningModel>> {
    let parsed: PsResponseRaw =
        serde_json::from_str(body).context("Failed to parse /api/ps response")?;
    Ok(map_ps_entries(parsed))
}

impl OllamaClient {
    /// `GET /api/ps` — list models currently held in RAM by Ollama.
    ///
    /// Used by the Phase 6 Governor's polling loop to compute
    /// `GovernorMetrics.loaded_models` (Req 5.1, 5.4). The 5-second
    /// total deadline (Req 5.1) is enforced via `tokio::time::timeout`
    /// covering connect, request, and read; the connect-timeout
    /// configured on the underlying `reqwest::Client` is the floor.
    ///
    /// Mapping:
    ///   - `size_total_mb = size.unwrap_or(0) / (1024 * 1024)` — integer
    ///     truncation from bytes to MiB. Zero passes through unchanged
    ///     (Req 5.6).
    ///   - `size_vram_mb = Some(v / (1024 * 1024))` when `size_vram` is
    ///     `Some(v)` and `v > 0`, else `None` (Req 5.1).
    ///   - `expires_at` is parsed RFC3339 → `timestamp()` (i64 epoch
    ///     seconds), defaulting to `0` on parse failure or absence.
    ///   - `name` is truncated at 256 *bytes* on a UTF-8 char boundary
    ///     (Req 5.1) so a multi-byte CJK or emoji name does not split
    ///     mid-codepoint.
    ///   - `idle_seconds` is left `None`; the Governor populates it
    ///     against `model_last_used` on each tick.
    ///
    /// Errors map cleanly to `Err(_)` for the four documented failure
    /// modes (Req 5.3): timeout, non-2xx HTTP, JSON decode, schema
    /// mismatch. The Governor turns any `Err` into
    /// `(loaded_models = Vec::new(), ollama_online = false)` with one
    /// warn-level log entry.
    #[instrument(skip(self))]
    pub async fn list_running(&self) -> Result<Vec<RunningModel>> {
        const PS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        let request = self.client.get(format!("{}/api/ps", self.base_url));

        let resp = match tokio::time::timeout(PS_TIMEOUT, request.send()).await {
            Ok(send_result) => send_result.context("GET /api/ps failed")?,
            Err(_) => {
                return Err(anyhow!(
                    "GET /api/ps timed out after {}s",
                    PS_TIMEOUT.as_secs()
                ));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Ollama /api/ps returned HTTP {}: {}",
                status,
                body
            ));
        }

        let parsed: PsResponseRaw = match tokio::time::timeout(PS_TIMEOUT, resp.json()).await {
            Ok(json_result) => json_result.context("Failed to parse /api/ps response")?,
            Err(_) => {
                return Err(anyhow!(
                    "Reading /api/ps body timed out after {}s",
                    PS_TIMEOUT.as_secs()
                ));
            }
        };

        let models: Vec<RunningModel> = map_ps_entries(parsed);

        debug!("/api/ps returned {} loaded model(s)", models.len());
        Ok(models)
    }

    /// Force-unload a single model by sending `keep_alive: "0s"` against
    /// `POST /api/generate` with an empty prompt.
    ///
    /// This is the documented Ollama mechanism for immediate eviction
    /// (`docs/DECISIONS.md`, 2026-05-22). Used by the Phase 6 Governor's
    /// auto-unload pass (Req 8.7), the user-initiated single-model
    /// unload command (Req 12.2), and the Critical-state batch unload
    /// (Req 9.1).
    ///
    /// Response handling per design.md "Bucket D" + Req 15.7:
    ///   - HTTP 2xx → `Ok(())`.
    ///   - HTTP 404 → `Ok(())`. The model is gone (already unloaded,
    ///     name not in registry); from the caller's perspective the
    ///     post-condition holds.
    ///   - Any other status, transport failure, or timeout → `Err`.
    ///
    /// All Phase 6 callers — Governor auto-unload, the user-initiated
    /// single-model unload command, the Critical-state batch unload,
    /// and the ingestion-worker embedding-swap path — go through this
    /// method.
    #[instrument(skip(self))]
    pub async fn force_unload(&self, name: &str) -> Result<()> {
        const UNLOAD_TIMEOUT: std::time::Duration =
            std::time::Duration::from_secs(10);

        let body = serde_json::json!({
            "model": name,
            "prompt": "",
            "keep_alive": "0s",
        });

        let request = self
            .client
            .post(format!("{}/api/generate", self.base_url))
            .json(&body);

        let resp = match tokio::time::timeout(UNLOAD_TIMEOUT, request.send()).await {
            Ok(send_result) => send_result.with_context(|| {
                format!("POST /api/generate (force_unload) failed for '{}'", name)
            })?,
            Err(_) => {
                return Err(anyhow!(
                    "force_unload of '{}' timed out after {}s",
                    name,
                    UNLOAD_TIMEOUT.as_secs()
                ));
            }
        };

        let status = resp.status();
        if status.is_success() {
            info!(model = %name, status = status.as_u16(), "force_unload: ok");
            return Ok(());
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            info!(
                model = %name,
                "force_unload: HTTP 404 — model already gone, treating as success"
            );
            return Ok(());
        }

        let body = resp.text().await.unwrap_or_default();
        Err(anyhow!(
            "force_unload of '{}' returned HTTP {}: {}",
            name,
            status,
            body
        ))
    }
}

/// Truncate a model name at 256 *bytes* without splitting a UTF-8
/// codepoint. Names already ≤ 256 bytes return unchanged; names longer
/// than 256 bytes are cut at the largest char boundary not exceeding
/// 256. This matches Req 5.1 ("≤ 256 bytes") while keeping the result a
/// valid Rust `String`.
fn truncate_name_at_256_bytes(name: String) -> String {
    if name.len() <= 256 {
        return name;
    }
    // Walk back from byte 256 to the previous char boundary.
    let mut end = 256;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

#[cfg(test)]
mod phase6_tests {
    use super::*;

    #[test]
    fn truncate_name_short_passthrough() {
        assert_eq!(truncate_name_at_256_bytes("short".into()), "short");
    }

    #[test]
    fn truncate_name_exactly_256_bytes_passthrough() {
        let s: String = "a".repeat(256);
        assert_eq!(truncate_name_at_256_bytes(s.clone()).len(), 256);
    }

    #[test]
    fn truncate_name_over_256_ascii() {
        let s: String = "a".repeat(300);
        let out = truncate_name_at_256_bytes(s);
        assert_eq!(out.len(), 256);
    }

    #[test]
    fn truncate_name_respects_char_boundary() {
        // 86 × 3-byte char + 50 × 1-byte ASCII = 258 + 50 = 308 bytes total.
        // Char boundary at byte 255 (85 × 3) is the largest ≤ 256 boundary.
        let mut s = "あ".repeat(86); // 258 bytes
        s.push_str(&"a".repeat(50));   // +50 bytes
        let out = truncate_name_at_256_bytes(s);
        assert!(out.len() <= 256);
        assert!(out.is_char_boundary(out.len()));
    }

    // ── Task 2.3 — list_running parser cases via parse_ps_json ─────────

    #[test]
    fn parse_ps_json_zero_entries() {
        let body = r#"{ "models": [] }"#;
        let out = parse_ps_json(body).expect("empty list parses");
        assert!(out.is_empty());
    }

    #[test]
    fn parse_ps_json_missing_models_key_defaults_empty() {
        // `models` is `#[serde(default)]` — a body with no key yields [].
        let body = r#"{}"#;
        let out = parse_ps_json(body).expect("missing models key parses");
        assert!(out.is_empty());
    }

    #[test]
    fn parse_ps_json_single_entry_full() {
        // 1 GiB total, 512 MiB VRAM.
        let body = r#"{ "models": [
            { "name": "gemma3", "size": 1073741824, "size_vram": 536870912,
              "expires_at": "2026-01-01T00:00:00Z" }
        ] }"#;
        let out = parse_ps_json(body).expect("parses");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "gemma3");
        assert_eq!(out[0].size_total_mb, 1024);
        assert_eq!(out[0].size_vram_mb, Some(512));
        // 2026-01-01T00:00:00Z = 1767225600 epoch seconds.
        assert_eq!(out[0].expires_at, 1767225600);
        assert_eq!(out[0].idle_seconds, None);
    }

    #[test]
    fn parse_ps_json_n_entries_order_preserved() {
        let body = r#"{ "models": [
            { "name": "a", "size": 2097152 },
            { "name": "b", "size": 4194304 },
            { "name": "c", "size": 6291456 }
        ] }"#;
        let out = parse_ps_json(body).expect("parses");
        let names: Vec<&str> = out.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(out[0].size_total_mb, 2);
        assert_eq!(out[1].size_total_mb, 4);
        assert_eq!(out[2].size_total_mb, 6);
    }

    #[test]
    fn parse_ps_json_missing_size_vram_is_none() {
        let body = r#"{ "models": [ { "name": "x", "size": 1048576 } ] }"#;
        let out = parse_ps_json(body).expect("parses");
        assert_eq!(out[0].size_vram_mb, None);
        assert_eq!(out[0].size_total_mb, 1);
    }

    #[test]
    fn parse_ps_json_zero_size_passes_through() {
        // Zero size → 0 MB, no fallback/estimate (Req 5.6); zero VRAM → None.
        let body = r#"{ "models": [ { "name": "x", "size": 0, "size_vram": 0 } ] }"#;
        let out = parse_ps_json(body).expect("parses");
        assert_eq!(out[0].size_total_mb, 0);
        assert_eq!(out[0].size_vram_mb, None);
    }

    #[test]
    fn parse_ps_json_missing_size_defaults_zero() {
        let body = r#"{ "models": [ { "name": "x" } ] }"#;
        let out = parse_ps_json(body).expect("parses");
        assert_eq!(out[0].size_total_mb, 0);
    }

    #[test]
    fn parse_ps_json_malformed_expires_at_defaults_zero() {
        let body = r#"{ "models": [
            { "name": "x", "size": 1048576, "expires_at": "not-a-date" }
        ] }"#;
        let out = parse_ps_json(body).expect("parses");
        assert_eq!(out[0].expires_at, 0);
    }

    #[test]
    fn parse_ps_json_invalid_json_is_err() {
        let body = r#"{ "models": [ "#;
        assert!(parse_ps_json(body).is_err());
    }

    #[test]
    fn parse_ps_json_schema_mismatch_is_err() {
        // `size` as a string is not a u64 — serde rejects it.
        let body = r#"{ "models": [ { "name": "x", "size": "huge" } ] }"#;
        assert!(parse_ps_json(body).is_err());
    }

    #[test]
    fn map_ps_entries_truncates_long_name() {
        let long = "a".repeat(300);
        let parsed = PsResponseRaw {
            models: vec![PsEntryRaw {
                name: long,
                size: Some(1048576),
                size_vram: None,
                expires_at: None,
            }],
        };
        let out = map_ps_entries(parsed);
        assert_eq!(out[0].name.len(), 256);
    }

    #[test]
    fn map_ps_entries_no_dedup_or_aggregation() {
        // Two identical names must remain two separate entries (Req 5.5).
        let parsed = PsResponseRaw {
            models: vec![
                PsEntryRaw {
                    name: "dup".into(),
                    size: Some(1048576),
                    size_vram: None,
                    expires_at: None,
                },
                PsEntryRaw {
                    name: "dup".into(),
                    size: Some(2097152),
                    size_vram: None,
                    expires_at: None,
                },
            ],
        };
        let out = map_ps_entries(parsed);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].size_total_mb, 1);
        assert_eq!(out[1].size_total_mb, 2);
    }
}
