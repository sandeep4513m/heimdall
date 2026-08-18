//! property_p7_candidate_determinism.rs — P7: Candidate-selection determinism.
//!
//! **Property 7: Candidate-selection determinism**
//!
//! **Validates: Requirements 8.1, 8.2**
//!
//! `select_unload_candidate` is a pure function: two consecutive calls
//! with identical inputs return identical results. We match the result by
//! `name` and `size_total_mb` (the tie-break keys) — both calls must
//! return `None`, or both must return models that agree on those fields.
//!
//! This pins down the total ordering of the tie-break chain (largest idle
//! → largest size → lexicographically smallest name) so a future refactor
//! that accidentally introduced nondeterminism (e.g. iterating a HashMap)
//! would fail here.

mod governor_strategies;

use governor_strategies::{arb_world, GovernorWorld};
use heimdall_lib::governor::select_unload_candidate;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p7_candidate_determinism() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_world(), |w: GovernorWorld| {
            let first = select_unload_candidate(
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
            let second = select_unload_candidate(
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

            match (first, second) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    prop_assert_eq!(&a.name, &b.name, "name differs between calls");
                    prop_assert_eq!(
                        a.size_total_mb,
                        b.size_total_mb,
                        "size_total_mb differs between calls"
                    );
                }
                (a, b) => {
                    return Err(TestCaseError::fail(format!(
                        "determinism violated: one call returned {:?}, the other {:?}",
                        a.map(|m| &m.name),
                        b.map(|m| &m.name),
                    )));
                }
            }
            Ok(())
        })
        .expect("P7: candidate determinism holds for all generated cases");
}
