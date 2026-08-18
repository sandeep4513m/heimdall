// src-tauri/src/rag_engine/loaders/pdf.rs
//
// PdfLoader — extracts text from PDF files using the `pdf` (pdf-rs) crate.
//
// Behaviour contract:
//   • Encrypted/password-protected PDFs → Err(RagError::LoaderError("PDF is encrypted: <path>"))
//   • Malformed/unparseable PDFs        → Err(RagError::LoaderError("PDF parse error in <path>: <e>"))
//   • All pages produce zero text       → Err(RagError::LoaderError("No extractable text in PDF: <path>"))
//   • Otherwise                         → Ok(Vec<LoadedContent>) — one entry per page that has text,
//                                         with metadata = Some("page=<N>") (1-indexed).

use pdf::content::{Op, TextDrawAdjusted as TDA};
use pdf::error::PdfError;
use pdf::file::FileOptions;

use super::{LoadedContent, Loader};
use crate::rag_engine::RagError;

pub struct PdfLoader;

#[async_trait::async_trait]
impl Loader for PdfLoader {
    async fn load(&self, source: &str) -> Result<Vec<LoadedContent>, RagError> {
        // Run the blocking PDF parse on the thread pool.
        let path = source.to_owned();
        let result = tokio::task::spawn_blocking(move || extract_pdf_text(&path)).await;

        match result {
            Ok(inner) => inner,
            Err(join_err) => Err(RagError::LoaderError(format!(
                "PDF loader task panicked for {source}: {join_err}"
            ))),
        }
    }

    fn extensions(&self) -> &[&str] {
        &["pdf"]
    }
}

/// Synchronous PDF text extraction — runs inside `spawn_blocking`.
fn extract_pdf_text(source: &str) -> Result<Vec<LoadedContent>, RagError> {
    // Parse the PDF.  FileOptions::cached().open() reads from disk.
    let file = FileOptions::cached().open(source).map_err(|e| {
        if is_encryption_error(&e) {
            RagError::LoaderError(format!("PDF is encrypted: {source}"))
        } else {
            RagError::LoaderError(format!("PDF parse error in {source}: {e}"))
        }
    })?;

    // Collect pages first so we can borrow `file` for the resolver afterwards.
    let pages: Vec<_> = file.pages().collect();
    let resolver = file.resolver();

    let mut results: Vec<LoadedContent> = Vec::new();

    for (idx, page_result) in pages.into_iter().enumerate() {
        let page_number = (idx + 1) as u32;

        let page = match page_result {
            Ok(p) => p,
            Err(e) => {
                // Skip unreadable pages but continue processing the rest.
                tracing::warn!("PDF page {page_number} unreadable in {source}: {e}");
                continue;
            }
        };

        let text = extract_page_text(&page, &resolver);
        let trimmed = text.trim().to_owned();

        if !trimmed.is_empty() {
            results.push(LoadedContent {
                text: trimmed,
                metadata: Some(format!("page={page_number}")),
            });
        }
    }

    if results.is_empty() {
        return Err(RagError::LoaderError(format!(
            "No extractable text in PDF: {source}"
        )));
    }

    Ok(results)
}

/// Extract all text from a single PDF page by walking its content stream operations.
/// `resolver` is obtained from `file.resolver()`.
fn extract_page_text(page: &pdf::object::PageRc, resolver: &impl pdf::object::Resolve) -> String {
    let contents = match &page.contents {
        Some(c) => c,
        None => return String::new(),
    };

    let ops = match contents.operations(resolver) {
        Ok(ops) => ops,
        Err(e) => {
            tracing::warn!("Failed to parse PDF page content operations: {e}");
            return String::new();
        }
    };

    let mut buf = String::new();

    for op in &ops {
        match op {
            Op::TextDraw { text } => {
                buf.push_str(&text.to_string_lossy());
            }
            Op::TextDrawAdjusted { array } => {
                for item in array {
                    if let TDA::Text(s) = item {
                        buf.push_str(&s.to_string_lossy());
                    }
                }
            }
            // New-line / position operators — insert a space so words don't run together.
            Op::TextNewline | Op::MoveTextPosition { .. } | Op::SetTextMatrix { .. } => {
                if !buf.ends_with(' ') && !buf.ends_with('\n') {
                    buf.push(' ');
                }
            }
            _ => {}
        }
    }

    buf
}

/// Returns true when the pdf-rs error indicates an encrypted / password-protected file.
fn is_encryption_error(e: &PdfError) -> bool {
    matches!(e, PdfError::InvalidPassword | PdfError::DecryptionFailure)
        || format!("{e}").to_lowercase().contains("encrypt")
        || format!("{e}").to_lowercase().contains("password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extensions() {
        let loader = PdfLoader;
        assert_eq!(loader.extensions(), &["pdf"]);
    }

    #[test]
    fn test_is_encryption_error_invalid_password() {
        assert!(is_encryption_error(&PdfError::InvalidPassword));
    }

    #[test]
    fn test_is_encryption_error_decryption_failure() {
        assert!(is_encryption_error(&PdfError::DecryptionFailure));
    }

    #[test]
    fn test_is_encryption_error_other() {
        // A non-encryption error should return false.
        assert!(!is_encryption_error(&PdfError::EOF));
    }
}
