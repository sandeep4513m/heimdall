//! integration_polling_lifecycle.rs — Polling-loop start / observe / cancel.
//!
//! **Validates: Requirements 1.1, 1.2, 1.3, 1.4, 16.2**
//!
//! Spins up a `Governor` against a `tauri::test::mock_app()` handle, an
//! in-memory SqlitePool, and an `OllamaClient` pointed at an unreachable
//! address (so `list_running` is skipped — no ollama PID resolves in the
//! sandbox — and the tick stays fast). The test then:
//!
//!   1. drives one `tick()` and asserts it returns a well-formed
//!      `GovernorMetrics` (thresholds populated, timestamp set);
//!   2. registers a `governor://metrics` listener via the mock app's
//!      event system and spawns `governor.run(token)`, asserting at least
//!      one metrics event is observed within 2.5 s (Req 1.2, 16.2);
//!   3. cancels the token and asserts the spawned task's `JoinHandle`
//!      completes within 2200 ms (Req 1.4).
//!
//! Requires the `tauri` `test` feature (declared in `[dev-dependencies]`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use heimdall_lib::adaptive_config::AppConfig;
use heimdall_lib::governor::Governor;
use heimdall_lib::models::{HardwareInfo, HardwareTier, ScalarKind, TierConfig};
use heimdall_lib::ollama_client::OllamaClient;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use tauri::test::{mock_app, MockRuntime};
use tauri::Listener;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

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
        tier: HardwareTier::Minimal,
        rag_enabled: true,
        embedding_model: "nomic-embed-text".to_string(),
        chunk_size_tokens: 256,
        chunk_overlap_tokens: 32,
        max_vectors: None,
        auto_unload_minutes: None,
        rag_top_k: 5,
        quantization: ScalarKind::F16,
        index_mmap: true,
        governor_warn_mb: 800,
        governor_unload_mb: 400,
        governor_critical_mb: 200,
        safe_headroom_pct: 0.80,
    }
}

fn hardware() -> HardwareInfo {
    HardwareInfo {
        total_ram_mb: 3900,
        available_ram_mb: 2000,
        vram_mb: None,
        cpu_cores: 4,
        detected_tier: HardwareTier::Minimal,
        effective_tier: HardwareTier::Minimal,
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
fn polling_lifecycle_start_observe_cancel() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    rt.block_on(async {
        let app = mock_app();
        let handle = app.handle().clone();
        let pool = memory_pool().await;
        let governor = build_governor(&handle, pool);

        // (1) One tick returns a well-formed snapshot.
        let metrics = governor.tick().await;
        assert!(
            metrics.timestamp_unix_ms > 0,
            "tick must stamp a wall-clock time"
        );
        assert!(
            metrics.thresholds.warn_mb >= metrics.thresholds.unload_mb,
            "thresholds must be populated and ordered (warn >= unload)"
        );

        // (2) Observe ≥ 1 `governor://metrics` event within 2.5 s.
        let count = Arc::new(AtomicUsize::new(0));
        {
            let count = count.clone();
            // `listen_any` catches the broadcast emit regardless of target.
            handle.listen_any("governor://metrics", move |_event| {
                count.fetch_add(1, Ordering::SeqCst);
            });
        }

        let token = CancellationToken::new();
        let join = tokio::spawn({
            let g = governor.clone();
            let t = token.clone();
            async move { g.run(t).await }
        });

        // Poll for the first emit. First tick fires immediately (sleep
        // AFTER the tick), so this should land well under 2.5 s.
        let observe_deadline = Instant::now() + Duration::from_millis(2500);
        while count.load(Ordering::SeqCst) == 0 && Instant::now() < observe_deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            count.load(Ordering::SeqCst) >= 1,
            "expected at least one governor://metrics event within 2.5 s"
        );

        // (3) Cancel and assert the loop exits within 2200 ms (Req 1.4).
        token.cancel();
        let exited = tokio::time::timeout(Duration::from_millis(2200), join).await;
        assert!(
            exited.is_ok(),
            "governor.run must exit within 2200 ms of cancellation"
        );
        exited
            .unwrap()
            .expect("governor.run task must not panic");
    });
}
