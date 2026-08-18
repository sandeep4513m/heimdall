//! property_p2_memory_invariants.rs — P2: Memory invariants.
//!
//! **Property 2: Memory invariants**
//!
//! **Validates: Requirements 2.5, 2.6, 2.7, 2.8, 3.6, 3.7, 3.8, 3.9**
//!
//! The Governor never reports impossible memory numbers. Because the
//! live readings depend on the host machine, we test the pure helpers
//! the polling loop is wired through (`normalize_meminfo`, `clamp_rss`,
//! `clamp_self_rss`, `compute_cpu_percent`) so the invariants hold for
//! every conceivable `/proc` reading, including pathological ones:
//!
//! - `available_ram_mb <= total_ram_mb`            (Req 2.6)
//! - `swap_used_mb <= swap_total_mb`               (Req 2.7)
//! - `cpu_aggregate_percent ∈ [0.0, 100.0]`, never NaN (Req 2.5, 2.8)
//! - every `cpu_per_core_percent` value ∈ [0.0, 100.0], never NaN (Req 2.8)
//! - `ollama_rss_mb` clamps to `None` when it exceeds total (Req 3.8)
//! - `heimdall_rss_mb <= total_ram_mb` after clamp    (Req 3.9)

mod governor_strategies;

use heimdall_lib::governor::{
    clamp_rss, clamp_self_rss, compute_cpu_percent, normalize_meminfo, CpuJiffies,
};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};

/// Strategy producing two `/proc/stat`-style samples sharing the same set
/// of cpu names so `compute_cpu_percent` matches them. `s2` counters are
/// always >= `s1` counters (monotonic jiffy counters) but we do NOT force
/// `idle_delta <= total_delta`; the function must clamp regardless.
fn arb_stat_pair() -> impl Strategy<Value = (Vec<CpuJiffies>, Vec<CpuJiffies>)> {
    // Up to 8 cores plus the aggregate "cpu" line.
    prop::collection::vec(
        (
            0u64..1_000_000, // s1.total
            0u64..1_000_000, // s1.idle
            0u64..1_000_000, // total delta
            0u64..1_000_000, // idle delta
        ),
        1..=9,
    )
    .prop_map(|rows| {
        let mut s1 = Vec::with_capacity(rows.len());
        let mut s2 = Vec::with_capacity(rows.len());
        for (i, (t1, i1, dt, di)) in rows.into_iter().enumerate() {
            let name = if i == 0 {
                "cpu".to_string()
            } else {
                format!("cpu{}", i - 1)
            };
            // idle never exceeds total within a single sample.
            let idle1 = i1.min(t1);
            s1.push(CpuJiffies {
                name: name.clone(),
                total: t1,
                idle: idle1,
            });
            s2.push(CpuJiffies {
                name,
                total: t1.saturating_add(dt),
                idle: idle1.saturating_add(di),
            });
        }
        (s1, s2)
    })
}

#[test]
fn p2_meminfo_invariants() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(
            &(any::<u64>(), any::<u64>(), any::<u64>(), any::<u64>()),
            |(total, available, swap_total, swap_free)| {
                let (t, a, st, su) =
                    normalize_meminfo(total, available, swap_total, swap_free);
                prop_assert!(a <= t, "available {} must be <= total {}", a, t);
                prop_assert!(su <= st, "swap_used {} must be <= swap_total {}", su, st);
                Ok(())
            },
        )
        .expect("P2: meminfo invariants hold");
}

#[test]
fn p2_rss_clamp_invariants() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(
            &(proptest::option::of(any::<u64>()), any::<u64>(), any::<u64>()),
            |(ollama_rss, heimdall_rss, total)| {
                // Req 3.8: ollama_rss clamps to None when > total (total > 0).
                let clamped = clamp_rss(ollama_rss, total);
                if let Some(v) = clamped {
                    if total > 0 {
                        prop_assert!(
                            v <= total,
                            "clamped ollama_rss {} must be <= total {}",
                            v,
                            total
                        );
                    }
                }
                // Req 3.9: heimdall_rss clamped down to total.
                let self_clamped = clamp_self_rss(heimdall_rss, total);
                if total > 0 {
                    prop_assert!(
                        self_clamped <= total,
                        "clamped heimdall_rss {} must be <= total {}",
                        self_clamped,
                        total
                    );
                }
                Ok(())
            },
        )
        .expect("P2: RSS clamp invariants hold");
}

#[test]
fn p2_cpu_percent_in_range_never_nan() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    });

    runner
        .run(&arb_stat_pair(), |(s1, s2)| {
            let (agg, per_core) = compute_cpu_percent(&s1, &s2);
            prop_assert!(!agg.is_nan(), "aggregate cpu% must never be NaN");
            prop_assert!(
                (0.0..=100.0).contains(&agg),
                "aggregate cpu% {} must be in [0, 100]",
                agg
            );
            for c in &per_core {
                prop_assert!(!c.is_nan(), "per-core cpu% must never be NaN");
                prop_assert!(
                    (0.0..=100.0).contains(c),
                    "per-core cpu% {} must be in [0, 100]",
                    c
                );
            }
            Ok(())
        })
        .expect("P2: cpu-percent range invariants hold");
}
