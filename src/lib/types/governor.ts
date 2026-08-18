// src/lib/types/governor.ts
//
// Frontend-side mirrors of the Phase 6 Governor types defined in
// `src-tauri/src/models.rs`. Wire forms match the snake_case serde
// representations exactly.

import type { ModelCapabilities } from './model';

/// One of `Calm | Warn | Unload | Critical`. Severity ordering is the
/// natural ordering — `Calm < Warn < Unload < Critical`. See P4
/// (risk-state monotonicity).
export type RiskState = 'calm' | 'warn' | 'unload' | 'critical';

/// VRAM availability for the current tick. `absent` means no discrete
/// GPU was found at any `/sys/class/drm/card<N>/` path — the parent
/// component should unmount the VRAM card entirely (Req 4.5, 12.8).
/// `unavailable` means at least one GPU was identified but a read
/// failed; render the literal text "VRAM: unavailable" (Glossary).
export type VramStatus = 'ok' | 'unavailable' | 'absent';

/// Whether `/proc` was readable on this tick. `unreadable` means every
/// `*_mb` field is `0` and every `Option` is `None` (Bucket B).
export type ProcStatus = 'readable' | 'unreadable';

/// Hardware tier — matches the Rust `HardwareTier` enum.
export type HardwareTier = 'minimal' | 'standard' | 'full';

/// Phase identifier for `governor://embedding_swap` events.
export type EmbeddingSwapPhase =
  | 'unloading_chat'
  | 'unloading_embedding'
  | 'reloading_chat';

/// Hardware-aware classification of a model against the active tier.
export type ModelRecommendation =
  | 'fits_comfortably'
  | 'requires_management'
  | 'exceeds_tier';

/// One Ollama-loaded model, mapped from a single `/api/ps` entry.
/// `idle_seconds` is `null` until the model has streamed at least one
/// chat token in this session.
export interface RunningModel {
  name: string;
  size_vram_mb: number | null;
  size_total_mb: number;
  expires_at: number;
  idle_seconds: number | null;
}

/// The three governor thresholds actually used to derive this tick's
/// `risk_state`. When configured values fail validation the Governor
/// falls back to documented per-tier defaults and reflects them here
/// (Req 6.9).
export interface GovernorThresholds {
  warn_mb: number;
  unload_mb: number;
  critical_mb: number;
}

/// One polling-tick snapshot of every system resource the Governor
/// watches. Emitted on `governor://metrics` once per tick (Req 1.9).
export interface GovernorMetrics {
  total_ram_mb: number;
  available_ram_mb: number;
  swap_total_mb: number;
  swap_used_mb: number;
  cpu_aggregate_percent: number;
  cpu_per_core_percent: number[];
  ollama_online: boolean;
  ollama_rss_mb: number | null;
  heimdall_rss_mb: number;
  webview_rss_mb: number | null;
  vram_total_mb: number | null;
  vram_used_mb: number | null;
  vram_status: VramStatus;
  loaded_models: RunningModel[];
  risk_state: RiskState;
  thresholds: GovernorThresholds;
  detected_tier: HardwareTier;
  effective_tier: HardwareTier;
  proc_status: ProcStatus;
  cgroup_detected: boolean;
  timestamp_unix_ms: number;
}

/// Payload for `governor://critical` — emitted on the edge transition
/// into `Critical`.
export interface CriticalEvent {
  available_ram_mb: number;
  critical_threshold_mb: number;
  scheduled_unloads: string[];
}

/// Payload for `governor://no_candidates` — emitted only when
/// `risk_state ∈ {Unload, Critical}` and no eligible candidates remain.
export interface NoCandidatesEvent {
  available_ram_mb: number;
  risk_state: RiskState;
  loaded_count: number;
}

/// Payload for `governor://embedding_swap`.
export interface EmbeddingSwapEvent {
  phase: EmbeddingSwapPhase;
  chat_model?: string | null;
}

/// Row payload for the Phase 6 Models tab (Req 13.8, 14.2).
export interface ModelsTabRow {
  name: string;
  size: number;
  digest: string;
  modified_at: string;
  capabilities: ModelCapabilities | null;
  last_used_unix: number | null;
  currently_loaded: boolean;
  recommendation: ModelRecommendation;
}

/// Wire form of `model://pull-progress` — keyed by model name so
/// concurrent pulls render independently. Mirrors the Rust
/// `PullProgressEvent` struct in `src-tauri/src/models.rs`.
export interface PullProgressEvent {
  model: string;
  status: string;
  completed: number | null;
  total: number | null;
}

/// Curated catalog entry returned by the `models_catalog_list` Tauri
/// command. Mirrors the Rust `CatalogEntry` struct in
/// `src-tauri/src/catalog.rs`. The `capabilities` field is a list of
/// short tags (`chat`, `vision`, `embedding`, `thinking`, `tools`) the
/// model is expected to support — authoritative capability data still
/// comes from `ModelRegistry` once the model has actually been pulled.
export interface CatalogEntry {
  name: string;
  size_mb: number;
  capabilities: string[];
  min_tier: HardwareTier;
}

/// Predictive ingestion-pressure preview (Legendary feature, Task 28.1).
///
/// Returned by the gated `governor_preview_ingestion` Tauri command —
/// mirrors the Rust `IngestionFitPreview` struct in
/// `src-tauri/src/models.rs`. `status` is a traffic-light derived from the
/// Governor's `EmbeddingFitDecision`:
///   - `"green"`    ← FitsAlongside (chat + embedding both fit)
///   - `"amber"`    ← RequiresChatUnload (chat must be evicted first)
///   - `"red"`      ← InsufficientEvenAlone (embedding alone too big)
///   - `"disabled"` ← the feature flag is off; all MB fields are 0
///
/// `budget_mb` is `floor(available_mb * safe_headroom_pct)`.
export type IngestionFitStatus = 'green' | 'amber' | 'red' | 'disabled';

export interface IngestionFitPreview {
  status: IngestionFitStatus;
  embedding_mb: number;
  chat_mb: number;
  available_mb: number;
  budget_mb: number;
}
