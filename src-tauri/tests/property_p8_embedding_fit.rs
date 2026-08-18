//! property_p8_embedding_fit.rs — P8: Adaptive embedding fit decision invariants.
//!
//! **Property 8: Adaptive embedding fit decision invariants**
//!
//! **Validates: Requirements 10.1, 10.2, 10.8**
//!
//! `can_load_embedding` is a pure three-branch decision driven by the
//! safe budget `budget = floor(mem_available_mb * safe_headroom_pct)`:
//!
//!   - `embedding > budget`              → `InsufficientEvenAlone`  (Req 10.8)
//!   - `embedding + chat <= budget`      → `FitsAlongside`          (Req 10.1)
//!   - otherwise                         → `RequiresChatUnload`     (Req 10.1)
//!
//! The branches are mutually exclusive and total; this test re-derives the
//! expected branch from the same integer-truncated budget and asserts the
//! function agrees. Boundary cases (pct = 1.0, near-epsilon pct, and
//! `embedding == budget` exactly) are injected by the generator.

mod governor_strategies;

use governor_strategies::{arb_fit_inputs, FitInputs};
use heimdall_lib::governor::can_load_embedding;
use heimdall_lib::models::EmbeddingFitDecision;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p8_embedding_fit() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_fit_inputs(), |fi: FitInputs| {
            let FitInputs {
                mem_available_mb,
                embedding_size_mb,
                chat_size_mb,
                safe_headroom_pct,
            } = fi;

            let got = can_load_embedding(
                embedding_size_mb,
                chat_size_mb,
                mem_available_mb,
                safe_headroom_pct,
            );

            // Re-derive the expected branch using the same budget formula.
            let budget = ((mem_available_mb as f32) * safe_headroom_pct).floor() as u64;
            let expected = if embedding_size_mb > budget {
                EmbeddingFitDecision::InsufficientEvenAlone
            } else if embedding_size_mb.saturating_add(chat_size_mb) <= budget {
                EmbeddingFitDecision::FitsAlongside
            } else {
                EmbeddingFitDecision::RequiresChatUnload
            };

            prop_assert_eq!(
                got,
                expected,
                "embedding={} chat={} avail={} pct={} budget={}",
                embedding_size_mb,
                chat_size_mb,
                mem_available_mb,
                safe_headroom_pct,
                budget
            );

            // Boundary sanity: embedding exactly at budget is NOT
            // InsufficientEvenAlone (the comparison is strict `>`).
            if embedding_size_mb == budget {
                prop_assert_ne!(
                    got,
                    EmbeddingFitDecision::InsufficientEvenAlone,
                    "embedding == budget must not be InsufficientEvenAlone"
                );
            }
            Ok(())
        })
        .expect("P8: embedding-fit decision table holds for all generated cases");
}
