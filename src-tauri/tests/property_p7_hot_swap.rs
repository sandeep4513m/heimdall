//! property_p7_hot_swap.rs — P7: Hot-Swap Safety.
//!
//! **Property 7: Hot-Swap Safety (simplified)**
//!
//! **Validates: Requirements 2.3, 7.1, 7.1.a, 7.1.b, 7.1.c, 7.1.d**
//!
//! Simplified version: pre-populate cache with old digest, call evict,
//! verify cache is empty and read_row returns None for old digest. Then
//! persist a new row with new digest, verify read_row returns the new row.
//!
//! This tests the eviction + re-persist cycle without needing concurrent
//! operations or mock OllamaClient.

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
fn p7_hot_swap_evict_then_repersist() {
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

            // ── Phase 1: Populate with old digest ─────────────────────────
            registry
                .persist(&caps_old)
                .await
                .expect("persist old caps succeeds");

            // Insert into cache.
            let arc_old = Arc::new(caps_old.clone());
            {
                let mut cache = registry.cache.lock().await;
                cache.insert(model_name.clone(), arc_old);
            }

            // Verify old row is readable.
            let row = registry
                .read_row(&model_name, &d_old)
                .await
                .expect("read_row should not error");
            prop_assert!(
                row.is_some(),
                "read_row(model, old_digest) should return Some before eviction"
            );

            // ── Phase 2: Evict (simulating digest change detection) ───────
            registry
                .evict(&model_name)
                .await
                .expect("evict succeeds");

            // Verify cache is empty for this model.
            {
                let cache = registry.cache.lock().await;
                prop_assert!(
                    !cache.contains_key(&model_name),
                    "cache should be empty after eviction"
                );
            }

            // Verify read_row with old digest returns None.
            let row = registry
                .read_row(&model_name, &d_old)
                .await
                .expect("read_row should not error");
            prop_assert!(
                row.is_none(),
                "read_row(model, old_digest) should return None after eviction"
            );

            // ── Phase 3: Re-persist with new digest ───────────────────────
            registry
                .persist(&caps_new)
                .await
                .expect("persist new caps succeeds");

            // Verify read_row with new digest returns the new row.
            let row = registry
                .read_row(&model_name, &d_new)
                .await
                .expect("read_row should not error");
            let row = row.expect("read_row(model, new_digest) should return Some after re-persist");

            // Verify the new row has the correct data.
            prop_assert_eq!(&row.model_name, &model_name);
            prop_assert_eq!(&row.digest, &d_new);
            prop_assert_eq!(row.completion, caps_new.completion);
            prop_assert_eq!(row.vision, caps_new.vision);
            prop_assert_eq!(row.thinking, caps_new.thinking);
            prop_assert_eq!(row.tools, caps_new.tools);
            prop_assert_eq!(row.embedding, caps_new.embedding);
            prop_assert_eq!(row.capability_source, caps_new.capability_source);

            // Verify read_row with old digest still returns None
            // (the old row was replaced, not resurrected).
            let old_row = registry
                .read_row(&model_name, &d_old)
                .await
                .expect("read_row should not error");
            prop_assert!(
                old_row.is_none(),
                "read_row(model, old_digest) should still return None after re-persist with new digest"
            );

            Ok(())
        })
    });

    result.expect("property holds for all generated cases");
}
