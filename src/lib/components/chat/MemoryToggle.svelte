<!-- src/lib/components/chat/MemoryToggle.svelte -->
<!-- Per-conversation memory toggle in the chat toolbar. -->
<script lang="ts">
  interface Props {
    enabled: boolean;
    onToggle: () => void;
  }

  let { enabled, onToggle }: Props = $props();
</script>

<button
  class="memory-toggle"
  class:enabled
  onclick={onToggle}
  title={enabled ? 'Memory enabled for this chat — click to disable' : 'Memory disabled for this chat — click to enable'}
  aria-label={enabled ? 'Disable memory for this conversation' : 'Enable memory for this conversation'}
  aria-pressed={enabled}
>
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M12 2a5 5 0 0 1 5 5v3a5 5 0 0 1 -10 0v-3a5 5 0 0 1 5 -5z"/>
    <path d="M8 14s1.5 2 4 2 4 -2 4 -2"/>
    <path d="M12 17v4"/>
    <path d="M9 21h6"/>
  </svg>
  <span class="toggle-label">Mem</span>
</button>

<style>
  .memory-toggle {
    display: flex;
    align-items: center;
    gap: 3px;
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 3px var(--space-xs);
    border-radius: var(--radius-sm);
    cursor: pointer;
    border: 0.5px solid var(--border-subtle);
    background: transparent;
    color: var(--text-ghost);
    letter-spacing: 0.04em;
    transition: color 0.15s, background 0.15s, border-color 0.15s;
  }

  .memory-toggle:hover {
    color: var(--text-dim);
    border-color: var(--border-dim);
  }

  .memory-toggle.enabled {
    color: var(--gold-primary);
    border-color: var(--gold-dim);
    background: var(--gold-bg);
  }

  .memory-toggle.enabled:hover {
    background: var(--bg-elevated);
  }

  .toggle-label {
    line-height: 1;
  }
</style>
