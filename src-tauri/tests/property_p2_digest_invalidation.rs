//! property_p2_digest_invalidation.rs — P2: Digest Invalidation.
//!
//! **Property 2: Digest Invalidation**
//!
//! **Validates: Requirements 2.1, 2.2**
//!
//! Strategy: `prop_model_name()` × two distinct `prop_digest()` values ×
//! two distinct `prop_model_capabilities()` values.
//!
//! Predicate: pre-populate the registry cache with `(m, d_old, caps_old)`,
//! persist the row, then call `evict(m)` and verify the cache no longer
//! has the entry and `read_row(m, d_old)` returns None.

mod proptest_strategies;

use std::sync::Arc;

use heimdall_lib::model_registry::ModelRegistry;
use heimdall_lib::models::ModelCapabilities;
use heimdall_lib::ollama_client::OllamaClient;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use proptest_strategies::{prop_digest, prop_model_capabilities, prop_model_name};
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

#[test]
fn p2_digest_invalidation() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    let mut runner = TestRunner::new(ProptestConfig {
        cases: 16,
        ..ProptestConfig::default()
    });

    // Strategy: model name, two distinct digests, two distinct capabilities.
    let strategy = (
        prop_model_name(),
        prop_digest(),
        prop_digest(),
        prop_model_capabilities(),
        prop_model_capabilities(),
    )
        .prop_filter(
            "digests must be distinct",
            |(_, d1, d2, _, _)| d1 != d2,
        );

    let result = runner.run(&strategy, |(model_name, d_old, d_new, mut caps_old, mut caps_new)| {
        // Ensure caps use the correct model_name and digests.
        caps_old.model_name = model_name.clone();
        caps_old.digest = d_old.clone();
        caps_new.model_name = model_name.clone();
        caps_new.digest = d_new.clone();

        rt.block_on(async {
            let pool = memory_pool().await;
            heimdall_lib::db::run_migrations(&pool)
                .await
                .expect("migrations succeed");

            let registry = build_registry(pool);

            // Pre-populate: persist the old row and insert into cache.
            registry
                .persist(&caps_old)
                .await
                .expect("persist old caps succeeds");

            let arc_old = Arc::new(caps_old.clone());
            {
                let mut cache = registry.cache.lock().await;
                cache.insert(model_name.clone(), arc_old);
            }

            // Verify cache has the entry before eviction.
            {
                let cache = registry.cache.lock().await;
                prop_assert!(
                    cache.contains_key(&model_name),
                    "cache should contain the model before eviction"
                );
            }

            // Call evict.
            registry
                .evict(&model_name)
                .await
                .expect("evict succeeds");

            // Verify cache no longer has the entry.
            {
                let cache = registry.cache.lock().await;
                prop_assert!(
                    !cache.contains_key(&model_name),
                    "cache should NOT contain the model after eviction"
                );
            }

            // Verify read_row with old digest returns None (row was deleted).
            let row = registry
                .read_row(&model_name, &d_old)
                .await
                .expect("read_row should not error");
            prop_assert!(
                row.is_none(),
                "read_row(model, old_digest) should return None after eviction"
            );

            Ok(())
        })
    });

    result.expect("property holds for all generated cases");
}
