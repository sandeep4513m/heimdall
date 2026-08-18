// src-tauri/src/rag_engine/loaders/mod.rs

pub mod code;
pub mod docx;
pub mod folder;
pub mod pdf;
pub mod text;
pub mod url;

use super::RagError;

/// Content loaded from a document source.
#[derive(Debug)]
pub struct LoadedContent {
    pub text: String,
    /// Optional metadata (e.g. "page=5" for PDFs, "source=/abs/path" for folder ingestion).
    pub metadata: Option<String>,
}

/// Trait implemented by every document loader.
#[async_trait::async_trait]
pub trait Loader: Send + Sync {
    /// Load content from the given path or URL string.
    async fn load(&self, source: &str) -> Result<Vec<LoadedContent>, RagError>;
    /// File extensions this loader handles (lowercase, without dot).
    fn extensions(&self) -> &[&str];
}

/// Route a file path to the appropriate loader by extension.
/// Returns None for unsupported extensions.
pub fn dispatch_loader(path: &str) -> Option<Box<dyn Loader>> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("txt") | Some("md") => Some(Box::new(text::TextLoader)),
        Some("rs")
        | Some("py")
        | Some("ts")
        | Some("tsx")
        | Some("js")
        | Some("jsx")
        | Some("mjs")
        | Some("cjs")
        | Some("go")
        | Some("c")
        | Some("cpp")
        | Some("h")
        | Some("java")
        | Some("rb")
        | Some("sh")
        | Some("toml")
        | Some("yaml")
        | Some("yml")
        | Some("json")
        | Some("html")
        | Some("css") => Some(Box::new(code::CodeLoader)),
        Some("pdf") => Some(Box::new(pdf::PdfLoader)),
        Some("docx") => Some(Box::new(docx::DocxLoader)),
        _ => None,
    }
}

/// What kind of source the ingestion worker is processing.
///
/// Used by `dispatch_source` to give the worker a uniform return type that
/// routes URL ingestion through `UrlLoader`, directory ingestion through
/// `FolderLoader::load_folder`, and single-file ingestion through the
/// extension-keyed `dispatch_loader` chain.
pub enum SourceKind {
    /// HTTP/HTTPS URL — use `UrlLoader::load`.
    Url(Box<dyn Loader>),
    /// Single file — use the contained `Loader::load`.
    File(Box<dyn Loader>),
    /// Directory — use `FolderLoader::load_folder` to walk and dispatch per-file.
    Folder,
    /// Unrecognised. Worker will skip with a warning.
    Unsupported,
}

/// Classify a source string and return how the worker should handle it.
///
/// - `http://` or `https://` → `Url(UrlLoader)`
/// - existing directory      → `Folder`
/// - existing/non-existing path with a known extension → `File(<loader>)`
/// - anything else           → `Unsupported`
///
/// File-existence checks for the directory branch are intentional:
/// a string like "/some/file.pdf" is not a directory, so even if it
/// doesn't exist on disk, we'll route it through the file loader and let
/// the loader produce a clean "file not found" error rather than guessing.
pub fn dispatch_source(source: &str) -> SourceKind {
    if source.starts_with("http://") || source.starts_with("https://") {
        return SourceKind::Url(Box::new(url::UrlLoader));
    }

    let path = std::path::Path::new(source);
    if path.is_dir() {
        return SourceKind::Folder;
    }

    match dispatch_loader(source) {
        Some(loader) => SourceKind::File(loader),
        None => SourceKind::Unsupported,
    }
}
