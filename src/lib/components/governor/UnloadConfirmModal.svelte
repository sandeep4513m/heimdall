<!--
  src/lib/components/governor/UnloadConfirmModal.svelte

  In-component modal asking the user whether to cancel an in-flight
  chat stream and force-unload its model. Used by `ModelList.svelte`
  when `governor_unload_model({ name, force: false })` returns
  `Err("currently_streaming")` (Req 12.3).

  Constraints:
  - **No `window.alert` / `window.confirm` / `window.prompt`** (Req 11.10, 12.3)
  - Focus is trapped between the two action buttons while the modal is
    open. Focus returns to the trigger button on close.
  - Zero hex; every colour comes from `src/app.css` CSS custom
    properties.
-->
<script lang="ts">
  import { onMount, tick } from 'svelte';

  interface Props {
    modelName: string;
    onConfirm: () => void;
    onCancel: () => void;
    triggerEl?: HTMLElement | null;
  }

  let { modelName, onConfirm, onCancel, triggerEl = null }: Props = $props();

  let confirmBtn = $state<HTMLButtonElement | null>(null);
  let cancelBtn = $state<HTMLButtonElement | null>(null);
  let dialogEl = $state<HTMLDivElement | null>(null);

  onMount(() => {
    // Defer focus until after the dialog is in the DOM so screen
    // readers announce its label and the focus ring lands cleanly.
    void tick().then(() => confirmBtn?.focus());
    return () => {
      // Return focus to the trigger when the modal unmounts.
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
      // Tiny focus trap: only two interactive elements, so cycle
      // between them by hand.
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

<!-- The backdrop is a click-only sink; keyboard interactivity lives on
     the dialog. Suppressing svelte-a11y/click_events_have_key_events
     since the dialog handles all keyboard escape/tab paths. -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="modal-backdrop" onclick={handleBackdrop}>
  <div
    class="modal-dialog"
    role="alertdialog"
    aria-modal="true"
    aria-labelledby="unload-modal-title"
    aria-describedby="unload-modal-body"
    bind:this={dialogEl}
  >
    <h3 class="modal-title" id="unload-modal-title">
      Stream in progress
    </h3>
    <p class="modal-body" id="unload-modal-body">
      <span class="model-name">{modelName}</span> is currently emitting
      tokens. Unloading it now will end the active conversation
      mid-reply.
    </p>
    <div class="modal-actions">
      <button
        type="button"
        class="action-btn danger"
        bind:this={confirmBtn}
        onclick={onConfirm}
      >
        Cancel stream and unload
      </button>
      <button
        type="button"
        class="action-btn"
        bind:this={cancelBtn}
        onclick={onCancel}
      >
        Cancel
      </button>
    </div>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    /* Translucent overlay derived from the existing `--bg-app` token —
       no hardcoded color values. Modern WebKit (Tauri) supports
       `color-mix` natively. */
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
