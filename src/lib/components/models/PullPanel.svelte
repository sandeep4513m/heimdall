<!--
  src/lib/components/models/PullPanel.svelte

  Phase 6 pull affordance — capability filter, curated catalog, and a
  free-form input that bypasses the catalog. Mounted at the top of
  `ModelsTab.svelte`.

  Behaviour:
  - Capability filter: Chat | Vision | Embedding | Thinking. Default
    is `Chat`. Catalog entries are filtered client-side by capability
    plus the user's `effective_tier` (Req 14.1, 14.2).
  - Catalog comes from the `models_catalog_list` Tauri command (loaded
    once on mount). On fetch failure render the literal string
    "Catalog unavailable; type a model name to pull."
  - Empty filtered list renders "No catalog entries match your hardware tier."
  - Each catalog entry is a `<button>` activated by Enter or Space —
    native button semantics handle both keys (a11y).
  - Free-form input: trimmed; non-empty; max 256 bytes; pulls anything
    Ollama can resolve (Req 14.3).
  - Progress is read from `modelsStore.pullProgress[name]`; if a pull
    is already in flight from another window the progress bar reflects
    that (R4 mitigation).
  - On pull start: optimistically `markPullStarted(name)`, invoke
    `pull_model({ modelName })`, then `markPullDone(name)` regardless
    of outcome (the latter triggers a list refresh so the new model
    appears in the table).

  Zero hex; every colour from `src/app.css` CSS custom properties.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { modelsStore } from '$lib/stores/models.svelte';
  import { governorStore } from '$lib/stores/governor.svelte';
  import type { CatalogEntry, HardwareTier } from '$lib/types/governor';

  type CapabilityFilter = 'Chat' | 'Vision' | 'Embedding' | 'Thinking';

  const FILTERS: CapabilityFilter[] = ['Chat', 'Vision', 'Embedding', 'Thinking'];

  // Tag synonyms used by `model_catalog.json`. Keep this list narrow —
  // the catalog is curated, so we only need to recognise the tags it
  // actually emits.
  const FILTER_TAGS: Record<CapabilityFilter, string[]> = {
    Chat: ['chat'],
    Vision: ['vision'],
    Embedding: ['embedding'],
    Thinking: ['thinking'],
  };

  const TIER_RANK: Record<HardwareTier, number> = {
    minimal: 0,
    standard: 1,
    full: 2,
  };

  let filter = $state<CapabilityFilter>('Chat');
  let entries = $state<CatalogEntry[]>([]);
  let catalogError = $state<string | null>(null);
  let catalogLoading = $state<boolean>(true);

  let manualName = $state<string>('');
  let manualPullError = $state<string | null>(null);

  // Pull from the Governor store so the catalog filter respects the
  // active tier the user has configured.
  let effectiveTier = $derived<HardwareTier>(
    governorStore.metrics?.effective_tier ?? 'minimal',
  );

  let visibleEntries = $derived<CatalogEntry[]>(
    entries.filter((e) => entryMatches(e, filter, effectiveTier)),
  );

  function entryMatches(
    e: CatalogEntry,
    f: CapabilityFilter,
    tier: HardwareTier,
  ): boolean {
    const tags = FILTER_TAGS[f];
    const hasCap = e.capabilities.some((c) => tags.includes(c));
    if (!hasCap) return false;
    return TIER_RANK[e.min_tier] <= TIER_RANK[tier];
  }

  function formatSizeMb(mb: number): string {
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${mb.toFixed(0)} MB`;
  }

  // Mirror of `compute_recommendation` for catalog entries — purely
  // for the inline label. The backend computes the real recommendation
  // for installed models in `models_tab_list`.
  function recommendationLabel(sizeMb: number): string {
    const total = governorStore.metrics?.total_ram_mb ?? 0;
    if (total === 0) return '';
    const overheadMb =
      effectiveTier === 'minimal' ? 200 : effectiveTier === 'standard' ? 400 : 600;
    const combined = sizeMb + overheadMb;
    if (combined < total / 2) return 'fits comfortably';
    if (combined < total) return 'requires management';
    return 'exceeds tier';
  }

  function recommendationClass(sizeMb: number): string {
    const label = recommendationLabel(sizeMb);
    if (label === 'fits comfortably') return 'rec-ok';
    if (label === 'requires management') return 'rec-warn';
    if (label === 'exceeds tier') return 'rec-danger';
    return '';
  }

  async function loadCatalog(): Promise<void> {
    catalogLoading = true;
    catalogError = null;
    try {
      entries = await invoke<CatalogEntry[]>('models_catalog_list');
    } catch (err) {
      entries = [];
      catalogError = err instanceof Error ? err.message : String(err);
    } finally {
      catalogLoading = false;
    }
  }

  async function startPull(name: string): Promise<void> {
    const trimmed = name.trim();
    if (trimmed.length === 0) return;
    // Length is bytes, not chars — UTF-8 byte length per Req 14.3.
    if (new TextEncoder().encode(trimmed).length > 256) {
      manualPullError = 'Model name must be at most 256 bytes.';
      return;
    }
    manualPullError = null;
    modelsStore.markPullStarted(trimmed);
    try {
      await invoke('pull_model', { modelName: trimmed });
    } catch (err) {
      manualPullError = err instanceof Error ? err.message : String(err);
    } finally {
      // Always run — regardless of success or failure — so the row
      // refresh fires and the in-flight badge clears.
      await modelsStore.markPullDone(trimmed);
    }
  }

  function pullCatalogEntry(e: CatalogEntry) {
    void startPull(e.name);
  }

  function pullManual() {
    const v = manualName.trim();
    if (v.length === 0) return;
    manualName = '';
    void startPull(v);
  }

  function progressPercent(name: string): number | null {
    const p = modelsStore.pullProgress[name];
    if (!p || p.total === null || p.total === 0) return null;
    if (p.completed === null) return null;
    return Math.max(0, Math.min(100, (p.completed / p.total) * 100));
  }

  function progressLabel(name: string): string {
    const p = modelsStore.pullProgress[name];
    if (!p) return '';
    return p.status;
  }

  onMount(() => {
    void loadCatalog();
  });
</script>

<section class="pull-panel" aria-label="Pull a new model">
  <header class="pull-header">
    <span class="pull-title">Pull a model</span>
    <div class="filter-tabs" role="tablist" aria-label="Capability filter">
      {#each FILTERS as f (f)}
        <button
          type="button"
          role="tab"
          aria-selected={filter === f}
          class="filter-tab"
          class:active={filter === f}
          onclick={() => (filter = f)}
        >
          {f}
        </button>
      {/each}
    </div>
  </header>

  {#if catalogError}
    <p class="catalog-error" role="status">
      Catalog unavailable; type a model name to pull.
    </p>
  {:else if catalogLoading}
    <p class="catalog-skeleton">Loading catalog…</p>
  {:else if visibleEntries.length === 0}
    <p class="empty-hint">No catalog entries match your hardware tier.</p>
  {:else}
    <ul class="catalog-grid" aria-label="Curated catalog">
      {#each visibleEntries as e (e.name)}
        <li>
          <button
            type="button"
            class="catalog-entry"
            onclick={() => pullCatalogEntry(e)}
            disabled={modelsStore.isPulling(e.name)}
            aria-label="Pull {e.name}"
          >
            <span class="catalog-name">{e.name}</span>
            <span class="catalog-meta">
              <span class="catalog-size">{formatSizeMb(e.size_mb)}</span>
              {#if recommendationLabel(e.size_mb)}
                <span class="catalog-recommendation {recommendationClass(e.size_mb)}">
                  {recommendationLabel(e.size_mb)}
                </span>
              {/if}
            </span>
            {#if modelsStore.isPulling(e.name)}
              {@const pct = progressPercent(e.name)}
              <div class="catalog-progress" aria-label="Pull progress">
                <div
                  class="catalog-progress-fill"
                  style:width={pct === null ? '8%' : `${pct}%`}
                  class:indeterminate={pct === null}
                ></div>
              </div>
              <span class="catalog-progress-label">
                {progressLabel(e.name) || 'starting…'}
              </span>
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  <div class="manual-row">
    <label for="manual-pull-input" class="manual-label">
      Or pull by name
    </label>
    <div class="manual-inputs">
      <input
        id="manual-pull-input"
        type="text"
        class="manual-input"
        placeholder="e.g. llama3.2:3b"
        bind:value={manualName}
        maxlength={256}
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            pullManual();
          }
        }}
      />
      <button
        type="button"
        class="manual-pull-btn"
        onclick={pullManual}
        disabled={manualName.trim().length === 0}
      >
        Pull
      </button>
    </div>
    {#if manualPullError}
      <p class="manual-error" role="alert">{manualPullError}</p>
    {/if}
  </div>
</section>

<style>
  .pull-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    padding: var(--space-md);
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }

  .pull-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-md);
    flex-wrap: wrap;
  }

  .pull-title {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .filter-tabs {
    display: flex;
    gap: var(--space-xs);
    flex-wrap: wrap;
  }

  .filter-tab {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 2px var(--space-sm);
    border-radius: var(--radius-pill);
    cursor: pointer;
    border: 0.5px solid var(--border-dim);
    background: transparent;
    color: var(--text-dim);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    transition: color 0.15s, border-color 0.15s, background 0.15s;
  }

  .filter-tab:hover:not(.active) {
    color: var(--text-primary);
    border-color: var(--gold-dim);
  }

  .filter-tab.active {
    background: var(--gold-bg);
    border-color: var(--gold-dim);
    color: var(--gold-primary);
  }

  .filter-tab:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .catalog-error,
  .catalog-skeleton,
  .empty-hint {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-dim);
    margin: 0;
    padding: var(--space-md);
    text-align: center;
    background: var(--bg-elevated);
    border: 0.5px dashed var(--border-subtle);
    border-radius: var(--radius-sm);
  }

  .catalog-error {
    color: var(--status-warn-text);
    background: var(--status-warn-bg);
    border-color: var(--status-warn-border);
    border-style: solid;
  }

  .catalog-grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: var(--space-sm);
  }

  .catalog-entry {
    width: 100%;
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: var(--space-sm) var(--space-md);
    border-radius: var(--radius-sm);
    border: 0.5px solid var(--border-subtle);
    background: var(--bg-elevated);
    color: var(--text-primary);
    cursor: pointer;
    font-family: var(--font-ui);
    transition: border-color 0.15s, color 0.15s, background 0.15s;
  }

  .catalog-entry:hover:not(:disabled) {
    border-color: var(--gold-dim);
    color: var(--gold-primary);
  }

  .catalog-entry:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .catalog-entry:disabled {
    cursor: progress;
    opacity: 0.85;
  }

  .catalog-name {
    font-size: 12px;
    font-weight: 500;
  }

  .catalog-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-xs);
    flex-wrap: wrap;
  }

  .catalog-size {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .catalog-recommendation {
    font-family: var(--font-ui);
    font-size: 9px;
    padding: 1px var(--space-xs);
    border-radius: var(--radius-pill);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    border: 0.5px solid transparent;
  }

  .rec-ok {
    background: var(--status-ok-bg);
    border-color: var(--status-ok-border);
    color: var(--accent-green);
  }

  .rec-warn {
    background: var(--status-warn-bg);
    border-color: var(--status-warn-border);
    color: var(--status-warn-text);
  }

  .rec-danger {
    background: var(--status-danger-bg);
    border-color: var(--accent-red);
    color: var(--accent-red);
  }

  .catalog-progress {
    width: 100%;
    height: 4px;
    background: var(--bg-surface);
    border-radius: var(--radius-sm);
    overflow: hidden;
    border: 0.5px solid var(--border-subtle);
  }

  .catalog-progress-fill {
    height: 100%;
    background: var(--gold-dim);
    transition: width 0.2s ease-out;
  }

  .catalog-progress-fill.indeterminate {
    animation: pull-progress 1.5s ease-in-out infinite;
  }

  @keyframes pull-progress {
    0% {
      width: 8%;
      transform: translateX(0);
    }
    50% {
      width: 24%;
      transform: translateX(150%);
    }
    100% {
      width: 8%;
      transform: translateX(900%);
    }
  }

  .catalog-progress-label {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-ghost);
    letter-spacing: 0.04em;
  }

  .manual-row {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding-top: var(--space-sm);
    border-top: 0.5px solid var(--border-subtle);
  }

  .manual-label {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .manual-inputs {
    display: flex;
    gap: var(--space-xs);
  }

  .manual-input {
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

  .manual-input::placeholder {
    color: var(--text-ghost);
  }

  .manual-input:focus {
    border-color: var(--gold-dim);
  }

  .manual-pull-btn {
    font-family: var(--font-ui);
    font-size: 11px;
    padding: 4px var(--space-md);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid var(--gold-dim);
    background: var(--gold-bg);
    color: var(--gold-primary);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    transition: border-color 0.15s, background 0.15s;
  }

  .manual-pull-btn:hover:not(:disabled) {
    border-color: var(--gold-primary);
  }

  .manual-pull-btn:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .manual-pull-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .manual-error {
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
