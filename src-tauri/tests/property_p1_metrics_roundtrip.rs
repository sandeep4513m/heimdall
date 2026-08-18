//! property_p1_metrics_roundtrip.rs — P1: GovernorMetrics serialise round-trip.
//!
//! **Property 1: GovernorMetrics serialise round-trip**
//!
//! **Validates: Requirements 1.9, 2.3, 5.4, 16.3**
//!
//! For any generated `GovernorMetrics` `m`, deserialising the JSON
//! produced by serialising `m` yields a value `PartialEq`-equal to `m`:
//!
//! ```text
//! serde_json::from_str(&serde_json::to_string(&m)?)? == m
//! ```
//!
//! `GovernorMetrics` is the payload emitted on every `governor://metrics`
//! tick (Req 1.9), so this guards the wire contract the frontend store
//! decodes. The generator (`arb_governor_metrics`) covers every field
//! including all enum variants, a 0..=32 per-core CPU vector, and a
//! 0..=8 loaded-model vector. f32 fields are constrained to finite,
//! non-NaN values so the `PartialEq` round-trip is well-defined.

mod governor_strategies;

use governor_strategies::arb_governor_metrics;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p1_metrics_roundtrip() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_governor_metrics(), |m| {
            let json = serde_json::to_string(&m)
                .map_err(|e| TestCaseError::fail(format!("serialise failed: {e}")))?;
            let back: heimdall_lib::models::GovernorMetrics = serde_json::from_str(&json)
                .map_err(|e| TestCaseError::fail(format!("deserialise failed: {e}")))?;
            prop_assert_eq!(
                back,
                m,
                "GovernorMetrics must survive a JSON serialise→deserialise round-trip"
            );
            Ok(())
        })
        .expect("P1: GovernorMetrics round-trip holds for all generated cases");
}
