// src-tauri/src/rag_engine/loaders/docx.rs
//
// DocxLoader — extracts plain text from .docx files using docx-rs.
//
// Extraction rules:
//   - Paragraphs: use Paragraph::raw_text() which concatenates all text runs.
//   - Tables: row-major order; cells separated by '\t', rows separated by '\n'.
//   - List items: prefix with "- " (bullets) or "N. " (numbered) when the
//     paragraph has numbering metadata.
//   - Hyperlinks: visible text only (raw_text already handles this).
//   - Skipped: headers, footers, images, comments, tracked-change deletions.
//   - Blocks joined with "\n\n".
//
// Limits:
//   - Rejects legacy .doc (binary) with a descriptive error.
//   - Rejects files > 200 MB.

use super::{LoadedContent, Loader};
use crate::rag_engine::RagError;

const MAX_BYTES: usize = 200 * 1024 * 1024; // 200 MB

pub struct DocxLoader;

#[async_trait::async_trait]
impl Loader for DocxLoader {
    async fn load(&self, source: &str) -> Result<Vec<LoadedContent>, RagError> {
        // Reject legacy .doc (not .docx) — case-insensitive suffix check.
        let lower = source.to_lowercase();
        if lower.ends_with(".doc") && !lower.ends_with(".docx") {
            return Err(RagError::LoaderError(format!(
                "Legacy .doc format not supported; please save as .docx: {source}"
            )));
        }

        // Read file bytes.
        let bytes = tokio::fs::read(source)
            .await
            .map_err(|e| RagError::LoaderError(format!("Failed to read {source}: {e}")))?;

        // Reject oversized files.
        if bytes.len() > MAX_BYTES {
            return Err(RagError::LoaderError(format!(
                "File too large ({} bytes > 200 MB limit): {source}",
                bytes.len()
            )));
        }

        // Parse the DOCX package.
        let docx = docx_rs::read_docx(&bytes)
            .map_err(|e| RagError::LoaderError(format!("DOCX parse error in {source}: {e}")))?;

        // Walk document children and collect text blocks.
        let mut blocks: Vec<String> = Vec::new();

        for child in &docx.document.children {
            match child {
                docx_rs::DocumentChild::Paragraph(para) => {
                    let block = extract_paragraph_text(para);
                    if !block.is_empty() {
                        blocks.push(block);
                    }
                }
                docx_rs::DocumentChild::Table(table) => {
                    let block = extract_table_text(table);
                    if !block.is_empty() {
                        blocks.push(block);
                    }
                }
                // Skip: BookmarkStart/End, CommentStart/End, StructuredDataTag,
                // TableOfContents, Section — none carry body text we need.
                _ => {}
            }
        }

        let text = blocks.join("\n\n");
        Ok(vec![LoadedContent {
            text,
            metadata: None,
        }])
    }

    fn extensions(&self) -> &[&str] {
        &["docx"]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract plain text from a paragraph, prepending a list marker when the
/// paragraph has numbering (list item).
fn extract_paragraph_text(para: &docx_rs::Paragraph) -> String {
    let raw = para.raw_text();
    if raw.trim().is_empty() {
        return String::new();
    }

    // Detect list items via the paragraph's numbering property.
    if para.has_numbering {
        // docx-rs exposes numbering as a method on ParagraphProperty.
        // We emit "- " for all list items as a pragmatic fallback since
        // resolving the abstract numbering definition (bullet vs numbered)
        // requires a full lookup through the Numberings table.
        return format!("- {}", raw.trim_end());
    }

    raw
}

/// Extract plain text from a table in row-major order.
/// Cells within a row are separated by '\t'; rows are separated by '\n'.
fn extract_table_text(table: &docx_rs::Table) -> String {
    let mut rows: Vec<String> = Vec::new();

    for row_child in &table.rows {
        let docx_rs::TableChild::TableRow(row) = row_child;
        let mut cells: Vec<String> = Vec::new();

        for cell_child in &row.cells {
            let docx_rs::TableRowChild::TableCell(cell) = cell_child;
            let cell_text = extract_cell_text(cell);
            cells.push(cell_text);
        }

        rows.push(cells.join("\t"));
    }

    rows.join("\n")
}

/// Concatenate all paragraph text within a table cell (paragraphs separated
/// by a single space to avoid losing word boundaries).
fn extract_cell_text(cell: &docx_rs::TableCell) -> String {
    let mut parts: Vec<String> = Vec::new();

    for content in &cell.children {
        match content {
            docx_rs::TableCellContent::Paragraph(para) => {
                let t = para.raw_text();
                if !t.trim().is_empty() {
                    parts.push(t);
                }
            }
            // Nested tables: recurse.
            docx_rs::TableCellContent::Table(nested) => {
                let t = extract_table_text(nested);
                if !t.is_empty() {
                    parts.push(t);
                }
            }
            _ => {}
        }
    }

    parts.join(" ")
}
