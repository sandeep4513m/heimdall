<script lang="ts">
  import { ragStore } from '$lib/stores/rag.svelte';
  import Icon from '../icons/Icon.svelte';
  import { iconPlus, iconX } from '../icons/index';

  let newCollectionName = $state('');
  let isCreating = $state(false);
  let formError = $state<string | null>(null);

  // Inline confirmation state — replaces native `confirm()`.
  // Tracks the display_name of the collection awaiting delete confirmation.
  let pendingDeleteName = $state<string | null>(null);

  // Inline rename state — replaces native `prompt()`.
  // Tracks { oldName, newName } when a rename row is in edit mode.
  let renameTarget = $state<{ oldName: string; newName: string } | null>(null);

  async function handleCreate() {
    const name = newCollectionName.trim();
    if (!name) return;
    formError = null;
    try {
      await ragStore.createCollection(name);
      newCollectionName = '';
      isCreating = false;
    } catch (e) {
      formError = humanizeError(e, `Could not create collection “${name}”.`);
    }
  }

  function startDelete(name: string) {
    pendingDeleteName = name;
  }

  function cancelDelete() {
    pendingDeleteName = null;
  }

  async function confirmDelete(name: string) {
    pendingDeleteName = null;
    try {
      await ragStore.deleteCollection(name);
    } catch (e) {
      formError = humanizeError(e, `Could not delete collection “${name}”.`);
    }
  }

  function startRename(name: string) {
    renameTarget = { oldName: name, newName: name };
  }

  function cancelRename() {
    renameTarget = null;
  }

  async function commitRename() {
    if (!renameTarget) return;
    const { oldName, newName } = renameTarget;
    const trimmed = newName.trim();
    if (!trimmed || trimmed === oldName) {
      renameTarget = null;
      return;
    }
    try {
      await ragStore.renameCollection(oldName, trimmed);
      renameTarget = null;
    } catch (e) {
      formError = humanizeError(e, `Could not rename “${oldName}”.`);
    }
  }

  function humanizeError(e: unknown, fallback: string): string {
    if (typeof e === 'string') {
      const lower = e.toLowerCase();
      if (lower.includes('already exists') || lower.includes('unique constraint') || lower.includes('uniqueness')) {
        return 'A collection with that name already exists.';
      }
      if (lower.includes('invalidcollectionname') || lower.includes('invalid collection name')) {
        return 'Names can only contain letters, numbers, spaces, hyphens and underscores (1–64 chars).';
      }
      return e;
    }
    return fallback;
  }
</script>

