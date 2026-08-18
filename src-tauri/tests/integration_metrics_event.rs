//! integration_metrics_event.rs — Single-tick metrics payload round-trips.
//!
//! **Validates: Requirements 1.7, 1.9, 16.3**
//!
//! Drives one `Governor::tick()` against a `mock_app()` handle and an
//! unreachable Ollama, serialises the resulting `GovernorMetrics`, and
//! asserts the payload:
//!   - round-trips through JSON unchanged (the P1 wire contract), and
//!   - carries the expected fields (populated thresholds, a tier, a
//!     non-zero timestamp, an empty loaded-model list when Ollama is
//!     absent).
//!
//! This is the focused single-tick companion to the polling-lifecycle
//! test — it pins the exact event payload the frontend store decodes
//! from `governor://metrics`.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use heimdall_lib::adaptive_config::AppConfig;
use heimdall_lib::governor::Governor;
use heimdall_lib::models::{
    GovernorMetrics, HardwareInfo, HardwareTier, ScalarKind, TierConfig,
};
use heimdall_lib::ollama_client::OllamaClient;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use tauri::test::{mock_app, MockRuntime};
use tokio::sync::Mutex as AsyncMutex;

async fn memory_pool() -> sqlx::SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("memory url parses")
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("memory pool connects")
}

fn tier_config() -> TierConfig {
    TierConfig {
        tier: HardwareTier::Standard,
        rag_enabled: true,
        embedding_model: "nomic-embed-text".to_string(),
        chunk_size_tokens: 512,
        chunk_overlap_tokens: 64,
        max_vectors: None,
        auto_unload_minutes: None,
        rag_top_k: 10,
        quantization: ScalarKind::F32,
        index_mmap: true,
        governor_warn_mb: 1500,
        governor_unload_mb: 800,
        governor_critical_mb: 400,
        safe_headroom_pct: 0.80,
    }
}

fn hardware() -> HardwareInfo {
    HardwareInfo {
        total_ram_mb: 16000,
        available_ram_mb: 8000,
        vram_mb: None,
        cpu_cores: 8,
        detected_tier: HardwareTier::Standard,
        effective_tier: HardwareTier::Standard,
    }
}

fn build_governor(
    app: &tauri::AppHandle<MockRuntime>,
    pool: sqlx::SqlitePool,
) -> Arc<Governor<MockRuntime>> {
    Arc::new(Governor::new(
        OllamaClient::new("http://127.0.0.1:1"),
        pool,
        Arc::new(tokio::sync::RwLock::new(tier_config())),
        hardware(),
        Arc::new(AsyncMutex::new(AppConfig::default())),
        Arc::new(std::sync::Mutex::new(HashMap::new())),
        Arc::new(AsyncMutex::new(HashMap::new())),
        Arc::new(std::sync::Mutex::new(HashMap::new())),
        Arc::new(AsyncMutex::new(HashMap::new())),
        Arc::new(std::sync::Mutex::new(None)),
        Arc::new(AtomicBool::new(false)),
        app.clone(),
    ))
}

#[test]
fn metrics_event_payload_round_trips_and_has_fields() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    rt.block_on(async {
        let app = mock_app();
        let handle = app.handle().clone();
        let pool = memory_pool().await;
        let governor = build_governor(&handle, pool);

        let metrics = governor.tick().await;

        // Serialise → deserialise round-trip (Req 1.9, 16.3 wire contract).
        let json = serde_json::to_string(&metrics).expect("serialise");
        let back: GovernorMetrics =
            serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, metrics, "metrics payload must round-trip via JSON");

        // Expected fields.
        assert!(metrics.timestamp_unix_ms > 0, "timestamp populated");
        assert_eq!(
            metrics.thresholds.warn_mb, 1500,
            "thresholds reflect the configured Standard tier"
        );
        assert_eq!(metrics.thresholds.unload_mb, 800);
        assert_eq!(metrics.thresholds.critical_mb, 400);
        assert_eq!(metrics.effective_tier, HardwareTier::Standard);
        // Ollama is unreachable in the sandbox → no loaded models, offline.
        assert!(
            metrics.loaded_models.is_empty(),
            "no loaded models when Ollama is absent"
        );
        assert!(!metrics.ollama_online, "ollama_online false when absent");

        // The serialised form must use the snake_case wire tag for the
        // risk state enum so the frontend store decodes it.
        assert!(
            json.contains("\"risk_state\""),
            "payload carries risk_state field"
        );
    });
}
