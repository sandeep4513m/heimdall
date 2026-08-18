<!-- src/lib/components/memory/FactList.svelte -->
<!-- Displays confirmed facts with per-fact actions: edit, delete, and a
     "from {conversation title}" provenance pill linking back to the source
     conversation. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { memoryStore } from '$lib/stores/memory.svelte';
  import FactEditor from './FactEditor.svelte';
  import Icon from '$lib/components/icons/Icon.svelte';
  import { iconX } from '$lib/components/icons/index';

  interface Props {
    /** Case-insensitive substring filter over fact text. Empty = no filter. */
    searchQuery?: string;
  }

  let { searchQuery = '' }: Props = $props();

  interface ConvLite {
    id: string;
    title: string | null;
  }

  let editingId = $state<string | null>(null);
  let confirmDeleteId = $state<string | null>(null);
  // convId → title map for the provenance pill. Loaded once on mount.
  let convTitles = $state<Map<string, string>>(new Map());

  onMount(async () => {
    try {
      const convs = await invoke<ConvLite[]>('list_conversations');
      const m = new Map<string, string>();
      for (const c of convs) {
        m.set(c.id, c.title ?? 'Untitled');
      }
      convTitles = m;
    } catch {
      // Non-critical — UI just won't show provenance pills.
    }
  });

  let filteredFacts = $derived.by(() => {
    const q = searchQuery.trim().toLowerCase();
    if (!q) return memoryStore.confirmedFacts;
    return memoryStore.confirmedFacts.filter((f) => f.fact.toLowerCase().includes(q));
  });

  function formatDate(ts: number): string {
    return new Date(ts * 1000).toLocaleDateString(undefined, {
      month: 'short',
      day: 'numeric',
      year: 'numeric',
    });
  }

  async function handleEdit(id: string, newText: string) {
    await memoryStore.editFact(id, newText);
    editingId = null;
  }

  async function handleDelete(id: string) {
    await memoryStore.deleteFact(id);
    confirmDeleteId = null;
  }

  /** Switch to the source conversation in the chat history.
   *  Dispatches a CustomEvent the chat layer listens for. */
  function openSourceConversation(convId: string) {
    window.dispatchEvent(
      new CustomEvent('heimdall:open-conversation', { detail: { conversationId: convId } }),
    );
  }
</script>

<div class="fact-list">
  {#if memoryStore.confirmedFacts.length === 0}
    <div class="empty-state">
      <p class="empty-text">No confirmed facts yet.</p>
      <p class="empty-sub">Facts extracted from conversations will appear here after you confirm them.</p>
    </div>
  {:else if filteredFacts.length === 0}
    <div class="empty-state">
      <p class="empty-text">No facts match "{searchQuery}".</p>
    </div>
  {:else}
    {#each filteredFacts as fact (fact.id)}
      <div class="fact-row">
        {#if editingId === fact.id}
          <FactEditor
            initialText={fact.fact}
            onSave={(text) => handleEdit(fact.id, text)}
            onCancel={() => (editingId = null)}
          />
        {:else}
          <div class="fact-body">
            <span class="fact-text">{fact.fact}</span>
            <div class="fact-meta">
              <span class="fact-date" title="Confirmed on {formatDate(fact.created_at)}">
                {formatDate(fact.created_at)}
              </span>
              <span class="injected-badge" title="This fact is injected into chats with memory enabled">
                active
              </span>
              {#if fact.source_conversation_id && convTitles.has(fact.source_conversation_id)}
                <button
                  class="provenance-pill"
                  onclick={() => openSourceConversation(fact.source_conversation_id!)}
                  title="Open the conversation this fact came from"
                >
                  from {convTitles.get(fact.source_conversation_id)}
                </button>
              {/if}
            </div>
          </div>
          <div class="fact-actions">
            <button
              class="action-btn edit-btn"
              onclick={() => (editingId = fact.id)}
              title="Edit fact"
              aria-label="Edit fact"
            >
              Edit
            </button>
            {#if confirmDeleteId === fact.id}
              <button
                class="action-btn confirm-delete-btn"
                onclick={() => handleDelete(fact.id)}
                title="Confirm delete"
              >
                Delete?
              </button>
              <button
                class="action-btn cancel-btn"
                onclick={() => (confirmDeleteId = null)}
                title="Cancel"
                aria-label="Cancel delete"
              >
                <Icon paths={iconX} size={10} stroke={2} />
              </button>
            {:else}
              <button
                class="action-btn delete-btn"
                onclick={() => (confirmDeleteId = fact.id)}
                title="Delete fact"
                aria-label="Delete fact"
              >
                <Icon paths={iconX} size={10} stroke={2} />
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  {/if}
</div>

<style>
  .fact-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .empty-state {
    padding: var(--space-xl);
    text-align: center;
    color: var(--text-dim);
  }

  .empty-text {
    font-size: 13px;
    margin-bottom: var(--space-xs);
  }

  .empty-sub {
    font-size: 11px;
    color: var(--text-ghost);
    line-height: 1.6;
  }

  .fact-row {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-bottom: 0.5px solid var(--border-subtle);
    transition: background 0.1s;
  }

  .fact-row:hover {
    background: var(--bg-elevated);
  }

  .fact-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .fact-text {
    font-size: 12px;
    color: var(--text-primary);
    line-height: 1.5;
    word-break: break-word;
  }

  .fact-meta {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .fact-date {
    font-size: 10px;
    color: var(--text-ghost);
  }

  .injected-badge {
    font-size: 9px;
    color: var(--accent-green);
    border: 0.5px solid var(--status-ok-border);
    background: var(--status-ok-bg);
    border-radius: var(--radius-pill);
    padding: 0 var(--space-xs);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .provenance-pill {
    font-family: var(--font-ui);
    font-size: 9px;
    color: var(--text-dim);
    border: 0.5px solid var(--border-subtle);
    background: transparent;
    border-radius: var(--radius-pill);
    padding: 0 var(--space-xs);
    letter-spacing: 0.04em;
    cursor: pointer;
    max-width: 200px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: color 0.1s, border-color 0.1s, background 0.1s;
  }

  .provenance-pill:hover {
    color: var(--gold-primary);
    border-color: var(--gold-dim);
    background: var(--gold-bg);
  }

  .fact-actions {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    flex-shrink: 0;
    opacity: 0;
    transition: opacity 0.1s;
  }

  .fact-row:hover .fact-actions {
    opacity: 1;
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

  .delete-btn {
    color: var(--text-ghost);
    width: 20px;
    height: 20px;
  }

  .delete-btn:hover {
    color: var(--accent-red);
    border-color: var(--accent-red);
  }

  .confirm-delete-btn {
    color: var(--accent-red);
    border-color: var(--accent-red);
    font-size: 10px;
    padding: 2px var(--space-xs);
  }

  .confirm-delete-btn:hover {
    background: var(--status-danger-bg);
  }

  .cancel-btn {
    color: var(--text-ghost);
    width: 20px;
    height: 20px;
  }

  .cancel-btn:hover {
    color: var(--text-dim);
  }
</style>
