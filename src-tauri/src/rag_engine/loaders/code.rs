// src-tauri/src/rag_engine/loaders/code.rs

use super::{LoadedContent, Loader};
use crate::rag_engine::RagError;

const MAX_FILE_SIZE: u64 = 200 * 1024 * 1024; // 200 MB

pub struct CodeLoader;

#[async_trait::async_trait]
impl Loader for CodeLoader {
    async fn load(&self, source: &str) -> Result<Vec<LoadedContent>, RagError> {
        let metadata = tokio::fs::metadata(source).await?;
        let file_size = metadata.len();

        if file_size > MAX_FILE_SIZE {
            return Err(RagError::LoaderError(format!(
                "File exceeds 200 MB limit: {}",
                source
            )));
        }

        let content = tokio::fs::read_to_string(source).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                RagError::LoaderError(format!("UTF-8 decode error: {source}"))
            } else {
                RagError::IoError(e)
            }
        })?;

        Ok(vec![LoadedContent {
            text: content,
            metadata: None,
        }])
    }

    fn extensions(&self) -> &[&str] {
        &[
            "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "c", "cpp", "h", "java",
            "rb", "sh", "toml", "yaml", "yml", "json", "html", "css",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_load_rust_file() {
        let mut f = NamedTempFile::with_suffix(".rs").unwrap();
        writeln!(f, "fn main() {{ println!(\"hello\"); }}").unwrap();
        let loader = CodeLoader;
        let result = loader.load(f.path().to_str().unwrap()).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].text.contains("fn main"));
        assert!(result[0].metadata.is_none());
    }

    #[tokio::test]
    async fn test_load_json_file() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        writeln!(f, r#"{{"key": "value"}}"#).unwrap();
        let loader = CodeLoader;
        let result = loader.load(f.path().to_str().unwrap()).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].text.contains("key"));
    }

    #[tokio::test]
    async fn test_load_nonexistent_file() {
        let loader = CodeLoader;
        let result = loader.load("/nonexistent/path/file.rs").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_extensions() {
        let loader = CodeLoader;
        let exts = loader.extensions();
        assert!(exts.contains(&"rs"));
        assert!(exts.contains(&"py"));
        assert!(exts.contains(&"ts"));
        assert!(exts.contains(&"json"));
        assert!(exts.contains(&"yaml"));
    }
}
