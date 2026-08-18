//! property_p3_per_model_accounting.rs — P3: Per-loaded-model accounting.
//!
//! **Property 3: Per-loaded-model accounting**
//!
//! **Validates: Requirements 5.5, 5.6, 5.7**
//!
//! For any list of loaded models — including duplicates, zero sizes, and
//! the empty list — the `/api/ps` mapping (`OllamaClient::parse_ps_json`,
//! the pure core of `list_running`) preserves:
//!   - count: one `RunningModel` per input entry, no dedup/aggregation,
//!   - names in order,
//!   - `size_total_mb` in order.
//!
//! We drive the property through the real JSON parse path: each generated
//! model is rendered into an `/api/ps` entry with `size` in bytes
//! (`size_total_mb * 1024 * 1024`), then `parse_ps_json` maps it back. The
//! generator draws names from a small shared pool (so duplicates are
//! common) and sizes including 0 (the zero-size passthrough, Req 5.6).

mod governor_strategies;

use governor_strategies::arb_running_models;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

#[test]
fn p3_per_model_accounting() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_running_models(), |models| {
            // Build an /api/ps JSON body from the generated models. `size`
            // is expressed in bytes so the bytes→MiB mapping reproduces
            // `size_total_mb` exactly. Names from the shared pool are all
            // <= 256 bytes, so truncation is a no-op here.
            let entries: Vec<serde_json::Value> = models
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "name": m.name,
                        "size": m.size_total_mb * 1024 * 1024,
                    })
                })
                .collect();
            let body = serde_json::json!({ "models": entries }).to_string();

            let parsed = heimdall_lib::ollama_client::parse_ps_json(&body)
                .map_err(|e| TestCaseError::fail(format!("parse failed: {e}")))?;

            // Count preserved (no dedup / no aggregation) — Req 5.5.
            prop_assert_eq!(
                parsed.len(),
                models.len(),
                "mapping must preserve entry count with no dedup/aggregation"
            );

            // Names and sizes preserved in order — Req 5.5, 5.6, 5.7.
            for (got, expected) in parsed.iter().zip(models.iter()) {
                prop_assert_eq!(
                    &got.name,
                    &expected.name,
                    "names must match in order"
                );
                prop_assert_eq!(
                    got.size_total_mb,
                    expected.size_total_mb,
                    "size_total_mb must match in order (zero passes through)"
                );
            }
            Ok(())
        })
        .expect("P3: per-model accounting holds for all generated cases");
}
