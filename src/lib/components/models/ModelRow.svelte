<!--
  src/lib/components/models/ModelRow.svelte

  One row in the Models tab. Surfaces the model's name, formatted size,
  capability badges, family / parameter size / quantization, last-used
  time, and a hardware-aware recommendation. Action area exposes:

  - Set as chat default     — invokes `set_default_model` (gated on
    `text` or `tools` capability)
  - Set as vision default   — invokes `set_default_vision_model`
    (gated on `vision`)
  - Set as embedding default — invokes `set_default_embedding_model`
    (gated on `embedding`)
  - Delete                  — mounts `DeleteConfirmModal`; on confirm
    invokes `delete_model({ name })` then `modelsStore.markDeleted(name)`

  Errors render inline at the row, never as a global toast. The
  recommendation badge is rendered using the literal copy
  "fits comfortably" / "requires management" / "exceeds tier" — the
  three states map to the existing `--status-ok-*`, `--status-warn-*`,
  and `--accent-red` tokens.

  Zero hex; every colour from `src/app.css` CSS custom properties.
-->
<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { modelsStore } from '$lib/stores/models.svelte';
  import DeleteConfirmModal from './DeleteConfirmModal.svelte';
  import type { ModelsTabRow, ModelRecommendation } from '$lib/types/governor';
  import type { ModelCapabilities } from '$lib/types/model';

  interface Props {
    row: ModelsTabRow;
    loaded: boolean;
  }

  let { row, loaded }: Props = $props();

  let actionInFlight = $state<string | null>(null);
  let rowError = $state<string | null>(null);
  let confirmDelete = $state<{ trigger: HTMLElement | null } | null>(null);

  // Capability-gated affordances.
  let canChatDefault = $derived(canChat(row.capabilities));
  let canVisionDefault = $derived(canVision(row.capabilities));
  let canEmbeddingDefault = $derived(canEmbed(row.capabilities));

  function canChat(c: ModelCapabilities | null): boolean {
    if (!c) return false;
    return c.completion || c.tools;
  }
  function canVision(c: ModelCapabilities | null): boolean {
    return !!c?.vision;
  }
  function canEmbed(c: ModelCapabilities | null): boolean {
    return !!c?.embedding;
  }

  function formatSizeBytes(bytes: number): string {
    if (bytes <= 0) return '—';
    const mb = bytes / (1024 * 1024);
    if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
    return `${mb.toFixed(0)} MB`;
  }

  function formatLastUsed(unix: number | null): string {
    if (unix === null) return 'never used';
    const elapsed = Math.max(0, Math.floor(Date.now() / 1000 - unix));
    if (elapsed < 60) return `${elapsed}s ago`;
    if (elapsed < 3600) return `${Math.floor(elapsed / 60)}m ago`;
    if (elapsed < 86400) return `${Math.floor(elapsed / 3600)}h ago`;
    return `${Math.floor(elapsed / 86400)}d ago`;
  }

  function recommendationLabel(r: ModelRecommendation): string {
    switch (r) {
      case 'fits_comfortably':
        return 'fits comfortably';
      case 'requires_management':
        return 'requires management';
      case 'exceeds_tier':
        return 'exceeds tier';
      default:
        return r;
    }
  }

  function recommendationClass(r: ModelRecommendation): string {
    switch (r) {
      case 'fits_comfortably':
        return 'rec-ok';
      case 'requires_management':
        return 'rec-warn';
      case 'exceeds_tier':
        return 'rec-danger';
      default:
        return 'rec-ok';
    }
  }

  // Derive the small list of capability badges we display. Order is
  // stable: text, vision, thinking, embedding, tools.
  let capabilityBadges = $derived(buildBadges(row.capabilities));

  function buildBadges(c: ModelCapabilities | null): string[] {
    if (!c) return [];
    const out: string[] = [];
    if (c.completion) out.push('text');
    if (c.vision) out.push('vision');
    if (c.thinking) out.push('thinking');
    if (c.embedding) out.push('embedding');
    if (c.tools) out.push('tools');
    return out;
  }

  async function runAction<T>(key: string, fn: () => Promise<T>): Promise<void> {
    rowError = null;
    actionInFlight = key;
    try {
      await fn();
    } catch (err) {
      rowError = err instanceof Error ? err.message : String(err);
    } finally {
      actionInFlight = null;
    }
  }

  async function setChatDefault() {
    await runAction('chat', () =>
      invoke('set_default_model', { modelName: row.name }),
    );
  }
  async function setVisionDefault() {
    await runAction('vision', () =>
      invoke('set_default_vision_model', { modelName: row.name }),
    );
  }
  async function setEmbeddingDefault() {
    await runAction('embedding', () =>
      invoke('set_default_embedding_model', { modelName: row.name }),
    );
  }

  function openDeleteModal(ev: MouseEvent) {
    rowError = null;
    confirmDelete = { trigger: (ev.currentTarget as HTMLElement) ?? null };
  }

  async function handleConfirmDelete() {
    const target = confirmDelete;
    confirmDelete = null;
    if (!target) return;
    await runAction('delete', async () => {
      await invoke('delete_model', { modelName: row.name });
      await modelsStore.markDeleted(row.name);
    });
  }

  function handleCancelDelete() {
    confirmDelete = null;
  }
</script>

