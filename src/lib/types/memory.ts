// src/lib/types/memory.ts
//
// Frontend-side mirrors of the Rust memory types in src-tauri/src/models.rs.
// Keep these in lock-step with the backend.

/// A memory fact — confirmed or pending.
export interface MemoryFact {
  id: string;
  fact: string;
  source_conversation_id: string | null;
  confirmed_by_user: boolean;
  created_at: number;
  dedup_status: string | null;       // 'new' | 'possible_update' | 'duplicate'
  conflict_with_id: string | null;
  update_hint_id: string | null;
  batch_id: string | null;
}

/// Memory system settings.
export interface MemorySettings {
  global_enabled: boolean;
  decay_threshold_days: number;
  fact_count: number;
  episode_count: number;
}

/// Result of a memory extraction pass.
export interface ExtractionResult {
  facts_extracted: CandidateFact[];
  episode_created: boolean;
  skipped_reason: string | null;
  extraction_error: string | null;
  episode_error: string | null;
}

/// A candidate fact produced by the extraction engine.
export interface CandidateFact {
  id: string;
  text: string;
  dedup_status: string; // 'new' | 'possible_update' | 'duplicate'
  conflict_with: string | null;
}

/// Event emitted when extraction completes for a conversation.
export interface ExtractionCompleteEvent {
  conversation_id: string;
  facts_count: number;
  episode_created: boolean;
  extraction_error: string | null;
  episode_error: string | null;
  skipped_reason: string | null;
}

/// Event emitted by chat_stream when memory context is injected for a turn.
/// The frontend uses this to attach a "Memory used" badge to the assistant
/// response that comes from this turn.
export interface MemoryUsedEvent {
  conversation_id: string;
  memory_text: string;
  num_ctx: number;
}
