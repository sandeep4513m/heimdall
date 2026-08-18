//! property_p5_stream_aware_unload.rs — P5: Stream-aware unload invariant.
//!
//! **Property 5: Stream-aware unload invariant**
//!
//! **Validates: Requirements 7.6, 7.9, 9.6, 15.6**
//!
//! The auto-unload candidate selector MUST NEVER return a model that is
//! currently streaming. For any generated world, the result of
//! `select_unload_candidate` is either `None` or a model whose name does
//! not appear in any value of `active_stream_models`.
//!
//! Boundary cases the generator exercises: every loaded model streaming,
//! no model streaming, an empty streaming set, whitespace-only names.

mod governor_strategies;

use governor_strategies::{arb_world, GovernorWorld};
use heimdall_lib::governor::select_unload_candidate;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p5_stream_aware_unload() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_world(), |w: GovernorWorld| {
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
                prop_assert!(
                    !w.streaming_values.contains(&m.name),
                    "selected candidate {:?} is currently streaming — \
                     stream-aware guard violated",
                    m.name
                );
            }
            Ok(())
        })
        .expect("P5: stream-aware invariant holds for all generated cases");
}