<div class="collections-list-container">
  <div class="header">
    <h2>Collections</h2>
    <button
      class="icon-btn"
      onclick={() => {
        isCreating = !isCreating;
        formError = null;
      }}
      title={isCreating ? 'Cancel' : 'New Collection'}
      aria-label={isCreating ? 'Cancel new collection' : 'New collection'}
    >
      <Icon paths={isCreating ? iconX : iconPlus} size={16} stroke={2} />
    </button>
  </div>

  {#if isCreating}
    <div class="create-form">
      <input
        type="text"
        bind:value={newCollectionName}
        placeholder="Name (e.g. docs_2026)"
        onkeydown={(e) => {
          if (e.key === 'Enter') handleCreate();
          if (e.key === 'Escape') {
            isCreating = false;
            formError = null;
          }
        }}
        autofocus
      />
      <button onclick={handleCreate} disabled={!newCollectionName.trim()}>Add</button>
    </div>
  {/if}

  {#if formError}
    <div class="form-error">{formError}</div>
  {/if}

  <div class="list">
    {#each ragStore.collections as col (col.id)}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="collection-item"
        class:selected={ragStore.selectedCollection?.id === col.id}
        class:editing={renameTarget?.oldName === col.display_name || pendingDeleteName === col.display_name}
        onclick={() => ragStore.selectCollection(col)}
      >
        {#if renameTarget?.oldName === col.display_name}
          <input
            class="rename-input"
            type="text"
            bind:value={renameTarget.newName}
            onclick={(e) => e.stopPropagation()}
            onkeydown={(e) => {
              if (e.key === 'Enter') commitRename();
              if (e.key === 'Escape') cancelRename();
            }}
            autofocus
          />
          <div class="actions inline">
            <button class="action-btn" onclick={(e) => { e.stopPropagation(); commitRename(); }} title="Save rename">Save</button>
            <button class="action-btn" onclick={(e) => { e.stopPropagation(); cancelRename(); }} title="Cancel rename">
              <Icon paths={iconX} size={14} stroke={2} />
            </button>
          </div>
        {:else if pendingDeleteName === col.display_name}
          <span class="confirm-text">Delete this collection?</span>
          <div class="actions inline">
            <button class="action-btn destructive" onclick={(e) => { e.stopPropagation(); confirmDelete(col.display_name); }} title="Confirm delete">Delete</button>
            <button class="action-btn" onclick={(e) => { e.stopPropagation(); cancelDelete(); }} title="Cancel">
              <Icon paths={iconX} size={14} stroke={2} />
            </button>
          </div>
        {:else}
          <span class="col-name" title={col.display_name}>{col.display_name}</span>
          <div class="actions">
            <button class="action-btn" onclick={(e) => { e.stopPropagation(); startRename(col.display_name); }} title="Rename" aria-label="Rename">R</button>
            <button class="action-btn delete" onclick={(e) => { e.stopPropagation(); startDelete(col.display_name); }} title="Delete" aria-label="Delete">
              <Icon paths={iconX} size={14} stroke={2} />
            </button>
          </div>
        {/if}
      </div>
    {:else}
      {#if !isCreating}
        <div class="empty-cta">
          <p>Create your first knowledge collection</p>
          <button class="cta-btn" onclick={() => isCreating = true}>Create</button>
        </div>
      {/if}
    {/each}
  </div>
</div>

<style>
  .collections-list-container {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-md);
    border-bottom: 0.5px solid var(--border-subtle);
  }
  h2 {
    font-family: var(--font-brand);
    font-size: 14px;
    margin: 0;
    color: var(--text-base);
  }
  .icon-btn {
    background: transparent;
    border: none;
    color: var(--text-ghost);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: var(--radius-sm);
    width: 24px;
    height: 24px;
  }
  .icon-btn:hover {
    color: var(--text-base);
    background: var(--bg-elevated);
  }
  .create-form {
    display: flex;
    padding: var(--space-md);
    gap: var(--space-sm);
    border-bottom: 0.5px solid var(--border-subtle);
  }
  .create-form input {
    flex: 1;
    background: var(--bg-app);
    border: 0.5px solid var(--border-subtle);
    color: var(--text-base);
    font-family: var(--font-ui);
    font-size: 12px;
    padding: var(--space-sm);
    border-radius: var(--radius-sm);
  }
  .create-form button {
    background: var(--gold-primary);
    color: var(--bg-app);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0 var(--space-md);
    font-family: var(--font-ui);
    font-size: 12px;
    cursor: pointer;
  }
  .create-form button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .form-error {
    padding: var(--space-sm) var(--space-md);
    margin: 0 var(--space-md) var(--space-sm);
    border: 0.5px solid var(--accent-red);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    color: var(--accent-red);
    font-family: var(--font-ui);
    font-size: 11px;
  }
  .list {
    flex: 1;
    overflow-y: auto;
  }
  .collection-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-md);
    border-bottom: 0.5px solid var(--border-subtle);
    cursor: pointer;
    transition: background 0.15s;
    gap: var(--space-sm);
  }
  .collection-item:hover {
    background: var(--bg-elevated);
  }
  .collection-item.selected {
    background: var(--bg-elevated);
    border-left: 2px solid var(--gold-primary);
  }
  .collection-item.editing {
    background: var(--bg-elevated);
    cursor: default;
  }
  .col-name {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-base);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
    min-width: 0;
  }
  .rename-input {
    flex: 1;
    min-width: 0;
    background: var(--bg-app);
    border: 0.5px solid var(--gold-primary);
    color: var(--text-base);
    font-family: var(--font-ui);
    font-size: 13px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
  }
  .confirm-text {
    flex: 1;
    min-width: 0;
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-base);
  }
  .actions {
    display: none;
    gap: var(--space-xs);
  }
  .actions.inline {
    display: flex;
  }
  .collection-item:hover .actions {
    display: flex;
  }
  .action-btn {
    background: transparent;
    border: none;
    color: var(--text-ghost);
    cursor: pointer;
    font-family: var(--font-ui);
    font-size: 11px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
  }
  .action-btn:hover {
    color: var(--text-base);
    background: var(--bg-app);
  }
  .action-btn.delete:hover,
  .action-btn.destructive {
    color: var(--accent-red);
  }
  .action-btn.destructive:hover {
    background: var(--bg-app);
  }
  .empty-cta {
    padding: var(--space-xl) var(--space-md);
    text-align: center;
  }
  .empty-cta p {
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-dim);
    margin-bottom: var(--space-md);
  }
  .cta-btn {
    background: transparent;
    border: 0.5px solid var(--gold-primary);
    color: var(--gold-primary);
    padding: var(--space-sm) var(--space-lg);
    border-radius: var(--radius-md);
    font-family: var(--font-ui);
    font-size: 13px;
    cursor: pointer;
  }
  .cta-btn:hover {
    background: var(--gold-primary);
    color: var(--bg-app);
  }
</style>
