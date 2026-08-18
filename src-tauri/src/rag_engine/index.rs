/// index.rs — usearch VectorIndex wrapper
///
/// Wraps `usearch::Index` with a stable, caller-friendly API:
///   - `open(path, dimensions, quantization, mmap)` — read path; honours mmap
///   - `open_writable(path, dimensions, quantization)` — write path; never mmap
///   - `add(vector)` — append a vector, returns its auto-incremented u64 key
///   - `search(query, k)` — cosine kNN, returns (key, score) sorted DESC
///   - `save()` — persist to the path the index was opened from
///   - `remove(id)` — delete a vector by key
///   - `len()` — number of vectors currently in the index
///   - `dimensions()` — configured dimensionality
///
/// The mmap flag controls whether the index is memory-mapped (low RSS, good
/// for Tier 1/2) or fully loaded into RAM (fast search, good for Tier 3).
///
/// IMPORTANT: mmap'd indexes (`Index::restore_view`) are READ-ONLY. Calling
/// `add` or `remove` on a mmap'd index fails with usearch's "Can't add to an
/// immutable index" error. All write paths must go through `open_writable`,
/// which ignores the mmap flag and always loads the index fully into RAM.
///
/// Cosine similarity is used for all indexes (matches nomic-embed-text output).
/// Distances returned by usearch for cosine are in [0, 2] where 0 = identical.
/// We convert to similarity scores in [0, 1] via `score = 1.0 - distance / 2.0`
/// so callers always receive higher-is-better values.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::models;
use super::RagError;

/// A wrapper around a `usearch::Index` that manages its own key counter and
/// file path, and exposes a simplified API for the RAG engine.
pub struct VectorIndex {
    /// The underlying usearch index.
    inner: Index,
    /// Path the index was opened from / will be saved to.
    path: PathBuf,
    /// Monotonically increasing key counter. Starts at the current index size
    /// when reopening an existing file so keys never collide.
    next_key: AtomicU64,
    /// Configured dimensionality (cached to avoid FFI round-trips).
    dims: usize,
}

impl VectorIndex {
    /// Open or create a `VectorIndex` at `path`.
    ///
    /// - If the file exists, it is reopened (mmap or load depending on `mmap`).
    /// - If the file does not exist, a new empty index is created.
    ///
    /// # Arguments
    /// * `path`         — File path for the `.usearch` index file.
    /// * `dimensions`   — Number of dimensions per vector (e.g. 768).
    /// * `quantization` — `models::ScalarKind::F16` or `F32`.
    /// * `mmap`         — If `true`, use memory-mapped I/O (low RSS).
    ///                    If `false`, load fully into RAM (faster search).
    pub fn open(
        path: impl AsRef<Path>,
        dimensions: usize,
        quantization: models::ScalarKind,
        mmap: bool,
    ) -> Result<Self, RagError> {
        Self::open_inner(path, dimensions, quantization, mmap)
    }

    /// Open or create a `VectorIndex` for **writing**.
    ///
    /// Never uses memory-mapped I/O — mmap'd usearch indexes are read-only
    /// and `add` / `remove` calls on them fail with "Can't add to an
    /// immutable index". Use this constructor on every code path that calls
    /// `add`, `remove`, or `save` after a mutation.
    ///
    /// Cost: the entire index file is loaded into RAM. For RAG collections
    /// this is the same as `open(.., mmap=false)`. For the `_memories`
    /// episode index it is bounded (≤200 vectors × 768 × 4 bytes ≈ 600 KB).
    pub fn open_writable(
        path: impl AsRef<Path>,
        dimensions: usize,
        quantization: models::ScalarKind,
    ) -> Result<Self, RagError> {
        Self::open_inner(path, dimensions, quantization, false)
    }

    fn open_inner(
        path: impl AsRef<Path>,
        dimensions: usize,
        quantization: models::ScalarKind,
        mmap: bool,
    ) -> Result<Self, RagError> {
        let path = path.as_ref().to_path_buf();

        let usearch_quant = match quantization {
            models::ScalarKind::F16 => ScalarKind::F16,
            models::ScalarKind::F32 => ScalarKind::F32,
        };

        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: usearch_quant,
            connectivity: 0,       // auto
            expansion_add: 0,      // auto
            expansion_search: 0,   // auto
            multi: false,
        };