<article class="model-row" aria-label={row.name}>
  <header class="row-head">
    <div class="row-head-left">
      <span class="model-name" title={row.name}>{row.name}</span>
      {#if loaded}
        <span class="loaded-pill" title="Currently loaded by Ollama">
          loaded
        </span>
      {/if}
    </div>
    <span class="model-size" title="On-disk size">
      {formatSizeBytes(row.size)}
    </span>
  </header>

  <div class="row-meta">
    {#if capabilityBadges.length > 0}
      <div class="badge-row" aria-label="Capabilities">
        {#each capabilityBadges as cap (cap)}
          <span class="cap-badge">{cap}</span>
        {/each}
      </div>
    {:else}
      <span class="cap-loading">capabilities pending…</span>
    {/if}

    <span class="recommendation {recommendationClass(row.recommendation)}">
      {recommendationLabel(row.recommendation)}
    </span>
  </div>

  <dl class="model-details">
    {#if row.capabilities?.family}
      <div>
        <dt>family</dt>
        <dd>{row.capabilities.family}</dd>
      </div>
    {/if}
    {#if row.capabilities?.parameter_size}
      <div>
        <dt>params</dt>
        <dd>{row.capabilities.parameter_size}</dd>
      </div>
    {/if}
    {#if row.capabilities?.quantization_level}
      <div>
        <dt>quant</dt>
        <dd>{row.capabilities.quantization_level}</dd>
      </div>
    {/if}
    <div>
      <dt>last used</dt>
      <dd>{formatLastUsed(row.last_used_unix)}</dd>
    </div>
  </dl>

  <div class="row-actions">
    <button
      type="button"
      class="action-btn"
      disabled={!canChatDefault || actionInFlight !== null}
      onclick={setChatDefault}
      title={canChatDefault
        ? 'Set as default chat model'
        : 'Model has no text or tools capability'}
    >
      {actionInFlight === 'chat' ? 'Setting…' : 'Chat default'}
    </button>

    <button
      type="button"
      class="action-btn"
      disabled={!canVisionDefault || actionInFlight !== null}
      onclick={setVisionDefault}
      title={canVisionDefault
        ? 'Set as default vision model'
        : 'Model has no vision capability'}
    >
      {actionInFlight === 'vision' ? 'Setting…' : 'Vision default'}
    </button>

    <button
      type="button"
      class="action-btn"
      disabled={!canEmbeddingDefault || actionInFlight !== null}
      onclick={setEmbeddingDefault}
      title={canEmbeddingDefault
        ? 'Set as default embedding model'
        : 'Model has no embedding capability'}
    >
      {actionInFlight === 'embedding' ? 'Setting…' : 'Embed default'}
    </button>

    <button
      type="button"
      class="action-btn danger"
      disabled={actionInFlight !== null}
      onclick={openDeleteModal}
    >
      {actionInFlight === 'delete' ? 'Deleting…' : 'Delete'}
    </button>
  </div>

  {#if rowError}
    <p class="row-error" role="alert">
      {rowError}
    </p>
  {/if}
</article>

{#if confirmDelete}
  <DeleteConfirmModal
    modelName={row.name}
    isLoaded={loaded}
    onConfirm={handleConfirmDelete}
    onCancel={handleCancelDelete}
    triggerEl={confirmDelete.trigger}
  />
{/if}

<style>
  .model-row {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    padding: var(--space-md);
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }

  .row-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-md);
    min-width: 0;
  }

  .row-head-left {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    min-width: 0;
  }

  .model-name {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-primary);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .loaded-pill {
    font-family: var(--font-ui);
    font-size: 9px;
    padding: 1px var(--space-xs);
    border-radius: var(--radius-pill);
    background: var(--status-ok-bg);
    border: 0.5px solid var(--status-ok-border);
    color: var(--accent-green);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .model-size {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .row-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-sm);
    flex-wrap: wrap;
  }

  .badge-row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
  }

  .cap-badge {
    font-family: var(--font-ui);
    font-size: 9px;
    padding: 1px var(--space-xs);
    border-radius: var(--radius-pill);
    background: var(--bg-surface);
    border: 0.5px solid var(--border-dim);
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .cap-loading {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-ghost);
    font-style: italic;
  }

  .recommendation {
    font-family: var(--font-ui);
    font-size: 9px;
    padding: 1px var(--space-sm);
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

  .model-details {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-md);
    margin: 0;
    padding: var(--space-xs) 0;
    border-top: 0.5px solid var(--border-subtle);
  }

  .model-details > div {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .model-details dt {
    font-family: var(--font-ui);
    font-size: 9px;
    color: var(--text-ghost);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    margin: 0;
  }

  .model-details dd {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-secondary);
    margin: 0;
  }

  .row-actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
  }

  .action-btn {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 3px var(--space-sm);
    border-radius: var(--radius-pill);
    cursor: pointer;
    border: 0.5px solid var(--border-dim);
    background: transparent;
    color: var(--text-dim);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    transition: color 0.15s, border-color 0.15s, background 0.15s;
  }

  .action-btn:hover:not(:disabled) {
    border-color: var(--gold-dim);
    color: var(--gold-primary);
  }

  .action-btn:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .action-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .action-btn.danger {
    color: var(--accent-red);
    border-color: transparent;
  }

  .action-btn.danger:hover:not(:disabled) {
    border-color: var(--accent-red);
    background: var(--status-danger-bg);
    color: var(--accent-red);
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
