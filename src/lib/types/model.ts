/**
 * model.ts — TypeScript types for Heimdall's Model Intelligence Registry.
 *
 * Mirrors the Rust types in `src-tauri/src/models.rs` field-for-field so
 * that values returned by the `get_model_capabilities` /
 * `refresh_model_capabilities` Tauri commands deserialise without
 * adapter shims on the frontend.
 *
 * Wire form notes:
 * - `CapabilitySource` is serialised by serde with
 *   `#[serde(rename_all = "snake_case")]`, hence the lowercase string
 *   union below.
 * - Rust `Option<String>` serialises to `string | null` (not
 *   `undefined`), so the optional fields use `| null`.
 * - Rust `i64` timestamps cross the IPC boundary as JS `number`
 *   (Unix seconds, well within `Number.MAX_SAFE_INTEGER`).
 *
 * See `.kiro/specs/model-intelligence-registry/design.md` (TypeScript
 * Types section) and Requirements 12.2 for the contract this file
 * implements.
 */

/**
 * Which detection layer produced a `ModelCapabilities` row.
 *
 * Matches the Rust `CapabilitySource` enum with
 * `#[serde(rename_all = "snake_case")]`:
 * - `'api_show'`     — Ollama's `/api/show.capabilities` (ground truth).
 * - `'template'`     — Inferred from template markers (`{{ .Images }}`,
 *                      `{{ .Think }}`).
 * - `'heuristic'`    — Inferred from model-name substring matching;
 *                      last-resort fallback.
 * - `'user_override'` — User explicitly set capabilities in the Models tab.
 */
export type CapabilitySource =
  | 'api_show'
  | 'template'
  | 'heuristic'
  | 'user_override';

/**
 * Authoritative answer to "what can this model do?". Multi-capability:
 * a single model is often `completion + vision + tools` simultaneously,
 * so each flag is an independent boolean rather than a single tag.
 *
 * Mirrors `ModelCapabilities` in `src-tauri/src/models.rs` field-for-field.
 */
export interface ModelCapabilities {
  /** Model identifier as reported by Ollama (e.g. `gemma3`, `llama3.1:8b`). */
  model_name: string;
  /** SHA-256 digest of the model blob set; the cache invalidation key. */
  digest: string;
  /** Standard text completion. Almost always `true` for chat models. */
  completion: boolean;
  /** Accepts image input (multimodal vision models). */
  vision: boolean;
  /** Native thinking via `<think>` tags / Ollama's `thinking` field. */
  thinking: boolean;
  /** Function-calling / tool-use via Ollama's tools API. */
  tools: boolean;
  /** Embedding generation (RAG path); models that emit vectors not text. */
  embedding: boolean;
  /** Which detection layer produced this row. */
  capability_source: CapabilitySource;
  /**
   * Raw capability strings from `/api/show`, when
   * `capability_source === 'api_show'`. Empty array for other sources.
   * Preserves original order and any unrecognised strings (e.g. `"audio"`)
   * verbatim, per Requirement 3.1.b.
   */
  raw_capabilities: string[];
  /** Family name (`"gemma"`, `"llama"`, `"qwen"`) from `/api/show.details`. */
  family: string | null;
  /** Parameter size string (`"7B"`, `"70B"`) from `/api/show.details`. */
  parameter_size: string | null;
  /** Quantization level (`"Q4_K_M"`, `"Q8_0"`) from `/api/show.details`. */
  quantization_level: string | null;
  /** Unix timestamp (seconds) of first detection for this digest. */
  detected_at: number;
  /** Unix timestamp (seconds) of last update (refresh, override edit). */
  updated_at: number;
}

/**
 * A model returned by Ollama's `/api/tags` endpoint, enriched with cached
 * capabilities from the Model Intelligence Registry.
 *
 * Mirrors the Rust `OllamaModel` struct in `src-tauri/src/models.rs`
 * field-for-field.
 *
 * Wire form notes:
 * - Rust `Option<ModelCapabilities>` serialises to `ModelCapabilities | null`
 *   (not `undefined`). `null` means the registry has not yet detected this
 *   model — frontend should treat that as "loading".
 * - The legacy `capability` field is the snake_case-serialised
 *   `ModelCapability` enum (`'text_only' | 'vision' | 'embedding' |
 *   'audio' | 'multimodal' | 'thinking'`), kept loosely typed as `string`
 *   for one release per the migration plan.
 */
export interface OllamaModel {
  /** Model identifier as reported by Ollama (e.g. `gemma3`, `llama3.1:8b`). */
  name: string;
  /** Size of the model blob set in bytes. */
  size: number;
  /** SHA-256 digest of the model blob set; the cache invalidation key. */
  digest: string;
  /** RFC 3339 timestamp of when Ollama last modified this model entry. */
  modified_at: string;
  /**
   * Authoritative capabilities from the registry. `null` only when the
   * model has never been seen by the registry (warm-up not yet run).
   * Frontend should treat `null` as "loading".
   */
  capabilities: ModelCapabilities | null;
  /**
   * @deprecated Read `capabilities` instead. This single-enum field is
   * kept for one release as a backward-compatibility shim during
   * migration step 1. It will be removed in migration step 3 (next
   * release) — see `.kiro/specs/model-intelligence-registry/design.md`
   * (Migration Strategy section).
   */
  capability: string;
}
