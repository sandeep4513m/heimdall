//! governor_strategies.rs — Shared `proptest` generators for the Phase 6
//! Governor property suite (P1–P8, governor-intelligence tasks 3.2, 4.3,
//! 7.2, 8.2, 11.2, 11.3, 11.4, 14.3).
//!
//! Cargo compiles every file at the top level of `tests/` as its own
//! integration-test crate, and ALSO re-compiles this file into any sibling
//! test that declares `mod governor_strategies;`. The blanket
//! `#![allow(dead_code)]` silences the unused-symbol warnings the latter
//! mode emits when a given test only uses a subset of the generators. This
//! mirrors the existing `proptest_strategies.rs` pattern used by the Phase
//! 3.5 registry tests.
//!
//! Every public item is `pub` so sibling tests reach them via
//! `mod governor_strategies;` followed by `use governor_strategies::*;`.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use heimdall_lib::models::{
    GovernorMetrics, GovernorThresholds, HardwareTier, ProcStatus, RiskState,
    RunningModel, VramStatus,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Primitive / string strategies
// ---------------------------------------------------------------------------

/// A model name in the alphabet Ollama actually uses on the wire plus a
/// couple of pathological shapes the guards must survive. Length 0..=40 so
/// the empty-name and whitespace-only boundary cases (P5) are exercised.
///
/// The `prop_oneof!` biases toward realistic names but injects an empty
/// string and a whitespace-only string so the candidate selector's name
/// comparisons are fuzzed against degenerate input.
pub fn arb_model_name() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => proptest::string::string_regex("[a-z0-9][a-z0-9\\-:.]{0,39}")
            .expect("model-name regex compiles"),
        1 => Just(String::new()),
        1 => Just("   ".to_string()),
    ]
}

/// A small pool of recurring names so generated worlds frequently have
/// the *same* name appear in `loaded_models`, the streaming set, and the
/// embedding slot — which is exactly the overlap the guards must handle.
pub fn arb_shared_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("gemma3".to_string()),
        Just("llama3:8b".to_string()),
        Just("nomic-embed-text".to_string()),
        Just("qwen3".to_string()),
        Just("phi4-mini".to_string()),
        arb_model_name(),
    ]
}

// ---------------------------------------------------------------------------
// RunningModel strategies (P1, P3, P5, P6, P7)
// ---------------------------------------------------------------------------

/// One `RunningModel`. `size_total_mb` spans 0 (the zero-size passthrough
/// case, Req 5.6) up to ~64 GB. `size_vram_mb` is `None` or `Some(small)`.
/// `expires_at` covers negative, zero, and positive epoch values.
/// `idle_seconds` is `None` or `Some(_)`.
pub fn arb_running_model() -> BoxedStrategy<RunningModel> {
    (
        arb_shared_name(),
        proptest::option::of(0u64..32_768),
        0u64..65_536,
        any::<i64>(),
        proptest::option::of(any::<u64>()),
    )
        .prop_map(
            |(name, size_vram_mb, size_total_mb, expires_at, idle_seconds)| {
                RunningModel {
                    name,
                    size_vram_mb,
                    size_total_mb,
                    expires_at,
                    idle_seconds,
                }
            },
        )
        .boxed()
}

/// A `Vec<RunningModel>` of length 0..=16, including the empty vector,
/// duplicates (because `arb_shared_name` draws from a small pool), and
/// zero-size entries. Used by P3 (per-model accounting) and as the loaded
/// set in `arb_world`.
pub fn arb_running_models() -> impl Strategy<Value = Vec<RunningModel>> {
    prop::collection::vec(arb_running_model(), 0..=16)
}

// ---------------------------------------------------------------------------
// Threshold strategies (P4)
// ---------------------------------------------------------------------------

/// A valid threshold triple satisfying `warn > unload > critical > 0`
/// (Req 6.8). Built from three positive gaps so the strict ordering holds
/// by construction without rejection sampling.
pub fn arb_thresholds() -> impl Strategy<Value = (u64, u64, u64)> {
    // critical in [1, 4000]; unload = critical + gap1; warn = unload + gap2.
    (1u64..=4_000, 1u64..=4_000, 1u64..=4_000).prop_map(|(critical, gap1, gap2)| {
        let unload = critical + gap1;
        let warn = unload + gap2;
        (warn, unload, critical)
    })
}

/// A `GovernorThresholds` value with the same validity invariant, for the
/// P1 round-trip generator.
pub fn arb_governor_thresholds() -> impl Strategy<Value = GovernorThresholds> {
    arb_thresholds().prop_map(|(warn_mb, unload_mb, critical_mb)| GovernorThresholds {
        warn_mb,
        unload_mb,
        critical_mb,
    })
}

// ---------------------------------------------------------------------------
// Enum strategies (P1)
// ---------------------------------------------------------------------------

