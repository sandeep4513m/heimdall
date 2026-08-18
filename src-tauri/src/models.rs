/// models.rs — Shared data types for Heimdall
///
/// All structs that cross the Rust/frontend boundary are defined here.
/// Every type that is returned from a Tauri command derives Serialize.
/// Every type that is received from the frontend derives Deserialize.
/// sqlx::FromRow is derived on types that map directly to database rows.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Database row types
// ---------------------------------------------------------------------------

/// A conversation thread.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Conversation {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A single message within a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Message {
    pub id: String,
    pub conversation_id: Option<String>,
    /// "user" | "assistant" | "system"
    pub role: String,
    pub content: Option<String>,
    /// "text" | "image" | "audio"
    pub input_type: Option<String>,
    pub tokens_used: Option<i64>,
    /// JSON array of base64-encoded image strings (no data URL prefix)
    pub images: Option<String>,
    /// Thinking block content from <think>…</think> tags (assistant only)
    pub thinking: Option<String>,
    pub created_at: i64,
}

/// A user-confirmed or pending memory fact.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MemoryFact {
    pub id: String,
    pub fact: String,
    pub source_conversation_id: Option<String>,
    pub confirmed_by_user: bool,
    pub created_at: i64,
    pub dedup_status: Option<String>,
    pub conflict_with_id: Option<String>,
    pub update_hint_id: Option<String>,
    pub batch_id: Option<String>,
}

/// A memory episode — a conversation summary stored as a vector.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MemoryEpisode {
    pub id: String,
    pub summary: String,
    pub source_conversation_id: Option<String>,
    pub vector_id: Option<i64>,
    pub created_at: i64,
    pub decayed: bool,
    pub restored: bool,
}

/// Memory system settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySettings {
    pub global_enabled: bool,
    pub decay_threshold_days: u32,
    pub fact_count: u64,
    pub episode_count: u64,
}

/// Result of a memory extraction pass, sent to frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub facts_extracted: Vec<CandidateFact>,
    pub episode_created: bool,
    pub skipped_reason: Option<String>,
    /// Set when fact extraction ran but failed (model error, JSON parse failure after retry).
    pub extraction_error: Option<String>,
    /// Set when episode summary generation or storage failed.
    pub episode_error: Option<String>,
}

/// A candidate fact produced by the extraction engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateFact {
    pub id: String,
    pub text: String,
    pub dedup_status: String, // "new" | "possible_update" | "duplicate"
    pub conflict_with: Option<String>,
}

/// A text chunk stored after RAG ingestion.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RagChunk {
    pub id: String,
    pub collection: String,
    pub source_path: String,
    pub chunk_index: i64,
    pub content: String,
    pub token_count: i64,
    /// Internal usearch vector ID — None until embedding is stored.
    pub vector_id: Option<i64>,
    pub created_at: i64,
}

/// A background ingestion job.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct IngestionJob {
    pub id: String,
    pub source_path: Option<String>,
    pub collection: Option<String>,
    /// "pending" | "running" | "done" | "failed"
    pub status: Option<String>,
    pub chunks_total: i64,
    pub chunks_done: i64,
    pub error: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Ollama API types
// ---------------------------------------------------------------------------

/// A model returned by the Ollama /api/tags endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub modified_at: String,
    /// Authoritative capabilities from the registry. `None` only when the
    /// model has never been seen by the registry (warm-up not yet run).
    /// Frontend should treat `None` as "loading".
    #[serde(default)]
    pub capabilities: Option<ModelCapabilities>,
    /// DEPRECATED: kept for one release for any code path that still
    /// reads the single-enum form. Populated from `capabilities` via
    /// `legacy_capability_from(&caps)`. Removed in the next release.
    #[deprecated(note = "Read `capabilities` instead. Removed in next release.")]
    pub capability: ModelCapability,
}

/// What kind of input a model accepts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    TextOnly,
    Vision,
    Embedding,
    Audio,
    Multimodal,
    /// Model exposes internal reasoning via <think> tags (e.g. deepseek-r1, qwen3).
    Thinking,
}

