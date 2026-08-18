<!--
  src/lib/components/governor/ModelList.svelte

  Per-row view of every Ollama-loaded model in the most recent
  Governor snapshot. One row per `loadedModels` entry showing name,
  formatted RAM, idle time, last-used tooltip, and an action area
  (Unload, auto-unload toggle).

  Behaviour:
  - "Unload" → invoke `governor_unload_model({ name, force: false })`.
    On `Err("currently_streaming")` mount `UnloadConfirmModal`; on
    confirm re-invoke with `force: true`. (Req 12.2, 12.3)
  - Auto-unload toggle binds to `auto_unload_per_model[name]` via
    `governor_set_auto_unload_for_model`. (Req 12.4)
  - Errors render inline at the row, never as a global toast.

  Zero hex; every colour from `src/app.css` CSS custom properties.
-->
<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { loadedModels } from '$lib/stores/governor.svelte';
  import UnloadConfirmModal from './UnloadConfirmModal.svelte';
  import type { RunningModel } from '$lib/types/governor';

  // Per-row UI state. Keyed by model name. Re-creating the maps on
  // change is fine — these are tiny and recreated only on user action.
  let unloadingNames = $state<Record<string, boolean>>({});
  let rowErrors = $state<Record<string, string>>({});
  let confirmModalFor = $state<{ name: string; trigger: HTMLElement | null } | null>(null);

  // Per-model auto-unload toggle. Optimistic UI: we flip locally on
  // click, fire the Tauri command, and roll back on failure.
  let autoUnloadOverrides = $state<Record<string, boolean>>({});

  function formatMb(v: number): string {
    if (v >= 1024) return `${(v / 1024).toFixed(1)} GB`;
    return `${v.toFixed(0)} MB`;
  }

  function formatIdle(seconds: number | null | undefined): string {
    if (seconds === null || seconds === undefined) return 'never used';
    if (seconds < 60) return `${seconds}s ago`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
    return `${Math.floor(seconds / 3600)}h ago`;
  }

  function lastUsedTooltip(model: RunningModel): string {
    if (model.idle_seconds === null || model.idle_seconds === undefined) {
      return 'Never streamed in this session.';
    }
    const when = new Date(Date.now() - model.idle_seconds * 1000);
    return `Last used ${when.toLocaleTimeString()}`;
  }

  function isAutoUnloadEnabled(name: string): boolean {
    // Default `true` when no override is set (Req 8.6).
    return autoUnloadOverrides[name] ?? true;
  }

  async function handleUnload(model: RunningModel, ev: MouseEvent) {
    const name = model.name;
    const trigger = (ev.currentTarget as HTMLElement) ?? null;
    delete rowErrors[name];
    rowErrors = { ...rowErrors };
    unloadingNames = { ...unloadingNames, [name]: true };

    try {
      await invoke('governor_unload_model', { name, force: false });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg === 'currently_streaming') {
        confirmModalFor = { name, trigger };
      } else {
        rowErrors = { ...rowErrors, [name]: msg };
      }
    } finally {
      unloadingNames = { ...unloadingNames, [name]: false };
    }
  }

  async function handleConfirmForceUnload() {
    const target = confirmModalFor;
    if (!target) return;
    const name = target.name;
    confirmModalFor = null;
    unloadingNames = { ...unloadingNames, [name]: true };
    try {
      await invoke('governor_unload_model', { name, force: true });
      // Clear any stale row error.
      delete rowErrors[name];
      rowErrors = { ...rowErrors };
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      rowErrors = { ...rowErrors, [name]: msg };
    } finally {
      unloadingNames = { ...unloadingNames, [name]: false };
    }
  }

  function handleCancelConfirm() {
    confirmModalFor = null;
  }

  async function handleToggleAutoUnload(model: RunningModel) {
    const name = model.name;
    const current = isAutoUnloadEnabled(name);
    const next = !current;
    autoUnloadOverrides = { ...autoUnloadOverrides, [name]: next };
    delete rowErrors[name];
    rowErrors = { ...rowErrors };
    try {
      await invoke('governor_set_auto_unload_for_model', {
        name,
        enabled: next,
      });
    } catch (err) {
      // Roll back optimistic update.
      autoUnloadOverrides = { ...autoUnloadOverrides, [name]: current };
      const msg = err instanceof Error ? err.message : String(err);
      rowErrors = { ...rowErrors, [name]: msg };
    }
  }
