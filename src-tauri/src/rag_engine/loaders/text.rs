// src-tauri/src/rag_engine/loaders/text.rs

use tokio::io::AsyncReadExt;

use super::{LoadedContent, Loader};
use crate::rag_engine::RagError;

const MAX_FILE_SIZE: u64 = 200 * 1024 * 1024; // 200 MB
const STREAM_THRESHOLD: u64 = 50 * 1024 * 1024; // 50 MB
const CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8 MB chunks
const FRONT_MATTER_SCAN: usize = 64 * 1024; // 64 KB scan window for YAML front-matter

/// UTF-8 BOM bytes.
const BOM: &[u8] = b"\xEF\xBB\xBF";

pub struct TextLoader;

#[async_trait::async_trait]
impl Loader for TextLoader {
    async fn load(&self, source: &str) -> Result<Vec<LoadedContent>, RagError> {
        let metadata = tokio::fs::metadata(source).await?;
        let file_size = metadata.len();

        if file_size > MAX_FILE_SIZE {
            return Err(RagError::LoaderError(format!(
                "File exceeds 200 MB limit: {}",
                source
            )));
        }

        let raw_bytes = if file_size > STREAM_THRESHOLD {
            read_in_chunks(source, file_size).await?
        } else {
            tokio::fs::read(source).await?
        };

        // Strip UTF-8 BOM if present.
        let bytes = if raw_bytes.starts_with(BOM) {
            &raw_bytes[BOM.len()..]
        } else {
            &raw_bytes[..]
        };

        // Decode as UTF-8.
        let text = std::str::from_utf8(bytes).map_err(|e| {
            RagError::LoaderError(format!(
                "UTF-8 decode error at byte {}: {}",
                e.valid_up_to(),
                source
            ))
        })?;

        // Normalize line endings: CRLF → LF, bare CR → LF.
        let text = normalize_line_endings(text);

        // Strip YAML front-matter for .md files.
        let is_markdown = std::path::Path::new(source)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("md"))
            .unwrap_or(false);

        let text = if is_markdown {
            strip_yaml_front_matter(&text)
        } else {
            text
        };

        Ok(vec![LoadedContent {
            text,
            metadata: None,
        }])
    }

    fn extensions(&self) -> &[&str] {
        &["txt", "md"]
    }
}

/// Read a large file in 8 MB chunks, collecting into a single Vec<u8>.
async fn read_in_chunks(path: &str, file_size: u64) -> Result<Vec<u8>, RagError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = Vec::with_capacity(file_size as usize);
    let mut chunk = vec![0u8; CHUNK_SIZE];

    loop {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    Ok(buf)
}

/// Normalize line endings: CRLF → LF, bare CR → LF.
fn normalize_line_endings(s: &str) -> String {
    // Replace CRLF first, then bare CR.
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Strip YAML front-matter from a Markdown string.
///
/// Front-matter is defined as content between an opening `---` on the very
/// first line and a closing `---` on a subsequent line, scanned within the
/// first 64 KB of the file.
fn strip_yaml_front_matter(text: &str) -> String {
    // Only scan the first FRONT_MATTER_SCAN bytes to avoid O(n) on huge files.
    let scan_end = text
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i < FRONT_MATTER_SCAN)
        .last()
        .map(|i| i + 1)
        .unwrap_or(text.len());

    let scan = &text[..scan_end];

    // Must start with "---" followed by a newline (or end of scan).
    if !scan.starts_with("---") {
        return text.to_owned();
    }

    // Find the end of the opening "---" line.
    let after_open = match scan.find('\n') {
        Some(pos) => pos + 1,
        None => return text.to_owned(), // No newline after opening ---
    };

    // The opening line must be exactly "---" (possibly with trailing whitespace).
    let open_line = scan[..after_open].trim();
    if open_line != "---" {
        return text.to_owned();
    }

    // Search for the closing "---" line.
    let rest = &scan[after_open..];
    let mut search_pos = 0;
    while search_pos < rest.len() {
        let line_end = rest[search_pos..].find('\n').map(|p| search_pos + p + 1);
        let line_end = line_end.unwrap_or(rest.len());
        let line = rest[search_pos..line_end].trim();

        if line == "---" {
            // Found closing delimiter; return everything after it.
            let content_start = after_open + line_end;
            return text[content_start..].to_owned();
        }

        if line_end == rest.len() {
            break;
        }
        search_pos = line_end;
    }

    // No closing delimiter found — return original text unchanged.
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_crlf() {
        let input = "line1\r\nline2\r\nline3";
        assert_eq!(normalize_line_endings(input), "line1\nline2\nline3");
    }

    #[test]
    fn test_normalize_bare_cr() {
        let input = "line1\rline2\rline3";
        assert_eq!(normalize_line_endings(input), "line1\nline2\nline3");
    }

    #[test]
    fn test_normalize_mixed() {
        let input = "line1\r\nline2\rline3\nline4";
        assert_eq!(normalize_line_endings(input), "line1\nline2\nline3\nline4");
    }

    #[test]
    fn test_strip_front_matter_basic() {
        let input = "---\ntitle: Hello\ndate: 2024-01-01\n---\n# Content\nBody text.";
        let result = strip_yaml_front_matter(input);
        assert_eq!(result, "# Content\nBody text.");
    }

    #[test]
    fn test_strip_front_matter_no_front_matter() {
        let input = "# Just a heading\nSome content.";
        let result = strip_yaml_front_matter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_front_matter_no_closing_delimiter() {
        let input = "---\ntitle: Hello\nNo closing delimiter";
        let result = strip_yaml_front_matter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_front_matter_empty_body() {
        let input = "---\ntitle: Hello\n---\n";
        let result = strip_yaml_front_matter(input);
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_loader_extensions() {
        let loader = TextLoader;
        assert_eq!(loader.extensions(), &["txt", "md"]);
    }
}
