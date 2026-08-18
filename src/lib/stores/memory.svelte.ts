// src/lib/stores/memory.svelte.ts
//
// Svelte 5 runes store for the Phase 5 Memory System.
// Mirrors the pattern from rag.svelte.ts.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { MemoryFact, MemorySettings, ExtractionResult, ExtractionCompleteEvent } from '$lib/types/memory';

const DEFAULT_SETTINGS: MemorySettings = {
  global_enabled: true,
  decay_threshold_days: 90,
  fact_count: 0,
  episode_count: 0,
};

class MemoryStore {
  facts = $state<MemoryFact[]>([]);
  settings = $state<MemorySettings>({ ...DEFAULT_SETTINGS });
  isExtracting = $state<boolean>(false);
  /// Set to true when new pending facts arrive — ChatPanel reads this to show a notification.
  hasNewPendingFacts = $state<boolean>(false);
  /// Set when the last extraction run failed (not skipped — actually errored).
  lastExtractionError = $state<string | null>(null);
  /// True when extraction ran successfully but found zero facts (not an error, not skipped).
  lastExtractionWasEmpty = $state<boolean>(false);

  private unlistenExtraction: UnlistenFn | null = null;

  get pendingFacts(): MemoryFact[] {
    return this.facts.filter((f) => !f.confirmed_by_user);
  }

  get confirmedFacts(): MemoryFact[] {
    return this.facts.filter((f) => f.confirmed_by_user);
  }

  get factCount(): number {
    return this.confirmedFacts.length;
  }

  get episodeCount(): number {
    return this.settings.episode_count;
  }

  /// Whether the soft warning threshold (150) has been reached.
  get showSoftWarning(): boolean {
    return this.factCount >= 150 && this.factCount < 200;
  }

  /// Whether the hard cap (200) has been reached.
  get atHardCap(): boolean {
    return this.factCount >= 200;
  }

  /// Start listening for extraction_complete events from the backend.
  /// Call this once from a component that stays mounted (e.g., ChatPanel onMount).
  async startListening(): Promise<void> {
    if (this.unlistenExtraction) return; // already listening
    this.unlistenExtraction = await listen<ExtractionCompleteEvent>(
      'memory://extraction_complete',
      async (event) => {
        const p = event.payload;

        // Clear any previous error/empty markers on each new extraction event.
        this.lastExtractionError = null;
        this.lastExtractionWasEmpty = false;

        if (p.facts_count > 0) {
          await this.loadFacts();
          await this.loadSettings();
          this.hasNewPendingFacts = true;
        } else if (p.episode_created) {
          await this.loadSettings();
        }

        // Skipped runs (memory disabled, < 4 user messages, etc.) are
        // structurally normal — never surface anything.
        if (p.skipped_reason) {
          return;
        }

        // Failure routing.
        //
        // Catastrophic failures keep the red banner — the user can act on
        // these (start Ollama, install a model, free disk). Parse-quality
        // failures (model returned malformed output, validation dropped
        // every candidate) route to the calm "no new facts" hint instead.
        // The user did nothing wrong; nothing to alarm them about.
        const errorText = (p.extraction_error || p.episode_error || '').toLowerCase();
        const isCatastrophic =
          errorText.includes('ollama') ||
          errorText.includes('connection') ||
          errorText.includes('reachable') ||
          errorText.includes('http') ||
          errorText.includes('timed out') ||
          errorText.includes('timeout') ||
          errorText.includes('no chat-capable model') ||
          errorText.includes('disk') ||
          errorText.includes('database') ||
          errorText.includes('sqlite') ||
          errorText.includes('embedding model unavailable');

        if (isCatastrophic && p.extraction_error) {
          this.lastExtractionError = p.extraction_error;
          return;
        }
        if (isCatastrophic && p.episode_error && p.facts_count === 0) {
          this.lastExtractionError = p.episode_error;
          return;
        }

        // Non-catastrophic, no facts, no episode → calm "found nothing" hint.
        if (p.facts_count === 0 && !p.episode_created) {
          this.lastExtractionWasEmpty = true;
        }
      },
    );
  }

  /// Stop listening. Call from onDestroy if needed.
  stopListening(): void {
    if (this.unlistenExtraction) {
      this.unlistenExtraction();
      this.unlistenExtraction = null;
    }
  }

  /// Acknowledge the notification (user saw it or navigated to Memory panel).
  dismissNewFacts(): void {
    this.hasNewPendingFacts = false;
  }

  dismissExtractionError(): void {
    this.lastExtractionError = null;
  }

  dismissExtractionEmpty(): void {
    this.lastExtractionWasEmpty = false;
  }

  async loadFacts(): Promise<void> {
    try {
      this.facts = await invoke<MemoryFact[]>('memory_list_facts');
      // If pending facts exist (carried over from a previous session, or
      // surfaced by an action other than a live extraction event), make
      // sure the ChatPanel banner reappears so the user is not left in
      // the dark on app restart.
      if (this.pendingFacts.length > 0) {
        this.hasNewPendingFacts = true;
      }
    } catch {
      this.facts = [];
    }
  }

  async loadSettings(): Promise<void> {
    try {
      this.settings = await invoke<MemorySettings>('memory_get_settings');
    } catch {
      // Keep defaults
    }
  }

  async confirmFact(id: string): Promise<void> {
    await invoke('memory_confirm_fact', { id });
    await this.loadFacts();
    await this.loadSettings();
  }

  async confirmAll(ids: string[]): Promise<void> {
    await invoke('memory_confirm_all', { ids });
    await this.loadFacts();
    await this.loadSettings();
  }

  async rejectFact(id: string): Promise<void> {
    await invoke('memory_reject_fact', { id });
    await this.loadFacts();
    await this.loadSettings();
  }

  async rejectAll(ids: string[]): Promise<void> {
    await invoke('memory_reject_all', { ids });
    await this.loadFacts();
    await this.loadSettings();
  }

  async editFact(id: string, text: string): Promise<void> {
    await invoke('memory_edit_fact', { id, text });
    await this.loadFacts();
  }

  async deleteFact(id: string): Promise<void> {
    await invoke('memory_delete_fact', { id });
    await this.loadFacts();
    await this.loadSettings();
  }

  async deleteAllFacts(): Promise<void> {
    await invoke('memory_delete_all_facts');
    await this.loadFacts();
    await this.loadSettings();
  }

  async deleteAllEpisodes(): Promise<void> {
    await invoke('memory_delete_all_episodes');
    await this.loadSettings();
  }

  async exportFacts(): Promise<string> {
    return await invoke<string>('memory_export_facts');
  }

  async updateSettings(settings: MemorySettings): Promise<void> {
    await invoke('memory_update_settings', { settings });
    await this.loadSettings();
  }

  async setConversationMemory(convId: string, enabled: boolean): Promise<void> {
    await invoke('memory_set_conversation_memory', { convId, enabled });
  }

  async getConversationMemory(convId: string): Promise<boolean> {
    try {
      return await invoke<boolean>('memory_get_conversation_memory', { convId });
    } catch {
      return true;
    }
  }

  async extract(conversationId: string): Promise<ExtractionResult | null> {
    this.isExtracting = true;
    try {
      const result = await invoke<ExtractionResult>('memory_extract', { conversationId });
      await this.loadFacts();
      await this.loadSettings();
      return result;
    } catch {
      return null;
    } finally {
      this.isExtracting = false;
    }
  }
}

export const memoryStore = new MemoryStore();