/// Detailed model information from /api/show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub family: String,
    pub parameter_size: String,
    pub quantization_level: String,
    pub capability: ModelCapability,
    /// Raw modelfile template (used for capability detection).
    pub template: Option<String>,
    /// Raw capabilities reported by Ollama's /api/show endpoint.
    /// Possible values: "completion", "vision", "tools", "thinking".
    /// Empty on old Ollama versions that don't report capabilities.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

// ---------------------------------------------------------------------------
// Model Intelligence Registry types (Phase 3.5)
//
// `ModelCapabilities` is the authoritative multi-capability replacement for
// the legacy single-valued `ModelCapability` enum above. The legacy enum is
// kept for one release as a backward-compatibility shim populated via
// `legacy_capability_from(&caps)` (added in task 1.2).
//
// `CapabilitySource` records which detection layer produced a row so the
// future Models tab can show provenance.
//
// `ModelSettings` is the per-model override table — empty in this release;
// populated by the future Models tab.
// ---------------------------------------------------------------------------

/// What a model can do. Multi-capability — a single model is often
/// completion + vision + tools simultaneously. This struct is the
/// authoritative answer to "what does this model support?" and replaces
/// the single-valued `ModelCapability` enum at the registry boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, sqlx::FromRow)]
pub struct ModelCapabilities {
    pub model_name: String,
    pub digest: String,
    /// Standard text completion. Almost always true for chat models.
    pub completion: bool,
    /// Accepts image input (multimodal vision models).
    pub vision: bool,
    /// Native thinking via `<think>` tags / Ollama's `thinking` field.
    pub thinking: bool,
    /// Function-calling / tool-use via Ollama's tools API.
    pub tools: bool,
    /// Embedding generation (RAG path); models that emit vectors not text.
    pub embedding: bool,
    /// Which detection layer produced this row.
    pub capability_source: CapabilitySource,
    /// Raw capability strings from /api/show, when source = ApiShow.
    /// Empty for other sources.
    #[serde(default)]
    pub raw_capabilities: Vec<String>,
    /// Family ("gemma", "llama", "qwen") from /api/show.details.
    #[serde(default)]
    pub family: Option<String>,
    /// Parameter size string ("7B", "70B") from /api/show.details.
    #[serde(default)]
    pub parameter_size: Option<String>,
    /// Quantization level ("Q4_K_M", "Q8_0") from /api/show.details.
    #[serde(default)]
    pub quantization_level: Option<String>,
    /// Unix timestamp of first detection for this digest.
    pub detected_at: i64,
    /// Unix timestamp of last update (refresh, override edit).
    pub updated_at: i64,
}

/// Which detection layer produced a `ModelCapabilities` row.
/// Visible in the future Models tab so the user can see provenance.
///
/// Wire form is snake_case: `api_show | template | heuristic | user_override`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Ollama's /api/show returned a non-empty capabilities array.
    /// This is the ground truth.
    ApiShow,
    /// Inferred from template inspection (e.g. `{{ .Images }}`).
    /// Used when /api/show didn't include capabilities (old Ollama).
    Template,
    /// Inferred from model name substring matching.
    /// Last-resort fallback; unreliable.
    Heuristic,
    /// User explicitly set this in the Models tab.
    /// Trumps all auto-detection on subsequent reads.
    UserOverride,
}

/// Per-model override values for chat options. Empty in this release;
/// the future Models tab populates it. All override columns are nullable
/// so a partial override (e.g. only `temperature`) is valid; `updated_at`
/// is non-null and always reflects the last write.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, Default)]
pub struct ModelSettings {
    pub model_name: String,
    pub temperature: Option<f32>,
    pub num_ctx: Option<u32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub system_prompt: Option<String>,
    pub default_keep_alive: Option<String>,
    pub updated_at: i64,
}

/// Migration shim: collapse the multi-capability `ModelCapabilities` view
/// down to the legacy single-valued `ModelCapability` enum so any caller
/// still reading `OllamaModel::capability` gets a sensible answer for one
/// release. Removed alongside `OllamaModel::capability` in step 3.
///
/// Priority: `Embedding > Vision > Thinking > TextOnly`. Most-restrictive
/// category first — an embedding model isn't a chat model, a vision model
/// is more important to surface than thinking support (which is a
/// generation behaviour, not an input modality), and `TextOnly` is the
/// fall-through default. Mirrors the existing
/// `OllamaClient::capability_from_ollama_array` ordering so the new shim
/// produces the same answer as the legacy path on every input.
pub fn legacy_capability_from(caps: &ModelCapabilities) -> ModelCapability {
    if caps.embedding {
        ModelCapability::Embedding
    } else if caps.vision {
        ModelCapability::Vision
    } else if caps.thinking {
        ModelCapability::Thinking
    } else {
        ModelCapability::TextOnly
    }
}

