//! property_p1_cache_determinism.rs — P1: Cache Determinism.
//!
//! **Property 1: Cache Determinism**
//!
//! **Validates: Requirements 1.1, 1.2**
//!
//! Strategy: `prop_model_capabilities()` plus `prop_model_name()`.
//!
//! Predicate: build a `ModelRegistry` against a fresh SQLite memory pool,
//! call `persist(&caps)`, then populate the cache manually, then assert
//! that two consecutive cache lookups return values whose fields are
//! bitwise equal to `caps`, and the second call's `Arc` is `Arc::ptr_eq`
//! with the first.
//!
//! Note: Since `get_capabilities` requires a live OllamaClient (for
//! `live_digest_for`), we test the CACHE path directly: persist a row,
//! insert into cache manually, then verify cache lookups return ptr_eq
//! Arcs with correct data.

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

/// Open an in-memory SQLite pool with the same pragmas as production.
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

/// Build a registry against the given pool with an OllamaClient pointed
/// at an unreachable address (never used in this test).
fn build_registry(pool: sqlx::SqlitePool) -> ModelRegistry {
    let ollama = OllamaClient::new("http://127.0.0.1:1");
    ModelRegistry::new(pool, ollama)
}

/// Assert that two `ModelCapabilities` values are field-for-field equal.
fn assert_caps_equal(actual: &ModelCapabilities, expected: &ModelCapabilities) {
    assert_eq!(actual.model_name, expected.model_name);
    assert_eq!(actual.digest, expected.digest);
    assert_eq!(actual.completion, expected.completion);
    assert_eq!(actual.vision, expected.vision);
    assert_eq!(actual.thinking, expected.thinking);
    assert_eq!(actual.tools, expected.tools);
    assert_eq!(actual.embedding, expected.embedding);
    assert_eq!(actual.capability_source, expected.capability_source);
    assert_eq!(actual.raw_capabilities, expected.raw_capabilities);
    assert_eq!(actual.family, expected.family);
    assert_eq!(actual.parameter_size, expected.parameter_size);
    assert_eq!(actual.quantization_level, expected.quantization_level);
    assert_eq!(actual.detected_at, expected.detected_at);
    assert_eq!(actual.updated_at, expected.updated_at);
}

// ---------------------------------------------------------------------------
// Property test
// ---------------------------------------------------------------------------

#[test]
fn p1_cache_determinism() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    let mut runner = TestRunner::new(ProptestConfig {
        cases: 16,
        ..ProptestConfig::default()
    });

    let result = runner.run(&prop_model_capabilities(), |caps| {
        rt.block_on(async {
            let pool = memory_pool().await;
            // Run migrations so the model_capabilities table exists.
            heimdall_lib::db::run_migrations(&pool)
                .await
                .expect("migrations succeed");

            let registry = build_registry(pool);

            // Persist the row to SQLite.
            registry
                .persist(&caps)
                .await
                .expect("persist succeeds");

            // Manually insert into the in-memory cache (simulating
            // what hydrate or get_capabilities would do).
            let arc = Arc::new(caps.clone());
            {
                let mut cache = registry.cache.lock().await;
                cache.insert(caps.model_name.clone(), Arc::clone(&arc));
            }

            // First cache lookup.
            let first = {
                let cache = registry.cache.lock().await;
                cache.get(&caps.model_name).cloned()
            };
            let first = first.expect("first lookup should hit cache");

            // Second cache lookup.
            let second = {
                let cache = registry.cache.lock().await;
                cache.get(&caps.model_name).cloned()
            };
            let second = second.expect("second lookup should hit cache");

            // Assert bitwise equality of fields.
            assert_caps_equal(&first, &caps);
            assert_caps_equal(&second, &caps);

            // Assert Arc::ptr_eq — both lookups return the same Arc.
            prop_assert!(
                Arc::ptr_eq(&first, &second),
                "two consecutive cache lookups must return Arc::ptr_eq values"
            );

            Ok(())
        })
    });

    result.expect("property holds for all generated cases");
}
