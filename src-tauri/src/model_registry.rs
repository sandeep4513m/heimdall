/// model_registry.rs — Authoritative source for what models can do.
///
/// Lazy + smart cache: detect on first use, cache forever (in SQLite),
/// invalidate when digest changes. Single source of truth for chat,
/// vision, thinking, embedding, and tools across the app.
///
/// This file currently contains only the struct skeleton, the
/// `WARM_UP_CONCURRENCY` bound, the `DetectFuture` type alias, the
/// `new` constructor, and the private `clone_for_task` helper used by
/// background tasks. The detection chain (`detect_capabilities`),
/// persistence (`persist` / `read_row` / `evict`), the public API
/// (`get_capabilities` / `list_with_capabilities` / `refresh` /
/// `warm_up`), and the settings accessors are added in subsequent
/// tasks (2.3 – 2.10) per the implementation plan.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use futures_util::future::{FutureExt, Shared};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, error, info, instrument, warn};

use crate::models::{
    legacy_capability_from, CapabilitySource, ModelCapabilities, ModelCapability, ModelSettings,
    OllamaModel,
};
use crate::ollama_client::OllamaClient;

/// Caps concurrent `/api/show` calls during warm-up. Ollama serializes
/// `/api/show` requests internally on a single I/O thread; going above
/// 4 buys nothing and risks contention with interactive chat traffic.
///
/// Per Requirement 14.2 (tier-uniform bound) the same limit applies on
/// every hardware tier — even Tier 1 (4 GB RAM) — so warm-up behaviour
/// is indistinguishable across tiers.
const WARM_UP_CONCURRENCY: usize = 4;

/// Future stored in the in-flight dedup map. The output is
/// `Result<_, String>` rather than `anyhow::Error` because
/// `futures_util::future::Shared` requires `Output: Clone` and
/// `anyhow::Error` does not implement `Clone`. Callers map any
/// stringified error back into their own error type at the boundary.
pub(crate) type DetectFuture = Pin<
    Box<dyn std::future::Future<Output = Result<Arc<ModelCapabilities>, String>> + Send>,
>;

/// Authoritative registry of model capabilities.
///
/// Cheap to clone the inner `Arc`s for spawning background tasks — see
/// `clone_for_task`. The struct itself is intended to live behind an
/// `Arc<ModelRegistry>` inside `AppState`.
pub struct ModelRegistry {
    /// SQLite pool. Shared with the rest of the app; the registry only
    /// touches the `model_capabilities` and `model_settings` tables.
    pub(crate) db: SqlitePool,

    /// Cheap-to-clone HTTP wrapper for Ollama. Used by the registry to
    /// issue `/api/show` and `/api/tags` calls during detection and
    /// digest invalidation.
    pub(crate) ollama: OllamaClient,

    /// In-memory cache. Keyed by `model_name` only — the digest lives
    /// inside `ModelCapabilities` and is checked on
    /// `list_with_capabilities`. Read-mostly; written only on detection
    /// or refresh.
    pub cache: Arc<Mutex<HashMap<String, Arc<ModelCapabilities>>>>,

    /// Dedup of in-flight detections. If two tasks ask about the same
    /// `model_name` at once, only one `/api/show` call happens; both
    /// callers await the same `Shared<DetectFuture>`.
    ///
    /// Per Requirement 5.1.b each `model_name` is an independent
    /// deduplication key — concurrent detections for distinct names
    /// run in parallel rather than being serialised.
    pub(crate) in_flight: Arc<Mutex<HashMap<String, Shared<DetectFuture>>>>,

    /// Bounds concurrent `/api/show` calls during `warm_up` to
    /// `WARM_UP_CONCURRENCY` (Requirement 10.2). Wrapped in `Arc` so
    /// every spawned warm-up task shares the same permit pool.
    pub(crate) warm_up_sem: Arc<Semaphore>,
}

impl ModelRegistry {
    /// Construct a registry. Cheap: no I/O. The caller typically wraps
    /// the result in `Arc` and runs `hydrate()` and `warm_up()`
    /// afterwards from `bootstrap`.
    pub fn new(db: SqlitePool, ollama: OllamaClient) -> Self {
        Self {
            db,
            ollama,
            cache: Arc::new(Mutex::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            warm_up_sem: Arc::new(Semaphore::new(WARM_UP_CONCURRENCY)),
        }
    }

    /// Cheap clone of every inner `Arc` plus the `SqlitePool` and
    /// `OllamaClient` (both internally `Arc`-wrapped). Designed for use
    /// inside `tokio::spawn` so a background task owns its own
    /// `ModelRegistry` value without borrowing `&self`.
    ///
    /// This is the building block for `warm_up` and any future
    /// background refresh task. Keeping it `pub(crate)` rather than
    /// public means only the registry's own modules can spawn detached
    /// copies.
    #[allow(dead_code)] // Wired up by tasks 2.6 / 2.9.
    pub(crate) fn clone_for_task(&self) -> Self {
        Self {
            db: self.db.clone(),
            ollama: self.ollama.clone(),
            cache: Arc::clone(&self.cache),
            in_flight: Arc::clone(&self.in_flight),
            warm_up_sem: Arc::clone(&self.warm_up_sem),
        }
    }
}

// ---------------------------------------------------------------------------
// Three-layer capability detection (task 2.3)
//
// The registry's *only* answer to "what can this model do?" comes from this
// strict-priority chain:
//
//   1. /api/show.capabilities — ground truth from Ollama itself.
//   2. Template inspection ({{ .Images }} / {{ .Think }}) — fallback for
//      older Ollama versions that do not return a capabilities array.
//   3. Name substring heuristic — last-resort fallback ported verbatim from
//      `OllamaClient::detect_capability_from_name` so behaviour is unchanged
//      when /api/show is silent.
//
// The first layer to produce *any* positive flag wins; the resulting
// `capability_source` records which layer fired so the future Models tab
// can surface provenance. When no layer fires, `completion = true` is the
// fall-through default — every model can do at least text completion, and
// Heimdall must never return a `ModelCapabilities` value that says "this
// model can do nothing".
// ---------------------------------------------------------------------------

/// Recognised capability strings from `/api/show.capabilities`. Matched
/// case-sensitively per Requirement 3.1; any string outside this set is
/// preserved verbatim in `raw_capabilities` (Requirement 3.1.b) but does
/// not move any flag (Requirement 3.1.a).
///
/// `#[allow(dead_code)]` because the constants are referenced from the
/// `match` arms inside `parse_api_show_capabilities`, which the compiler
/// does not always count as a "use" when the function itself has no
/// non-test callers yet (the public API methods that call it are wired up
/// in tasks 2.6 / 2.7).
#[allow(dead_code)]
const CAP_COMPLETION: &str = "completion";
#[allow(dead_code)]
const CAP_VISION: &str = "vision";
#[allow(dead_code)]
const CAP_THINKING: &str = "thinking";
#[allow(dead_code)]
const CAP_TOOLS: &str = "tools";
#[allow(dead_code)]
const CAP_EMBEDDING: &str = "embedding";

impl ModelRegistry {
    /// Layer 1: parse Ollama's `/api/show.capabilities` array into the five
    /// flag tuple `(completion, vision, thinking, tools, embedding)`.
    ///
    /// Per Requirement 3.1, each flag is `true` iff a case-sensitive exact
    /// match for the corresponding recognised string appears one or more
    /// times in the input. Per Requirement 3.1.a, unrecognised strings are
    /// silently ignored when computing flags. The verbatim array is kept
    /// elsewhere (in `raw_capabilities`) to satisfy 3.1.b — this function
    /// only inspects the array, never mutates it.
    ///
    /// `&[String]` rather than `&[&str]` to match the shape returned by
    /// `ShowResponseRaw::capabilities` directly without a borrow dance.
    ///
    /// Visibility note: exposed as `pub` (rather than `pub(crate)`) so the
    /// P3 property test in `tests/property_p3_multi_capability.rs` — an
    /// integration test crate that lives outside `heimdall_lib` — can call
    /// the parser directly. The parser is a pure function with no
    /// invariants that would be broken by external callers.
    #[allow(dead_code)] // Wired up by `detect_capabilities` (this file) and task 2.6.
    pub fn parse_api_show_capabilities(
        caps: &[String],
    ) -> (bool, bool, bool, bool, bool) {
        let mut completion = false;
        let mut vision = false;
        let mut thinking = false;
        let mut tools = false;
        let mut embedding = false;

        for c in caps {
            // Case-sensitive `==` against the recognised set. Anything
            // outside this set is silently ignored — we do not lowercase,
            // trim, or otherwise normalise; Ollama's wire form is the
            // contract.
            match c.as_str() {
                CAP_COMPLETION => completion = true,
                CAP_VISION => vision = true,
                CAP_THINKING => thinking = true,
                CAP_TOOLS => tools = true,
                CAP_EMBEDDING => embedding = true,
                _ => {}
            }
        }

        (completion, vision, thinking, tools, embedding)
    }

    /// Layer 2: inspect the model template for known capability markers.
    /// Returns `(vision, thinking)` — the two markers Ollama's templates
    /// have historically used.
    ///
    /// `{{ .Images }}` indicates the template injects images into the
    /// prompt (vision). `{{ .Think }}` indicates the template gates a
    /// thinking-aware section (native thinking). We accept both the
    /// canonically-spaced form `{{ .Images }}` and the no-space form
    /// `{{.Images}}` because Ollama has shipped both over time and the
    /// template is otherwise opaque to us — see
    /// `OllamaClient::detect_capability_from_template` for the same
    /// dual-form check on the legacy path.
    ///
    /// Layer 2 only addresses `vision` and `thinking`. The other three
    /// flags (`completion`, `tools`, `embedding`) are not derivable from
    /// template markers and remain at their layer-3/default values when
    /// this layer fires.
    /// Visibility note: exposed as `pub` (rather than `pub(crate)`) so the
    /// P6 property test in `tests/property_p6_source_priority.rs` can call
    /// the parser directly from an integration test crate.
    #[allow(dead_code)] // Wired up by `detect_capabilities` (this file) and task 2.6.
    pub fn parse_template_markers(template: &str) -> (bool, bool) {
        let vision = template.contains("{{ .Images }}") || template.contains("{{.Images}}");
        let thinking = template.contains("{{ .Think }}") || template.contains("{{.Think}}");
        (vision, thinking)
    }

    /// Layer 3: name-substring heuristic, ported verbatim from
    /// `OllamaClient::detect_capability_from_name` and the related
    /// `detect_capability_from_template` family lists.
    ///
    /// Returns `(vision, thinking, embedding, tools)`. The `completion`
    /// flag is not produced by this layer — it is the fall-through default
    /// (`true`) applied by `detect_capabilities` when no layer fires.
    ///
    /// Preserves the legacy ordering for diagnosability:
    ///   * Embedding wins over Vision/Thinking — embedding model names
    ///     occasionally collide with reasoning tokens (e.g. a future
    ///     `qwen3-embed`) and embedding is the more restrictive
    ///     classification.
    ///   * Vision wins over Thinking — a hypothetical `gemma4-vision`
    ///     model should be flagged as vision-capable rather than
    ///     thinking-capable, mirroring the legacy single-enum priority.
    ///
    /// `tools` is currently never set by this heuristic (the legacy code
    /// had no name-based tools detection); it is left `false` so the
    /// caller can always rely on layer 1 (`/api/show.capabilities`) for
    /// authoritative tool-use answers.
    /// Visibility note: exposed as `pub` (rather than `pub(crate)`) so the
    /// P6 property test in `tests/property_p6_source_priority.rs` can call
    /// the heuristic directly from an integration test crate.
    #[allow(dead_code)] // Wired up by `detect_capabilities` (this file) and task 2.6.
    pub fn name_heuristic(name: &str) -> (bool, bool, bool, bool) {
        let lower = name.to_lowercase();

        // Embedding family — checked first. Same substring set as
        // OllamaClient::detect_capability_from_name plus the family-name
        // markers (`bert`, `nomic`) from detect_capability_from_template.
        let embedding = lower.contains("embed")
            || lower.contains("nomic")
            || lower.contains("minilm")
            || lower.contains("mxbai-embed")
            || lower.contains("snowflake")
            || lower.contains("bge-")
            || lower.contains("all-minilm")
            || lower.contains("bert");

        // Vision family — checked before thinking so a hybrid name wins
        // for vision. Verbatim port of the legacy substring list.
        let vision = !embedding
            && (lower.contains("llava")
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
                || lower.contains("clip"));

        // Thinking family — only fires when neither embedding nor vision
        // matched, mirroring the legacy `if/else if` chain. Note Gemma 3
        // intentionally not listed: only Gemma 4+ supports thinking.
        let thinking = !embedding
            && !vision
            && (lower.contains("deepseek-r1")
                || lower.contains("deepseek-r2")
                || lower.contains("qwen3")
                || lower.contains("qwq")
                || lower.contains("gemma4")
                || lower.contains("gemma-4"));

        // No legacy name-based tools detection — leave at false.
        let tools = false;

        (vision, thinking, embedding, tools)
    }

