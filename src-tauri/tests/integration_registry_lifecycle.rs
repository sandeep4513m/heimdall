//! integration_registry_lifecycle.rs — End-to-end list / get / repull cycle.
//!
//! **Validates: Requirements 1.1, 1.2, 2.1, 2.2, 2.3, 9.1, 9.2**
//!
//! Boot a registry against a fresh SQLite temp-file pool with an
//! OllamaClient pointed at an unreachable URL (127.0.0.1:1).
//!
//! Test the cache-only paths: persist rows, hydrate, verify cache hits,
//! verify eviction, verify `legacy_capability_from` produces expected
//! variants.
//!
//! This tests the lifecycle without needing a live Ollama instance.

use std::sync::Arc;

use heimdall_lib::db;
use heimdall_lib::model_registry::ModelRegistry;
use heimdall_lib::models::{
    legacy_capability_from, CapabilitySource, ModelCapabilities, ModelCapability,
};
use heimdall_lib::ollama_client::OllamaClient;
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

/// Build a test `ModelCapabilities` for a vision model.
fn vision_caps(name: &str, digest: &str) -> ModelCapabilities {
    ModelCapabilities {
        model_name: name.to_string(),
        digest: digest.to_string(),
        completion: true,
        vision: true,
        thinking: false,
        tools: false,
        embedding: false,
        capability_source: CapabilitySource::ApiShow,
        raw_capabilities: vec!["completion".to_string(), "vision".to_string()],
        family: Some("gemma".to_string()),
        parameter_size: Some("7B".to_string()),
        quantization_level: Some("Q4_K_M".to_string()),
        detected_at: 1700000000,
        updated_at: 1700000000,
    }
}

/// Build a test `ModelCapabilities` for a thinking model.
fn thinking_caps(name: &str, digest: &str) -> ModelCapabilities {
    ModelCapabilities {
        model_name: name.to_string(),
        digest: digest.to_string(),
        completion: true,
        vision: false,
        thinking: true,
        tools: false,
        embedding: false,
        capability_source: CapabilitySource::ApiShow,
        raw_capabilities: vec!["completion".to_string(), "thinking".to_string()],
        family: Some("deepseek".to_string()),
        parameter_size: Some("7B".to_string()),
        quantization_level: None,
        detected_at: 1700000000,
        updated_at: 1700000000,
    }
}

/// Build a test `ModelCapabilities` for an embedding model.
fn embedding_caps(name: &str, digest: &str) -> ModelCapabilities {
    ModelCapabilities {
        model_name: name.to_string(),
        digest: digest.to_string(),
        completion: false,
        vision: false,
        thinking: false,
        tools: false,
        embedding: true,
        capability_source: CapabilitySource::ApiShow,
        raw_capabilities: vec!["embedding".to_string()],
        family: Some("nomic".to_string()),
        parameter_size: Some("137M".to_string()),
        quantization_level: None,
        detected_at: 1700000000,
        updated_at: 1700000000,
    }
}

