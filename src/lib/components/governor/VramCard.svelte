<!--
  src/lib/components/governor/VramCard.svelte

  VRAM resource card.
-->
<script lang="ts">
  import { vramStatus, vramTotal, vramUsed } from '$lib/stores/governor.svelte';

  function formatMb(v: number | null | undefined): string {
    if (v === null || v === undefined) return '—';
    if (v >= 1024) return `${(v / 1024).toFixed(1)} GB`;
    return `${v.toFixed(0)} MB`;
  }

  let usedFraction = $derived(
    vramTotal() && vramTotal()! > 0 && vramUsed() !== null && vramUsed() !== undefined
      ? Math.min(1, Math.max(0, vramUsed()! / vramTotal()!))
      : 0,
  );
</script>

<div class="vram-card" aria-label="VRAM">
  <span class="card-label">VRAM</span>

  {#if vramStatus() === 'ok'}
    <span class="card-value">{Math.round(usedFraction * 100)}%</span>
    <div
      class="meter"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={Math.round(usedFraction * 100)}
    >
      <div class="meter-fill" style:width="{usedFraction * 100}%"></div>
    </div>
    <span class="card-sub-label">{formatMb(vramUsed())} / {formatMb(vramTotal())}</span>
  {:else if vramStatus() === 'unavailable'}
    <span class="card-value-muted">VRAM: unavailable</span>
  {:else}
    <span class="card-value-muted">—</span>
  {/if}
</div>

<style>
  .vram-card {
    display: flex;
    flex-direction: column;
    padding: 10px 12px;
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    min-width: 0;
  }

  .card-label {
    font-family: var(--font-ui);
    font-size: 9px;
    letter-spacing: 0.2em;
    text-transform: uppercase;
    color: var(--text-ghost);
    margin-bottom: 5px;
  }

  .card-value {
    font-family: var(--font-ui);
    font-size: 20px;
    font-weight: 300;
    line-height: 1;
    margin-bottom: 7px;
    color: var(--status-ok-text);
  }

  .card-value-muted {
    font-family: var(--font-ui);
    font-size: 14px;
    font-weight: 300;
    color: var(--text-ghost);
  }

  .meter {
    width: 100%;
    height: 3px;
    background: var(--border-subtle);
    border-radius: 2px;
    overflow: hidden;
  }

  .meter-fill {
    height: 100%;
    border-radius: 2px;
    background: var(--status-ok-text);
    transition: width 0.2s ease-out;
  }

  .card-sub-label {
    font-family: var(--font-ui);
    font-size: 9px;
    color: var(--text-ghost);
    margin-top: 5px;
    letter-spacing: 0.06em;
  }
</style>