    /// Three-layer detection orchestrator.
    ///
    /// Strict priority: layer 1 (api_show capabilities) → layer 2 (template
    /// markers) → layer 3 (name heuristic). The first layer that produces
    /// *any* positive signal wins and stamps `capability_source`; otherwise
    /// the fallback is `Heuristic` with `completion = true` and every other
    /// flag false (Requirement 6.2.b).
    ///
    /// Failure handling:
    ///   * `/api/show` HTTP error or JSON parse error: log at `warn`, fall
    ///     through to layer 2 with no template (`None`), then layer 3.
    ///     Per the design's error-handling matrix this is "degraded mode"
    ///     and the resulting `capability_source` is whichever fallback
    ///     fires (`Heuristic`, since no template is available).
    ///   * Empty `capabilities` array (very old Ollama): treated identical
    ///     to "layer 1 produced no signal" — fall to layer 2, which uses
    ///     the template Ollama did include in the response.
    ///
    /// `digest` is passed through to the resulting `ModelCapabilities`
    /// without further validation — the caller has already obtained it
    /// from `/api/tags` and is responsible for the digest-aware persist
    /// path. Family/parameter_size/quantization come from `/api/show`
    /// when the call succeeded; they remain `None` on the degraded path.
    #[allow(dead_code)] // Wired up by `detect_and_persist` in task 2.6.
    pub(crate) async fn detect_capabilities(
        &self,
        model_name: &str,
        digest: &str,
    ) -> Result<ModelCapabilities> {
        // Issue the single /api/show call. The OllamaClient already
        // applies a 5-second per-request timeout (Requirement 13.6)
        // and never retries; we treat any error here as "layer 1
        // failed" and fall through.
        let show_result = self.ollama.show(model_name).await;

        let (raw_caps, template, family, parameter_size, quantization_level): (
            Vec<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = match show_result {
            Ok(resp) => {
                let raw = resp.capabilities.unwrap_or_default();
                let (family, parameter_size, quantization_level) = match resp.details {
                    Some(d) => (d.family, d.parameter_size, d.quantization_level),
                    None => (None, None, None),
                };
                (raw, resp.template, family, parameter_size, quantization_level)
            }
            Err(e) => {
                // Per the design's error-handling matrix: HTTP 500 /
                // malformed JSON falls through to layer 2 (using any
                // locally cached template from a prior row) and then
                // layer 3. The current schema does not persist the raw
                // template, so the cached-template path is a no-op
                // here — we degrade directly to layer 3 with empty raw
                // caps and no template. The structure leaves room to
                // wire a cached template through later without changing
                // the algorithm shape.
                warn!(
                    model = %model_name,
                    error = %e,
                    "/api/show failed — degrading to template/heuristic fallback",
                );
                (Vec::new(), None, None, None, None)
            }
        };

        // ── Layer 1: api_show capabilities (ground truth) ─────────────────
        if !raw_caps.is_empty() {
            let (completion, vision, thinking, tools, embedding) =
                Self::parse_api_show_capabilities(&raw_caps);

            // Requirement 6.1 says we set `capability_source = ApiShow` only
            // when at least one element of the capabilities array is drawn
            // from the recognised set. An array of *only* unrecognised
            // strings (e.g. a future `["audio"]` from a newer Ollama) does
            // not count as a usable layer-1 signal — fall through to
            // layer 2/3 so we still produce a sensible row.
            if completion || vision || thinking || tools || embedding {
                debug!(
                    model = %model_name,
                    raw_caps = ?raw_caps,
                    "capability source: api_show",
                );
                let now = Utc::now().timestamp();
                return Ok(ModelCapabilities {
                    model_name: model_name.to_string(),
                    digest: digest.to_string(),
                    completion,
                    vision,
                    thinking,
                    tools,
                    embedding,
                    capability_source: CapabilitySource::ApiShow,
                    raw_capabilities: raw_caps,
                    family,
                    parameter_size,
                    quantization_level,
                    detected_at: now,
                    updated_at: now,
                });
            }
        }

        // ── Layer 2: template marker inspection ───────────────────────────
        if let Some(ref tmpl) = template {
            let (tmpl_vision, tmpl_thinking) = Self::parse_template_markers(tmpl);
            if tmpl_vision || tmpl_thinking {
                debug!(
                    model = %model_name,
                    vision = tmpl_vision,
                    thinking = tmpl_thinking,
                    "capability source: template",
                );
                let now = Utc::now().timestamp();
                return Ok(ModelCapabilities {
                    model_name: model_name.to_string(),
                    digest: digest.to_string(),
                    // Requirement 6.2.a: completion stays at default true,
                    // tools/embedding at default false; only the matched
                    // template marker(s) flip a flag.
                    completion: true,
                    vision: tmpl_vision,
                    thinking: tmpl_thinking,
                    tools: false,
                    embedding: false,
                    capability_source: CapabilitySource::Template,
                    // Layer 2 does not inspect the api_show array so
                    // raw_capabilities stays empty — there's no array to
                    // surface that came from this layer.
                    raw_capabilities: Vec::new(),
                    family,
                    parameter_size,
                    quantization_level,
                    detected_at: now,
                    updated_at: now,
                });
            }
        }

        // ── Layer 3: name-substring heuristic (last resort) ───────────────
        let (h_vision, h_thinking, h_embedding, h_tools) = Self::name_heuristic(model_name);
        debug!(
            model = %model_name,
            vision = h_vision,
            thinking = h_thinking,
            embedding = h_embedding,
            tools = h_tools,
            "capability source: heuristic",
        );
        let now = Utc::now().timestamp();
        Ok(ModelCapabilities {
            model_name: model_name.to_string(),
            digest: digest.to_string(),
            // Requirement 6.2.b: when no layer fires, completion = true,
            // every other flag false. The heuristic may set vision /
            // thinking / embedding to true; tools remains false for now
            // since the legacy heuristic had no tools detection.
            completion: true,
            vision: h_vision,
            thinking: h_thinking,
            tools: h_tools,
            embedding: h_embedding,
            capability_source: CapabilitySource::Heuristic,
            raw_capabilities: Vec::new(),
            family,
            parameter_size,
            quantization_level,
            detected_at: now,
            updated_at: now,
        })
    }
}

// ---------------------------------------------------------------------------
// SQLite persistence layer (task 2.4)
//
// Three methods own the registry's interaction with the
// `model_capabilities` table:
//
//   * `persist` — INSERT OR REPLACE a single row keyed by `model_name`.
//     `raw_capabilities` is serialised to a JSON string. Write errors
//     bubble up to the caller; the cache-only fallback for a write
//     failure lives in `get_capabilities` (task 2.6).
//
//   * `read_row` — SELECT a row by `model_name` and treat any of three
//     conditions as "no row": the row is absent, the persisted digest
//     does not equal the live digest the caller passed in, or the
//     `raw_capabilities` JSON fails to deserialise. The last case is
//     logged at `warn` so a corrupted row never crashes the registry —
//     the next detection cycle simply re-detects and overwrites.
//
//   * `evict` — Remove the in-memory cache entry first (always succeeds)
//     then DELETE the SQLite row scoped by both `model_name` and the
//     previously cached `digest`. The digest scope guarantees we only
//     remove the row we know about: if a concurrent task has already
//     persisted a fresh row with a new digest, the DELETE simply matches
//     zero rows. SQLite delete failures are logged at `warn` and never
//     propagate — eviction is a best-effort cleanup, not a critical path.
//
// All three methods are pub(crate); the public API in `get_capabilities`
// / `list_with_capabilities` / `refresh` (later tasks) compose them.
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// Persist a freshly detected `ModelCapabilities` row, replacing any
    /// older entry for the same `model_name`.
    ///
    /// `raw_capabilities` is serialised to a JSON string; the rest of the
    /// fields map to the columns defined in `db::run_migrations`. The
    /// `capability_source` enum serialises via its `#[serde(rename_all =
    /// "snake_case")]` form (`api_show | template | heuristic |
    /// user_override`) so the on-disk text is identical to the wire form
    /// we ship to the frontend.
    ///
    /// On write failure the error is returned verbatim so the caller can
    /// decide whether to retry, fall back to the cache only (the path in
    /// `get_capabilities`), or surface the error.
    #[allow(dead_code)] // Wired up by `get_capabilities` (task 2.6) and `refresh` (task 2.8).
    #[instrument(skip(self, caps), fields(model = %caps.model_name, digest = %caps.digest))]
    pub async fn persist(&self, caps: &ModelCapabilities) -> Result<()> {
        // Serialise the verbatim capability array. `serde_json::to_string`
        // never fails for `Vec<String>`, but plumb the error through with
        // `?` rather than `.unwrap()` so a future change of type doesn't
        // silently panic at runtime.
        let raw_caps_json = serde_json::to_string(&caps.raw_capabilities)
            .context("Failed to serialise raw_capabilities to JSON")?;

        // Serialise the enum via its serde wire form so the SQL TEXT
        // column ends up holding `"api_show"` / `"template"` / etc rather
        // than Rust's `Debug` form. `serde_json::to_value(...).as_str()`
        // would also work but is slightly more allocation than needed.
        let source_str = match caps.capability_source {
            CapabilitySource::ApiShow => "api_show",
            CapabilitySource::Template => "template",
            CapabilitySource::Heuristic => "heuristic",
            CapabilitySource::UserOverride => "user_override",
        };

        sqlx::query(
            "INSERT OR REPLACE INTO model_capabilities (
                model_name,
                digest,
                completion,
                vision,
                thinking,
                tools,
                embedding,
                capability_source,
                raw_capabilities,
                family,
                parameter_size,
                quantization_level,
                detected_at,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);",
        )
        .bind(&caps.model_name)
        .bind(&caps.digest)
        .bind(caps.completion as i64)
        .bind(caps.vision as i64)
        .bind(caps.thinking as i64)
        .bind(caps.tools as i64)
        .bind(caps.embedding as i64)
        .bind(source_str)
        .bind(&raw_caps_json)
        .bind(caps.family.as_deref())
        .bind(caps.parameter_size.as_deref())
        .bind(caps.quantization_level.as_deref())
        .bind(caps.detected_at)
        .bind(caps.updated_at)
        .execute(&self.db)
        .await
        .context("Failed to upsert model_capabilities row")?;

        debug!(
            model = %caps.model_name,
            digest = %caps.digest,
            source = source_str,
            "persisted model_capabilities row",
        );

        Ok(())
    }

