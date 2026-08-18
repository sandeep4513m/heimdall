<!--
  src/lib/components/governor/ResourceCard.svelte

  One resource card item — RAM, Swap, CPU.
-->
<script lang="ts">
  interface Props {
    label: string;
    used?: number | null;
    total?: number | null;
    percent?: number | null;
    rss_mb?: number | null;
    ariaLabel?: string;
    warn?: boolean;
  }

  let {
    label,
    used = null,
    total = null,
    percent = null,
    rss_mb = null,
    ariaLabel,
    warn = false,
  }: Props = $props();

  function formatMb(v: number | null | undefined): string {
    if (v === null || v === undefined) return '—';
    if (v >= 1024) return `${(v / 1024).toFixed(1)} GB`;
    return `${v.toFixed(0)} MB`;
  }

  function formatPercent(v: number | null | undefined): string {
    if (v === null || v === undefined) return '—';
    return `${v.toFixed(0)}%`;
  }

  let mode = $derived(
    used !== null && used !== undefined && total !== null && total !== undefined
      ? 'pair'
      : percent !== null && percent !== undefined
        ? 'percent'
        : 'rss',
  );

  let usedFraction = $derived(
    mode === 'pair' && total && total > 0 && used !== null && used !== undefined
      ? Math.min(1, Math.max(0, used / total))
      : 0,
  );

  let pairText = $derived(`${formatMb(used)} / ${formatMb(total)}`);
  let percentText = $derived(formatPercent(percent));
  let rssText = $derived(formatMb(rss_mb));
</script>

<div class="resource-card" aria-label={ariaLabel ?? label}>
  <span class="card-label">{label}</span>

  {#if mode === 'pair'}
    <span class="card-value" class:warn>{Math.round(usedFraction * 100)}%</span>
    <div
      class="meter"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={Math.round(usedFraction * 100)}
    >
      <div class="meter-fill" class:warn style:width="{usedFraction * 100}%"></div>
    </div>
    <span class="card-sub-label">{pairText}</span>
  {:else if mode === 'percent'}
    <span class="card-value" class:warn>{percentText}</span>
    <div
      class="meter"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={Math.round(percent ?? 0)}
    >
      <div class="meter-fill" class:warn style:width="{Math.max(0, Math.min(100, percent ?? 0))}%"></div>
    </div>
    <span class="card-sub-label"></span>
  {:else}
    <span class="card-value" class:warn>{rssText}</span>
  {/if}
</div>

<style>
  .resource-card {
    display: flex;
    flex-direction: column;
    padding: 10px 12px;
    background: #0d1017;
    border: 0.5px solid #1e2535;
    border-radius: 8px;
    min-width: 0;
  }

  .card-label {
    font-family: var(--font-ui);
    font-size: 9px;
    letter-spacing: 0.2em;
    text-transform: uppercase;
    color: #4a5068;
    margin-bottom: 5px;
  }

  .card-value {
    font-family: var(--font-ui);
    font-size: 20px;
    font-weight: 300;
    line-height: 1;
    margin-bottom: 7px;
    color: #4a9e6a;
  }
  .card-value.warn {
    color: #c8832a;
  }

  .meter {
    width: 100%;
    height: 3px;
    background: #1e2535;
    border-radius: 2px;
    overflow: hidden;
  }

  .meter-fill {
    height: 100%;
    border-radius: 2px;
    background: #4a9e6a;
    transition: width 0.2s ease-out;
  }
  .meter-fill.warn {
    background: #c8832a;
  }

  .card-sub-label {
    font-family: var(--font-ui);
    font-size: 9px;
    color: #4a5068;
    margin-top: 5px;
    letter-spacing: 0.06em;
  }
</style>
