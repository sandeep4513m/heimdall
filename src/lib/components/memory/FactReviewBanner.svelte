<!-- src/lib/components/memory/FactReviewBanner.svelte -->
<!-- Batch review UI for newly extracted facts grouped by batch_id. -->
<script lang="ts">
  import { memoryStore } from '$lib/stores/memory.svelte';
  import FactEditor from './FactEditor.svelte';
  import Icon from '$lib/components/icons/Icon.svelte';
  import { iconX } from '$lib/components/icons/index';
  import type { MemoryFact } from '$lib/types/memory';

  interface Props {
    batchId: string;
    facts: MemoryFact[];
  }

  let { batchId, facts }: Props = $props();

  let editingId = $state<string | null>(null);
  let localFacts = $state<MemoryFact[]>([...facts]);

  // Keep localFacts in sync when parent updates
  $effect(() => {
    localFacts = [...facts];
  });

  async function confirmAll() {
    const ids = localFacts.map((f) => f.id);
    await memoryStore.confirmAll(ids);
  }

  async function rejectAll() {
    const ids = localFacts.map((f) => f.id);
    await memoryStore.rejectAll(ids);
  }

  async function confirmOne(id: string) {
    await memoryStore.confirmFact(id);
  }

  async function rejectOne(id: string) {
    await memoryStore.rejectFact(id);
  }

  async function handleEdit(id: string, newText: string) {
    await memoryStore.editFact(id, newText);
    editingId = null;
  }

  function dedupLabel(status: string | null): string {
    if (status === 'possible_update') return 'similar to existing';
    return '';
  }
</script>

{#if localFacts.length > 0}
  <div class="review-banner">
    <div class="banner-header">
      <span class="banner-title">
        {localFacts.length} new {localFacts.length === 1 ? 'fact' : 'facts'} extracted
      </span>
      <div class="banner-actions">
        <button class="btn-confirm-all" onclick={confirmAll}>
          Confirm all
        </button>
        <button class="btn-reject-all" onclick={rejectAll}>
          Reject all
        </button>
      </div>
    </div>

    <div class="fact-rows">
      {#each localFacts as fact (fact.id)}
        <div class="review-row" class:conflict={fact.conflict_with_id !== null}>
          {#if editingId === fact.id}
            <FactEditor
              initialText={fact.fact}
              onSave={(text) => handleEdit(fact.id, text)}
              onCancel={() => (editingId = null)}
            />
          {:else}
            <div class="review-body">
              <span class="review-text">{fact.fact}</span>
              {#if fact.dedup_status === 'possible_update'}
                <span class="dedup-badge">{dedupLabel(fact.dedup_status)}</span>
              {/if}
              {#if fact.conflict_with_id}
                <span class="conflict-badge">conflicts with existing</span>
              {/if}
            </div>
            <div class="review-actions">
              <button
                class="action-btn edit-btn"
                onclick={() => (editingId = fact.id)}
                title="Edit before confirming"
              >
                Edit
              </button>
              <button
                class="action-btn confirm-btn"
                onclick={() => confirmOne(fact.id)}
                title="Confirm this fact"
              >
                ✓
              </button>
              <button
                class="action-btn reject-btn"
                onclick={() => rejectOne(fact.id)}
                title="Reject this fact"
                aria-label="Reject fact"
              >
                <Icon paths={iconX} size={10} stroke={2} />
              </button>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .review-banner {
    background: var(--bg-elevated);
    border: 0.5px solid var(--gold-dim);
    border-radius: var(--radius-md);
    margin: var(--space-md);
    overflow: hidden;
  }

  .banner-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-sm) var(--space-md);
    border-bottom: 0.5px solid var(--border-subtle);
    background: var(--gold-bg);
  }

  .banner-title {
    font-size: 11px;
    color: var(--gold-primary);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .banner-actions {
    display: flex;
    gap: var(--space-xs);
  }

  .btn-confirm-all, .btn-reject-all {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 2px var(--space-sm);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid transparent;
    transition: background 0.1s, color 0.1s;
  }

  .btn-confirm-all {
    background: var(--status-ok-bg);
    border-color: var(--status-ok-border);
    color: var(--accent-green);
  }

  .btn-confirm-all:hover {
    background: var(--status-ok-border);
    color: var(--text-primary);
  }

  .btn-reject-all {
    background: transparent;
    border-color: var(--border-subtle);
    color: var(--text-dim);
  }

  .btn-reject-all:hover {
    border-color: var(--accent-red);
    color: var(--accent-red);
  }

  .fact-rows {
    display: flex;
    flex-direction: column;
  }

  .review-row {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-bottom: 0.5px solid var(--border-subtle);
  }

  .review-row:last-child {
    border-bottom: none;
  }

  .review-row.conflict {
    background: var(--status-warn-bg);
  }

  .review-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .review-text {
    font-size: 12px;
    color: var(--text-primary);
    line-height: 1.5;
    word-break: break-word;
  }

  .dedup-badge {
    font-size: 9px;
    color: var(--status-warn-text);
    border: 0.5px solid var(--status-warn-border);
    background: var(--status-warn-bg);
    border-radius: var(--radius-pill);
    padding: 0 var(--space-xs);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    align-self: flex-start;
  }

  .conflict-badge {
    font-size: 9px;
    color: var(--accent-red);
    border: 0.5px solid var(--accent-red);
    background: var(--status-danger-bg);
    border-radius: var(--radius-pill);
    padding: 0 var(--space-xs);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    align-self: flex-start;
  }

  .review-actions {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    flex-shrink: 0;
  }

  .action-btn {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 2px var(--space-xs);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid transparent;
    background: transparent;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.1s, color 0.1s, border-color 0.1s;
  }

  .edit-btn {
    color: var(--text-dim);
    border-color: var(--border-subtle);
  }

  .edit-btn:hover {
    color: var(--gold-primary);
    border-color: var(--gold-dim);
    background: var(--gold-bg);
  }

  .confirm-btn {
    color: var(--accent-green);
    border-color: var(--status-ok-border);
    background: var(--status-ok-bg);
    width: 22px;
    height: 22px;
  }

  .confirm-btn:hover {
    background: var(--status-ok-border);
    color: var(--text-primary);
  }

  .reject-btn {
    color: var(--text-ghost);
    width: 22px;
    height: 22px;
  }

  .reject-btn:hover {
    color: var(--accent-red);
    border-color: var(--accent-red);
  }
</style>