    /// Read a single `model_capabilities` row by primary key and return
    /// it only when it is fresh, well-formed, and matches the supplied
    /// `live_digest`.
    ///
    /// Returns `None` in any of these cases:
    ///   * No row exists for `model_name`.
    ///   * A row exists but its `digest` column does not equal
    ///     `live_digest` — the row is stale and the caller is expected to
    ///     trigger detection for the new digest.
    ///   * A row exists with a matching digest but its `raw_capabilities`
    ///     JSON column fails to deserialise. This is logged at `warn` —
    ///     the corrupted row stays in place; the caller will re-detect
    ///     and the next `persist()` overwrites it.
    ///
    /// The function returns `Err` only on actual SQLite failures (e.g.
    /// connection errors). A transient SQL error is genuinely abnormal
    /// and worth surfacing to the caller; data shape problems are not.
    #[allow(dead_code)] // Wired up by `get_capabilities` (task 2.6) and `list_with_capabilities` (task 2.7).
    #[instrument(skip(self), fields(model = %model_name, live_digest = %live_digest))]
    pub async fn read_row(
        &self,
        model_name: &str,
        live_digest: &str,
    ) -> Result<Option<ModelCapabilities>> {
        // Read every column individually rather than using
        // `query_as::<_, ModelCapabilities>`. The struct has a
        // `Vec<String>` field (`raw_capabilities`) that is stored as a
        // JSON TEXT column on disk, and the `capability_source` enum
        // needs string-to-enum mapping. `sqlx::FromRow` cannot do either
        // automatically, so we read primitive columns and assemble the
        // struct here.
        type Row = (
            String,         // model_name
            String,         // digest
            i64,            // completion
            i64,            // vision
            i64,            // thinking
            i64,            // tools
            i64,            // embedding
            String,         // capability_source
            Option<String>, // raw_capabilities (JSON text)
            Option<String>, // family
            Option<String>, // parameter_size
            Option<String>, // quantization_level
            i64,            // detected_at
            i64,            // updated_at
        );

        let row: Option<Row> = sqlx::query_as(
            "SELECT
                model_name,
                digest,
                completion,
                vision,
                thinking,
                tools,
                embedding,
                capability_source,
                raw_capabilities,
                family,
                parameter_size,
                quantization_level,
                detected_at,
                updated_at
             FROM model_capabilities
             WHERE model_name = ?
             LIMIT 1;",
        )
        .bind(model_name)
        .fetch_optional(&self.db)
        .await
        .context("Failed to SELECT model_capabilities row")?;

        let Some((
            db_name,
            db_digest,
            completion,
            vision,
            thinking,
            tools,
            embedding,
            source_str,
            raw_caps_json,
            family,
            parameter_size,
            quantization_level,
            detected_at,
            updated_at,
        )) = row
        else {
            // No row at all — caller's first detection.
            return Ok(None);
        };

        // Digest mismatch: the user re-pulled the model since we last
        // saw it. Returning `None` causes the caller to fall through to
        // detection, which will overwrite this row via INSERT OR REPLACE.
        // We do *not* delete the stale row here — eviction is the
        // explicit responsibility of `evict`, called from
        // `list_with_capabilities` when it observes the mismatch.
        if db_digest != live_digest {
            debug!(
                model = %model_name,
                cached_digest = %db_digest,
                live_digest = %live_digest,
                "read_row: digest mismatch — returning None",
            );
            return Ok(None);
        }

        // Parse the raw_capabilities JSON. Treat any deserialisation
        // failure as if the row didn't exist — log once at `warn` so the
        // operator notices, but never crash the registry over a malformed
        // text column. The next detection will overwrite the bad row.
        let raw_capabilities: Vec<String> = match raw_caps_json.as_deref() {
            Some(s) => match serde_json::from_str::<Vec<String>>(s) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        model = %model_name,
                        error = %e,
                        "read_row: raw_capabilities JSON failed to deserialise — treating row as absent",
                    );
                    return Ok(None);
                }
            },
            // NULL raw_capabilities is legal for non-ApiShow rows
            // (the Template and Heuristic layers don't produce an array).
            None => Vec::new(),
        };

        // Map the text capability_source back to the enum. An unknown
        // value is treated like a malformed row: log at `warn` and
        // return None so the caller re-detects and overwrites.
        let capability_source = match source_str.as_str() {
            "api_show" => CapabilitySource::ApiShow,
            "template" => CapabilitySource::Template,
            "heuristic" => CapabilitySource::Heuristic,
            "user_override" => CapabilitySource::UserOverride,
            other => {
                warn!(
                    model = %model_name,
                    capability_source = other,
                    "read_row: unknown capability_source value — treating row as absent",
                );
                return Ok(None);
            }
        };

        Ok(Some(ModelCapabilities {
            model_name: db_name,
            digest: db_digest,
            completion: completion != 0,
            vision: vision != 0,
            thinking: thinking != 0,
            tools: tools != 0,
            embedding: embedding != 0,
            capability_source,
            raw_capabilities,
            family,
            parameter_size,
            quantization_level,
            detected_at,
            updated_at,
        }))
    }

    /// Evict a stale entry from the in-memory cache and the SQLite row
    /// for that `model_name`.
    ///
    /// Order matters: the cache is removed *first* so even if the SQLite
    /// delete fails we never serve a stale `Arc<ModelCapabilities>` from
    /// memory. The DELETE is scoped by the `(model_name, digest)` pair
    /// of the entry that was just removed from the cache so a concurrent
    /// `persist()` writing a fresh row for the new digest is not
    /// clobbered by this eviction.
    ///
    /// Per Requirement 2.1 the SQLite delete failure must be logged at
    /// `warn` and must not propagate as a user-visible error, so the
    /// surrounding `list_with_capabilities()` flow can complete and
    /// return its result. The function therefore always returns `Ok(())`.
    ///
    /// When no entry was in the cache (the typical "evict before any
    /// detection" case), we have no digest to scope by, so the DELETE is
    /// skipped — there is nothing to evict from disk that wasn't
    /// already absent from cache.
    #[allow(dead_code)] // Wired up by `list_with_capabilities` (task 2.7).
    #[instrument(skip(self), fields(model = %model_name))]
    pub async fn evict(&self, model_name: &str) -> Result<()> {
        // Step 1: remove from in-memory cache. We capture the digest of
        // the evicted entry so the SQLite DELETE below can scope to that
        // specific row and avoid clobbering a freshly-persisted entry.
        let evicted_digest: Option<String> = {
            let mut cache = self.cache.lock().await;
            cache.remove(model_name).map(|arc| arc.digest.clone())
        };

        // Step 2: DELETE from SQLite, scoped to the digest we just
        // evicted from cache. If the cache had no entry there's no
        // digest to scope by; skip the DELETE.
        let Some(digest) = evicted_digest else {
            debug!(
                model = %model_name,
                "evict: no cached entry — skipping SQLite delete",
            );
            return Ok(());
        };

        let delete_result = sqlx::query(
            "DELETE FROM model_capabilities
             WHERE model_name = ? AND digest = ?;",
        )
        .bind(model_name)
        .bind(&digest)
        .execute(&self.db)
        .await;

        match delete_result {
            Ok(out) => {
                debug!(
                    model = %model_name,
                    digest = %digest,
                    rows_affected = out.rows_affected(),
                    "evict: SQLite delete complete",
                );
            }
            Err(e) => {
                // Per Requirement 2.1: log at warn, do not propagate.
                warn!(
                    model = %model_name,
                    digest = %digest,
                    error = %e,
                    "evict: SQLite delete failed — cache eviction still applied",
                );
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Startup hydration (task 2.5)
//
// `hydrate()` runs once at bootstrap, after the SQLite pool is built and
// before the warm-up task is spawned. It reads every row in
// `model_capabilities` in a single SELECT and inserts a fully-restored
// `Arc<ModelCapabilities>` into the in-memory cache for each well-formed
// row, so the very first `get_capabilities(model_name)` call after launch
// is a cache hit (≤1 ms) for every previously detected model whose live
// digest is unchanged (Requirement 1.3).
//
// The function is *non-fatal at startup* (Requirement 1.4): a SQLite
// failure on the bulk SELECT, or a row-level deserialisation failure on
// any individual row, is logged at `error` level and otherwise swallowed.
// `hydrate()` always returns `Ok(())` so bootstrap continues regardless.
// Models whose rows failed to load simply fall through to the normal
// detection path on first use.
//
// Idempotency: re-running `hydrate()` just overwrites the cache entries
// with the same data. There is no harm in calling it twice; the design
// document explicitly notes the method is idempotent.
//
// Per-row decode mirrors `read_row` exactly — same JSON parse for
// `raw_capabilities`, same text-to-enum mapping for `capability_source`,
// same `i64`-to-`bool` mapping for the five flag columns. The two
// methods deliberately read the same shape so that any future schema
// change has only one decode path to update.
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// Load every persisted `model_capabilities` row into the in-memory
    /// cache. Called once from `bootstrap` after `db::init_pool` returns.
    ///
    /// Always returns `Ok(())`. SQLite failures and per-row decode
    /// failures are logged at `error` level (Requirement 1.4) and the
    /// affected rows are simply absent from the cache afterwards — the
    /// next `get_capabilities` call for those models will fall through
    /// to the detection path defined in Requirement 1.1.
    ///
    /// Cache replacement semantics: each row's `model_name` is inserted
    /// unconditionally, so calling `hydrate()` after entries already
    /// exist replaces them with the on-disk values. Bootstrap calls
    /// this exactly once before any other registry method runs, so
    /// cache contention is not a concern in practice; the lock is
    /// nonetheless taken once at the end so we hold it for the minimum
    /// time necessary.
    #[instrument(skip(self))]
    pub async fn hydrate(&self) -> Result<()> {
        // Same column shape as `read_row`. Reading every primitive
        // column individually rather than via `query_as::<_,
        // ModelCapabilities>` for the same reasons documented on
        // `read_row`: the JSON `Vec<String>` field and the string-to-
        // enum mapping for `capability_source` cannot be expressed via
        // `sqlx::FromRow` without manual handling.
        type Row = (
            String,         // model_name
            String,         // digest
            i64,            // completion
            i64,            // vision
            i64,            // thinking
            i64,            // tools
            i64,            // embedding
            String,         // capability_source
            Option<String>, // raw_capabilities (JSON text)
            Option<String>, // family
            Option<String>, // parameter_size
            Option<String>, // quantization_level
            i64,            // detected_at
            i64,            // updated_at
        );

        // Single bulk SELECT (Requirement 1.3 — "in a single query").
        // No WHERE clause: every row in the table is a candidate.
        let rows_result: Result<Vec<Row>, sqlx::Error> = sqlx::query_as(
            "SELECT
                model_name,
                digest,
                completion,
                vision,
                thinking,
                tools,
                embedding,
                capability_source,
                raw_capabilities,
                family,
                parameter_size,
                quantization_level,
                detected_at,
                updated_at
             FROM model_capabilities;",
        )
        .fetch_all(&self.db)
        .await;

        let rows = match rows_result {
            Ok(r) => r,
            Err(e) => {
                // Per Requirement 1.4: a SQLite read failure inside
                // `hydrate()` is logged at `error` level, the cache
                // stays empty for the affected rows (here, all of
                // them), and we still return `Ok(())` so bootstrap
                // continues. Subsequent `get_capabilities` calls fall
                // through to detection.
                error!(
                    error = %e,
                    "hydrate: SQLite SELECT against model_capabilities failed — \
                     starting with an empty cache",
                );
                return Ok(());
            }
        };

        let total_rows = rows.len();

        // Decode each row into an `Arc<ModelCapabilities>`. Per-row
        // failures are logged at `error` and the row is skipped — one
        // bad row must not poison the whole hydrate.
        let mut decoded: Vec<(String, Arc<ModelCapabilities>)> = Vec::with_capacity(total_rows);

        for row in rows {
            let (
                db_name,
                db_digest,
                completion,
                vision,
                thinking,
                tools,
                embedding,
                source_str,
                raw_caps_json,
                family,
                parameter_size,
                quantization_level,
                detected_at,
                updated_at,
            ) = row;

            // Parse `raw_capabilities`. NULL is legal (template /
            // heuristic rows have no array). A non-NULL value that
            // fails to parse is treated as a row-level failure: log at
            // `error`, skip the row.
            let raw_capabilities: Vec<String> = match raw_caps_json.as_deref() {
                Some(s) => match serde_json::from_str::<Vec<String>>(s) {
                    Ok(v) => v,
                    Err(e) => {
                        error!(
                            model = %db_name,
                            error = %e,
                            "hydrate: raw_capabilities JSON failed to deserialise — \
                             skipping row, will re-detect on first use",
                        );
                        continue;
                    }
                },
                None => Vec::new(),
            };

            // Map text `capability_source` back to the enum. An
            // unknown value (e.g. a future variant from a newer
            // schema) is also a row-level failure.
            let capability_source = match source_str.as_str() {
                "api_show" => CapabilitySource::ApiShow,
                "template" => CapabilitySource::Template,
                "heuristic" => CapabilitySource::Heuristic,
                "user_override" => CapabilitySource::UserOverride,
                other => {
                    error!(
                        model = %db_name,
                        capability_source = other,
                        "hydrate: unknown capability_source value — \
                         skipping row, will re-detect on first use",
                    );
                    continue;
                }
            };

            // Bitwise restoration (Requirement 1.3): every flag, the
            // capability_source, and the parsed raw_capabilities are
            // reconstructed exactly as persisted. Integer 0/1 columns
            // map back to bool via `!= 0`, matching `read_row`.
            let caps = ModelCapabilities {
                model_name: db_name.clone(),
                digest: db_digest,
                completion: completion != 0,
                vision: vision != 0,
                thinking: thinking != 0,
                tools: tools != 0,
                embedding: embedding != 0,
                capability_source,
                raw_capabilities,
                family,
                parameter_size,
                quantization_level,
                detected_at,
                updated_at,
            };

            decoded.push((db_name, Arc::new(caps)));
        }

        let decoded_count = decoded.len();

        // Take the cache lock once and insert all decoded entries in a
        // single critical section. Bootstrap is single-threaded with
        // respect to the registry at this point, so this is mostly a
        // matter of style — but keeping the lock scope minimal means a
        // future caller racing `hydrate()` with `get_capabilities()`
        // sees a consistent snapshot rather than a half-loaded cache.
        {
            let mut cache = self.cache.lock().await;
            for (name, arc) in decoded {
                cache.insert(name, arc);
            }
        }

        debug!(
            total_rows,
            loaded = decoded_count,
            skipped = total_rows - decoded_count,
            "hydrate: cache populated from model_capabilities",
        );

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public `get_capabilities` API + supporting helpers (task 2.6)
//
// `get_capabilities(model_name)` is *the* hot path through the registry —
// every chat turn, every model selection, every warm-up tick passes
// through it. Three properties matter:
//
//   1. **Zero HTTP, zero SQLite on the warm-cache path.** The first thing
//      the function does is take the cache mutex and look the entry up.
//      If it's there, we return an `Arc::clone` of the cached value and
//      release every lock before returning. No `/api/tags`, no `/api/show`,
//      no `SELECT`. Requirement 1.2 in its strictest form.
//
//   2. **N concurrent waiters share one detection.** When two or more
//      callers miss the cache for the same `model_name` simultaneously,
//      only one `/api/show` HTTP call is issued. The dedup is implemented
//      via `Shared<DetectFuture>` stored in `self.in_flight`: the first
//      caller installs the shared future; every subsequent caller clones
//      and awaits the same handle. All N callers receive `Arc::ptr_eq`-
//      equal `Arc<ModelCapabilities>` values on success because the inner
//      future allocates exactly one `Arc` whose ownership clones flow out
//      to every poller. Requirement 5.1 / 5.1.b.
//
//   3. **Failures are not sticky.** Whether the inner future succeeds or
//      fails, the dedup entry is removed from `in_flight` after the join
//      completes. A subsequent `get_capabilities` call after a failure is
//      eligible to start a fresh detection attempt (Requirement 5.1.a).
//
// `detect_and_persist(model_name, digest)` is the inner helper called when
// the dedup future actually does work: run the three-layer detection,
// `INSERT OR REPLACE` the row, and return an `Arc<ModelCapabilities>`.
// Persist failures degrade gracefully — they are logged at `warn` and the
// call still returns the freshly detected `Arc` so the caller can serve
// it from the in-memory cache (cache-only fallback per the design's
// error-handling matrix; the next successful detection overwrites the
// missing row).
//
// `live_digest_for(model_name)` is the small private helper that fetches
// `/api/tags` once and returns the digest reported for `model_name`.
// `get_capabilities` calls it on cache miss (per Requirement 1.1, the
// digest persisted alongside capabilities must equal the live digest from
// `/api/tags`). Re-uses `OllamaClient::list_models` since that is the
// existing public API; task 2.7's `list_with_capabilities` will introduce
// a more focused `list_tags_raw` that strips the legacy capability
// synthesis, at which point this helper can switch over.
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// Return the live digest reported by Ollama's `/api/tags` for
    /// `model_name`. Errors when `/api/tags` itself fails or when the
    /// model is not present in the response (e.g. the user `ollama rm`d
    /// it between bootstrapping and now).
    ///
    /// Currently a thin wrapper over `OllamaClient::list_models` — the
    /// same `/api/tags` round-trip the rest of the app uses. Task 2.7
    /// introduces `OllamaClient::list_tags_raw` for the list flow; this
    /// helper can switch over then. Until then the per-model heuristic
    /// detection inside `list_models` is wasted work but is otherwise
    /// inert (it only computes a `ModelCapability` enum that we discard).
    #[allow(dead_code)] // Invoked by the in-flight dedup future inside `get_capabilities`.
    #[instrument(skip(self), fields(model = %model_name))]
    pub(crate) async fn live_digest_for(&self, model_name: &str) -> Result<String> {
        let models = self
            .ollama
            .list_models()
            .await
            .with_context(|| {
                format!(
                    "/api/tags failed while resolving live digest for model '{}'",
                    model_name
                )
            })?;

        models
            .iter()
            .find(|m| m.name == model_name)
            .map(|m| m.digest.clone())
            .ok_or_else(|| anyhow!("model '{}' not in /api/tags response", model_name))
    }

    /// Detect capabilities for `(model_name, digest)` via the three-layer
    /// chain and persist the result. Returns the freshly built
    /// `Arc<ModelCapabilities>` so `get_capabilities` can simultaneously
    /// insert it into the in-memory cache and return it to every concurrent
    /// waiter on the shared future.
    ///
    /// Persist semantics: on `persist` failure the function logs at `warn`
    /// and continues — the `Arc` is still returned to the caller. This is
    /// the "cache-only fallback" path in the design's error-handling
    /// matrix: a transient SQLite error must not prevent the user from
    /// chatting with their model, only delay the durable record. The next
    /// successful detection (e.g. on a subsequent app launch) will
    /// overwrite the missing row.
    ///
    /// The `Result<_, String>` return type is required by the dedup
    /// machinery: `Shared<F>` requires `F::Output: Clone`, and
    /// `anyhow::Error` does not implement `Clone`. The `String` form is
    /// re-wrapped into an `anyhow::Error` at the boundary of
    /// `get_capabilities`.
    #[allow(dead_code)] // Invoked by the in-flight dedup future inside `get_capabilities`.
    #[instrument(skip(self), fields(model = %model_name, digest = %digest))]
    pub(crate) async fn detect_and_persist(
        &self,
        model_name: &str,
        digest: &str,
    ) -> Result<Arc<ModelCapabilities>, String> {
        // Layer 1/2/3 detection. Errors here are real (every layer
        // fell through to a propagated error rather than producing a
        // value) and must surface to all N waiters.
        let caps = self
            .detect_capabilities(model_name, digest)
            .await
            .map_err(|e| {
                format!(
                    "detect_capabilities failed for model '{}': {}",
                    model_name, e
                )
            })?;

        let arc = Arc::new(caps);

        // Persist the row. On failure log and continue — the in-memory
        // cache will still serve the value for this process lifetime
        // even if the durable row is missing. The user-facing UX never
        // notices the difference; the next clean detection run repairs
        // the on-disk store.
        if let Err(e) = self.persist(&arc).await {
            warn!(
                model = %model_name,
                digest = %digest,
                error = %e,
                "persist failed — falling back to cache-only mode for this row",
            );
        }

        Ok(arc)
    }

    /// Authoritative answer to "what can this model do?".
    ///
    /// Three-step algorithm matching the design pseudocode:
    ///
    ///   1. **Cache lookup.** If `self.cache` has an entry for
    ///      `model_name`, clone the `Arc` and return immediately. This
    ///      branch issues zero HTTP and zero SQLite calls (Requirement
    ///      1.2). The lock is released before any further work runs.
    ///
    ///   2. **In-flight dedup.** Take the `in_flight` lock; if another
    ///      task is already detecting `model_name`, clone the existing
    ///      `Shared<DetectFuture>` and await it. Otherwise build the
    ///      detection future via `clone_for_task` (so the boxed future
    ///      owns its own `Arc`s and never aliases `&self`), wrap it in
    ///      `Shared`, and install it under `model_name`. Subsequent
    ///      concurrent callers in step 2 will find this entry and join
    ///      it. All N waiters poll the same shared future and observe
    ///      `Arc::ptr_eq`-equal results on success per Requirement 5.1.
    ///
    ///   3. **Cleanup.** After the join completes — success or failure —
    ///      remove the dedup entry. Failure is non-sticky: a subsequent
    ///      `get_capabilities` call sees an empty `in_flight` slot and
    ///      is eligible to start a fresh detection attempt
    ///      (Requirement 5.1.a).
    ///
    /// The detection future itself does:
    ///   a. Fetch the live digest from `/api/tags` (single HTTP call).
    ///   b. Try the digest-aware DB read; on hit, populate the cache
    ///      and return the row (Requirement 1.1 — the SQLite-cached
    ///      path bypasses `/api/show`).
    ///   c. On DB miss, run `detect_and_persist` to produce a fresh
    ///      `ModelCapabilities` row, persist it, populate the cache,
    ///      and return.
    #[instrument(skip(self), fields(model = %model_name))]
    pub async fn get_capabilities(
        &self,
        model_name: &str,
    ) -> Result<Arc<ModelCapabilities>> {
        // ── Step 1: in-memory cache (warm path) ───────────────────────────
        // Take the cache lock, look up by name, clone the Arc on hit,
        // drop the lock. Zero HTTP, zero SQLite — Requirement 1.2.
        {
            let cache = self.cache.lock().await;
            if let Some(arc) = cache.get(model_name) {
                debug!(model = %model_name, "get_capabilities: cache hit");
                return Ok(Arc::clone(arc));
            }
        }

        // ── Step 2: in-flight dedup (cold path) ───────────────────────────
        // Either join an existing Shared<DetectFuture> for this model_name
        // or install a new one. The shared future captures `clone_for_task`
        // so it owns its own Arcs — `&self` is not aliased into the
        // long-lived boxed future, which keeps the borrow checker happy
        // and prevents lifetime issues when the future outlives the
        // current `&self` borrow.
        let join: Shared<DetectFuture> = {
            let mut in_flight = self.in_flight.lock().await;
            if let Some(shared) = in_flight.get(model_name) {
                debug!(
                    model = %model_name,
                    "get_capabilities: joining in-flight detection",
                );
                shared.clone()
            } else {
                debug!(
                    model = %model_name,
                    "get_capabilities: starting new detection",
                );
                let owned_name = model_name.to_string();
                let me = self.clone_for_task();

                let fut: DetectFuture = Box::pin(async move {
                    // a. Resolve the live digest via /api/tags. Errors
                    //    here propagate to every waiter on the shared
                    //    future; per Requirement 5.1.a all N callers
                    //    observe the failure together and a subsequent
                    //    call after they've all returned is eligible to
                    //    retry.
                    let digest = me
                        .live_digest_for(&owned_name)
                        .await
                        .map_err(|e| {
                            format!(
                                "live_digest_for failed for model '{}': {}",
                                owned_name, e
                            )
                        })?;

                    // b. Digest-aware DB read. A hit means we already
                    //    detected this exact (model_name, digest) pair on
                    //    a previous run; reuse it without /api/show.
                    //    `read_row` returns `None` on a digest mismatch,
                    //    which falls through to step (c) for a fresh
                    //    detection.
                    if let Some(row) = me
                        .read_row(&owned_name, &digest)
                        .await
                        .map_err(|e| {
                            format!(
                                "read_row failed for model '{}': {}",
                                owned_name, e
                            )
                        })?
                    {
                        let arc = Arc::new(row);
                        // Insert into the in-memory cache so the *next*
                        // `get_capabilities` call hits the warm path
                        // and skips this DB read entirely.
                        me.cache
                            .lock()
                            .await
                            .insert(owned_name.clone(), Arc::clone(&arc));
                        debug!(
                            model = %owned_name,
                            "get_capabilities: served from SQLite, populated cache",
                        );
                        return Ok(arc);
                    }

                    // c. Cold path — run the three-layer detection chain
                    //    and persist the result. detect_and_persist takes
                    //    care of all three layers, the persist call, and
                    //    the warn-and-continue fallback when persist
                    //    fails.
                    let arc = me.detect_and_persist(&owned_name, &digest).await?;

                    // Populate the cache so subsequent calls hit warm.
                    me.cache
                        .lock()
                        .await
                        .insert(owned_name.clone(), Arc::clone(&arc));
                    debug!(
                        model = %owned_name,
                        "get_capabilities: detected fresh, persisted, populated cache",
                    );
                    Ok(arc)
                });

                let shared: Shared<DetectFuture> = fut.shared();
                in_flight.insert(model_name.to_string(), shared.clone());
                shared
            }
        };

        // Await the shared future. On `Shared`, every poller after the
        // first sees a clone of the same `Result<Arc<...>, String>` —
        // `Arc::ptr_eq` therefore holds across all N waiters' returned
        // values on success.
        let result = join.await;

        // ── Step 3: cleanup the dedup entry ───────────────────────────────
        // Run under both success and failure paths. On failure this is
        // what makes failures non-sticky (Requirement 5.1.a).
        {
            let mut in_flight = self.in_flight.lock().await;
            in_flight.remove(model_name);
        }

        // Re-wrap the stringified error back into anyhow at the public
        // API boundary; callers see a normal `anyhow::Error` and can
        // chain context as usual.
        result.map_err(|s| anyhow!(s))
    }
}

// ---------------------------------------------------------------------------
// Public `list_with_capabilities` API (task 2.7)
//
// `list_with_capabilities()` is the single source of truth for "what models
// are installed, and what does the registry know about them?". It runs in
// two phases:
//
//   1. Fetch the live tag list from Ollama via `list_tags_raw()` — exactly
//      one `/api/tags` round-trip per invocation, timed out at 5 seconds
//      (Requirement 13.6) and never retried.
//
//   2. For each live entry, resolve its capabilities by walking the cache
//      and (on cache miss) the SQLite row keyed by the live digest. The
//      digest is the cache-validity signal: a cache entry whose digest
//      does *not* equal the live digest is stale and the model has been
//      re-pulled from outside Heimdall (Requirement 2.1). Stale entries
//      are evicted in place — `info!`-logged so the user can see the
//      hot-swap event in their logs — and the returned `OllamaModel`
//      carries `capabilities: None`, signalling to the caller that a
//      fresh detection is needed (typically via `warm_up`).
//
// The function never issues `/api/show`. Re-detection of evicted entries
// is the responsibility of the warm-up dispatched by the bootstrap
// (`task 3.1`) or `list_models` Tauri command (`task 3.4`); keeping this
// method strictly read-only with respect to the network keeps it cheap
// (≤50 ms in the design's perf table) and preserves the "list returns
// fast, capabilities resolve in the background" UX of the design's cold-
// start flow.
//
// Failure modes:
//
//   * `/api/tags` failure or 5s timeout (Requirement 13.1): the registry
//     treats Ollama as unreachable. We return the entries currently in the
//     in-memory cache (an empty list when the cache is empty) and emit a
//     single `warn!` log record so operators can see the cause. The
//     resulting `OllamaModel` values carry `capabilities: Some(_)` since
//     they came from the cache; `size` and `modified_at` come from the
//     cached `ModelCapabilities` where available, or default placeholders
//     otherwise (no live tag data is available to fill them).
//
//   * Per-model `/api/show` 404: handled at the `/api/show` observation
//     sites (e.g. `detect_capabilities`, `refresh`) per Requirement 13.2.
//     By the time those sites are done evicting cache + DB, the deleted
//     model is also absent from `/api/tags`, so it does not appear here.
//     This method does not reach into the network for any single model.
//
//   * Per-model SQLite `read_row` failure: very rare (corrupted JSON or
//     unknown `capability_source` value). `read_row` already returns
//     `Ok(None)` for those cases, so the entry surfaces with
//     `capabilities: None` — the warm-up path will re-detect it and
//     overwrite the bad row.
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// List every locally available model and attach the registry's best
    /// known capabilities to each entry.
    ///
    /// **Algorithm (per task 2.7):**
    ///
    ///   1. `OllamaClient::list_tags_raw()` once. On error or 5-second
    ///      timeout, fall back to the in-memory cache snapshot and `warn!`
    ///      once (Requirement 13.1). Do not propagate the error.
    ///
    ///   2. For each live entry resolve `capabilities`:
    ///      * cache hit + digest match → reuse the cached `Arc`;
    ///      * cache hit + digest mismatch → `info!`-log the hot-swap event
    ///        and call `self.evict(name)`; the returned entry carries
    ///        `capabilities: None`;
    ///      * cache miss → call `self.read_row(name, live_digest)`; on
    ///        hit insert into the cache and return the row; on miss
    ///        (or `read_row` decode failure logged at `warn`) the entry
    ///        carries `capabilities: None`.
    ///
    ///   3. Each returned `OllamaModel` populates the deprecated
    ///      `capability: ModelCapability` field via `legacy_capability_from`
    ///      when capabilities are known, or `TextOnly` when they are not.
    ///      This keeps step-1 callers compiling during the migration; the
    ///      authoritative answer always lives in `capabilities`.
    ///
    /// The function returns `Result<Vec<OllamaModel>>` for ergonomic
    /// reasons (downstream callers can use `?`) but in practice it never
    /// produces `Err`: a SQLite read failure on an individual row is
    /// folded into `capabilities: None`, and an `/api/tags` failure is
    /// folded into the cache-snapshot fallback.
    #[instrument(skip(self))]
    pub async fn list_with_capabilities(&self) -> Result<Vec<OllamaModel>> {
        // ── Phase 1: fetch live tags ──────────────────────────────────────
        // `OllamaClient::list_tags_raw` already enforces the 5s per-request
        // timeout (Requirement 13.6) and never retries. Any error here —
        // transport failure, non-2xx status, JSON parse failure, timeout —
        // collapses into the cache-snapshot fallback below.
        let live_tags = match self.ollama.list_tags_raw().await {
            Ok(tags) => tags,
            Err(e) => {
                // Requirement 13.1: log once at `warn` and serve the cache.
                warn!(
                    error = %e,
                    source = "in-memory cache",
                    "list_with_capabilities: /api/tags unavailable, serving cached entries",
                );
                return Ok(self.snapshot_cache_as_models().await);
            }
        };

        // ── Phase 2: per-tag capability resolution ────────────────────────
        let mut out = Vec::with_capacity(live_tags.len());
        for tag in live_tags {
            // First look up the cache. We clone the `Arc` (not the inner
            // value) so the mutex is held for the minimum time necessary.
            let cached: Option<Arc<ModelCapabilities>> = {
                let cache = self.cache.lock().await;
                cache.get(&tag.name).cloned()
            };

            // Decide what to do based on (cache state, digest match).
            let resolved: Option<Arc<ModelCapabilities>> = match cached {
                Some(arc) if arc.digest == tag.digest => {
                    // Cache hit + matching digest — reuse.
                    Some(arc)
                }
                Some(arc) => {
                    // Cache hit + digest mismatch — the user re-pulled the
                    // model externally. Log the hot-swap event at `info`
                    // (Requirement 2.1 / 2.3), evict cache + SQLite row,
                    // and surface `capabilities: None` so the caller's
                    // warm-up path triggers a fresh detection.
                    info!(
                        model = %tag.name,
                        old_digest = %arc.digest,
                        new_digest = %tag.digest,
                        "digest changed, evicting stale capabilities",
                    );
                    // `evict` always returns Ok per its contract: the
                    // SQLite delete is best-effort and logs at `warn` on
                    // failure rather than propagating.
                    let _ = self.evict(&tag.name).await;
                    None
                }
                None => {
                    // Cache miss — try the digest-aware DB read. A hit
                    // means we previously detected this exact
                    // (model_name, digest) pair on a prior process run
                    // (the cache hasn't been hydrated for this entry yet,
                    // or hydrate failed silently). On hit we populate the
                    // cache so the next call hits warm.
                    match self.read_row(&tag.name, &tag.digest).await {
                        Ok(Some(row)) => {
                            let arc = Arc::new(row);
                            let mut cache = self.cache.lock().await;
                            cache.insert(tag.name.clone(), Arc::clone(&arc));
                            Some(arc)
                        }
                        Ok(None) => {
                            // No row, or the persisted row's digest
                            // doesn't match the live digest. Either way
                            // the caller's warm-up path is responsible
                            // for re-detection.
                            None
                        }
                        Err(e) => {
                            // Genuine SQLite error (connection, etc).
                            // `read_row` already converts decode errors
                            // (corrupt JSON, unknown capability_source)
                            // into Ok(None) with a `warn!`, so we only
                            // see *transport* errors here. Log and treat
                            // as cache miss; do not propagate.
                            warn!(
                                model = %tag.name,
                                error = %e,
                                "list_with_capabilities: read_row failed, surfacing capabilities=None",
                            );
                            None
                        }
                    }
                }
            };

            // Build the OllamaModel. The deprecated `capability` field
            // is populated via `legacy_capability_from` for backward
            // compatibility during migration step 1; readers that have
            // moved to step 2 ignore it and read `capabilities` instead.
            // `TextOnly` is the safe default when we have no capability
            // info — it matches the legacy `detect_capability_from_name`
            // fallback for unrecognised model names.
            let legacy = resolved
                .as_deref()
                .map(legacy_capability_from)
                .unwrap_or(ModelCapability::TextOnly);

            #[allow(deprecated)]
            out.push(OllamaModel {
                name: tag.name,
                size: tag.size,
                digest: tag.digest,
                modified_at: tag.modified_at,
                capabilities: resolved.as_deref().cloned(),
                capability: legacy,
            });
        }

        debug!(
            count = out.len(),
            cached = out.iter().filter(|m| m.capabilities.is_some()).count(),
            "list_with_capabilities: returned",
        );
        Ok(out)
    }

    /// Build a `Vec<OllamaModel>` from the in-memory cache only, used as
    /// the fallback on `/api/tags` failure (Requirement 13.1).
    ///
    /// Live tag fields (`size`, `modified_at`) cannot be filled from
    /// the cache alone, so they take placeholder values: `size = 0` and
    /// `modified_at = ""`. Frontend code that reads these fields should
    /// already tolerate missing data (the model selector renders fine
    /// without size info), and the cached `ModelCapabilities.digest`
    /// is preserved on each entry so subsequent invalidation logic
    /// remains correct once Ollama is reachable again.
    async fn snapshot_cache_as_models(&self) -> Vec<OllamaModel> {
        let cache = self.cache.lock().await;
        cache
            .values()
            .map(|arc| {
                let caps = (**arc).clone();
                #[allow(deprecated)]
                OllamaModel {
                    name: caps.model_name.clone(),
                    size: 0,
                    digest: caps.digest.clone(),
                    modified_at: String::new(),
                    capability: legacy_capability_from(&caps),
                    capabilities: Some(caps),
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// `refresh` (task 2.8)
//
// Force re-detection of a model's capabilities, ignoring any cached row.
// Used by the `refresh_model_capabilities` Tauri command when the user
// explicitly requests a re-scan (e.g. after updating Ollama).
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// Force re-detection of capabilities for `model_name`, ignoring any
    /// cached or persisted row.
    ///
    /// Algorithm:
    ///   1. Resolve the live digest from `/api/tags`.
    ///   2. Run the three-layer detection chain (`detect_capabilities`).
    ///   3. `INSERT OR REPLACE` the row into SQLite.
    ///   4. Replace the in-memory cache entry.
    ///   5. Return the new `Arc<ModelCapabilities>`.
    ///
    /// Unlike `get_capabilities`, this method never short-circuits on a
    /// cache hit — it always issues at least one `/api/show` call. Use
    /// it sparingly (user-initiated refresh only).
    #[instrument(skip(self), fields(model = %model_name))]
    pub async fn refresh(&self, model_name: &str) -> Result<Arc<ModelCapabilities>> {
        // 1. Resolve the live digest.
        let digest = self.live_digest_for(model_name).await?;

        // 2. Three-layer detection (always runs, ignores cache).
        let caps = self.detect_capabilities(model_name, &digest).await?;
        let arc = Arc::new(caps);

        // 3. Persist (INSERT OR REPLACE). On failure, log and continue
        //    (cache-only fallback).
        if let Err(e) = self.persist(&arc).await {
            warn!(
                model = %model_name,
                digest = %digest,
                error = %e,
                "refresh: persist failed — cache-only mode for this row",
            );
        }

        // 4. Replace the in-memory cache entry.
        {
            let mut cache = self.cache.lock().await;
            cache.insert(model_name.to_string(), Arc::clone(&arc));
        }

        debug!(model = %model_name, digest = %digest, "refresh: complete");
        Ok(arc)
    }
}

// ---------------------------------------------------------------------------
// `warm_up` background task (task 2.9)
//
// Spawns a background task that pre-populates the cache for a list of
// model names. Each model acquires a permit from `warm_up_sem` (capacity
// 4) before calling `get_capabilities`, bounding concurrent `/api/show`
// calls. Errors are logged at `warn` and do not propagate.
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// Pre-populate the cache for the given model names in the background.
    ///
    /// Returns synchronously after spawning a `tokio::spawn` task. The
    /// spawned task processes each model name concurrently (up to
    /// `WARM_UP_CONCURRENCY` = 4 at a time via the semaphore). Errors
    /// on individual models are logged at `warn` and do not propagate.
    ///
    /// An empty list returns immediately without spawning anything.
    pub fn warm_up(&self, model_names: Vec<String>) {
        if model_names.is_empty() {
            return;
        }

        let me = self.clone_for_task();

        tokio::spawn(async move {
            let mut handles = Vec::with_capacity(model_names.len());

            for name in model_names {
                let registry = me.clone_for_task();
                let sem = Arc::clone(&me.warm_up_sem);

                handles.push(tokio::spawn(async move {
                    // Acquire a permit — bounds concurrent /api/show calls.
                    let _permit = sem.acquire().await;
                    if let Err(e) = registry.get_capabilities(&name).await {
                        warn!(
                            model_name = %name,
                            category = "warm_up",
                            error = %e,
                            "warm_up: failed to pre-populate capabilities",
                        );
                    }
                }));
            }

            // Await all spawned tasks (best-effort — panics are logged
            // by tokio's default panic hook).
            for handle in handles {
                let _ = handle.await;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// `get_settings` / `set_settings` (task 2.10)
//
// Foundation for per-model settings overrides. The Tauri command stubs can
// return `NotImplemented` per design; this is the backend persistence layer.
// ---------------------------------------------------------------------------

impl ModelRegistry {
    /// Read per-model settings. Returns the persisted row when present,
    /// or a `ModelSettings::default()` with `model_name` set when absent.
    #[instrument(skip(self), fields(model = %model_name))]
    pub async fn get_settings(&self, model_name: &str) -> Result<ModelSettings> {
        type Row = (
            String,         // model_name
            Option<f64>,    // temperature (SQLite REAL)
            Option<i64>,    // num_ctx
            Option<f64>,    // top_p
            Option<i64>,    // top_k
            Option<String>, // system_prompt
            Option<String>, // default_keep_alive
            i64,            // updated_at
        );

        let row: Option<Row> = sqlx::query_as(
            "SELECT
                model_name,
                temperature,
                num_ctx,
                top_p,
                top_k,
                system_prompt,
                default_keep_alive,
                updated_at
             FROM model_settings
             WHERE model_name = ?
             LIMIT 1;",
        )
        .bind(model_name)
        .fetch_optional(&self.db)
        .await
        .context("Failed to SELECT model_settings row")?;

        match row {
            Some((name, temp, num_ctx, top_p, top_k, system_prompt, keep_alive, updated_at)) => {
                Ok(ModelSettings {
                    model_name: name,
                    temperature: temp.map(|v| v as f32),
                    num_ctx: num_ctx.map(|v| v as u32),
                    top_p: top_p.map(|v| v as f32),
                    top_k: top_k.map(|v| v as u32),
                    system_prompt,
                    default_keep_alive: keep_alive,
                    updated_at,
                })
            }
            None => {
                Ok(ModelSettings {
                    model_name: model_name.to_string(),
                    ..Default::default()
                })
            }
        }
    }

    /// Persist per-model settings via `INSERT OR REPLACE`.
    #[instrument(skip(self, settings), fields(model = %settings.model_name))]
    pub async fn set_settings(&self, settings: &ModelSettings) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO model_settings (
                model_name,
                temperature,
                num_ctx,
                top_p,
                top_k,
                system_prompt,
                default_keep_alive,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?);",
        )
        .bind(&settings.model_name)
        .bind(settings.temperature.map(|v| v as f64))
        .bind(settings.num_ctx.map(|v| v as i64))
        .bind(settings.top_p.map(|v| v as f64))
        .bind(settings.top_k.map(|v| v as i64))
        .bind(settings.system_prompt.as_deref())
        .bind(settings.default_keep_alive.as_deref())
        .bind(settings.updated_at)
        .execute(&self.db)
        .await
        .context("Failed to upsert model_settings row")?;

        debug!(model = %settings.model_name, "set_settings: persisted");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests for the pure-function detection helpers.
//
// `detect_capabilities` itself depends on a live `OllamaClient` and is
// covered by the integration test in task 5.9; the three layer parsers
// below are pure data and worth exercising directly.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_show_capabilities_recognises_all_five() {
        let input = vec![
            "completion".to_string(),
            "vision".to_string(),
            "thinking".to_string(),
            "tools".to_string(),
            "embedding".to_string(),
        ];
        assert_eq!(
            ModelRegistry::parse_api_show_capabilities(&input),
            (true, true, true, true, true)
        );
    }

    #[test]
    fn parse_api_show_capabilities_empty_array_no_flags() {
        assert_eq!(
            ModelRegistry::parse_api_show_capabilities(&[]),
            (false, false, false, false, false)
        );
    }

    #[test]
    fn parse_api_show_capabilities_unknown_strings_ignored() {
        // Per Requirement 3.1.a, unrecognised strings do not move any
        // flag. The recognised "vision" still fires.
        let input = vec![
            "audio".to_string(),
            "vision".to_string(),
            "code-interpreter".to_string(),
        ];
        assert_eq!(
            ModelRegistry::parse_api_show_capabilities(&input),
            (false, true, false, false, false)
        );
    }

    #[test]
    fn parse_api_show_capabilities_is_case_sensitive() {
        // "Vision" (capital V) is *not* the recognised "vision". Per
        // Requirement 3.1 the match is case-sensitive.
        let input = vec!["Vision".to_string(), "COMPLETION".to_string()];
        assert_eq!(
            ModelRegistry::parse_api_show_capabilities(&input),
            (false, false, false, false, false)
        );
    }

    #[test]
    fn parse_api_show_capabilities_duplicates_dont_change_result() {
        let input = vec![
            "vision".to_string(),
            "vision".to_string(),
            "vision".to_string(),
        ];
        assert_eq!(
            ModelRegistry::parse_api_show_capabilities(&input),
            (false, true, false, false, false)
        );
    }

    #[test]
    fn parse_template_markers_detects_canonical_spacing() {
        let tmpl = "Hello {{ .Prompt }} {{ .Images }} world";
        assert_eq!(ModelRegistry::parse_template_markers(tmpl), (true, false));
    }

    #[test]
    fn parse_template_markers_detects_no_space_form() {
        // Ollama has shipped both `{{ .Images }}` and `{{.Images}}` over
        // time; both must be detected.
        let tmpl = "{{.Images}} and {{.Think}}";
        assert_eq!(ModelRegistry::parse_template_markers(tmpl), (true, true));
    }

    #[test]
    fn parse_template_markers_no_markers() {
        let tmpl = "Plain template with no markers";
        assert_eq!(ModelRegistry::parse_template_markers(tmpl), (false, false));
    }

    #[test]
    fn parse_template_markers_only_thinking() {
        // The task explicitly specifies the literal `{{ .Think }}` /
        // `{{.Think}}` markers — Go-template control forms like
        // `{{ if .Think }}` are *not* matched by this layer.
        let tmpl = "system prompt {{ .Think }} suffix";
        assert_eq!(ModelRegistry::parse_template_markers(tmpl), (false, true));
    }

    #[test]
    fn name_heuristic_embedding_wins_over_vision() {
        // Hypothetical name colliding with both: embedding takes priority
        // because it is the more restrictive classification.
        let (vision, thinking, embedding, _tools) =
            ModelRegistry::name_heuristic("nomic-vision-embed");
        assert!(embedding);
        assert!(!vision);
        assert!(!thinking);
    }

    #[test]
    fn name_heuristic_vision_for_known_substrings() {
        for name in ["llava", "moondream", "minicpm-v", "qwen2-vl", "bakllava"] {
            let (vision, _, _, _) = ModelRegistry::name_heuristic(name);
            assert!(vision, "expected vision=true for {}", name);
        }
    }

    #[test]
    fn name_heuristic_thinking_for_known_substrings() {
        for name in ["deepseek-r1", "qwen3", "qwq", "gemma4", "gemma-4"] {
            let (_, thinking, _, _) = ModelRegistry::name_heuristic(name);
            assert!(thinking, "expected thinking=true for {}", name);
        }
    }

    #[test]
    fn name_heuristic_gemma3_is_not_thinking() {
        // The original gemma3 vision bug — gemma3 is *not* thinking, and
        // its name doesn't match any vision substring either. Layer 3
        // produces all-false; the registry then needs layer 1 / layer 2
        // to surface vision support.
        let (vision, thinking, embedding, tools) = ModelRegistry::name_heuristic("gemma3");
        assert!(!vision);
        assert!(!thinking);
        assert!(!embedding);
        assert!(!tools);
    }

    #[test]
    fn name_heuristic_case_insensitive() {
        // The legacy `detect_capability_from_name` lowercases its input;
        // the port must do the same so an upper-case name still classifies.
        let (vision, _, _, _) = ModelRegistry::name_heuristic("LLAVA");
        assert!(vision);
    }

    #[test]
    fn name_heuristic_text_only_default() {
        // Plain chat models match no substring — every flag stays false.
        for name in ["llama3", "mistral", "phi-2"] {
            let (vision, thinking, embedding, tools) = ModelRegistry::name_heuristic(name);
            assert!(
                !vision && !thinking && !embedding && !tools,
                "expected all-false for {} but got ({},{},{},{})",
                name,
                vision,
                thinking,
                embedding,
                tools,
            );
        }
    }

    #[test]
    fn name_heuristic_tools_always_false() {
        // Layer 3 has no tools detection; tools is always false.
        for name in ["deepseek-r1", "llava", "mxbai-embed-large", "gemma3"] {
            let (_, _, _, tools) = ModelRegistry::name_heuristic(name);
            assert!(!tools, "expected tools=false for {}", name);
        }
    }

    // ----------------------------------------------------------------------
    // SQLite I/O tests for `persist`, `read_row`, `evict` (task 2.4).
    //
    // These tests open a real SQLite database against a unique temp-file
    // path per test (in-memory pools cannot share state across pool
    // connections in sqlx 0.7), run the standard `db::init_pool` so the
    // `model_capabilities` schema is migrated in, then exercise the
    // registry's persistence layer directly. The `OllamaClient` instance
    // is constructed but never invoked — these tests cover only the
    // SQLite path.
    //
    // Tempfile choice: `std::env::temp_dir()` plus a UUID gives us a
    // path without pulling in the `tempfile` crate as a new dev
    // dependency. Each test cleans up its own file via a tiny RAII
    // guard so a panic mid-test still removes the database.
    // ----------------------------------------------------------------------

    use std::path::PathBuf;
    use uuid::Uuid;

    use crate::db::init_pool;
    use crate::ollama_client::OllamaClient;

    /// Drops the temporary database file when the guard goes out of scope,
    /// even when a test panics. SQLite's WAL/SHM files share the prefix so
    /// we wipe the whole stem.
    struct TempDbGuard {
        path: PathBuf,
    }

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            // Best-effort cleanup — don't panic in Drop if the file
            // disappeared for some reason.
            let _ = std::fs::remove_file(&self.path);
            let mut wal = self.path.clone();
            wal.set_extension("db-wal");
            let _ = std::fs::remove_file(&wal);
            let mut shm = self.path.clone();
            shm.set_extension("db-shm");
            let _ = std::fs::remove_file(&shm);
        }
    }

    /// Build a fresh `ModelRegistry` against a unique temp-file SQLite
    /// pool with all migrations applied. The returned `TempDbGuard` must
    /// stay alive for the duration of the test so the file is cleaned
    /// up at end-of-scope.
    async fn fresh_registry() -> (ModelRegistry, TempDbGuard) {
        let mut path = std::env::temp_dir();
        path.push(format!("heimdall-test-{}.db", Uuid::new_v4()));
        let guard = TempDbGuard { path: path.clone() };

        let pool = init_pool(&path)
            .await
            .expect("init_pool succeeds for a fresh temp file");

        // The OllamaClient is required by the constructor but is not
        // touched by any of the SQLite tests below.
        let ollama = OllamaClient::new("http://localhost:11434");
        let registry = ModelRegistry::new(pool, ollama);
        (registry, guard)
    }

    fn sample_caps(model_name: &str, digest: &str) -> ModelCapabilities {
        ModelCapabilities {
            model_name: model_name.to_string(),
            digest: digest.to_string(),
            completion: true,
            vision: true,
            thinking: false,
            tools: false,
            embedding: false,
            capability_source: CapabilitySource::ApiShow,
            raw_capabilities: vec!["completion".to_string(), "vision".to_string()],
            family: Some("gemma".to_string()),
            parameter_size: Some("3B".to_string()),
            quantization_level: Some("Q4_K_M".to_string()),
            detected_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn persist_and_read_row_round_trip() {
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("gemma3", "sha256:aaa");

        registry.persist(&caps).await.expect("persist succeeds");

        let read_back = registry
            .read_row("gemma3", "sha256:aaa")
            .await
            .expect("read_row succeeds")
            .expect("row exists");

        // Bit-for-bit equality: every field must round-trip.
        assert_eq!(read_back, caps);
    }

    #[tokio::test]
    async fn persist_overwrites_existing_row_on_same_model_name() {
        let (registry, _guard) = fresh_registry().await;

        let original = sample_caps("gemma3", "sha256:old");
        registry.persist(&original).await.expect("persist 1 succeeds");

        // Replace with a fresh row at a new digest — INSERT OR REPLACE
        // must overwrite by primary key.
        let updated = ModelCapabilities {
            digest: "sha256:new".to_string(),
            vision: false,
            thinking: true,
            capability_source: CapabilitySource::Template,
            raw_capabilities: vec![],
            ..sample_caps("gemma3", "sha256:new")
        };
        registry.persist(&updated).await.expect("persist 2 succeeds");

        // The old digest no longer points at a row — read_row enforces
        // digest match.
        let stale = registry
            .read_row("gemma3", "sha256:old")
            .await
            .expect("read_row succeeds");
        assert!(stale.is_none(), "old digest must no longer match");

        let fresh = registry
            .read_row("gemma3", "sha256:new")
            .await
            .expect("read_row succeeds")
            .expect("row exists for new digest");
        assert_eq!(fresh.digest, "sha256:new");
        assert!(!fresh.vision);
        assert!(fresh.thinking);
        assert_eq!(fresh.capability_source, CapabilitySource::Template);
    }

    #[tokio::test]
    async fn read_row_returns_none_when_no_row() {
        let (registry, _guard) = fresh_registry().await;

        let out = registry
            .read_row("never-persisted", "sha256:anything")
            .await
            .expect("read_row succeeds");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn read_row_returns_none_on_digest_mismatch() {
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("llava", "sha256:cached");
        registry.persist(&caps).await.expect("persist succeeds");

        // Live digest differs from cached digest — caller's signal to
        // re-detect.
        let out = registry
            .read_row("llava", "sha256:different")
            .await
            .expect("read_row succeeds");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn read_row_handles_non_apishow_source() {
        // Heuristic / template source rows have an empty
        // raw_capabilities array; verify they round-trip too.
        let (registry, _guard) = fresh_registry().await;
        let caps = ModelCapabilities {
            capability_source: CapabilitySource::Heuristic,
            raw_capabilities: vec![],
            ..sample_caps("phi-2", "sha256:bbb")
        };
        registry.persist(&caps).await.expect("persist succeeds");

        let read_back = registry
            .read_row("phi-2", "sha256:bbb")
            .await
            .expect("read_row succeeds")
            .expect("row exists");
        assert_eq!(read_back.capability_source, CapabilitySource::Heuristic);
        assert!(read_back.raw_capabilities.is_empty());
    }

    #[tokio::test]
    async fn read_row_recovers_when_raw_capabilities_json_is_corrupted() {
        // Inject a malformed JSON value directly via SQL, then verify
        // read_row treats the row as absent and logs at warn rather
        // than returning an error.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("broken", "sha256:ccc");
        registry.persist(&caps).await.expect("persist succeeds");

        sqlx::query(
            "UPDATE model_capabilities
             SET raw_capabilities = ?
             WHERE model_name = ?;",
        )
        .bind("not valid json {{{")
        .bind("broken")
        .execute(&registry.db)
        .await
        .expect("UPDATE succeeds");

        let out = registry
            .read_row("broken", "sha256:ccc")
            .await
            .expect("read_row should not return Err on bad JSON");
        assert!(out.is_none(), "corrupted row must be treated as absent");
    }

    #[tokio::test]
    async fn read_row_recovers_when_capability_source_is_unknown() {
        // Same idea as the JSON case: an unknown capability_source
        // value yields None, not Err.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("unknown-source", "sha256:ddd");
        registry.persist(&caps).await.expect("persist succeeds");

        sqlx::query(
            "UPDATE model_capabilities
             SET capability_source = ?
             WHERE model_name = ?;",
        )
        .bind("future_source_value")
        .bind("unknown-source")
        .execute(&registry.db)
        .await
        .expect("UPDATE succeeds");

        let out = registry
            .read_row("unknown-source", "sha256:ddd")
            .await
            .expect("read_row should not return Err on unknown source");
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn evict_removes_cache_and_db_row() {
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("evict-me", "sha256:eee");
        registry.persist(&caps).await.expect("persist succeeds");

        // Pre-populate the cache so evict has a digest to scope its
        // DELETE by.
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("evict-me".to_string(), Arc::new(caps.clone()));
        }

        registry.evict("evict-me").await.expect("evict returns Ok");

        // Cache no longer has the entry.
        {
            let cache = registry.cache.lock().await;
            assert!(cache.get("evict-me").is_none());
        }

        // SQLite row is gone too.
        let after = registry
            .read_row("evict-me", "sha256:eee")
            .await
            .expect("read_row succeeds");
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn evict_is_noop_when_cache_empty() {
        // No cache entry means we have no digest to scope by, so the
        // DB delete is skipped. The DB row (if any) stays put — a
        // future eviction with a known digest is the only legitimate
        // way to remove it.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("orphan", "sha256:fff");
        registry.persist(&caps).await.expect("persist succeeds");

        registry.evict("orphan").await.expect("evict returns Ok");

        // Row still present — we only evict what we know about.
        let still = registry
            .read_row("orphan", "sha256:fff")
            .await
            .expect("read_row succeeds");
        assert!(still.is_some());
    }

    #[tokio::test]
    async fn evict_with_stale_digest_does_not_clobber_fresh_row() {
        // Simulate the digest-invalidation race: the cache holds the
        // old entry; a concurrent task has already INSERT OR REPLACE-d
        // a fresh row at the new digest. evict() must DELETE only the
        // old digest's row (which now doesn't match the fresh digest)
        // and leave the fresh row intact.
        let (registry, _guard) = fresh_registry().await;

        // Persist the *new* row first (the "concurrent" path winning).
        let new_caps = sample_caps("racy", "sha256:new");
        registry
            .persist(&new_caps)
            .await
            .expect("persist new succeeds");

        // The cache still holds the old entry — that's exactly the
        // pre-condition `evict` is designed for.
        let old_caps = sample_caps("racy", "sha256:old");
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("racy".to_string(), Arc::new(old_caps.clone()));
        }

        registry.evict("racy").await.expect("evict returns Ok");

        // The fresh row at the new digest must still be present.
        let still = registry
            .read_row("racy", "sha256:new")
            .await
            .expect("read_row succeeds")
            .expect("fresh row not clobbered");
        assert_eq!(still.digest, "sha256:new");
    }

    // ----------------------------------------------------------------------
    // Tests for `hydrate()` (task 2.5).
    //
    // `hydrate()` is exercised end-to-end against a real SQLite database:
    // we `persist` rows, optionally corrupt a column via direct SQL to
    // simulate the row-level decode failures from Requirement 1.4, then
    // call `hydrate()` and inspect the in-memory cache.
    //
    // The function is contractually `Ok(())`-only, so every assertion
    // here is on the cache contents, never on a returned error variant.
    // ----------------------------------------------------------------------

    #[tokio::test]
    async fn hydrate_loads_persisted_rows_into_cache() {
        // Per Requirement 1.3: hydrate reads every row, restores every
        // flag plus capability_source plus raw_capabilities bitwise,
        // and a subsequent `get_capabilities`-equivalent cache lookup
        // returns the row without HTTP or SELECT.
        let (registry, _guard) = fresh_registry().await;

        let caps_a = sample_caps("gemma3", "sha256:aaa");
        let caps_b = ModelCapabilities {
            capability_source: CapabilitySource::Heuristic,
            raw_capabilities: vec![],
            vision: false,
            thinking: true,
            ..sample_caps("deepseek-r1", "sha256:bbb")
        };
        let caps_c = ModelCapabilities {
            capability_source: CapabilitySource::Template,
            raw_capabilities: vec![],
            vision: true,
            thinking: false,
            ..sample_caps("llava", "sha256:ccc")
        };
        registry.persist(&caps_a).await.expect("persist a");
        registry.persist(&caps_b).await.expect("persist b");
        registry.persist(&caps_c).await.expect("persist c");

        // Pre-condition: cache is empty before hydrate.
        {
            let cache = registry.cache.lock().await;
            assert!(cache.is_empty(), "cache must start empty");
        }

        registry.hydrate().await.expect("hydrate returns Ok");

        let cache = registry.cache.lock().await;
        assert_eq!(cache.len(), 3, "all three rows must be loaded");

        // Bit-for-bit equality on every restored row.
        let restored_a = cache.get("gemma3").expect("gemma3 in cache");
        assert_eq!(**restored_a, caps_a);

        let restored_b = cache.get("deepseek-r1").expect("deepseek-r1 in cache");
        assert_eq!(**restored_b, caps_b);

        let restored_c = cache.get("llava").expect("llava in cache");
        assert_eq!(**restored_c, caps_c);
    }

    #[tokio::test]
    async fn hydrate_on_empty_table_yields_empty_cache_and_returns_ok() {
        // Cold install: no persisted rows yet. hydrate must succeed and
        // leave the cache empty so the first get_capabilities call
        // falls through to detection per Requirement 1.1.
        let (registry, _guard) = fresh_registry().await;

        registry
            .hydrate()
            .await
            .expect("hydrate returns Ok on empty table");

        let cache = registry.cache.lock().await;
        assert!(cache.is_empty(), "no rows persisted, cache stays empty");
    }

    #[tokio::test]
    async fn hydrate_skips_row_with_corrupted_raw_capabilities_json() {
        // Requirement 1.4: a row-level deserialisation error must be
        // logged at error level, the affected row stays absent from
        // the cache, but the rest of the rows still load and hydrate
        // returns Ok.
        let (registry, _guard) = fresh_registry().await;

        let good = sample_caps("good-model", "sha256:111");
        let bad = sample_caps("bad-model", "sha256:222");
        registry.persist(&good).await.expect("persist good");
        registry.persist(&bad).await.expect("persist bad");

        // Corrupt the bad row's raw_capabilities JSON via direct SQL.
        sqlx::query(
            "UPDATE model_capabilities
             SET raw_capabilities = ?
             WHERE model_name = ?;",
        )
        .bind("not-valid-json {{{")
        .bind("bad-model")
        .execute(&registry.db)
        .await
        .expect("UPDATE succeeds");

        registry
            .hydrate()
            .await
            .expect("hydrate must still return Ok");

        let cache = registry.cache.lock().await;
        assert!(
            cache.contains_key("good-model"),
            "well-formed row must be loaded",
        );
        assert!(
            !cache.contains_key("bad-model"),
            "corrupted row must be skipped",
        );
    }

    #[tokio::test]
    async fn hydrate_skips_row_with_unknown_capability_source() {
        // The same Requirement 1.4 path for the other malformed-row
        // case `read_row` already covers: an unknown enum value.
        let (registry, _guard) = fresh_registry().await;

        let good = sample_caps("good", "sha256:aaa");
        let bad = sample_caps("future-source", "sha256:bbb");
        registry.persist(&good).await.expect("persist good");
        registry.persist(&bad).await.expect("persist bad");

        sqlx::query(
            "UPDATE model_capabilities
             SET capability_source = ?
             WHERE model_name = ?;",
        )
        .bind("audio_native")
        .bind("future-source")
        .execute(&registry.db)
        .await
        .expect("UPDATE succeeds");

        registry.hydrate().await.expect("hydrate returns Ok");

        let cache = registry.cache.lock().await;
        assert!(cache.contains_key("good"));
        assert!(!cache.contains_key("future-source"));
    }

    #[tokio::test]
    async fn hydrate_handles_null_raw_capabilities_for_non_apishow_rows() {
        // Heuristic / template rows persist with an empty Vec which
        // serialises to `"[]"`. Set the column to NULL via direct SQL
        // to mirror the read_row contract (NULL → empty Vec) and
        // confirm hydrate doesn't choke on it.
        let (registry, _guard) = fresh_registry().await;

        let caps = ModelCapabilities {
            capability_source: CapabilitySource::Heuristic,
            raw_capabilities: vec![],
            ..sample_caps("phi-2", "sha256:zzz")
        };
        registry.persist(&caps).await.expect("persist succeeds");

        sqlx::query(
            "UPDATE model_capabilities
             SET raw_capabilities = NULL
             WHERE model_name = ?;",
        )
        .bind("phi-2")
        .execute(&registry.db)
        .await
        .expect("UPDATE succeeds");

        registry.hydrate().await.expect("hydrate returns Ok");

        let cache = registry.cache.lock().await;
        let restored = cache.get("phi-2").expect("phi-2 in cache");
        assert!(restored.raw_capabilities.is_empty());
        assert_eq!(restored.capability_source, CapabilitySource::Heuristic);
    }

    #[tokio::test]
    async fn hydrate_is_idempotent() {
        // Per the design doc — "Idempotent" — calling hydrate twice
        // must leave the cache observationally identical to the
        // single-call case.
        let (registry, _guard) = fresh_registry().await;

        let caps = sample_caps("idem", "sha256:idem");
        registry.persist(&caps).await.expect("persist");

        registry.hydrate().await.expect("hydrate 1");
        let snapshot_1: ModelCapabilities = {
            let cache = registry.cache.lock().await;
            (**cache.get("idem").expect("loaded once")).clone()
        };

        registry.hydrate().await.expect("hydrate 2");
        let snapshot_2: ModelCapabilities = {
            let cache = registry.cache.lock().await;
            (**cache
                .get("idem")
                .expect("still loaded after second call"))
            .clone()
        };

        assert_eq!(snapshot_1, snapshot_2);
    }

    #[tokio::test]
    async fn hydrate_replaces_existing_cache_entries() {
        // If an entry was already in the cache before hydrate runs
        // (e.g. detected during a previous boot's lifetime, or
        // injected by a test), hydrate must overwrite it with the
        // on-disk value rather than leave the stale entry in place.
        let (registry, _guard) = fresh_registry().await;

        let on_disk = sample_caps("override-me", "sha256:disk");
        registry.persist(&on_disk).await.expect("persist");

        // Inject a different value into the cache directly.
        let stale = ModelCapabilities {
            digest: "sha256:stale".to_string(),
            vision: false,
            ..sample_caps("override-me", "sha256:stale")
        };
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("override-me".to_string(), Arc::new(stale.clone()));
        }

        registry.hydrate().await.expect("hydrate Ok");

        let cache = registry.cache.lock().await;
        let now_in_cache = cache.get("override-me").expect("entry present");
        assert_eq!(now_in_cache.digest, "sha256:disk");
        assert!(now_in_cache.vision);
    }

    // ----------------------------------------------------------------------
    // Tests for `get_capabilities()` and `detect_and_persist()` (task 2.6).
    //
    // Two execution paths matter for unit tests:
    //
    //   1. **Warm-cache path.** Pre-populate `cache` with an `Arc`,
    //      call `get_capabilities`, and verify the call returned an
    //      `Arc::ptr_eq`-equal handle without producing any side
    //      effects. Crucially, no `OllamaClient` mock is needed —
    //      the cache hit short-circuits before any HTTP / SQL work
    //      runs. This directly verifies Requirement 1.2.
    //
    //   2. **Concurrent dedup.** Pre-populate the cache so the inner
    //      future never actually runs (the first poll returns from
    //      the cache hit branch), then spawn N concurrent
    //      `get_capabilities` calls and assert all returned `Arc`s
    //      are pairwise `Arc::ptr_eq`. The same property holds for
    //      cold-path dedup but the cold path requires `OllamaClient`
    //      mocking, which lives in P5 (task 5.6) — the warm-path
    //      version below is sufficient to lock the basic property
    //      that the API never duplicates Arcs for cache-hit
    //      callers.
    //
    // The cold-path tests (cache miss → /api/tags → /api/show) are
    // covered by the integration tests in P1, P5, and 5.9 with a
    // proper mock `OllamaClient`. Those tests use `proptest_strategies`
    // and a hand-rolled mock HTTP server because `OllamaClient` itself
    // is concrete (not trait-based) in this codebase.
    //
    // `detect_and_persist` is covered directly: feed it a digest, let
    // it call the underlying detection chain (which fails when no
    // Ollama is running — that's fine, we only assert the persist /
    // warn-and-continue contract here against a mock `/api/show`
    // response... or against a deliberately failing chain, depending
    // on what the host can guarantee).
    // ----------------------------------------------------------------------

    #[tokio::test]
    async fn get_capabilities_cache_hit_returns_same_arc() {
        // Requirement 1.2: warm-cache path — cache hit returns the
        // existing Arc with zero HTTP, zero SQL. Assert the returned
        // Arc is `Arc::ptr_eq` with what was inserted, proving the
        // cache returned the *same* allocation rather than rebuilding.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("gemma3", "sha256:warm");
        let cached_arc = Arc::new(caps);

        // Inject directly into the cache, bypassing any detection.
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("gemma3".to_string(), Arc::clone(&cached_arc));
        }

        let returned = registry
            .get_capabilities("gemma3")
            .await
            .expect("warm cache must succeed");

        assert!(
            Arc::ptr_eq(&cached_arc, &returned),
            "cache hit must return the same Arc allocation",
        );
        // Nothing was inserted into in_flight on the warm path.
        let in_flight = registry.in_flight.lock().await;
        assert!(
            in_flight.is_empty(),
            "warm-cache path must not touch the in_flight map",
        );
    }

    #[tokio::test]
    async fn get_capabilities_cache_hit_no_db_row_required() {
        // The warm-cache path must not consult SQLite. We prove this
        // indirectly: pre-populate the in-memory cache, assert the
        // SQLite table is empty, then call `get_capabilities`. If the
        // call returns successfully and the SQLite table is *still*
        // empty, the function did not issue a SELECT followed by a
        // hidden upsert as a side effect.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("gemma3", "sha256:warm");
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("gemma3".to_string(), Arc::new(caps.clone()));
        }

        // Sanity: row count is zero before the call.
        let pre_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM model_capabilities;")
                .fetch_one(&registry.db)
                .await
                .expect("count succeeds");
        assert_eq!(pre_count.0, 0);

        let _arc = registry
            .get_capabilities("gemma3")
            .await
            .expect("warm cache must succeed");

        // Row count is *still* zero — the warm path issued no INSERT
        // and no SELECT (a SELECT alone wouldn't change the count, but
        // any code path that wrote a row would; the cache is the only
        // source of truth on the warm path).
        let post_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM model_capabilities;")
                .fetch_one(&registry.db)
                .await
                .expect("count succeeds");
        assert_eq!(
            post_count.0, 0,
            "warm-cache path must not write to model_capabilities",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn get_capabilities_concurrent_warm_callers_share_one_arc() {
        // N concurrent callers on the warm-cache path must all return
        // `Arc::ptr_eq`-equal handles to the same allocation. This is
        // the warm-path version of Requirement 5.1's pairwise
        // `Arc::ptr_eq` guarantee — the cold-path version requires a
        // mock OllamaClient and lives in property test P5.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("gemma3", "sha256:warm");
        let cached_arc = Arc::new(caps);
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("gemma3".to_string(), Arc::clone(&cached_arc));
        }

        // Wrap the registry in an Arc so each spawned task can hold
        // a clone — `clone_for_task` would also work but is inside
        // the registry's contract and we want to test the public
        // `get_capabilities` surface as users will see it.
        let registry = Arc::new(registry);

        const N: usize = 8;
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let r = Arc::clone(&registry);
            handles.push(tokio::spawn(async move {
                r.get_capabilities("gemma3").await.expect("warm hit")
            }));
        }

        let results: Vec<Arc<ModelCapabilities>> =
            futures_util::future::join_all(handles)
                .await
                .into_iter()
                .map(|j| j.expect("task did not panic"))
                .collect();

        // Pairwise Arc::ptr_eq across all N returns and against the
        // pre-inserted cached_arc — the cache returned exactly one
        // allocation, cloned N+1 ways.
        for r in &results {
            assert!(
                Arc::ptr_eq(&cached_arc, r),
                "every concurrent caller must observe the same Arc allocation",
            );
        }
    }

    #[tokio::test]
    async fn get_capabilities_cache_hit_does_not_install_in_flight_entry() {
        // Belt-and-braces: on the warm path, the in_flight map must
        // stay empty before, during, and after the call. Other tasks
        // racing concurrently must not see a transient entry leak.
        let (registry, _guard) = fresh_registry().await;
        let caps = sample_caps("gemma3", "sha256:warm");
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("gemma3".to_string(), Arc::new(caps));
        }

        // in_flight is empty before.
        assert!(registry.in_flight.lock().await.is_empty());

        let _ = registry
            .get_capabilities("gemma3")
            .await
            .expect("warm hit");

        // in_flight is empty after.
        assert!(registry.in_flight.lock().await.is_empty());
    }

    #[tokio::test]
    async fn detect_and_persist_writes_row_and_returns_arc_with_real_capabilities() {
        // detect_and_persist runs the detection chain end-to-end. With
        // no live Ollama, layer 1 fails (HTTP error) and we fall
        // through to layer 2 (no template available) then layer 3
        // (name heuristic). For "llava" the heuristic produces
        // vision = true.
        //
        // The persist call must succeed against the temp SQLite, and
        // the resulting Arc must reflect the heuristic flags.
        let (registry, _guard) = fresh_registry().await;

        let arc = registry
            .detect_and_persist("llava", "sha256:llava-1")
            .await
            .expect("detect_and_persist returns Ok even when Ollama is down");

        // Layer 3 produced vision = true for llava.
        assert!(arc.vision, "llava must be flagged vision via heuristic");
        assert_eq!(arc.capability_source, CapabilitySource::Heuristic);
        assert_eq!(arc.digest, "sha256:llava-1");

        // The row was persisted to SQLite.
        let row = registry
            .read_row("llava", "sha256:llava-1")
            .await
            .expect("read_row succeeds")
            .expect("row was persisted");
        assert_eq!(row.digest, "sha256:llava-1");
        assert!(row.vision);
        assert_eq!(row.capability_source, CapabilitySource::Heuristic);
    }

    #[tokio::test]
    async fn detect_and_persist_returns_arc_when_persist_would_fail() {
        // Persist failure must degrade to cache-only mode (warn + Ok).
        // We provoke a persist failure by closing the SQLite pool so
        // any subsequent query returns an error. The detection chain
        // still runs (it doesn't touch SQLite) and the resulting Arc
        // is returned.
        let (registry, _guard) = fresh_registry().await;
        registry.db.close().await;

        let arc = registry
            .detect_and_persist("llava", "sha256:closed")
            .await
            .expect(
                "detect_and_persist must succeed in cache-only mode \
                 when persist fails",
            );

        // The Arc still carries the detection result.
        assert!(arc.vision, "llava heuristic still produces vision=true");
        assert_eq!(arc.capability_source, CapabilitySource::Heuristic);
        assert_eq!(arc.digest, "sha256:closed");
    }

    // ----------------------------------------------------------------------
    // list_with_capabilities tests (task 2.7).
    //
    // The happy paths (cache hit + match, cache hit + mismatch eviction,
    // cache miss + DB hit, warm-up dispatch) require injecting fake
    // `/api/tags` responses, which the production `OllamaClient` cannot do
    // out of the box — the integration test in task 5.9 covers those
    // paths against a hand-rolled mock.
    //
    // Unit-level coverage focuses on what we *can* exercise without
    // mocking: the cache-snapshot fallback when `/api/tags` is
    // unreachable (Requirement 13.1) and the `snapshot_cache_as_models`
    // helper that builds it. Both run entirely against the in-memory
    // cache and an `OllamaClient` pointed at a closed loopback port,
    // so the connect attempt fails fast (refused / no route) rather
    // than waiting on the 5-second timeout.
    // ----------------------------------------------------------------------

    /// Build a registry whose `OllamaClient` points at a guaranteed-
    /// unreachable URL. `127.0.0.1:1` is the privileged port nothing
    /// listens on; on Linux the kernel rejects the connect immediately
    /// with ECONNREFUSED, so `list_tags_raw` returns an error in
    /// milliseconds rather than hitting the 5-second timeout.
    async fn unreachable_registry() -> (ModelRegistry, TempDbGuard) {
        let mut path = std::env::temp_dir();
        path.push(format!("heimdall-test-{}.db", Uuid::new_v4()));
        let guard = TempDbGuard { path: path.clone() };

        let pool = init_pool(&path)
            .await
            .expect("init_pool succeeds for a fresh temp file");

        // Port 1 is privileged and nothing meaningful listens on it.
        let ollama = OllamaClient::new("http://127.0.0.1:1");
        let registry = ModelRegistry::new(pool, ollama);
        (registry, guard)
    }

    #[tokio::test]
    async fn snapshot_cache_as_models_empty_cache_returns_empty_vec() {
        // Boundary case: with nothing in the cache the snapshot returns
        // an empty Vec, not an error. This is the path
        // `list_with_capabilities` takes when Ollama is unreachable
        // *and* the cache hasn't been hydrated.
        let (registry, _guard) = fresh_registry().await;

        let snap = registry.snapshot_cache_as_models().await;
        assert!(snap.is_empty(), "empty cache yields empty snapshot");
    }

    #[tokio::test]
    async fn snapshot_cache_as_models_preserves_capabilities_and_legacy_field() {
        // The snapshot must populate `capabilities` from the cached Arc,
        // populate the deprecated `capability` field via
        // `legacy_capability_from`, and preserve the cached digest.
        let (registry, _guard) = fresh_registry().await;
        let caps = ModelCapabilities {
            // A vision-capable cached entry — `legacy_capability_from`
            // must collapse this to `Vision`.
            vision: true,
            ..sample_caps("gemma3", "sha256:v1")
        };
        registry
            .cache
            .lock()
            .await
            .insert("gemma3".to_string(), Arc::new(caps.clone()));

        let snap = registry.snapshot_cache_as_models().await;
        assert_eq!(snap.len(), 1);
        let m = &snap[0];

        assert_eq!(m.name, "gemma3");
        assert_eq!(m.digest, "sha256:v1");
        // Live tag fields are unknown on the fallback path — must use
        // the documented placeholders.
        assert_eq!(m.size, 0);
        assert_eq!(m.modified_at, "");

        // Capabilities round-trip cleanly.
        let resolved = m.capabilities.as_ref().expect("capabilities populated");
        assert!(resolved.vision);
        assert_eq!(resolved.digest, "sha256:v1");

        // The deprecated `capability` field must reflect the legacy
        // priority (Vision > Thinking > TextOnly; embedding=false).
        #[allow(deprecated)]
        let legacy = &m.capability;
        assert_eq!(*legacy, ModelCapability::Vision);
    }

    #[tokio::test]
    async fn list_with_capabilities_falls_back_to_cache_on_api_tags_failure() {
        // Requirement 13.1: when /api/tags is unreachable the registry
        // MUST return the in-memory cache snapshot, log once at warn,
        // and not raise.
        let (registry, _guard) = unreachable_registry().await;

        // Pre-populate the cache with two entries so the snapshot
        // contains something observable.
        let caps_a = sample_caps("llava", "sha256:aaa");
        let caps_b = ModelCapabilities {
            thinking: true,
            vision: false,
            ..sample_caps("deepseek-r1", "sha256:bbb")
        };
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("llava".to_string(), Arc::new(caps_a.clone()));
            cache.insert("deepseek-r1".to_string(), Arc::new(caps_b.clone()));
        }

        let out = registry
            .list_with_capabilities()
            .await
            .expect("list_with_capabilities is non-erroring on /api/tags failure");

        // Both cached entries must be present (HashMap iteration order
        // is not stable, so compare by name set).
        assert_eq!(out.len(), 2, "cache snapshot must include every entry");
        let names: std::collections::HashSet<_> =
            out.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains("llava"));
        assert!(names.contains("deepseek-r1"));

        // Each entry carries its cached capabilities and the live-tag
        // placeholder fields — never raises.
        for m in &out {
            assert!(m.capabilities.is_some());
            assert_eq!(m.size, 0);
            assert_eq!(m.modified_at, "");
        }
    }

    #[tokio::test]
    async fn list_with_capabilities_returns_empty_when_cache_empty_and_ollama_unreachable() {
        // Edge case of Requirement 13.1: an empty cache + unreachable
        // Ollama yields an empty Vec, not an error.
        let (registry, _guard) = unreachable_registry().await;

        let out = registry
            .list_with_capabilities()
            .await
            .expect("list_with_capabilities never errors on /api/tags failure");

        assert!(out.is_empty(), "empty cache + offline Ollama => empty list");
    }

    #[tokio::test]
    async fn list_with_capabilities_fallback_does_not_evict_or_mutate_cache() {
        // The fallback path is read-only against the cache: it must not
        // evict any entry or change the cached digest, even though no
        // live tag has been observed.
        let (registry, _guard) = unreachable_registry().await;
        let caps = sample_caps("gemma3", "sha256:cached");
        {
            let mut cache = registry.cache.lock().await;
            cache.insert("gemma3".to_string(), Arc::new(caps.clone()));
        }

        let _ = registry
            .list_with_capabilities()
            .await
            .expect("returns Ok on /api/tags failure");

        // Cache still has gemma3 with the same digest.
        let cache = registry.cache.lock().await;
        let still = cache.get("gemma3").expect("entry still present");
        assert_eq!(still.digest, "sha256:cached");
    }
}