/// Build a test `ModelCapabilities` for a text-only model.
fn text_only_caps(name: &str, digest: &str) -> ModelCapabilities {
    ModelCapabilities {
        model_name: name.to_string(),
        digest: digest.to_string(),
        completion: true,
        vision: false,
        thinking: false,
        tools: false,
        embedding: false,
        capability_source: CapabilitySource::Heuristic,
        raw_capabilities: vec![],
        family: Some("llama".to_string()),
        parameter_size: Some("8B".to_string()),
        quantization_level: Some("Q4_0".to_string()),
        detected_at: 1700000000,
        updated_at: 1700000000,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test: persist rows, hydrate, verify cache hits return correct data.
#[tokio::test]
async fn lifecycle_persist_hydrate_cache_hit() {
    let pool = memory_pool().await;
    db::run_migrations(&pool).await.expect("migrations succeed");

    let registry = build_registry(pool.clone());

    // Persist several models.
    let gemma3 = vision_caps("gemma3", "sha256:aaaa000000000000000000000000000000000000000000000000000000000000");
    let deepseek = thinking_caps("deepseek-r1:7b", "sha256:bbbb000000000000000000000000000000000000000000000000000000000000");
    let nomic = embedding_caps("nomic-embed-text", "sha256:cccc000000000000000000000000000000000000000000000000000000000000");
    let llama = text_only_caps("llama3:8b", "sha256:dddd000000000000000000000000000000000000000000000000000000000000");

    registry.persist(&gemma3).await.expect("persist gemma3");
    registry.persist(&deepseek).await.expect("persist deepseek");
    registry.persist(&nomic).await.expect("persist nomic");
    registry.persist(&llama).await.expect("persist llama");

    // Hydrate — should load all four rows into cache.
    registry.hydrate().await.expect("hydrate succeeds");

    // Verify cache has all four entries.
    {
        let cache = registry.cache.lock().await;
        assert!(cache.contains_key("gemma3"), "gemma3 should be in cache");
        assert!(cache.contains_key("deepseek-r1:7b"), "deepseek should be in cache");
        assert!(cache.contains_key("nomic-embed-text"), "nomic should be in cache");
        assert!(cache.contains_key("llama3:8b"), "llama should be in cache");
    }

    // Verify cache hit returns correct data for gemma3.
    {
        let cache = registry.cache.lock().await;
        let arc = cache.get("gemma3").expect("gemma3 in cache");
        assert_eq!(arc.vision, true);
        assert_eq!(arc.thinking, false);
        assert_eq!(arc.completion, true);
        assert_eq!(arc.capability_source, CapabilitySource::ApiShow);
        assert_eq!(arc.raw_capabilities, vec!["completion", "vision"]);
    }

    // Verify two consecutive lookups return ptr_eq Arcs (Requirement 1.2).
    let first = {
        let cache = registry.cache.lock().await;
        cache.get("gemma3").cloned().unwrap()
    };
    let second = {
        let cache = registry.cache.lock().await;
        cache.get("gemma3").cloned().unwrap()
    };
    assert!(Arc::ptr_eq(&first, &second), "consecutive cache lookups must be ptr_eq");
}

/// Test: eviction removes from cache and SQLite.
#[tokio::test]
async fn lifecycle_eviction_removes_cache_and_db() {
    let pool = memory_pool().await;
    db::run_migrations(&pool).await.expect("migrations succeed");

    let registry = build_registry(pool.clone());

    let gemma3 = vision_caps("gemma3", "sha256:aaaa000000000000000000000000000000000000000000000000000000000000");
    registry.persist(&gemma3).await.expect("persist gemma3");

    // Insert into cache.
    {
        let mut cache = registry.cache.lock().await;
        cache.insert("gemma3".to_string(), Arc::new(gemma3.clone()));
    }

    // Evict.
    registry.evict("gemma3").await.expect("evict succeeds");

    // Cache should be empty.
    {
        let cache = registry.cache.lock().await;
        assert!(!cache.contains_key("gemma3"), "gemma3 should not be in cache after eviction");
    }

    // read_row with old digest should return None.
    let row = registry
        .read_row("gemma3", "sha256:aaaa000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("read_row should not error");
    assert!(row.is_none(), "read_row should return None after eviction");
}

/// Test: legacy_capability_from produces expected variants.
#[tokio::test]
async fn lifecycle_legacy_capability_from_variants() {
    let pool = memory_pool().await;
    db::run_migrations(&pool).await.expect("migrations succeed");

    // Vision model → Vision variant.
    let gemma3 = vision_caps("gemma3", "sha256:aaaa000000000000000000000000000000000000000000000000000000000000");
    assert_eq!(legacy_capability_from(&gemma3), ModelCapability::Vision);

    // Thinking model → Thinking variant.
    let deepseek = thinking_caps("deepseek-r1:7b", "sha256:bbbb000000000000000000000000000000000000000000000000000000000000");
    assert_eq!(legacy_capability_from(&deepseek), ModelCapability::Thinking);

    // Embedding model → Embedding variant.
    let nomic = embedding_caps("nomic-embed-text", "sha256:cccc000000000000000000000000000000000000000000000000000000000000");
    assert_eq!(legacy_capability_from(&nomic), ModelCapability::Embedding);

    // Text-only model → TextOnly variant.
    let llama = text_only_caps("llama3:8b", "sha256:dddd000000000000000000000000000000000000000000000000000000000000");
    assert_eq!(legacy_capability_from(&llama), ModelCapability::TextOnly);

    // Priority: embedding > vision (a model with both should be Embedding).
    let both = ModelCapabilities {
        model_name: "hybrid".to_string(),
        digest: "sha256:eeee000000000000000000000000000000000000000000000000000000000000".to_string(),
        completion: true,
        vision: true,
        thinking: false,
        tools: false,
        embedding: true,
        capability_source: CapabilitySource::ApiShow,
        raw_capabilities: vec!["completion".to_string(), "vision".to_string(), "embedding".to_string()],
        family: None,
        parameter_size: None,
        quantization_level: None,
        detected_at: 1700000000,
        updated_at: 1700000000,
    };
    assert_eq!(legacy_capability_from(&both), ModelCapability::Embedding);
}

/// Test: evict + re-persist cycle (hot-swap simulation).
#[tokio::test]
async fn lifecycle_hot_swap_evict_and_repersist() {
    let pool = memory_pool().await;
    db::run_migrations(&pool).await.expect("migrations succeed");

    let registry = build_registry(pool.clone());

    let old_digest = "sha256:aaaa000000000000000000000000000000000000000000000000000000000000";
    let new_digest = "sha256:ffff000000000000000000000000000000000000000000000000000000000000";

    // Persist old row.
    let old_caps = vision_caps("gemma3", old_digest);
    registry.persist(&old_caps).await.expect("persist old");

    // Insert into cache.
    {
        let mut cache = registry.cache.lock().await;
        cache.insert("gemma3".to_string(), Arc::new(old_caps.clone()));
    }

    // Simulate digest change detection: evict.
    registry.evict("gemma3").await.expect("evict succeeds");

    // Verify old row is gone.
    let row = registry.read_row("gemma3", old_digest).await.expect("read_row ok");
    assert!(row.is_none(), "old digest row should be gone");

    // Persist new row with new digest (simulating re-detection).
    let new_caps = ModelCapabilities {
        model_name: "gemma3".to_string(),
        digest: new_digest.to_string(),
        completion: true,
        vision: true,
        thinking: true, // Gained thinking in the new version!
        tools: false,
        embedding: false,
        capability_source: CapabilitySource::ApiShow,
        raw_capabilities: vec!["completion".to_string(), "vision".to_string(), "thinking".to_string()],
        family: Some("gemma".to_string()),
        parameter_size: Some("7B".to_string()),
        quantization_level: Some("Q4_K_M".to_string()),
        detected_at: 1700000001,
        updated_at: 1700000001,
    };
    registry.persist(&new_caps).await.expect("persist new");

    // Verify new row is readable.
    let row = registry.read_row("gemma3", new_digest).await.expect("read_row ok");
    let row = row.expect("new digest row should exist");
    assert_eq!(row.digest, new_digest);
    assert_eq!(row.thinking, true);
    assert_eq!(row.vision, true);

    // Verify old digest still returns None.
    let old_row = registry.read_row("gemma3", old_digest).await.expect("read_row ok");
    assert!(old_row.is_none(), "old digest should still return None");
}

/// Test: hydrate is idempotent — calling it twice doesn't corrupt cache.
#[tokio::test]
async fn lifecycle_hydrate_idempotent() {
    let pool = memory_pool().await;
    db::run_migrations(&pool).await.expect("migrations succeed");

    let registry = build_registry(pool.clone());

    let gemma3 = vision_caps("gemma3", "sha256:aaaa000000000000000000000000000000000000000000000000000000000000");
    registry.persist(&gemma3).await.expect("persist gemma3");

    // Hydrate twice.
    registry.hydrate().await.expect("first hydrate");
    registry.hydrate().await.expect("second hydrate");

    // Cache should still have the correct entry.
    let cache = registry.cache.lock().await;
    let arc = cache.get("gemma3").expect("gemma3 in cache");
    assert_eq!(arc.vision, true);
    assert_eq!(arc.digest, "sha256:aaaa000000000000000000000000000000000000000000000000000000000000");
}

/// Test: read_row returns None for digest mismatch (stale row detection).
#[tokio::test]
async fn lifecycle_read_row_digest_mismatch_returns_none() {
    let pool = memory_pool().await;
    db::run_migrations(&pool).await.expect("migrations succeed");

    let registry = build_registry(pool.clone());

    let caps = vision_caps("gemma3", "sha256:aaaa000000000000000000000000000000000000000000000000000000000000");
    registry.persist(&caps).await.expect("persist");

    // Query with a different digest — should return None.
    let row = registry
        .read_row("gemma3", "sha256:9999000000000000000000000000000000000000000000000000000000000000")
        .await
        .expect("read_row ok");
    assert!(row.is_none(), "read_row with mismatched digest should return None");
}