/// A single message in the Ollama chat request format.
///
/// Also used to deserialize streaming response chunks. Ollama v0.9+ sends
/// reasoning tokens in a native `thinking` field (separate from `content`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaChatMessage {
    pub role: String,
    pub content: String,
    /// Base64-encoded images for vision models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Native thinking field — Ollama extracts <think> tags server-side and
    /// streams reasoning tokens here. Only present in streaming responses
    /// from thinking models (deepseek-r1, qwen3, qwq, etc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// Request body sent to /api/chat.
#[derive(Debug, Serialize)]
pub struct OllamaChatRequest {
    pub model: String,
    pub messages: Vec<OllamaChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<OllamaOptions>,
    /// When true, Ollama populates the native `message.thinking` field for
    /// models that support it (Gemma 4, DeepSeek R1, Qwen 3, etc).
    /// Without this, only the `<think>` tag fallback works.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<bool>,
    /// How long Ollama should keep the model loaded after this request.
    /// Pass `"0s"` to force immediate unload (used by Phase 6 governor).
    /// Pass `"5m"` (Ollama default) by leaving as None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
    /// Constrained-generation directive for `/api/chat`.
    ///
    /// Accepts either the literal JSON string `"json"` (forces any valid JSON
    /// output) or a full JSON Schema object (forces output to match the
    /// schema). Used by the memory extraction engine to guarantee
    /// machine-parseable responses across all model sizes — small
    /// instruction-tuned models (phi4-mini, qwen2.5:0.5b) are unreliable
    /// at JSON output via prompt alone, but reliable when the inference
    /// layer constrains the token grammar.
    ///
    /// Wire shape examples:
    ///   `Some(serde_json::json!("json"))`
    ///   `Some(serde_json::json!({ "type": "object", "properties": ... }))`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<serde_json::Value>,
}

/// Optional generation parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Keep-alive duration string ("5m", "0s", etc). Phase 6 uses this
    /// to control model unload behaviour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
}

/// A single streaming chunk from /api/chat.
#[derive(Debug, Deserialize)]
pub struct OllamaChatChunk {
    pub message: OllamaChatMessage,
    pub done: bool,
    #[serde(default)]
    pub eval_count: Option<u32>,
    #[serde(default)]
    pub prompt_eval_count: Option<u32>,
}

/// Payload emitted to the frontend for each streaming token.
#[derive(Debug, Clone, Serialize)]
pub struct StreamTokenEvent {
    /// The conversation this stream belongs to.
    pub conversation_id: String,
    /// The token text fragment.
    pub token: String,
    /// True on the final chunk — signals the frontend to stop the spinner.
    pub done: bool,
    /// Total tokens used, populated only on the final chunk.
    pub tokens_used: Option<u32>,
}

/// Payload emitted to the frontend for thinking block content.
///
/// Emitted on `chat://thinking` while the model is inside a <think> block.
/// The frontend renders this in a collapsible "Thought for N seconds" block.
#[derive(Debug, Clone, Serialize)]
pub struct ThinkingEvent {
    /// The conversation this thinking block belongs to.
    pub conversation_id: String,
    /// A chunk of thinking content (may be partial).
    pub content: String,
    /// True when the </think> tag has been detected — thinking is complete.
    pub done: bool,
}

/// Health check response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaHealth {
    pub online: bool,
    pub version: Option<String>,
}

/// Pull progress event emitted to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullProgressEvent {
    pub model: String,
    pub status: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

