<!--
  src/lib/components/governor/GovernorPanel.svelte

  Phase 6 Governor panel — always-mounted, hidden via CSS when another
  panel is active so live metrics state survives navigation. Reads
  granular `$derived` slices from `governor.svelte.ts` so each child
  re-renders independently when only its slice changes (Req 11.7,
  11.8). A single full re-render is gated on `effective_tier` changes
  via `{#key tierKey}` (Req 11.9).

  Hero indicator surfaces `available_ram_mb` / `total_ram_mb` and the
  current `risk_state` via the existing `--status-*` and `--accent-red`
  tokens. Critical adds a 1.5 s pulse like the existing `nav-dot`.

  When `effective_tier !== detected_tier` a focusable "tier overridden"
  button is rendered; clicking it scrolls focus to ThresholdControls
  via `aria-controls` (Req 11.5).

  An `embedding_swap{phase: ReloadingChat}` event is surfaced inline
  here as a "model reloading" hint (Req 10.10). ChatPanel owns its own
  inline indicator via `governorStore.swapPhase`.

  **Zero hex** — every colour comes from `src/app.css` CSS custom
  properties.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import {
    governorStore,
    ramAvailable,
    ramTotal,
    swapTotal,
    swapUsed,
    cpuAggregate,
    vramStatus,
    riskState,
    effectiveTier,
    detectedTier,
  } from '$lib/stores/governor.svelte';
  import ResourceCard from './ResourceCard.svelte';
  import VramCard from './VramCard.svelte';
  import ModelList from './ModelList.svelte';
  import ThresholdControls from './ThresholdControls.svelte';

  // Tier-change re-render trigger. Numeric metric ticks do NOT bump
  // `tierKey`; only an `effective_tier` change does.
  let tierKey = $state<number>(0);
  let lastTier = $state<string | null>(null);

  $effect(() => {
    if (lastTier === null) {
      lastTier = effectiveTier();
      return;
    }
    if (lastTier !== effectiveTier()) {
      lastTier = effectiveTier();
      tierKey += 1;
    }
  });

  // The store is started here so the panel works even when mounted as
  // the first interactive surface. `startListening` is idempotent.
  onMount(() => {
    void governorStore.startListening();
  });

  // Loading skeleton until the first metrics event lands.
  let isLoading = $derived(governorStore.metrics === null);

  // RAM breakdown. Free (available) is the number that decides whether
  // another model can load, so it's the prominent figure. Used is
  // derived as total − available and clamped at zero for safety.
  let ramUsed = $derived(Math.max(0, ramTotal() - ramAvailable()));



  // Reloading-chat hint (Req 10.10) — visible while the chat model is
  // being transparently reloaded after an embedding swap.
  let showReloadHint = $derived(
    governorStore.swapPhase?.phase === 'reloading_chat',
  );

  function focusOverrideControl() {
    const el = document.getElementById('tier-override');
    if (el) {
      el.scrollIntoView({ block: 'center', behavior: 'smooth' });
      try {
        (el as HTMLElement).focus();
      } catch {
        /* not focusable in some sandboxes */
      }
    }
  }
</script>

