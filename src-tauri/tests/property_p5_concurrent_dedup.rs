//! property_p5_concurrent_dedup.rs — P5: Concurrent Detection Dedup.
//!
//! **Property 5: Concurrent Detection Dedup (warm cache path)**
//!
//! **Validates: Requirements 5.1, 5.1.a, 5.1.b**
//!
//! Tests the WARM CACHE path: pre-populate cache, spawn N concurrent
//! cache lookups via `get_capabilities`, assert all returned Arcs are
//! `Arc::ptr_eq`.
//!
//! Note: The cold-path dedup (which requires mocking OllamaClient) is
//! complex and can be deferred. This test proves the warm-cache
//! concurrent access property.

mod proptest_strategies;

use std::sync::Arc;

use heimdall_lib::model_registry::ModelRegistry;
use heimdall_lib::models::ModelCapabilities;
use heimdall_lib::ollama_client::OllamaClient;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use proptest_strategies::prop_model_capabilities;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn memory_pool() -> sqlx::SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("memory url parses")
        .pragma("foreign_keys", "ON")
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "NORMAL")
        .pragma("busy_timeout", "5000")
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("memory pool connects")
}

fn build_registry(pool: sqlx::SqlitePool) -> ModelRegistry {
    let ollama = OllamaClient::new("http://127.0.0.1:1");
    ModelRegistry::new(pool, ollama)
}

// ---------------------------------------------------------------------------
// Property test
// ---------------------------------------------------------------------------

/// Uses `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`
/// plus `proptest::test_runner::TestRunner` (manual driver because
/// `proptest!` macro is sync).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p5_concurrent_warm_cache_lookups_return_ptr_eq_arcs() {
    let mut runner = TestRunner::new(ProptestConfig {
        cases: 16,
        ..ProptestConfig::default()
    });

    // Strategy: a model capabilities value and N in 2..=16.
    let strategy = (prop_model_capabilities(), 2_usize..=16);

    let result = runner.run(&strategy, |(caps, n)| {
        // We need to block_on inside the proptest runner since the
        // runner itself is sync. We're already inside a multi-thread
        // tokio runtime from the outer #[tokio::test], so we use
        // tokio::task::block_in_place + a nested runtime for the
        // property body.
        tokio::task::block_in_place(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("nested runtime builds");

            rt.block_on(async {
                let pool = memory_pool().await;
                heimdall_lib::db::run_migrations(&pool)
                    .await
                    .expect("migrations succeed");

                let registry = Arc::new(build_registry(pool));

                // Pre-populate the cache with the generated capabilities.
                let arc = Arc::new(caps.clone());
                {
                    let mut cache = registry.cache.lock().await;
                    cache.insert(caps.model_name.clone(), Arc::clone(&arc));
                }

                // Spawn N concurrent get_capabilities calls.
                let mut handles = Vec::with_capacity(n);
                for _ in 0..n {
                    let reg = Arc::clone(&registry);
                    let name = caps.model_name.clone();
                    handles.push(tokio::spawn(async move {
                        reg.get_capabilities(&name).await
                    }));
                }

                // Collect all results.
                let mut results = Vec::with_capacity(n);
                for handle in handles {
                    let result = handle.await.expect("task did not panic");
                    let arc_result = result.expect("get_capabilities should succeed on warm cache");
                    results.push(arc_result);
                }

                // Assert all returned Arcs are ptr_eq to each other.
                let first = &results[0];
                for (i, other) in results.iter().enumerate().skip(1) {
                    prop_assert!(
                        Arc::ptr_eq(first, other),
                        "result[0] and result[{}] should be Arc::ptr_eq (warm cache path)",
                        i
                    );
                }

                // Also verify the data is correct.
                prop_assert_eq!(&first.model_name, &caps.model_name);
                prop_assert_eq!(&first.digest, &caps.digest);
                prop_assert_eq!(first.vision, caps.vision);
                prop_assert_eq!(first.thinking, caps.thinking);
                prop_assert_eq!(first.completion, caps.completion);
                prop_assert_eq!(first.tools, caps.tools);
                prop_assert_eq!(first.embedding, caps.embedding);

                Ok(())
            })
        })
    });

    result.expect("property holds for all generated cases");
}
