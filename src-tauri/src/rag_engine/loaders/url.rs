// src-tauri/src/rag_engine/loaders/url.rs
//
// UrlLoader — fetches an HTTP/HTTPS URL and extracts readable text.
//
// Content-Type routing:
//   text/html              → strip tags with `scraper`, extract visible text
//   text/plain, text/markdown, application/json → pass raw body as-is
//   anything else          → RagError::LoaderError (unsupported content type)
//
// Body is capped at 10 MB; excess is silently truncated and noted in metadata.
// Non-2xx responses are returned as RagError::LoaderError.
// Only http:// and https:// schemes are accepted.

use async_trait::async_trait;
use scraper::{Html, Selector};
use tracing::warn;

use super::{LoadedContent, Loader};
use crate::rag_engine::RagError;

/// Maximum response body size (10 MB).
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

pub struct UrlLoader;

#[async_trait]
impl Loader for UrlLoader {
    async fn load(&self, source: &str) -> Result<Vec<LoadedContent>, RagError> {
        // 1. Validate scheme.
        if !source.starts_with("http://") && !source.starts_with("https://") {
            return Err(RagError::LoaderError(
                "Only http:// and https:// URLs are supported".to_string(),
            ));
        }

        // 2. Build reqwest client (mirrors OllamaClient pattern).
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent("Heimdall/0.4")
            // reqwest follows up to 10 redirects by default — no override needed.
            // TLS validation is enabled by default — do NOT disable.
            .build()
            .map_err(|e| RagError::LoaderError(format!("Failed to build HTTP client: {e}")))?;

        // 3. Send GET request.
        let response = client
            .get(source)
            .send()
            .await
            .map_err(|e| RagError::LoaderError(format!("Request failed for {source}: {e}")))?;

        // 4. Check HTTP status.
        let status = response.status();
        if !status.is_success() {
            return Err(RagError::LoaderError(format!(
                "HTTP {status}: {source}"
            )));
        }

        // 5. Capture Content-Type before consuming the response body.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_lowercase();

        // 6. Read body up to MAX_BODY_BYTES.
        let (body_bytes, truncated) = read_body_capped(response, MAX_BODY_BYTES).await?;

        let body_str = String::from_utf8_lossy(&body_bytes).into_owned();

        // 7. Route by Content-Type.
        let text = if content_type.contains("text/html") {
            extract_html_text(&body_str)
        } else if content_type.contains("text/plain")
            || content_type.contains("text/markdown")
            || content_type.contains("application/json")
        {
            body_str
        } else {
            // Extract the bare content-type token for the error message.
            let ct_token = content_type
                .split(';')
                .next()
                .unwrap_or(&content_type)
                .trim()
                .to_string();
            return Err(RagError::LoaderError(format!(
                "Unsupported content type '{ct_token}' for URL: {source}"
            )));
        };

        // 8. Build metadata string.
        let metadata = if truncated {
            warn!(
                url = source,
                "Response body exceeded {MAX_BODY_BYTES} bytes; truncated to 10 MB"
            );
            Some(format!("url={source}; truncated=true"))
        } else {
            Some(format!("url={source}"))
        };

        Ok(vec![LoadedContent { text, metadata }])
    }