/// Forward-compat hint passed to `chat_stream` describing what context the
/// chat should be augmented with.
///
/// Phase 3 alpha: ignored (no-op). Phase 4 fills `rag_collections` and
/// the backend prepends retrieved chunks as a system message. Phase 5 fills
/// `memory_enabled` and the backend prepends confirmed memory facts.
///
/// All fields are `Option<...>` so future additions never break the frontend
/// IPC contract.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextHint {
    /// Which RAG collections to retrieve from before this turn. Phase 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rag_collections: Option<Vec<String>>,
    /// Whether to inject confirmed memory facts. Phase 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// RAG / Phase 4 types
// ---------------------------------------------------------------------------

/// A named knowledge collection.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Collection {
    pub id: String,
    pub display_name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_ingested_at: Option<i64>,
}

/// Summary statistics for a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionStats {
    pub display_name: String,
    pub chunks: u64,
    pub sources: u64,
    pub last_updated: Option<i64>,
    pub vector_bytes: u64,
}

/// A chunk returned from retrieval preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkPreview {
    pub chunk_id: String,
    pub content: String,
    pub source_path: String,
    pub chunk_index: i64,
    pub score: f32,
}

/// Vector quantization kind for usearch indexes.
///
/// This is a local mirror of `usearch::ScalarKind` so that `TierConfig` can
/// carry the quantization setting before the `usearch` crate is added to
/// `Cargo.toml` (task 2.1). When `usearch` is added, this enum will be
/// replaced or aliased to `usearch::ScalarKind`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScalarKind {
    /// 16-bit half-precision float. Used on Tier 1 to halve vector memory.
    F16,
    /// 32-bit single-precision float. Used on Tier 2/3 for full precision.
    F32,
}

// ---------------------------------------------------------------------------
// Adaptive config types
// ---------------------------------------------------------------------------

/// Hardware capability tier assigned at startup.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HardwareTier {
    /// < 6 GB RAM, no GPU
    Minimal,
    /// 6–16 GB RAM, optional GPU
    Standard,
    /// 16+ GB RAM, GPU available
    Full,
}

/// Detected hardware metrics.
///
/// `detected_tier` is what the hardware suggests; `effective_tier` is what
/// is actually used (may differ if the user set `tier_override` in config).
/// Both are surfaced so the Phase 6 Governor panel can show "you have a 4 GB
/// box; you've overridden to Standard" honestly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub vram_mb: Option<u64>,
    pub cpu_cores: u32,
    /// What `detect_hardware` decided based purely on the machine.
    pub detected_tier: HardwareTier,
    /// What the app actually behaves as. Equals `detected_tier` unless the
    /// user set `tier_override` in `config.toml`.
    pub effective_tier: HardwareTier,
}

