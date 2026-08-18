//! integration_4gb_acceptance.rs — 4 GB box acceptance (decision path).
//!
//! **Validates: Requirements 6.2–6.5, 7.6, 9.6 (acceptance scenario)**
//!
//! NOTE: This is a *decision-path* integration test, not a live
//! end-to-end test. The acceptance story — leave Heimdall running on a
//! 3900 MB box with a chat model loaded, ingest a folder, switch to chat
//! mid-ingestion, resume — cannot be reproduced in a unit harness without
//! a real machine and a live Ollama. Instead we assert the two
//! correctness guarantees that make that story safe on the decision
//! functions the Governor actually uses:
//!
//!   1. Across a sequence of `available_ram_mb` readings that never drop
//!      below the Tier-1 critical threshold, `derive_risk_state` never
//!      yields `Critical` (no spurious OOM panic, Req 6.2–6.5).
//!   2. Across the same sequence, with one model continuously streaming,
//!      `select_unload_candidate` never selects the streaming model
//!      (Req 7.6 / 9.6 — a chat reply is never truncated mid-token).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use heimdall_lib::governor::{derive_risk_state, select_unload_candidate};
use heimdall_lib::models::{RiskState, RunningModel};

// Tier 1 (Minimal) defaults from Req 6.6.
const WARN: u64 = 800;
const UNLOAD: u64 = 400;
const CRITICAL: u64 = 200;

fn rm(name: &str, size_total_mb: u64) -> RunningModel {
    RunningModel {
        name: name.to_string(),
        size_vram_mb: None,
        size_total_mb,
        expires_at: 0,
        idle_seconds: None,
    }
}

#[test]
fn never_critical_while_above_critical_threshold() {
    // A plausible sequence of MemAvailable readings on a 3900 MB box under
    // load — pressured, dipping into Warn/Unload, but always staying at or
    // above the Tier-1 critical floor (200 MB).
    let readings: [u64; 10] = [2000, 1500, 900, 700, 500, 450, 410, 300, 250, 201];
    for avail in readings {
        let state = derive_risk_state(avail, WARN, UNLOAD, CRITICAL);
        assert_ne!(
            state,
            RiskState::Critical,
            "available={avail} (>= critical {CRITICAL}) must never derive Critical"
        );
    }

    // Sanity: dropping below the critical floor *does* trip Critical, so
    // the assertion above is meaningful rather than vacuous.
    assert_eq!(
        derive_risk_state(CRITICAL - 1, WARN, UNLOAD, CRITICAL),
        RiskState::Critical
    );
}

#[test]
fn streaming_model_never_unloaded_across_sequence() {
    // Two models loaded; "gemma3" is continuously streaming a reply.
    let loaded = vec![rm("gemma3", 2000), rm("nomic-embed-text", 350)];
    let mut streaming = HashSet::new();
    streaming.insert("gemma3".to_string());

    let model_last_used: HashMap<String, Instant> = HashMap::new();
    let auto_unload_per_model: HashMap<String, bool> = HashMap::new();
    let excluded: HashSet<String> = HashSet::new();
    let start = Instant::now();

    // Simulate a sequence of ticks (idle time grows as `now` advances).
    for step in 0..8u64 {
        let now = start
            .checked_add(std::time::Duration::from_secs(step * 2))
            .unwrap_or(start);
        let chosen = select_unload_candidate(
            &loaded,
            &streaming,
            /* active_ingestions_nonempty = */ false,
            &model_last_used,
            "nomic-embed-text",
            &auto_unload_per_model,
            &excluded,
            start,
            now,
        );
        if let Some(m) = chosen {
            assert_ne!(
                m.name, "gemma3",
                "the streaming chat model must never be selected for unload"
            );
        }
    }
}

#[test]
fn ingestion_active_protects_embedding_model_across_sequence() {
    // During ingestion the embedding model must survive even under
    // pressure; the only unloadable model is the idle chat model.
    let loaded = vec![rm("gemma3", 2000), rm("nomic-embed-text", 350)];
    let streaming: HashSet<String> = HashSet::new();
    let model_last_used: HashMap<String, Instant> = HashMap::new();
    let auto_unload_per_model: HashMap<String, bool> = HashMap::new();
    let excluded: HashSet<String> = HashSet::new();
    let start = Instant::now();
    let now = start + std::time::Duration::from_secs(60);

    let chosen = select_unload_candidate(
        &loaded,
        &streaming,
        /* active_ingestions_nonempty = */ true,
        &model_last_used,
        "nomic-embed-text",
        &auto_unload_per_model,
        &excluded,
        start,
        now,
    );
    // gemma3 (the non-embedding model) is the only eligible candidate.
    assert_eq!(chosen.map(|m| m.name.as_str()), Some("gemma3"));
}
