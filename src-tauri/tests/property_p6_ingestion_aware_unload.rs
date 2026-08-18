//! property_p6_ingestion_aware_unload.rs — P6: Ingestion-aware unload invariant.
//!
//! **Property 6: Ingestion-aware unload invariant**
//!
//! **Validates: Requirements 7.7, 7.10, 9.5**
//!
//! While a RAG ingestion is in flight (`active_ingestions` non-empty),
//! the candidate selector MUST NEVER return the embedding model. For any
//! generated world with `active_ingestions_nonempty == true`, the result
//! of `select_unload_candidate` is either `None` or a model whose name is
//! NOT `embedding_model_name`.
//!
//! The generator places the embedding model in `loaded_models` for a
//! large fraction of cases (names are drawn from a shared pool that
//! includes `nomic-embed-text`), so the guard is genuinely exercised
//! rather than vacuously satisfied.

mod governor_strategies;

use governor_strategies::{arb_world_with_active_ingestion, GovernorWorld};
use heimdall_lib::governor::select_unload_candidate;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p6_ingestion_aware_unload() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_world_with_active_ingestion(), |w: GovernorWorld| {
            // Precondition baked into the generator.
            prop_assert!(w.active_ingestions_nonempty);

            let chosen = select_unload_candidate(
                &w.loaded,
                &w.streaming_values,
                w.active_ingestions_nonempty,
                &w.model_last_used,
                &w.embedding_model_name,
                &w.auto_unload_per_model,
                &w.excluded_for_event,
                w.polling_loop_start,
                w.now,
            );

            if let Some(m) = chosen {
                prop_assert_ne!(
                    &m.name,
                    &w.embedding_model_name,
                    "selected the embedding model while an ingestion is active — \
                     ingestion-aware guard violated"
                );
            }
            Ok(())
        })
        .expect("P6: ingestion-aware invariant holds for all generated cases");
}