/// Per-tier configuration values derived from hardware detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    pub tier: HardwareTier,
    pub rag_enabled: bool,
    pub embedding_model: String,
    pub chunk_size_tokens: u32,
    pub chunk_overlap_tokens: u32,
    pub max_vectors: Option<u64>,
    /// Minutes of idle before auto-unloading a model. None = never.
    pub auto_unload_minutes: Option<u32>,
    /// Top-k results for RAG retrieval.
    pub rag_top_k: u32,
    // ── Phase 4 additions ──
    /// Vector quantization: F16 on Tier 1, F32 on Tier 2/3.
    pub quantization: ScalarKind,
    /// Whether to memory-map usearch indexes (true on Tier 1/2, false on Tier 3).
    pub index_mmap: bool,
    // ── Phase 6 additions: Governor thresholds ──
    /// `MemAvailable` floor below which the Governor enters `Warn`.
    /// Defaults: Tier 1 = 800, Tier 2 = 1500, Tier 3 = 2000 (MB). Req 6.6.
    #[serde(default)]
    pub governor_warn_mb: u64,
    /// `MemAvailable` floor below which the Governor enters `Unload` and
    /// begins evicting idle models.
    /// Defaults: Tier 1 = 400, Tier 2 = 800, Tier 3 = 1000 (MB). Req 6.6.
    #[serde(default)]
    pub governor_unload_mb: u64,
    /// `MemAvailable` floor below which the Governor enters `Critical`,
    /// pauses ingestion, and batch-evicts every eligible model with no
    /// cooldown.
    /// Defaults: Tier 1 = 200, Tier 2 = 400, Tier 3 = 500 (MB). Req 6.6.
    #[serde(default)]
    pub governor_critical_mb: u64,
    /// Fraction of `MemAvailable` the adaptive embedding orchestrator
    /// treats as the safe budget for model loads. Default `0.80` across
    /// every tier (Req 10.2).
    #[serde(default)]
    pub safe_headroom_pct: f32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `ModelCapabilities` with the four flags relevant to
    /// `legacy_capability_from`. `completion` is irrelevant to the shim
    /// (the legacy enum has no `Completion` variant) so it is fixed `true`
    /// to match a typical chat model.
    fn caps(embedding: bool, vision: bool, thinking: bool, tools: bool) -> ModelCapabilities {
        ModelCapabilities {
            model_name: "test".into(),
            digest: "sha256:0".into(),
            completion: true,
            vision,
            thinking,
            tools,
            embedding,
            capability_source: CapabilitySource::Heuristic,
            raw_capabilities: vec![],
            family: None,
            parameter_size: None,
            quantization_level: None,
            detected_at: 0,
            updated_at: 0,
        }
    }

    /// Ground truth: `Embedding > Vision > Thinking > TextOnly`. `tools` is
    /// orthogonal — the legacy enum has no Tools variant so it never affects
    /// the result regardless of value.
    #[test]
    fn legacy_capability_from_priority_order() {
        // Embedding wins over everything.
        assert_eq!(
            legacy_capability_from(&caps(true, true, true, true)),
            ModelCapability::Embedding
        );
        assert_eq!(
            legacy_capability_from(&caps(true, false, false, false)),
            ModelCapability::Embedding
        );
        assert_eq!(
            legacy_capability_from(&caps(true, true, false, false)),
            ModelCapability::Embedding
        );

        // Vision wins when embedding is false.
        assert_eq!(
            legacy_capability_from(&caps(false, true, true, true)),
            ModelCapability::Vision
        );
        assert_eq!(
            legacy_capability_from(&caps(false, true, false, false)),
            ModelCapability::Vision
        );

        // Thinking wins when both embedding and vision are false.
        assert_eq!(
            legacy_capability_from(&caps(false, false, true, true)),
            ModelCapability::Thinking
        );
        assert_eq!(
            legacy_capability_from(&caps(false, false, true, false)),
            ModelCapability::Thinking
        );

        // Default fall-through.
        assert_eq!(
            legacy_capability_from(&caps(false, false, false, false)),
            ModelCapability::TextOnly
        );
        // Tools-only still maps to TextOnly (no Tools variant in legacy enum).
        assert_eq!(
            legacy_capability_from(&caps(false, false, false, true)),
            ModelCapability::TextOnly
        );
    }

    /// Tools is independent of the four legacy categories — flipping it
    /// must never change the legacy answer when none of the prioritised
    /// flags differ.
    #[test]
    fn legacy_capability_from_tools_is_orthogonal() {
        for &(e, v, t) in &[
            (false, false, false),
            (false, false, true),
            (false, true, false),
            (false, true, true),
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (true, true, true),
        ] {
            assert_eq!(
                legacy_capability_from(&caps(e, v, t, false)),
                legacy_capability_from(&caps(e, v, t, true)),
                "tools flag changed legacy answer for (e={}, v={}, t={})",
                e,
                v,
                t,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 6: Governor types
//
// All types crossing the Rust↔frontend boundary for the Phase 6 Governor.
// Defined here (rather than in `governor.rs`) so they can be referenced by
// `OllamaClient::list_running`, `AppState`, and the polling loop without
// pulling the Governor module's full set of dependencies.
//
// All structs derive `Debug, Clone, Serialize, Deserialize`. Property tests
// require equality on `GovernorMetrics` and `RunningModel`, so those carry
// `PartialEq` (and `Eq` where every nested field permits it). The struct
// `GovernorMetrics` does NOT derive `Eq` because it carries `f32` fields
// (`cpu_aggregate_percent`, `cpu_per_core_percent`).
//
// All enums use `#[serde(rename_all = "snake_case")]` so the wire form
// matches frontend expectations exactly (`calm`, `warn`, `unload`,
// `critical`, `unloading_chat`, etc).
// ---------------------------------------------------------------------------

/// Risk level derived once per polling tick from `available_ram_mb` and the
/// active tier's three thresholds. Severity ordering is the natural enum
/// ordering — `Calm < Warn < Unload < Critical` — and is what property test
/// P4 (risk-state monotonicity) relies on.
///
/// Wire form: `calm | warn | unload | critical`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RiskState {
    Calm,
    Warn,
    Unload,
    Critical,
}

/// Health/availability status for the VRAM readings on this tick.
///
/// - `Ok` — at least one identified discrete GPU returned both
///   `mem_info_vram_total` and `mem_info_vram_used` successfully.
/// - `Unavailable` — at least one discrete GPU was identified, but a read
///   failed (file missing, I/O error, permission denied, unparseable bytes).
/// - `Absent` — no discrete GPU was found at any
///   `/sys/class/drm/card<N>/` path.
///
/// Frontend renders the VRAM card differently in each case (Req 4.3, 4.4,
/// 4.5). Intel iGPUs (PCI vendor `0x8086`) are intentionally excluded
/// from identification and therefore never produce `Ok`.
///
/// Wire form: `ok | unavailable | absent`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VramStatus {
    Ok,
    Unavailable,
    Absent,
}

/// Whether `/proc` was readable for this polling tick. When `Unreadable`,
/// the tick still emits but every `*_mb` field carries a sentinel `0` and
/// every `Option` is `None` (Bucket B in design.md "Error Handling").
///
/// Wire form: `readable | unreadable`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcStatus {
    Readable,
    Unreadable,
}

