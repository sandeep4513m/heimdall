// src-tauri/src/rag_engine/loaders/folder.rs

use walkdir::WalkDir;

use super::{dispatch_loader, LoadedContent};
use crate::rag_engine::RagError;

const MAX_FILE_SIZE: u64 = 200 * 1024 * 1024; // 200 MB

/// Directory names that are always skipped during folder ingestion.
const SKIP_DIR_NAMES: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
    "coverage",
    "vendor",
];

pub struct FolderLoader;

impl FolderLoader {
    /// Recursively ingest all supported files in a directory.
    ///
    /// Returns `(Vec<LoadedContent>, Vec<String>)` where the second vec
    /// contains per-file error strings in the format `"relative/path: error message"`.
    pub async fn load_folder(
        &self,
        dir: &str,
    ) -> Result<(Vec<LoadedContent>, Vec<String>), RagError> {
        // Validate that dir is an existing directory.
        let dir_path = std::path::Path::new(dir);
        if !dir_path.is_dir() {
            return Err(RagError::LoaderError(format!(
                "Not a directory or does not exist: {}",
                dir
            )));
        }

        let mut all_content: Vec<LoadedContent> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        let walker = WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                // Always allow the root entry itself.
                if entry.depth() == 0 {
                    return true;
                }
                let name = entry.file_name().to_string_lossy();
                // Skip hidden directories (name starts with '.').
                if entry.file_type().is_dir() && name.starts_with('.') {
                    return false;
                }
                // Skip well-known build/dependency directories.
                if entry.file_type().is_dir() && SKIP_DIR_NAMES.contains(&name.as_ref()) {
                    return false;
                }
                true
            });

        for entry_result in walker {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("<walk error>: {}", e));
                    continue;
                }
            };

            // Only process regular files.
            if !entry.file_type().is_file() {
                continue;
            }

            let abs_path = entry.path();
            let rel_path = abs_path
                .strip_prefix(dir_path)
                .unwrap_or(abs_path)
                .to_string_lossy()
                .to_string();

            // Get lowercase extension; skip if missing.
            let has_ext = abs_path.extension().is_some();
            if !has_ext {
                continue; // No extension — skip silently.
            }

            // Build a path string to dispatch.
            let path_str = abs_path.to_string_lossy().to_string();

            // Check if we have a loader for this extension.
            let loader = match dispatch_loader(&path_str) {
                Some(l) => l,
                None => continue, // Unsupported extension — skip silently.
            };

            // Check file size before loading.
            let file_size = match std::fs::metadata(abs_path) {
                Ok(m) => m.len(),
                Err(e) => {
                    errors.push(format!("{}: {}", rel_path, e));
                    continue;
                }
            };

            if file_size > MAX_FILE_SIZE {
                errors.push(format!("{}: File exceeds 200 MB limit", rel_path));
                continue;
            }

            // Load the file.
            match loader.load(&path_str).await {
                Ok(mut chunks) => {
                    // Attach source metadata to each chunk.
                    for chunk in &mut chunks {
                        chunk.metadata = Some(format!("source={}", path_str));
                    }
                    all_content.extend(chunks);
                }
                Err(e) => {
                    errors.push(format!("{}: {}", rel_path, e));
                }
            }
        }

        Ok((all_content, errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        // Create some supported files.
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("config.toml"), "[package]\nname = \"test\"").unwrap();
        fs::write(dir.path().join("readme.txt"), "Hello world").unwrap();
        // Create a hidden directory that should be skipped.
        let hidden = dir.path().join(".git");
        fs::create_dir(&hidden).unwrap();
        fs::write(hidden.join("config"), "gitconfig").unwrap();
        // Create a node_modules directory that should be skipped.
        let nm = dir.path().join("node_modules");
        fs::create_dir(&nm).unwrap();
        fs::write(nm.join("index.js"), "module.exports = {}").unwrap();
        dir
    }

    #[tokio::test]
    async fn test_load_folder_basic() {
        let dir = create_test_dir();
        let loader = FolderLoader;
        let (content, errors) = loader
            .load_folder(dir.path().to_str().unwrap())
            .await
            .unwrap();

        // Should have loaded main.rs, config.toml, readme.txt (3 files).
        assert_eq!(content.len(), 3);
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn test_load_folder_skips_hidden_dirs() {
        let dir = create_test_dir();
        let loader = FolderLoader;
        let (content, _errors) = loader
            .load_folder(dir.path().to_str().unwrap())
            .await
            .unwrap();

        // None of the loaded content should come from .git.
        for chunk in &content {
            if let Some(meta) = &chunk.metadata {
                assert!(!meta.contains("/.git/"), "Should not load from .git: {}", meta);
            }
        }
    }

    #[tokio::test]
    async fn test_load_folder_skips_node_modules() {
        let dir = create_test_dir();
        let loader = FolderLoader;
        let (content, _errors) = loader
            .load_folder(dir.path().to_str().unwrap())
            .await
            .unwrap();

        for chunk in &content {
            if let Some(meta) = &chunk.metadata {
                assert!(
                    !meta.contains("node_modules"),
                    "Should not load from node_modules: {}",
                    meta
                );
            }
        }
    }

    #[tokio::test]
    async fn test_load_folder_metadata_set() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("hello.rs"), "fn hello() {}").unwrap();
        let loader = FolderLoader;
        let (content, _errors) = loader
            .load_folder(dir.path().to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(content.len(), 1);
        let meta = content[0].metadata.as_ref().unwrap();
        assert!(meta.starts_with("source="));
        assert!(meta.contains("hello.rs"));
    }

    #[tokio::test]
    async fn test_load_folder_not_a_directory() {
        let loader = FolderLoader;
        let result = loader.load_folder("/nonexistent/path/to/dir").await;
        assert!(result.is_err());
        match result {
            Err(RagError::LoaderError(msg)) => {
                assert!(msg.contains("Not a directory"));
            }
            _ => panic!("Expected LoaderError"),
        }
    }

    #[tokio::test]
    async fn test_load_folder_unsupported_extensions_skipped() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("image.png"), b"\x89PNG\r\n").unwrap();
        fs::write(dir.path().join("binary.exe"), b"\x4d\x5a").unwrap();
        fs::write(dir.path().join("code.rs"), "fn main() {}").unwrap();
        let loader = FolderLoader;
        let (content, errors) = loader
            .load_folder(dir.path().to_str().unwrap())
            .await
            .unwrap();

        // Only code.rs should be loaded.
        assert_eq!(content.len(), 1);
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn test_load_folder_partial_errors_still_returns_ok() {
        let dir = TempDir::new().unwrap();
        // Write a valid file.
        fs::write(dir.path().join("valid.rs"), "fn main() {}").unwrap();
        // Write a file with invalid UTF-8 but .rs extension.
        fs::write(dir.path().join("invalid.rs"), b"\xff\xfe invalid utf8 \x80\x81").unwrap();
        let loader = FolderLoader;
        let (content, errors) = loader
            .load_folder(dir.path().to_str().unwrap())
            .await
            .unwrap();

        // valid.rs should load, invalid.rs should produce an error.
        assert_eq!(content.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("invalid.rs"));
    }
}
