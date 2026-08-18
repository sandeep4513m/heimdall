// src/lib/stores/models.svelte.ts
//
// Svelte 5 runes store backing the Phase 6 Models tab. One `$state`
// slot for the current `ModelsTabRow[]` snapshot plus a per-model
// pull-progress map keyed by model name. Refreshes are explicit:
// on mount, on post-pull-success, and on post-delete-success — and
// **never** on a Governor metrics tick (Req 13.9). The "currently
// loaded" indicator (Req 13.3) is computed via `$derived` from
// `governorStore.loadedModels` instead of triggering a refetch.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { ModelsTabRow, PullProgressEvent } from '$lib/types/governor';
import { governorStore } from './governor.svelte';

class ModelsStore {
  /// Latest list returned by `models_tab_list`. Empty until the first
  /// successful refresh.
  rows = $state<ModelsTabRow[]>([]);

  /// In-flight pull progress keyed by model name. Entries arrive via
  /// `model://pull-progress` and are cleared on completion or by an
  /// explicit `markPullDone(name)` call.
  pullProgress = $state<Record<string, PullProgressEvent>>({});

  /// Latest error from `refresh()`. Rendered inline by `ModelsTab` —
  /// never as a global toast.
  lastError = $state<string | null>(null);

  /// Set true while `refresh()` is in flight so the UI can render a
  /// non-jarring placeholder instead of a flash-empty list.
  isLoading = $state<boolean>(false);

  private unlistenPull: UnlistenFn | null = null;

  /// Names that the user has just kicked off a pull for. Tracked
  /// independently of `pullProgress` so a row can render the "pulling"
  /// hint immediately, before Ollama has emitted its first event.
  private pendingPulls = new Set<string>();

  /// Index `rows` by name for `loadedNameSet` reactivity. Recomputed
  /// implicitly by `$derived` consumers.
  get rowsByName(): Map<string, ModelsTabRow> {
    const m = new Map<string, ModelsTabRow>();
    for (const r of this.rows) m.set(r.name, r);
    return m;
  }

  /// Set of currently-loaded model names, derived from the Governor
  /// store. Reading this never triggers a `models_tab_list` refetch.
  get currentlyLoadedNames(): Set<string> {
    return new Set(
      (governorStore.metrics?.loaded_models ?? []).map((m) => m.name),
    );
  }

  /// Subscribe to `model://pull-progress`. Idempotent.
  async startListening(): Promise<void> {
    if (this.unlistenPull) return;
    this.unlistenPull = await listen<PullProgressEvent>(
      'model://pull-progress',
      (e) => {
        const p = e.payload;
        if (!p?.model) return;
        // Reactivity: Svelte 5 records the assignment to the property.
        this.pullProgress = { ...this.pullProgress, [p.model]: p };
      },
    );
  }

  stopListening(): void {
    if (this.unlistenPull) {
      try {
        this.unlistenPull();
      } catch {
        /* already removed */
      }
      this.unlistenPull = null;
    }
  }

  /// Refresh the full list from the backend. Called on mount, after a
  /// successful pull, and after a successful delete.
  async refresh(): Promise<void> {
    this.isLoading = true;
    try {
      this.rows = await invoke<ModelsTabRow[]>('models_tab_list');
      this.lastError = null;
    } catch (err) {
      this.lastError = err instanceof Error ? err.message : String(err);
    } finally {
      this.isLoading = false;
    }
  }

  /// Optimistically mark a pull as kicked off so the row UI updates
  /// before the first `pull-progress` event arrives.
  markPullStarted(name: string): void {
    this.pendingPulls.add(name);
    // Touch `pullProgress` to trigger a re-render of any consumer that
    // reads it.
    this.pullProgress = { ...this.pullProgress };
  }

  /// Called when a pull finishes (success or error). Clears the
  /// per-model progress entry and refreshes the list so the new model
  /// appears with capabilities filled in.
  async markPullDone(name: string): Promise<void> {
    this.pendingPulls.delete(name);
    const next = { ...this.pullProgress };
    delete next[name];
    this.pullProgress = next;
    await this.refresh();
  }

  /// True when a pull for `name` is in flight (kicked off locally OR
  /// observed via the progress event from another window).
  isPulling(name: string): boolean {
    return this.pendingPulls.has(name) || name in this.pullProgress;
  }

  /// Refresh after a successful delete. The backend is the source of
  /// truth — we never mutate `rows` locally and let the refresh fill
  /// in the new state.
  async markDeleted(_name: string): Promise<void> {
    await this.refresh();
  }
}

export const modelsStore = new ModelsStore();

/// True when the named model is currently loaded according to the
/// Governor's most recent `/api/ps` snapshot. Components consume this
/// rather than the raw `governorStore.loadedModels` array so a re-load
/// does not invalidate every row's reactivity context.
export function isCurrentlyLoaded(name: string): boolean {
  const loaded = governorStore.metrics?.loaded_models ?? [];
  return loaded.some((m) => m.name === name);
}