</script>

<section class="model-list" aria-label="Loaded models">
  <header class="list-header">
    <span class="list-title">Loaded models</span>
    <span class="list-count">{loadedModels().length}</span>
  </header>

  {#if loadedModels().length === 0}
    <p class="empty-hint">No models currently loaded.</p>
  {:else}
    <ul class="rows">
      {#each loadedModels() as model (model.name)}
        <li class="row">
          <div class="row-main">
            <span class="model-name" title={model.name}>{model.name}</span>
            <span class="model-size">{formatMb(model.size_total_mb)}</span>
            <span class="model-idle" title={lastUsedTooltip(model)}>
              {formatIdle(model.idle_seconds)}
            </span>
          </div>

          <div class="row-actions">
            <label class="toggle" title="Auto-unload when idle">
              <input
                type="checkbox"
                checked={isAutoUnloadEnabled(model.name)}
                onchange={() => handleToggleAutoUnload(model)}
              />
              <span class="toggle-label">auto-unload</span>
            </label>

            <button
              type="button"
              class="unload-btn"
              disabled={unloadingNames[model.name] ?? false}
              onclick={(ev) => handleUnload(model, ev)}
            >
              {unloadingNames[model.name] ? 'Unloading…' : 'Unload'}
            </button>
          </div>

          {#if rowErrors[model.name]}
            <p class="row-error" role="alert">
              {rowErrors[model.name]}
            </p>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

{#if confirmModalFor}
  <UnloadConfirmModal
    modelName={confirmModalFor.name}
    onConfirm={handleConfirmForceUnload}
    onCancel={handleCancelConfirm}
    triggerEl={confirmModalFor.trigger}
  />
{/if}

<style>
  .model-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    padding: var(--space-md);
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }

  .list-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: var(--space-xs);
    border-bottom: 0.5px solid var(--border-subtle);
  }

  .list-title {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .list-count {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-ghost);
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-pill);
    padding: 0 var(--space-xs);
    min-width: 18px;
    text-align: center;
  }

  .empty-hint {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
    margin: var(--space-sm) 0 0;
  }

  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .row {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-sm);
  }

  .row-main {
    display: grid;
    grid-template-columns: 1fr auto auto;
    align-items: center;
    gap: var(--space-md);
    min-width: 0;
  }

  .model-name {
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .model-size {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .model-idle {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
  }

  .row-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-md);
  }

  .toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--space-xs);
    cursor: pointer;
    color: var(--text-dim);
    font-family: var(--font-ui);
    font-size: 10px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .toggle input {
    accent-color: var(--gold-primary);
    cursor: pointer;
  }

  .toggle-label {
    color: var(--text-dim);
  }

  .unload-btn {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 3px var(--space-sm);
    border-radius: var(--radius-pill);
    cursor: pointer;
    border: 0.5px solid var(--border-dim);
    background: transparent;
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    transition: color 0.15s, border-color 0.15s, background 0.15s;
  }

  .unload-btn:hover:not(:disabled) {
    border-color: var(--accent-red);
    color: var(--accent-red);
    background: var(--status-danger-bg);
  }

  .unload-btn:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .unload-btn:disabled {
    opacity: 0.5;
    cursor: progress;
  }

  .row-error {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--accent-red);
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    background: var(--status-danger-bg);
    border: 0.5px solid var(--accent-red);
    border-radius: var(--radius-sm);
  }
</style>