/// Hardware-aware classification of a model against the active tier.
///
/// Computed by `compute_recommendation(size_mb, tier, hw)` as a pure
/// function of model size, tier overhead constants, and total RAM
/// (Req 14.2, 14.4). No LLM call.
///
/// Wire form: `fits_comfortably | requires_management | exceeds_tier`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRecommendation {
    FitsComfortably,
    RequiresManagement,
    ExceedsTier,
}

/// Phase identifier for `governor://embedding_swap` events. Strongly typed
/// so a typo cannot reach the wire (frontend `models.svelte.ts` matches on
/// the snake_case form).
///
/// Wire form: `unloading_chat | unloading_embedding | reloading_chat`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingSwapPhase {
    UnloadingChat,
    UnloadingEmbedding,
    ReloadingChat,
}

/// Three-branch decision returned by `Governor::can_load_embedding`.
///
/// - `FitsAlongside` — embedding can load while chat stays loaded.
/// - `RequiresChatUnload` — chat must be evicted first.
/// - `InsufficientEvenAlone` — embedding alone exceeds the configured safe
///   RAM headroom (`safe_headroom_pct * available_ram_mb`); ingestion fails
///   the job (Req 10.8).
///
/// Wire form: `fits_alongside | requires_chat_unload | insufficient_even_alone`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingFitDecision {
    FitsAlongside,
    RequiresChatUnload,
    InsufficientEvenAlone,
}

/// Payload emitted on `governor://embedding_swap` whenever the Governor or
/// `chat_stream` transitions between chat and embedding model loadedness
/// (Req 10.6, 10.7, 10.9). `chat_model` carries the chat model name when
/// the phase concerns it; `None` for `UnloadingEmbedding`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingSwapEvent {
    pub phase: EmbeddingSwapPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_model: Option<String>,
}

/// One Ollama-loaded model, mapped from a single `/api/ps` entry.
///
/// `size_total_mb` is computed as `bytes / (1024 * 1024)` (integer
/// truncation). `size_vram_mb` is `None` when Ollama omits the field or
/// reports zero. `expires_at` is Unix epoch seconds; `0` when the source
/// string is missing or unparseable. `idle_seconds` is computed by the
/// Governor on each tick and is `None` until the model has streamed at
/// least one chat token in this session (Req 5.1, 7.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunningModel {
    pub name: String,
    pub size_vram_mb: Option<u64>,
    pub size_total_mb: u64,
    pub expires_at: i64,
    pub idle_seconds: Option<u64>,
}

