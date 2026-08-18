//! integration_api_ps_shape.rs — /api/ps parsing against a pinned fixture.
//!
//! **Validates: Requirements 5.1, 5.4, 5.5, 5.6**
//!
//! Stands up a `wiremock` server returning a pinned `/api/ps` JSON
//! fixture, points an `OllamaClient` at it, calls `list_running()`, and
//! asserts the parsed `Vec<RunningModel>` matches the expected count,
//! names, and sizes (bytes → MiB via integer truncation). This exercises
//! the full HTTP → JSON → `RunningModel` path end-to-end (the unit tests
//! in Task 2.3 cover the pure parser; this covers the wire integration).

use heimdall_lib::ollama_client::OllamaClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A realistic two-model `/api/ps` body. Sizes are in bytes:
///   gemma3:           2 GiB total, 1 GiB VRAM
///   nomic-embed-text: 300 MiB total, no VRAM field
const PS_FIXTURE: &str = r#"{
  "models": [
    {
      "name": "gemma3:latest",
      "model": "gemma3:latest",
      "size": 2147483648,
      "size_vram": 1073741824,
      "digest": "abc123",
      "expires_at": "2026-01-01T12:00:00Z"
    },
    {
      "name": "nomic-embed-text:latest",
      "model": "nomic-embed-text:latest",
      "size": 314572800,
      "digest": "def456",
      "expires_at": "2026-01-01T12:05:00Z"
    }
  ]
}"#;

#[test]
fn api_ps_shape_matches_fixture() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(PS_FIXTURE.as_bytes().to_vec(), "application/json"),
            )
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri());
        let models = client.list_running().await.expect("list_running succeeds");

        // Count + order preserved (Req 5.5).
        assert_eq!(models.len(), 2, "two models in the fixture");
        assert_eq!(models[0].name, "gemma3:latest");
        assert_eq!(models[1].name, "nomic-embed-text:latest");

        // Bytes → MiB truncation (Req 5.1).
        assert_eq!(models[0].size_total_mb, 2048, "2 GiB → 2048 MiB");
        assert_eq!(models[0].size_vram_mb, Some(1024), "1 GiB VRAM → 1024 MiB");
        assert_eq!(models[1].size_total_mb, 300, "300 MiB total");
        assert_eq!(
            models[1].size_vram_mb, None,
            "missing size_vram → None (Req 5.1)"
        );

        // expires_at parsed RFC3339 → epoch seconds.
        // 2026-01-01T12:00:00Z = 1767268800.
        assert_eq!(models[0].expires_at, 1767268800);
        // idle_seconds is Governor-computed; None straight off the wire.
        assert_eq!(models[0].idle_seconds, None);
    });
}

#[test]
fn api_ps_empty_list_is_ok_and_empty() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(br#"{ "models": [] }"#.to_vec(), "application/json"),
            )
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri());
        let models = client.list_running().await.expect("empty list ok");
        assert!(models.is_empty(), "empty /api/ps → empty Vec (Req 5.7)");
    });
}

#[test]
fn api_ps_http_500_is_err() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = OllamaClient::new(server.uri());
        let result = client.list_running().await;
        assert!(result.is_err(), "non-2xx HTTP maps to Err (Req 5.3)");
    });
}
