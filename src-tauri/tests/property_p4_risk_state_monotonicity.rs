//! property_p4_risk_state_monotonicity.rs — P4: Risk-state monotonicity.
//!
//! **Property 4: Risk-state monotonicity**
//!
//! **Validates: Requirements 6.2, 6.3, 6.4, 6.5, 6.7**
//!
//! Under a fixed, valid threshold configuration (`warn > unload >
//! critical > 0`), more available RAM can only ever mean an equal-or-less
//! severe risk state. Concretely, for any `a <= b`:
//!
//! ```text
//! derive_risk_state(a, w, u, c) >= derive_risk_state(b, w, u, c)
//! ```
//!
//! under the severity ordering `Calm < Warn < Unload < Critical`. `a <= b`
//! means `b` has at least as much free RAM, so its state must be
//! no-more-severe — i.e. `state(a) >= state(b)`.

mod governor_strategies;

use governor_strategies::arb_thresholds;
use heimdall_lib::governor::derive_risk_state;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p4_risk_state_monotonicity() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    // A pair of available-RAM readings (x, y); we sort them into (a, b)
    // with a <= b so the ordering precondition holds by construction.
    let strat = (arb_thresholds(), any::<u64>(), any::<u64>());

    runner
        .run(&strat, |((warn, unload, critical), x, y)| {
            let (a, b) = if x <= y { (x, y) } else { (y, x) };

            let state_a = derive_risk_state(a, warn, unload, critical);
            let state_b = derive_risk_state(b, warn, unload, critical);

            prop_assert!(
                state_a >= state_b,
                "a={} (state {:?}) has <= RAM than b={} (state {:?}); \
                 state(a) must be no-less-severe than state(b)",
                a,
                state_a,
                b,
                state_b
            );
            Ok(())
        })
        .expect("P4: risk-state monotonicity holds for all generated cases");
}