pub fn arb_risk_state() -> impl Strategy<Value = RiskState> {
    prop_oneof![
        Just(RiskState::Calm),
        Just(RiskState::Warn),
        Just(RiskState::Unload),
        Just(RiskState::Critical),
    ]
}

pub fn arb_vram_status() -> impl Strategy<Value = VramStatus> {
    prop_oneof![
        Just(VramStatus::Ok),
        Just(VramStatus::Unavailable),
        Just(VramStatus::Absent),
    ]
}

pub fn arb_proc_status() -> impl Strategy<Value = ProcStatus> {
    prop_oneof![Just(ProcStatus::Readable), Just(ProcStatus::Unreadable)]
}

pub fn arb_hardware_tier() -> impl Strategy<Value = HardwareTier> {
    prop_oneof![
        Just(HardwareTier::Minimal),
        Just(HardwareTier::Standard),
        Just(HardwareTier::Full),
    ]
}

// ---------------------------------------------------------------------------
// GovernorMetrics strategy (P1 round-trip)
// ---------------------------------------------------------------------------

/// A finite, non-NaN `f32` in `[0.0, 100.0]`. P1 round-trips
/// `GovernorMetrics` through JSON and asserts `PartialEq`; NaN would break
/// equality (`NaN != NaN`) and `f32::INFINITY` does not serialise to valid
/// JSON, so we constrain to a finite percentage range that mirrors the
/// real CPU readings.
fn arb_cpu_percent() -> impl Strategy<Value = f32> {
    (0u32..=10_000).prop_map(|n| n as f32 / 100.0)
}

/// A fully-populated `GovernorMetrics` covering every field (Task 3.2 /
/// P1). `cpu_per_core_percent` length 0..=32; `loaded_models` length
/// 0..=8; all enums via `prop_oneof!`; all f32 fields finite to guarantee
/// the JSON round-trip preserves `PartialEq`.
///
/// `proptest`'s `Strategy` is only implemented for tuples up to 12
/// elements, and `GovernorMetrics` has 21 fields — so we split the inputs
/// into three nested tuples and recombine in `prop_map`. The grouping is
/// purely syntactic.
pub fn arb_governor_metrics() -> BoxedStrategy<GovernorMetrics> {
    // Group 1: RAM + CPU.
    let ram_cpu = (
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        arb_cpu_percent(),
        prop::collection::vec(arb_cpu_percent(), 0..=32),
    );

    // Group 2: process + GPU.
    let proc_gpu = (
        any::<bool>(),
        proptest::option::of(any::<u64>()),
        any::<u64>(),
        proptest::option::of(any::<u64>()),
        proptest::option::of(any::<u64>()),
        proptest::option::of(any::<u64>()),
        arb_vram_status(),
    );

    // Group 3: loaded models + risk + tiers + diagnostics.
    let rest = (
        prop::collection::vec(arb_running_model(), 0..=8),
        arb_risk_state(),
        arb_governor_thresholds(),
        arb_hardware_tier(),
        arb_hardware_tier(),
        arb_proc_status(),
        any::<bool>(),
        any::<i64>(),
    );

    (ram_cpu, proc_gpu, rest)
        .prop_map(
            |(
                (
                    total_ram_mb,
                    available_ram_mb,
                    swap_total_mb,
                    swap_used_mb,
                    cpu_aggregate_percent,
                    cpu_per_core_percent,
                ),
                (
                    ollama_online,
                    ollama_rss_mb,
                    heimdall_rss_mb,
                    webview_rss_mb,
                    vram_total_mb,
                    vram_used_mb,
                    vram_status,
                ),
                (
                    loaded_models,
                    risk_state,
                    thresholds,
                    detected_tier,
                    effective_tier,
                    proc_status,
                    cgroup_detected,
                    timestamp_unix_ms,
                ),
            )| GovernorMetrics {
                total_ram_mb,
                available_ram_mb,
                swap_total_mb,
                swap_used_mb,
                cpu_aggregate_percent,
                cpu_per_core_percent,
                ollama_online,
                ollama_rss_mb,
                heimdall_rss_mb,
                webview_rss_mb,
                vram_total_mb,
                vram_used_mb,
                vram_status,
                loaded_models,
                risk_state,
                thresholds,
                detected_tier,
                effective_tier,
                proc_status,
                cgroup_detected,
                timestamp_unix_ms,
            },
        )
        .boxed()
}

// ---------------------------------------------------------------------------
// Candidate-selector "world" (P5, P6, P7)
// ---------------------------------------------------------------------------

