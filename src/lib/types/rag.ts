// src/lib/types/rag.ts
//
// Frontend-side mirrors of the Rust types defined in src-tauri/src/models.rs.
// Keep these in lock-step with the backend.

/// A named knowledge collection.
///
/// `id` is the slug used as the on-disk index filename and as the foreign
/// key in `rag_chunks.collection`. `display_name` is what the user typed.
/// All UI text should use `display_name`; all IPC arguments that flow to
/// retrieval/ingestion should use `display_name` too — the Rust commands
/// slug at the IPC boundary.
export interface Collection {
  id: string;
  display_name: string;
  created_at: number;
  updated_at: number;
  last_ingested_at: number | null;
}

export interface CollectionStats {
  display_name: string;
  chunks: number;
  sources: number;
  last_updated: number | null;
  vector_bytes: number;
}

export interface ChunkPreview {
  chunk_id: string;
  content: string;
  source_path: string;
  chunk_index: number;
  score: number;
}

export interface IngestionJob {
  id: string;
  source_path: string | null;
  collection: string | null;
  // 'pending' | 'running' | 'paused_low_memory' | 'interrupted' | 'cancelled' | 'done' | 'failed'
  status: string | null;
  chunks_total: number;
  chunks_done: number;
  error: string | null;
  created_at: number;
  completed_at: number | null;
}

export interface IngestionProgressEvent {
  job_id: string;
  chunks_done: number;
  chunks_total: number;
  status: string;
  current_file: string;
}

export interface IngestionCompleteEvent {
  job_id: string;
  success: boolean;
  error: string | null;
}