<div class="governor-panel">
  <header class="panel-header">
    <h2 class="panel-title">Governor</h2>
    <div class="header-meta">
      <span class="tier-badge" aria-label="Effective tier">
        {effectiveTier()}
      </span>
      {#if effectiveTier() !== detectedTier()}
        <button
          type="button"
          class="tier-override"
          aria-controls="threshold-controls"
          onclick={focusOverrideControl}
          title="Effective tier differs from detected ({detectedTier()}). Open override control."
        >
          tier overridden
        </button>
      {/if}
    </div>
  </header>

  {#if isLoading}
    <p class="skeleton">Heimdall is polling — first reading in a moment…</p>
  {:else}
    {#key tierKey}
      <div class="hero" role="status" aria-live="polite">
        <div class="hero-top-row">
          <div class="hero-free-group">
            <span class="hero-value" class:warn={riskState() !== 'calm'}>{ramAvailable()}</span>
            <span class="hero-free-unit">MB free</span>
          </div>
          <span class="status-badge" class:warn={riskState() !== 'calm'}>
            {riskState() === 'calm' ? 'COMFORTABLE' : 'WARNING'}
          </span>
        </div>
        <div class="hero-breakdown">
          <div class="hero-stat">
            <span class="hero-stat-label">Used</span>
            <span class="hero-stat-value">{ramUsed} MB</span>
          </div>
          <div class="hero-stat">
            <span class="hero-stat-label">Total</span>
            <span class="hero-stat-value">{ramTotal()} MB</span>
          </div>
          <div class="hero-stat">
            <span class="hero-stat-label">Free</span>
            <span class="hero-stat-value">{ramAvailable()} MB</span>
          </div>
        </div>
      </div>

      {#if showReloadHint}
        <p class="reload-hint" role="status">
          Model reloading after embedding swap…
        </p>
      {/if}

      <div class="resource-grid">
        <ResourceCard
          label="RAM"
          used={ramUsed}
          total={ramTotal()}
          warn={riskState() !== 'calm'}
        />
        <ResourceCard label="Swap" used={swapUsed()} total={swapTotal()} />
        <ResourceCard label="CPU" percent={cpuAggregate()} />
      </div>

      {#if vramStatus() !== 'absent'}
        <VramCard />
      {/if}

      <ModelList />
      <ThresholdControls />
    {/key}
  {/if}
</div>

<style>
  .governor-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
    height: 100%;
    overflow-y: auto;
    background: var(--bg-app);
    padding: var(--space-md) var(--space-lg);
  }

  .panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-shrink: 0;
  }

  .panel-title {
    font-family: var(--font-brand);
    font-size: 14px;
    font-weight: 600;
    letter-spacing: 0.08em;
    color: var(--gold-primary);
    margin: 0;
  }

  .header-meta {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .tier-badge {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 2px var(--space-sm);
    border-radius: var(--radius-pill);
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-dim);
    color: var(--text-dim);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .tier-override {
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 2px var(--space-sm);
    border-radius: var(--radius-pill);
    cursor: pointer;
    background: var(--gold-bg);
    border: 0.5px solid var(--gold-dim);
    color: var(--gold-primary);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    transition: border-color 0.15s, color 0.15s;
  }

  .tier-override:hover {
    border-color: var(--gold-primary);
  }

  .tier-override:focus-visible {
    outline: 1px solid var(--gold-primary);
    outline-offset: 2px;
  }

  .skeleton {
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-dim);
    padding: var(--space-xl);
    text-align: center;
    background: var(--bg-surface);
    border: 0.5px dashed var(--border-subtle);
    border-radius: var(--radius-md);
  }

  .hero {
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .hero-top-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .hero-free-group {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }

  .hero-value {
    font-family: var(--font-brand);
    font-size: 32px;
    font-weight: 300;
    line-height: 1;
    color: var(--status-ok-text);
  }
  .hero-value.warn {
    color: var(--status-warn-text);
  }

  .hero-free-unit {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-ghost);
  }

  .status-badge {
    font-family: var(--font-ui);
    font-size: 9px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    background: var(--status-ok-bg);
    border: 0.5px solid var(--status-ok-border);
    color: var(--status-ok-text);
    letter-spacing: 0.05em;
  }
  .status-badge.warn {
    background: var(--status-warn-bg);
    border: 0.5px solid var(--status-warn-border);
    color: var(--status-warn-text);
  }

  .hero-breakdown {
    display: flex;
    gap: var(--space-lg);
  }

  .hero-stat {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .hero-stat-label {
    font-family: var(--font-ui);
    font-size: 9px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-ghost);
  }

  .hero-stat-value {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-dim);
  }

  .reload-hint {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--gold-primary);
    margin: 0;
    padding: var(--space-xs) var(--space-md);
    background: var(--gold-bg);
    border: 0.5px solid var(--gold-dim);
    border-radius: var(--radius-sm);
  }

  .resource-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: var(--space-sm);
  }
</style>
