<!-- src/lib/components/memory/FactEditor.svelte -->
<!-- Inline text editor for a single fact. Emits save/cancel. -->
<script lang="ts">
  import { onMount } from 'svelte';

  interface Props {
    initialText: string;
    onSave: (text: string) => void;
    onCancel: () => void;
  }

  let { initialText, onSave, onCancel }: Props = $props();

  let text = $state(initialText);
  let inputEl = $state<HTMLTextAreaElement>(undefined!);

  onMount(() => {
    inputEl?.focus();
    inputEl?.select();
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      save();
    } else if (e.key === 'Escape') {
      onCancel();
    }
  }

  function save() {
    const trimmed = text.trim();
    if (trimmed.length > 0) {
      onSave(trimmed);
    }
  }
</script>

<div class="fact-editor">
  <textarea
    bind:this={inputEl}
    bind:value={text}
    onkeydown={handleKeydown}
    class="editor-input"
    rows={2}
    aria-label="Edit fact text"
  ></textarea>
  <div class="editor-actions">
    <button class="btn-save" onclick={save} disabled={text.trim().length === 0}>
      Save
    </button>
    <button class="btn-cancel" onclick={onCancel}>
      Cancel
    </button>
  </div>
</div>

<style>
  .fact-editor {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    width: 100%;
  }

  .editor-input {
    width: 100%;
    background: var(--bg-app);
    border: 0.5px solid var(--gold-dim);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-family: var(--font-ui);
    font-size: 12px;
    padding: var(--space-xs) var(--space-sm);
    resize: vertical;
    outline: none;
    line-height: 1.5;
  }

  .editor-input:focus {
    border-color: var(--gold-primary);
  }

  .editor-actions {
    display: flex;
    gap: var(--space-xs);
  }

  .btn-save, .btn-cancel {
    font-family: var(--font-ui);
    font-size: 11px;
    padding: 2px var(--space-sm);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid transparent;
    transition: background 0.1s, color 0.1s;
  }

  .btn-save {
    background: var(--gold-bg);
    border-color: var(--gold-dim);
    color: var(--gold-primary);
  }

  .btn-save:hover:not(:disabled) {
    background: var(--gold-dim);
    color: var(--text-primary);
  }

  .btn-save:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .btn-cancel {
    background: transparent;
    border-color: var(--border-subtle);
    color: var(--text-dim);
  }

  .btn-cancel:hover {
    color: var(--text-primary);
    border-color: var(--border-dim);
  }
</style>