/// Every input `select_unload_candidate` reads, bundled so P5/P6/P7 can
/// generate a complete, internally-consistent world in one shot.
///
/// Field meanings mirror the `select_unload_candidate` signature:
/// - `loaded` — the loaded-model set (0..=16, may contain duplicates).
/// - `streaming_values` — the *values* of `active_stream_models`
///   (model names currently streaming). Names sometimes overlap `loaded`.
/// - `active_ingestions_nonempty` — the ingestion guard flag.
/// - `model_last_used` — last-token timestamps for some subset of names.
/// - `embedding_model_name` — drawn from the shared pool so it sometimes
///   coincides with a loaded model.
/// - `auto_unload_per_model` — per-model toggle map (false = excluded).
/// - `excluded_for_event` — the 3-strikes exclusion set.
/// - `polling_loop_start` / `now` — two `Instant`s with `start <= now`.
#[derive(Debug)]
pub struct GovernorWorld {
    pub loaded: Vec<RunningModel>,
    pub streaming_values: HashSet<String>,
    pub active_ingestions_nonempty: bool,
    pub model_last_used: HashMap<String, Instant>,
    pub embedding_model_name: String,
    pub auto_unload_per_model: HashMap<String, bool>,
    pub excluded_for_event: HashSet<String>,
    pub polling_loop_start: Instant,
    pub now: Instant,
}

/// Build a `GovernorWorld`. The two `Instant`s are derived from a single
/// `now = Instant::now()` captured at build time minus generated offsets,
/// guaranteeing `polling_loop_start <= now` (idle-time math must never
/// underflow). `model_last_used` timestamps are likewise `<= now`.
pub fn arb_world() -> BoxedStrategy<GovernorWorld> {
    (
        arb_running_models(),
        prop::collection::hash_set(arb_shared_name(), 0..=6),
        any::<bool>(),
        arb_shared_name(),
        prop::collection::vec((arb_shared_name(), any::<bool>()), 0..=8),
        prop::collection::hash_set(arb_shared_name(), 0..=6),
        // Offsets in seconds: start_offset is how long ago the loop began;
        // per-model idle offset is how long ago each known model streamed.
        1u64..=86_400,
        prop::collection::vec((arb_shared_name(), 0u64..=86_400), 0..=8),
    )
        .prop_map(
            |(
                loaded,
                streaming_values,
                active_ingestions_nonempty,
                embedding_model_name,
                toggles,
                excluded_for_event,
                start_offset_secs,
                last_used_offsets,
            )| {
                let now = Instant::now();
                let polling_loop_start = now
                    .checked_sub(std::time::Duration::from_secs(start_offset_secs))
                    .unwrap_or(now);

                let mut model_last_used: HashMap<String, Instant> = HashMap::new();
                for (name, offset) in last_used_offsets {
                    // Clamp the per-model offset to the loop-start window so
                    // every timestamp sits in [polling_loop_start, now].
                    let off = offset.min(start_offset_secs);
                    let t = now
                        .checked_sub(std::time::Duration::from_secs(off))
                        .unwrap_or(now);
                    model_last_used.insert(name, t);
                }

                let auto_unload_per_model: HashMap<String, bool> =
                    toggles.into_iter().collect();

                GovernorWorld {
                    loaded,
                    streaming_values,
                    active_ingestions_nonempty,
                    model_last_used,
                    embedding_model_name,
                    auto_unload_per_model,
                    excluded_for_event,
                    polling_loop_start,
                    now,
                }
            },
        )
        .boxed()
}

/// A world guaranteed to have `active_ingestions_nonempty == true` — used
/// by P6 (ingestion-aware invariant). The embedding model name sometimes
/// appears in `loaded` and sometimes does not, which `arb_shared_name`
/// already arranges.
pub fn arb_world_with_active_ingestion() -> BoxedStrategy<GovernorWorld> {
    arb_world()
        .prop_map(|mut w| {
            w.active_ingestions_nonempty = true;
            w
        })
        .boxed()
}

// ---------------------------------------------------------------------------
// Embedding-fit inputs (P8)
// ---------------------------------------------------------------------------

/// Inputs to `can_load_embedding`. Sizes span `0..u32::MAX as u64`;
/// `safe_headroom_pct` is constrained to `(0.0, 1.0]` and includes the
/// boundary cases pct=1.0 and a near-epsilon pct.
#[derive(Debug)]
pub struct FitInputs {
    pub mem_available_mb: u64,
    pub embedding_size_mb: u64,
    pub chat_size_mb: u64,
    pub safe_headroom_pct: f32,
}

fn arb_headroom_pct() -> impl Strategy<Value = f32> {
    prop_oneof![
        // Generic pct in (0, 1].
        8 => (1u32..=10_000).prop_map(|n| n as f32 / 10_000.0),
        // Boundary: exactly 1.0.
        1 => Just(1.0_f32),
        // Boundary: near-epsilon (smallest bucket).
        1 => Just(0.0001_f32),
    ]
}

pub fn arb_fit_inputs() -> BoxedStrategy<FitInputs> {
    let max = u32::MAX as u64;
    (0u64..=max, 0u64..=max, 0u64..=max, arb_headroom_pct())
        .prop_map(
            |(mem_available_mb, embedding_size_mb, chat_size_mb, safe_headroom_pct)| {
                FitInputs {
                    mem_available_mb,
                    embedding_size_mb,
                    chat_size_mb,
                    safe_headroom_pct,
                }
            },
        )
        .boxed()
}