    /// URL loader has no file extensions — it is invoked directly by the
    /// ingestion worker when the source starts with http:// or https://.
    fn extensions(&self) -> &[&str] {
        &[]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the response body up to `cap` bytes.
/// Returns `(bytes, truncated)` where `truncated` is true when the body
/// exceeded the cap and was cut short.
async fn read_body_capped(
    response: reqwest::Response,
    cap: usize,
) -> Result<(Vec<u8>, bool), RagError> {
    use futures_util::StreamExt;

    let mut buf: Vec<u8> = Vec::with_capacity(cap.min(64 * 1024));
    let mut stream = response.bytes_stream();
    let mut truncated = false;

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| RagError::LoaderError(format!("Error reading response body: {e}")))?;

        let remaining = cap.saturating_sub(buf.len());
        if remaining == 0 {
            truncated = true;
            break;
        }

        if chunk.len() <= remaining {
            buf.extend_from_slice(&chunk);
        } else {
            buf.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
    }

    Ok((buf, truncated))
}

/// Strip HTML tags from `html` and return the visible text.
///
/// Algorithm:
/// 1. Parse the full document with `scraper::Html::parse_document`.
/// 2. Select the `<body>` element (fall back to the root if absent).
/// 3. Walk text nodes, skipping `<script>`, `<style>`, `<nav>`,
///    `<aside>`, and `<footer>` elements.
/// 4. Collapse runs of whitespace within each text node to a single space.
/// 5. Separate block-level elements with `\n\n` to preserve paragraph breaks.
fn extract_html_text(html: &str) -> String {
    let document = Html::parse_document(html);

    // Selectors for elements whose text content we want to skip entirely.
    let skip_sel = Selector::parse("script, style, nav, aside, footer").unwrap();

    // We'll collect text by walking the tree manually via the root element.
    // `scraper` exposes `ElementRef::text()` which yields all descendant text
    // nodes, but we need to skip certain subtrees. Instead we iterate over
    // the serialised tree and reconstruct text with paragraph breaks.
    //
    // Strategy: select all text nodes that are NOT inside a skipped element,
    // then join them with appropriate spacing.

    // Build a set of node IDs that belong to skipped subtrees.
    use std::collections::HashSet;
    // `NodeId` comes from the `ego_tree` crate (a transitive dep of `scraper`).
    // We use scraper's re-export path via the tree API.
    let mut skip_ids: HashSet<ego_tree::NodeId> = HashSet::new();
    for skip_el in document.select(&skip_sel) {
        // Mark the element itself and all its descendants.
        let node_id = skip_el.id();
        skip_ids.insert(node_id);
        for descendant in skip_el.descendants() {
            skip_ids.insert(descendant.id());
        }
    }

    // Block-level tags that should produce paragraph breaks.
    const BLOCK_TAGS: &[&str] = &[
        "p", "div", "h1", "h2", "h3", "h4", "h5", "h6", "li", "tr", "br",
        "blockquote", "pre", "article", "section", "header", "main",
    ];

    let mut output = String::with_capacity(html.len() / 4);
    let mut last_was_block = false;

    // Walk every node in document order.
    for node in document.tree.nodes() {
        let id = node.id();
        if skip_ids.contains(&id) {
            continue;
        }

        match node.value() {
            scraper::node::Node::Text(text) => {
                let collapsed = collapse_whitespace(text);
                if collapsed.is_empty() {
                    continue;
                }
                if last_was_block && !output.is_empty() {
                    output.push_str("\n\n");
                }
                output.push_str(&collapsed);
                last_was_block = false;
            }
            scraper::node::Node::Element(el) => {
                let tag = el.name().to_lowercase();
                if BLOCK_TAGS.contains(&tag.as_str()) {
                    last_was_block = true;
                }
            }
            _ => {}
        }
    }

    // Final cleanup: collapse runs of more than two consecutive newlines.
    let re_newlines = output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    re_newlines
}

/// Collapse runs of ASCII whitespace (spaces, tabs, newlines) to a single space.
fn collapse_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            if !prev_space {
                result.push(' ');
            }
            prev_space = true;
        } else {
            result.push(ch);
            prev_space = false;
        }
    }
    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- collapse_whitespace ---

    #[test]
    fn collapse_whitespace_basic() {
        assert_eq!(collapse_whitespace("  hello   world  "), "hello world");
    }

    #[test]
    fn collapse_whitespace_newlines() {
        assert_eq!(collapse_whitespace("foo\n\n\nbar"), "foo bar");
    }

    #[test]
    fn collapse_whitespace_empty() {
        assert_eq!(collapse_whitespace(""), "");
        assert_eq!(collapse_whitespace("   "), "");
    }

    // --- extract_html_text ---

    #[test]
    fn html_strips_script_and_style() {
        let html = r#"<html><body>
            <script>alert('x')</script>
            <style>.foo { color: red; }</style>
            <p>Hello world</p>
        </body></html>"#;
        let text = extract_html_text(html);
        assert!(!text.contains("alert"));
        assert!(!text.contains(".foo"));
        assert!(text.contains("Hello world"));
    }

    #[test]
    fn html_strips_nav_aside_footer() {
        let html = r#"<html><body>
            <nav>Navigation links</nav>
            <aside>Sidebar content</aside>
            <main><p>Main content</p></main>
            <footer>Footer text</footer>
        </body></html>"#;
        let text = extract_html_text(html);
        assert!(!text.contains("Navigation links"));
        assert!(!text.contains("Sidebar content"));
        assert!(!text.contains("Footer text"));
        assert!(text.contains("Main content"));
    }

    #[test]
    fn html_preserves_paragraph_breaks() {
        let html = r#"<html><body>
            <p>First paragraph.</p>
            <p>Second paragraph.</p>
        </body></html>"#;
        let text = extract_html_text(html);
        // Both paragraphs should be present.
        assert!(text.contains("First paragraph."));
        assert!(text.contains("Second paragraph."));
    }

    #[test]
    fn html_empty_body() {
        let html = "<html><body></body></html>";
        let text = extract_html_text(html);
        assert!(text.is_empty());
    }

    // --- UrlLoader::extensions ---

    #[test]
    fn url_loader_has_no_extensions() {
        let loader = UrlLoader;
        assert!(loader.extensions().is_empty());
    }

    // --- scheme validation (sync check, no network) ---

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let loader = UrlLoader;
        let result = loader.load("ftp://example.com/file.txt").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Only http://"));
    }

    #[tokio::test]
    async fn rejects_file_scheme() {
        let loader = UrlLoader;
        let result = loader.load("file:///etc/passwd").await;
        assert!(result.is_err());
    }
}
