// Feature: governor-intelligence, NF1: chat-stream latency impact
//
// NF1 (Req 16.1): the Phase 6 Governor's chat-stream hook MUST NOT add
// measurable latency to the token hot path. The hook does exactly one
// thing per emitted token — a non-blocking `try_lock` on the shared
// `model_last_used` map followed by an `insert(name, Instant::now())`
// (Task 10.1). This benchmark measures that bookkeeping in isolation
// against a trivial baseline so a regression in the hook cost shows up
// as a criterion delta.
//
// IMPORTANT: production stores `model_last_used` behind a
// `std::sync::Mutex` (NOT `parking_lot`) — see `AppState.model_last_used`
// in `lib.rs`. We mirror that exact type here so the measured cost is the
// real one. The closure in `chat_stream` calls `try_lock()` (giving up on
// contention so the hot path never blocks); we reproduce the uncontended
// fast path, which is the case the token loop actually hits.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// The real hot-path bookkeeping: `try_lock` the std::sync::Mutex map and
/// insert `(name, Instant::now())`. Mirrors the `on_token` closure wired
/// into `OllamaClient::chat_stream` from `lib.rs::chat_stream` (Task 10.1).
fn bench_token_emit_with_bookkeeping(c: &mut Criterion) {
    let map: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
    let model = "gemma3".to_string();

    c.bench_function("token_emit_with_bookkeeping", |b| {
        b.iter(|| {
            // try_lock is the production path — uncontended here, which is
            // the steady-state case during streaming (the Governor only
            // try_locks once every 2s).
            if let Ok(mut guard) = map.try_lock() {
                guard.insert(black_box(model.clone()), Instant::now());
            }
        });
    });
}

/// Baseline: a trivial unit of work with no map and no lock. The delta
/// between this and `bench_token_emit_with_bookkeeping` is the Governor
/// hook's per-token cost (NF1).
fn bench_token_emit_baseline(c: &mut Criterion) {
    c.bench_function("token_emit_baseline", |b| {
        b.iter(|| {
            // A trivial yield-equivalent: touch a black-boxed value so the
            // optimiser cannot elide the loop body.
            black_box(std::hint::black_box(0u64).wrapping_add(1));
        });
    });
}

criterion_group!(
    benches,
    bench_token_emit_with_bookkeeping,
    bench_token_emit_baseline
);
criterion_main!(benches);