        if path.exists() {
            // Reopen existing index.
            let path_str = path
                .to_str()
                .ok_or_else(|| RagError::IndexError("Index path is not valid UTF-8".into()))?;

            let index = if mmap {
                Index::restore_view(path_str)
                    .map_err(|e| RagError::IndexError(format!("Failed to mmap index: {e}")))?
            } else {
                Index::restore(path_str)
                    .map_err(|e| RagError::IndexError(format!("Failed to load index: {e}")))?
            };

            // Validate that the on-disk index matches the requested dimensions.
            let on_disk_dims = index.dimensions();
            if on_disk_dims != dimensions {
                return Err(RagError::CollectionUnavailable(format!(
                    "Dimension mismatch: index has {on_disk_dims} dims, requested {dimensions}"
                )));
            }

            let size = index.size();
            Ok(Self {
                inner: index,
                path,
                next_key: AtomicU64::new(size as u64),
                dims: dimensions,
            })
        } else {
            // Create a new empty index.
            let index = Index::new(&options)
                .map_err(|e| RagError::IndexError(format!("Failed to create index: {e}")))?;

            // Reserve a modest initial capacity so the first few adds don't
            // trigger repeated reallocations.
            index
                .reserve(1024)
                .map_err(|e| RagError::IndexError(format!("Failed to reserve capacity: {e}")))?;

            Ok(Self {
                inner: index,
                path,
                next_key: AtomicU64::new(0),
                dims: dimensions,
            })
        }
    }

    /// Add a vector to the index.
    ///
    /// Returns the internal usearch key assigned to this vector. Keys are
    /// auto-incremented u64 values starting from 0 (or from the existing
    /// index size when reopening).
    pub fn add(&self, vector: &[f32]) -> Result<u64, RagError> {
        if vector.len() != self.dims {
            return Err(RagError::IndexError(format!(
                "Vector has {} dimensions, index expects {}",
                vector.len(),
                self.dims
            )));
        }

        let key = self.next_key.fetch_add(1, Ordering::Relaxed);

        // Grow capacity if needed (usearch panics if capacity is exhausted).
        let current_capacity = self.inner.capacity();
        let current_size = self.inner.size();
        if current_size >= current_capacity.saturating_sub(1) {
            let new_capacity = (current_capacity * 2).max(1024);
            self.inner
                .reserve(new_capacity)
                .map_err(|e| RagError::IndexError(format!("Failed to grow index capacity: {e}")))?;
        }

        self.inner
            .add(key, vector)
            .map_err(|e| RagError::IndexError(format!("Failed to add vector: {e}")))?;

        Ok(key)
    }

    /// Search for the `k` nearest neighbours to `query`.
    ///
    /// Returns a `Vec<(key, score)>` sorted by score **descending** (highest
    /// similarity first). Scores are in `[0.0, 1.0]` where 1.0 = identical.
    ///
    /// usearch cosine distances are in `[0, 2]` (0 = identical vectors).
    /// We convert: `score = 1.0 - distance / 2.0`.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>, RagError> {
        if query.len() != self.dims {
            return Err(RagError::IndexError(format!(
                "Query has {} dimensions, index expects {}",
                query.len(),
                self.dims
            )));
        }

        if self.inner.size() == 0 {
            return Ok(Vec::new());
        }

        let count = k.min(self.inner.size());
        let matches = self
            .inner
            .search(query, count)
            .map_err(|e| RagError::IndexError(format!("Search failed: {e}")))?;

        let mut results: Vec<(u64, f32)> = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .map(|(&key, &dist)| {
                // Convert cosine distance [0, 2] → similarity [0, 1].
                let score = 1.0_f32 - dist / 2.0_f32;
                (key, score)
            })
            .collect();

        // Sort descending by score (highest similarity first).
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(results)
    }

    /// Persist the index to the file path it was opened from.
    pub fn save(&self) -> Result<(), RagError> {
        let path_str = self
            .path
            .to_str()
            .ok_or_else(|| RagError::IndexError("Index path is not valid UTF-8".into()))?;

        self.inner
            .save(path_str)
            .map_err(|e| RagError::IndexError(format!("Failed to save index: {e}")))?;

        Ok(())
    }

    /// Remove a vector by its key.
    ///
    /// Returns `Ok(())` whether or not the key existed (idempotent).
    pub fn remove(&self, id: u64) -> Result<(), RagError> {
        self.inner
            .remove(id)
            .map_err(|e| RagError::IndexError(format!("Failed to remove vector {id}: {e}")))?;
        Ok(())
    }

    /// Number of vectors currently in the index.
    pub fn len(&self) -> usize {
        self.inner.size()
    }

    /// Returns `true` if the index contains no vectors.
    pub fn is_empty(&self) -> bool {
        self.inner.size() == 0
    }

    /// Configured dimensionality of the index.
    pub fn dimensions(&self) -> usize {
        self.dims
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Create a unique temp directory for each test to avoid collisions.
    fn tmp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        // Use a unique subdirectory per test name to avoid cross-test collisions.
        path.push(format!("heimdall_index_test_{}", name));
        std::fs::create_dir_all(&path).ok();
        path.push("index.usearch");
        // Remove any leftover from a previous run.
        std::fs::remove_file(&path).ok();
        path
    }

    /// Create a simple 4-dim f32 index, add two vectors, search, verify results.
    #[test]
    fn create_add_search_basic() {
        let path = tmp_path("basic");

        let idx = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
        assert_eq!(idx.len(), 0);
        assert!(idx.is_empty());

        let v1 = vec![1.0_f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.0_f32, 1.0, 0.0, 0.0];

        let k1 = idx.add(&v1).unwrap();
        let k2 = idx.add(&v2).unwrap();
        assert_eq!(idx.len(), 2);
        assert_ne!(k1, k2);

        // Search with v1 — should return k1 first (score near 1.0).
        let results = idx.search(&v1, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, k1);
        assert!(results[0].1 > 0.99, "Expected score near 1.0, got {}", results[0].1);
    }

    /// Save and reload an index, verify vectors are still searchable.
    #[test]
    fn save_and_reload() {
        let path = tmp_path("reload");

        let vector = vec![0.5_f32, 0.5, 0.5, 0.5];
        let key;

        {
            let idx = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
            key = idx.add(&vector).unwrap();
            idx.save().unwrap();
        }

        // Reopen and search.
        let idx2 = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
        assert_eq!(idx2.len(), 1);
        let results = idx2.search(&vector, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, key);
        assert!(results[0].1 > 0.99);
    }

    /// Dimension mismatch on add returns an error.
    #[test]
    fn add_wrong_dimensions_errors() {
        let path = tmp_path("dims");

        let idx = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
        let bad_vec = vec![1.0_f32, 2.0, 3.0]; // 3 dims, not 4
        assert!(idx.add(&bad_vec).is_err());
    }

    /// Search on empty index returns empty vec (no panic).
    #[test]
    fn search_empty_index() {
        let path = tmp_path("empty");

        let idx = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
        let results = idx.search(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert!(results.is_empty());
    }

    /// Remove a vector and verify it's gone.
    #[test]
    fn remove_vector() {
        let path = tmp_path("remove");

        let idx = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
        let v = vec![1.0_f32, 0.0, 0.0, 0.0];
        let key = idx.add(&v).unwrap();
        assert_eq!(idx.len(), 1);

        idx.remove(key).unwrap();
        assert_eq!(idx.len(), 0);
    }

    /// Reopening with wrong dimensions returns CollectionUnavailable.
    #[test]
    fn reopen_dimension_mismatch_errors() {
        let path = tmp_path("mismatch");

        {
            let idx = VectorIndex::open(&path, 4, models::ScalarKind::F32, false).unwrap();
            idx.add(&[1.0, 0.0, 0.0, 0.0]).unwrap();
            idx.save().unwrap();
        }

        // Try to reopen with 8 dimensions — should fail.
        let result = VectorIndex::open(&path, 8, models::ScalarKind::F32, false);
        assert!(matches!(result, Err(RagError::CollectionUnavailable(_))));
    }
}