/// The three governor thresholds actually used to derive the current tick's
/// `risk_state`. Embedded in `GovernorMetrics` so the frontend slider can
/// snap to the values being applied — including the case where the
/// configured values failed validation and the Governor fell back to the
/// documented per-tier defaults (Req 6.9).
///
/// Invariant when valid: `warn_mb > unload_mb > critical_mb > 0` and
/// `warn_mb <= total_ram_mb` (Req 6.8).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernorThresholds {
    pub warn_mb: u64,
    pub unload_mb: u64,
    pub critical_mb: u64,
}

/// One polling-tick snapshot of every system resource the Governor watches.
/// Emitted on `governor://metrics` exactly once per tick (Req 1.9).
///
/// `cpu_per_core_percent` length equals the number of `cpu<N>` lines in the
/// second `/proc/stat` sample (Req 2.3). `loaded_models` preserves the
/// order returned by Ollama and never deduplicates (Req 5.5). `thresholds`
/// embeds the values actually used to compute `risk_state` for this tick.
///
/// Does not derive `Eq` — `cpu_aggregate_percent` and `cpu_per_core_percent`
/// are `f32`/`Vec<f32>`. Property test P1 uses `PartialEq` only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GovernorMetrics {
    // RAM (Req 2.3)
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    // CPU (Req 2.3)
    pub cpu_aggregate_percent: f32,
    pub cpu_per_core_percent: Vec<f32>,
    // Process (Req 3.2, 3.5, 3.6)
    pub ollama_online: bool,
    pub ollama_rss_mb: Option<u64>,
    pub heimdall_rss_mb: u64,
    pub webview_rss_mb: Option<u64>,
    // GPU (Req 4.2)
    pub vram_total_mb: Option<u64>,
    pub vram_used_mb: Option<u64>,
    pub vram_status: VramStatus,
    // Loaded models (Req 5.4)
    pub loaded_models: Vec<RunningModel>,
    // Risk (Req 6.1)
    pub risk_state: RiskState,
    /// Thresholds actually used this tick. Reflects fall-back defaults when
    /// the configured values failed validation (Req 6.9).
    pub thresholds: GovernorThresholds,
    // Tier (Req 11.5)
    pub detected_tier: HardwareTier,
    pub effective_tier: HardwareTier,
    // Diagnostics (Req 15.3, 15.4)
    pub proc_status: ProcStatus,
    pub cgroup_detected: bool,
    /// Wall-clock at tick-start, for staleness checks in the frontend.
    pub timestamp_unix_ms: i64,
}

/// Row payload for the Phase 6 Models tab (Req 13.8, 14.2).
///
/// One entry per locally-available Ollama model (`/api/tags`), enriched
/// with registry capabilities, last-used timestamp from this session,
/// loaded-status from the most recent `/api/ps` snapshot, and the
/// hardware-aware recommendation. `last_used_unix` is `None` for models
/// never streamed in this session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsTabRow {
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub modified_at: String,
    #[serde(default)]
    pub capabilities: Option<ModelCapabilities>,
    #[serde(default)]
    pub last_used_unix: Option<i64>,
    pub currently_loaded: bool,
    pub recommendation: ModelRecommendation,
}

/// Predictive ingestion-pressure preview (Legendary feature, Task 28.1).
///
/// Returned by the gated `governor_preview_ingestion` Tauri command. Maps
/// the Governor's `EmbeddingFitDecision` to a traffic-light `status` the
/// frontend `IngestionPressurePreview.svelte` renders, plus the raw MB
/// numbers behind the decision so the UI can explain it:
///
/// - `status: "green"`    ← `FitsAlongside` (chat + embedding both fit)
/// - `status: "amber"`    ← `RequiresChatUnload` (chat must be evicted)
/// - `status: "red"`      ← `InsufficientEvenAlone` (embedding alone too big)
/// - `status: "disabled"` ← the feature flag is off; all MB fields `0`
///
/// `budget_mb` is `floor(available_mb * safe_headroom_pct)` — the same
/// integer-truncated budget `can_load_embedding` uses (Req 10.1, 10.2).
///
/// Wire form is snake_case to match the frontend `IngestionFitPreview`
/// TypeScript interface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestionFitPreview {
    /// `"green" | "amber" | "red" | "disabled"`.
    pub status: String,
    pub embedding_mb: u64,
    pub chat_mb: u64,
    pub available_mb: u64,
    pub budget_mb: u64,
}
