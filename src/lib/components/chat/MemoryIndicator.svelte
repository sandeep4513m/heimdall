<!-- src/lib/components/chat/MemoryIndicator.svelte -->
<!-- Shows active confirmed fact count in the chat toolbar.
     Changes appearance at the soft warn (150 facts) and hard cap (200 facts)
     so users who never open the Memory panel still see the signal. -->
<script lang="ts">
  import { memoryStore } from '$lib/stores/memory.svelte';

  let tooltipText = $derived.by(() => {
    if (memoryStore.atHardCap) {
      return 'Memory full (200 facts) — open Memory panel to clean up';
    }
    if (memoryStore.showSoftWarning) {
      return `${memoryStore.factCount} memory facts active — getting full, consider reviewing`;
    }
    return `${memoryStore.factCount} memory ${memoryStore.factCount === 1 ? 'fact' : 'facts'} active`;
  });
</script>

{#if memoryStore.settings.global_enabled && memoryStore.factCount > 0}
  <span
    class="memory-indicator"
    class:warn={memoryStore.showSoftWarning}
    class:full={memoryStore.atHardCap}
    title={tooltipText}
    aria-label={tooltipText}
  >
    {memoryStore.factCount} mem
  </span>
{/if}

<style>
  .memory-indicator {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--gold-dim);
    letter-spacing: 0.04em;
    padding: 2px var(--space-xs);
    border-radius: var(--radius-sm);
    border: 0.5px solid var(--border-warm);
    background: var(--gold-bg);
    user-select: none;
  }

  .memory-indicator.warn {
    color: var(--status-warn-text);
    border-color: var(--status-warn-border);
    background: var(--status-warn-bg);
  }

  .memory-indicator.full {
    color: var(--accent-red);
    border-color: var(--accent-red);
    background: var(--status-danger-bg);
  }
</style>
