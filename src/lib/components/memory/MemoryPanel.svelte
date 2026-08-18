<!-- src/lib/components/memory/MemoryPanel.svelte -->
<!-- Main memory management panel — facts, episodes, settings. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { memoryStore } from '$lib/stores/memory.svelte';
  import FactList from './FactList.svelte';
  import FactReviewBanner from './FactReviewBanner.svelte';
  import type { MemoryFact } from '$lib/types/memory';

  let confirmDeleteAllFacts = $state(false);
  let confirmDeleteAllEpisodes = $state(false);
  let exportMessage = $state<string | null>(null);
  let decayInput = $state<number>(90);
  let searchQuery = $state<string>('');

  onMount(async () => {
    await memoryStore.loadFacts();
    await memoryStore.loadSettings();
    decayInput = memoryStore.settings.decay_threshold_days;
  });

  // Group pending facts by batch_id
  let pendingBatches = $derived(() => {
    const batches = new Map<string, MemoryFact[]>();
    for (const f of memoryStore.pendingFacts) {
      const key = f.batch_id ?? 'ungrouped';
      const arr = batches.get(key) ?? [];
      arr.push(f);
      batches.set(key, arr);
    }
    return [...batches.entries()];
  });

  async function toggleGlobal() {
    await memoryStore.updateSettings({
      ...memoryStore.settings,
      global_enabled: !memoryStore.settings.global_enabled,
    });
  }

  async function saveDecayThreshold() {
    const days = Math.max(1, Math.min(3650, decayInput));
    await memoryStore.updateSettings({
      ...memoryStore.settings,
      decay_threshold_days: days,
    });
  }

  async function handleExport() {
    try {
      const json = await memoryStore.exportFacts();
      const blob = new Blob([json], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = 'heimdall-memory-facts.json';
      a.click();
      URL.revokeObjectURL(url);
      exportMessage = 'Exported.';
      setTimeout(() => (exportMessage = null), 2000);
    } catch {
      exportMessage = 'Export failed.';
      setTimeout(() => (exportMessage = null), 3000);
    }
  }

  async function handleDeleteAllFacts() {
    await memoryStore.deleteAllFacts();
    confirmDeleteAllFacts = false;
  }

  async function handleDeleteAllEpisodes() {
    await memoryStore.deleteAllEpisodes();
    confirmDeleteAllEpisodes = false;
  }
</script>

<div class="memory-panel">

  <!-- Header -->
  <div class="panel-header">
    <h2 class="panel-title">Memory</h2>
    <div class="header-controls">
      <button
        class="toggle-btn"
        class:enabled={memoryStore.settings.global_enabled}
        onclick={toggleGlobal}
        title={memoryStore.settings.global_enabled ? 'Memory enabled — click to disable' : 'Memory disabled — click to enable'}
        aria-label="Toggle global memory"
      >
        {memoryStore.settings.global_enabled ? 'On' : 'Off'}
      </button>
    </div>
  </div>

  <!-- Soft warning at 150 facts -->
  {#if memoryStore.showSoftWarning}
    <div class="warning-banner soft-warning">
      Your memory is getting large — consider reviewing old facts.
    </div>
  {/if}

  <!-- Hard cap at 200 facts -->
  {#if memoryStore.atHardCap}
    <div class="warning-banner hard-cap">
      Memory is full (200 facts). Delete some facts below before confirming new ones.
    </div>
  {/if}

  <!-- Pending review batches -->
  {#each pendingBatches() as [batchId, batchFacts] (batchId)}
    <FactReviewBanner {batchId} facts={batchFacts} />
  {/each}

  <!-- Confirmed facts section -->
  <div class="section">
    <div class="section-header">
      <span class="section-label">
        Confirmed facts
        <span class="count-badge">{memoryStore.factCount}</span>
      </span>
      <div class="section-actions">
        <button class="text-btn" onclick={handleExport} title="Export confirmed facts as JSON">
          Export
        </button>
        {#if exportMessage}
          <span class="export-msg">{exportMessage}</span>
        {/if}
        {#if confirmDeleteAllFacts}
          <button class="text-btn danger-btn" onclick={handleDeleteAllFacts}>
            Confirm delete all
          </button>
          <button class="text-btn" onclick={() => (confirmDeleteAllFacts = false)}>
            Cancel
          </button>
        {:else}
          <button
            class="text-btn danger-btn"
            onclick={() => (confirmDeleteAllFacts = true)}
            title="Delete all confirmed facts"
          >
            Delete all
          </button>
        {/if}
      </div>
    </div>
    {#if memoryStore.confirmedFacts.length > 0}
      <div class="search-row">
        <input
          type="search"
          placeholder="Search facts…"
          bind:value={searchQuery}
          class="search-input"
          aria-label="Search confirmed facts"
        />
        {#if searchQuery}
          <button
            class="search-clear"
            onclick={() => (searchQuery = '')}
            title="Clear search"
            aria-label="Clear search"
          >×</button>
        {/if}
      </div>
    {/if}
    <FactList {searchQuery} />
  </div>

  <!-- Episodes section -->
  <div class="section">
    <div class="section-header">
      <span class="section-label">
        Episodes
        <span class="count-badge">{memoryStore.episodeCount}</span>
      </span>
      <div class="section-actions">
        {#if confirmDeleteAllEpisodes}
          <button class="text-btn danger-btn" onclick={handleDeleteAllEpisodes}>
            Confirm delete all
          </button>
          <button class="text-btn" onclick={() => (confirmDeleteAllEpisodes = false)}>
            Cancel
          </button>
        {:else}
          <button
            class="text-btn danger-btn"
            onclick={() => (confirmDeleteAllEpisodes = true)}
            title="Delete all episode summaries"
          >
            Delete all
          </button>
        {/if}
      </div>
    </div>
    <div class="episodes-info">
      <p class="info-text">
        Episodes are conversation summaries stored as searchable vectors.
        They are injected automatically when relevant to new conversations.
      </p>
      <div class="decay-row">
        <label class="decay-label" for="decay-input">
          Exclude episodes older than
        </label>
        <input
          id="decay-input"
          type="number"
          min={1}
          max={3650}
          bind:value={decayInput}
          onblur={saveDecayThreshold}
          onkeydown={(e) => e.key === 'Enter' && saveDecayThreshold()}
          class="decay-input"
          aria-label="Episode decay threshold in days"
        />
        <span class="decay-unit">days</span>
      </div>
    </div>
  </div>

</div>

<style>
  .memory-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow-y: auto;
    background: var(--bg-app);
  }

  .panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-md) var(--space-lg);
    border-bottom: 0.5px solid var(--border-subtle);
    background: var(--bg-surface);
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

  .header-controls {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .toggle-btn {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 3px var(--space-sm);
    border-radius: var(--radius-pill);
    cursor: pointer;
    border: 0.5px solid var(--border-dim);
    background: var(--bg-elevated);
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }

  .toggle-btn.enabled {
    background: var(--status-ok-bg);
    border-color: var(--status-ok-border);
    color: var(--accent-green);
  }

  .toggle-btn:hover {
    border-color: var(--gold-dim);
    color: var(--gold-primary);
  }

  .warning-banner {
    font-size: 11px;
    padding: var(--space-sm) var(--space-md);
    line-height: 1.5;
    flex-shrink: 0;
  }

  .soft-warning {
    background: var(--status-warn-bg);
    border-bottom: 0.5px solid var(--status-warn-border);
    color: var(--status-warn-text);
  }

  .hard-cap {
    background: var(--status-danger-bg);
    border-bottom: 0.5px solid var(--accent-red);
    color: var(--accent-red);
  }

  .section {
    border-bottom: 0.5px solid var(--border-subtle);
  }

  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-surface);
    border-bottom: 0.5px solid var(--border-subtle);
  }

  .section-label {
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .count-badge {
    font-size: 10px;
    color: var(--text-ghost);
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-pill);
    padding: 0 var(--space-xs);
    min-width: 18px;
    text-align: center;
  }

  .section-actions {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .text-btn {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 2px var(--space-xs);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid var(--border-subtle);
    background: transparent;
    color: var(--text-dim);
    transition: color 0.1s, border-color 0.1s;
  }

  .text-btn:hover {
    color: var(--text-primary);
    border-color: var(--border-dim);
  }

  .danger-btn {
    color: var(--accent-red);
    border-color: transparent;
  }

  .danger-btn:hover {
    border-color: var(--accent-red);
    background: var(--status-danger-bg);
  }

  .export-msg {
    font-size: 10px;
    color: var(--accent-green);
  }

  .search-row {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-surface);
    border-bottom: 0.5px solid var(--border-subtle);
  }

  .search-input {
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

  .search-input::placeholder {
    color: var(--text-ghost);
  }

  .search-input:focus {
    border-color: var(--gold-dim);
  }

  .search-clear {
    font-family: var(--font-ui);
    font-size: 14px;
    line-height: 1;
    width: 22px;
    height: 22px;
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

  .search-clear:hover {
    color: var(--accent-red);
    border-color: var(--accent-red);
    background: var(--status-danger-bg);
  }

  .episodes-info {
    padding: var(--space-md);
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }

  .info-text {
    font-size: 11px;
    color: var(--text-dim);
    line-height: 1.6;
  }

  .decay-row {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .decay-label {
    font-size: 11px;
    color: var(--text-dim);
  }

  .decay-input {
    width: 56px;
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-dim);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-family: var(--font-ui);
    font-size: 12px;
    padding: 2px var(--space-xs);
    text-align: center;
    outline: none;
  }

  .decay-input:focus {
    border-color: var(--gold-dim);
  }

  .decay-unit {
    font-size: 11px;
    color: var(--text-dim);
  }
</style>
