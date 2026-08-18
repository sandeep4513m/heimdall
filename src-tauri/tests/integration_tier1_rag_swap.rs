//! integration_tier1_rag_swap.rs — Tier 1 embedding-swap decision path.
//!
//! **Validates: Requirements 10.1, 10.2, 10.3**
//!
//! NOTE: This is a *decision-path* integration test, not a live
//! end-to-end test. Wiring the full IngestionWorker + Governor +
//! Ollama stack through a unit harness is impractical (it needs a real
//! AppState, a spawned worker, and a live model), so we exercise the
//! exact decision the worker delegates to — `can_load_embedding` — across
//! the Tier-1 scenarios the swap orchestration branches on:
//!
//!   - chat loaded + embedding+chat over budget → `RequiresChatUnload`
//!     (the worker force-unloads chat, sets `chat_reload_pending`, emits
//!     `unloading_chat`), and
//!   - chat + embedding both fit                → `FitsAlongside`
//!     (the worker proceeds without unloading anything),
//!   - embedding alone over budget              → `InsufficientEvenAlone`
//!     (the worker fails the job, Req 10.8).
//!
//! These are the three branches that gate the embedding-swap event
//! sequence in `IngestionWorker`.

use heimdall_lib::governor::can_load_embedding;
use heimdall_lib::models::EmbeddingFitDecision;

// A representative Tier-1 (4 GB) scenario: ~2000 MB available, an 80%
// headroom → 1600 MB budget. Embedding model ~350 MB.
const AVAIL_MB: u64 = 2000;
const PCT: f32 = 0.80;
const EMBED_MB: u64 = 350;

fn budget() -> u64 {
    ((AVAIL_MB as f32) * PCT).floor() as u64
}

#[test]
fn tier1_chat_loaded_requires_unload_when_over_budget() {
    // A 1500 MB chat model + 350 MB embedding = 1850 > 1600 budget →
    // the chat model must be evicted first.
    let chat_mb = 1500;
    assert!(EMBED_MB + chat_mb > budget(), "scenario must exceed budget");
    let decision = can_load_embedding(EMBED_MB, chat_mb, AVAIL_MB, PCT);
    assert_eq!(decision, EmbeddingFitDecision::RequiresChatUnload);
}

#[test]
fn tier1_both_fit_when_under_budget() {
    // A small 800 MB chat model + 350 MB embedding = 1150 <= 1600 budget →
    // both fit, no unload needed.
    let chat_mb = 800;
    assert!(EMBED_MB + chat_mb <= budget(), "scenario must fit");
    let decision = can_load_embedding(EMBED_MB, chat_mb, AVAIL_MB, PCT);
    assert_eq!(decision, EmbeddingFitDecision::FitsAlongside);
}

#[test]
fn tier1_no_chat_loaded_fits_alongside() {
    // No chat model loaded (chat_size 0) — embedding alone fits.
    let decision = can_load_embedding(EMBED_MB, 0, AVAIL_MB, PCT);
    assert_eq!(decision, EmbeddingFitDecision::FitsAlongside);
}

#[test]
fn tier1_embedding_alone_too_big_fails() {
    // Pathological tiny box: only 200 MB available → 160 MB budget, which
    // a 350 MB embedding model alone exceeds → job must fail (Req 10.8).
    let tiny_avail = 200;
    let tiny_budget = ((tiny_avail as f32) * PCT).floor() as u64;
    assert!(EMBED_MB > tiny_budget, "embedding must exceed the tiny budget");
    let decision = can_load_embedding(EMBED_MB, 0, tiny_avail, PCT);
    assert_eq!(decision, EmbeddingFitDecision::InsufficientEvenAlone);
}

#[test]
fn tier1_decision_branches_are_mutually_exclusive() {
    // Sweep a small grid and assert exactly one branch fires per input —
    // the property the worker relies on to pick exactly one action.
    for avail in [200u64, 1000, 2000, 4000] {
        for chat in [0u64, 500, 1500, 3000] {
            for embed in [100u64, 350, 1800] {
                let d = can_load_embedding(embed, chat, avail, PCT);
                let b = ((avail as f32) * PCT).floor() as u64;
                let expected = if embed > b {
                    EmbeddingFitDecision::InsufficientEvenAlone
                } else if embed + chat <= b {
                    EmbeddingFitDecision::FitsAlongside
                } else {
                    EmbeddingFitDecision::RequiresChatUnload
                };
                assert_eq!(
                    d, expected,
                    "branch mismatch for avail={avail} chat={chat} embed={embed}"
                );
            }
        }
    }
}
