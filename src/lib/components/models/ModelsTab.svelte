<!--
  src/lib/components/models/ModelsTab.svelte

  Phase 6 Models tab — lists every locally-available Ollama model with
  capability badges, size, family, parameter size, quantization, last
  used time, and hardware-aware recommendation. Mounts the `PullPanel`
  at the top so the user can browse / pull / type a model name without
  leaving the tab.

  Behaviour:
  - Refreshes on mount via `modelsStore.refresh()`. Subsequent refreshes
    happen on post-pull-success and post-delete-success — never on a
    Governor metrics tick (Req 13.9).
  - Client-side filter is a case-insensitive substring match on the
    model name. Whitespace-only input is treated as no filter.
  - "Currently loaded" indicator cross-refs `governorStore.metrics.loaded_models`
    so the marker stays live without triggering a list refetch (Req 13.3).
  - `lastError` from the store renders inline above the list — never
    as a global toast.
  - Loading skeleton until the first refresh lands so the list does
    not flash empty.

  Zero hex; every colour from `src/app.css` CSS custom properties.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { modelsStore } from '$lib/stores/models.svelte';
  import { governorStore } from '$lib/stores/governor.svelte';
  import ModelRow from './ModelRow.svelte';
  import PullPanel from './PullPanel.svelte';
  import type { ModelsTabRow } from '$lib/types/governor';

  let filterInput = $state<string>('');

  // Currently-loaded names cross-ref. Reading `governorStore.metrics`
  // directly inside `$derived` keeps reactivity granular — this slice
  // re-runs only when the metrics snapshot changes, not on every store
  // mutation elsewhere.
  let loadedNames = $derived<Set<string>>(
    new Set((governorStore.metrics?.loaded_models ?? []).map((m) => m.name)),
  );

  // Whitespace-only input → no filter (Req 13.4 implies a sensible
  // default — empty trimmed query reveals everything).
  let activeQuery = $derived(filterInput.trim().toLowerCase());

  let visibleRows = $derived<ModelsTabRow[]>(
    activeQuery.length === 0
      ? modelsStore.rows
      : modelsStore.rows.filter((r) => r.name.toLowerCase().includes(activeQuery)),
  );

  onMount(() => {
    void modelsStore.refresh();
  });
</script>

<div class="models-tab">
  <header class="panel-header">
    <h2 class="panel-title">Models</h2>
    <span class="row-count" aria-label="Total models">
      {modelsStore.rows.length}
    </span>
  </header>

  <!-- Pull panel: catalog + free-form pull. Mounted inline at the top
       so the user can pull without leaving the tab. -->
  <PullPanel />

  <!-- Filter row (substring match on name, case-insensitive). -->
  <div class="filter-row">
    <input
      type="search"
      class="filter-input"
      placeholder="Filter models…"
      bind:value={filterInput}
      aria-label="Filter models by name"
    />
    {#if filterInput}
      <button
        type="button"
        class="filter-clear"
        onclick={() => (filterInput = '')}
        title="Clear filter"
        aria-label="Clear filter"
      >×</button>
    {/if}
  </div>

  {#if modelsStore.lastError}
    <p class="error-banner" role="alert">
      {modelsStore.lastError}
    </p>
  {/if}

  {#if modelsStore.isLoading && modelsStore.rows.length === 0}
    <p class="skeleton">Loading installed models…</p>
  {:else if visibleRows.length === 0 && modelsStore.rows.length > 0}
    <p class="empty-hint">No models match "{filterInput}".</p>
  {:else if modelsStore.rows.length === 0}
    <p class="empty-hint">
      No Ollama models installed yet. Use the catalog above to pull one,
      or type a model name directly.
    </p>
  {:else}
    <ul class="rows" aria-label="Installed models">
      {#each visibleRows as row (row.digest || row.name)}
        <li>
          <ModelRow {row} loaded={loadedNames.has(row.name)} />
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .models-tab {
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
    height: 100%;
    overflow-y: auto;
    background: var(--bg-app);
    padding: var(--space-md) var(--space-lg);
  }

  .panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-shrink: 0;
  }

  .panel-title {
    font-family: var(--font-brand);
    font-size: 14px;
    font-weight: 600;
    letter-spacing: 0.08em;
    color: var(--gold-primary);
    margin: 0;
  }

  .row-count {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-ghost);
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-pill);
    padding: 0 var(--space-sm);
    min-width: 22px;
    text-align: center;
  }

  .filter-row {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .filter-input {
    flex: 1;
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-dim);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-family: var(--font-ui);
    font-size: 12px;
    padding: 4px var(--space-sm);
    outline: none;
    transition: border-color 0.1s;
  }

  .filter-input::placeholder {
    color: var(--text-ghost);
  }

  .filter-input:focus {
    border-color: var(--gold-dim);
  }

  .filter-clear {
    font-family: var(--font-ui);
    font-size: 14px;
    line-height: 1;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    border: 0.5px solid var(--border-subtle);
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 0.1s, border-color 0.1s, background 0.1s;
  }

  .filter-clear:hover {
    color: var(--accent-red);
    border-color: var(--accent-red);
    background: var(--status-danger-bg);
  }

  .error-banner {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--accent-red);
    margin: 0;
    padding: var(--space-sm) var(--space-md);
    background: var(--status-danger-bg);
    border: 0.5px solid var(--accent-red);
    border-radius: var(--radius-sm);
  }

  .skeleton {
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-dim);
    padding: var(--space-xl);
    text-align: center;
    background: var(--bg-surface);
    border: 0.5px dashed var(--border-subtle);
    border-radius: var(--radius-md);
    margin: 0;
  }

  .empty-hint {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
    margin: 0;
    padding: var(--space-md);
    text-align: center;
    line-height: 1.6;
  }

  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }
</style>
