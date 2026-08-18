<!--
  src/lib/components/models/DeleteConfirmModal.svelte

  In-component modal asking the user to confirm deleting an Ollama
  model from disk. Used by `ModelRow.svelte`.

  Constraints (Req 13.6):
  - **No `window.alert` / `window.confirm` / `window.prompt`**.
  - Focus is trapped between the two action buttons while the modal
    is open.
  - Returns focus to the trigger button on close.
  - Indicates whether the model is currently loaded so the user
    understands the broader impact.
  - Zero hex; every colour comes from `src/app.css` CSS custom
    properties.
-->
<script lang="ts">
  import { onMount, tick } from 'svelte';

  interface Props {
    modelName: string;
    isLoaded: boolean;
    onConfirm: () => void;
    onCancel: () => void;
    triggerEl?: HTMLElement | null;
  }

  let {
    modelName,
    isLoaded,
    onConfirm,
    onCancel,
    triggerEl = null,
  }: Props = $props();

  let confirmBtn = $state<HTMLButtonElement | null>(null);
  let cancelBtn = $state<HTMLButtonElement | null>(null);

  onMount(() => {
    // Default focus to Cancel — destructive actions should require an
    // explicit reach.
    void tick().then(() => cancelBtn?.focus());
    return () => {
      try {
        triggerEl?.focus();
      } catch {
        /* trigger may be gone — ignore */
      }
    };
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onCancel();
      return;
    }
    if (e.key === 'Tab') {
      const target = e.target as HTMLElement | null;
      e.preventDefault();
      if (target === confirmBtn) {
        cancelBtn?.focus();
      } else {
        confirmBtn?.focus();
      }
    }
  }

  function handleBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      onCancel();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="modal-backdrop" onclick={handleBackdrop}>
  <div
    class="modal-dialog"
    role="alertdialog"
    aria-modal="true"
    aria-labelledby="delete-modal-title"
    aria-describedby="delete-modal-body"
  >
    <h3 class="modal-title" id="delete-modal-title">Delete model</h3>
    <p class="modal-body" id="delete-modal-body">
      Delete <span class="model-name">{modelName}</span> from local
      Ollama storage? You can re-pull it later, but its blobs will be
      removed immediately.
      {#if isLoaded}
        <span class="loaded-warning">
          This model is currently loaded — Ollama will unload it before
          deletion.
        </span>
      {/if}
    </p>
    <div class="modal-actions">
      <button
        type="button"
        class="action-btn"
        bind:this={cancelBtn}
        onclick={onCancel}
      >
        Cancel
      </button>
      <button
        type="button"
        class="action-btn danger"
        bind:this={confirmBtn}
        onclick={onConfirm}
      >
        Delete model
      </button>
    </div>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-app) 60%, transparent);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: var(--space-lg);
  }

  .modal-dialog {
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-dim);
    border-radius: var(--radius-lg);
    padding: var(--space-xl);
    max-width: 420px;
    width: 100%;
    box-shadow: var(--shadow-popover);
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
  }

  .modal-title {
    font-family: var(--font-brand);
    font-size: 14px;
    font-weight: 600;
    letter-spacing: 0.06em;
    color: var(--gold-primary);
    margin: 0;
  }

  .modal-body {
    font-family: var(--font-ui);
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-secondary);
    margin: 0;
  }

  .model-name {
    color: var(--text-primary);
    font-variant-numeric: tabular-nums;
  }

  .loaded-warning {
    display: block;
    margin-top: var(--space-xs);
    color: var(--status-warn-text);
    font-size: 11px;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-sm);
    margin-top: var(--space-sm);
  }

  .action-btn {
    font-family: var(--font-ui);
    font-size: 11px;
    padding: var(--space-xs) var(--space-md);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid var(--border-dim);
    background: transparent;
    color: var(--text-primary);
    transition: background 0.15s, border-color 0.15s, color 0.15s;
  }

  .action-btn:hover {
    border-color: var(--gold-dim);
    color: var(--gold-primary);
  }

  .action-btn:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .action-btn.danger {
    color: var(--accent-red);
    border-color: var(--accent-red);
  }

  .action-btn.danger:hover {
    background: var(--status-danger-bg);
    color: var(--accent-red);
  }
</style>
