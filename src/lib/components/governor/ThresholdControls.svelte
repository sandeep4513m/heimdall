<!--
  src/lib/components/governor/ThresholdControls.svelte

  Three numeric sliders for warn / unload / critical of the active
  tier, plus a manual tier-override picker ("Auto-detect" + the three
  tier names). Edits persist via the Tauri commands
  `governor_set_thresholds` and `set_tier_override`.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import {
    effectiveTier,
    detectedTier,
    thresholds,
    ramTotal,
  } from "$lib/stores/governor.svelte";
  import type { HardwareTier } from "$lib/types/governor";

  let warnEdit = $state<number>(thresholds().warn_mb);
  let unloadEdit = $state<number>(thresholds().unload_mb);
  let criticalEdit = $state<number>(thresholds().critical_mb);

  let lastSyncKey = $state<string>("");
  $effect(() => {
    const t = thresholds();
    const key = `${effectiveTier()}/${t.warn_mb}/${t.unload_mb}/${t.critical_mb}`;
    if (key !== lastSyncKey) {
      lastSyncKey = key;
      warnEdit = t.warn_mb;
      unloadEdit = t.unload_mb;
      criticalEdit = t.critical_mb;
    }
  });

  let validationError = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  let saving = $state<boolean>(false);

  let sliderMax = $derived(Math.max(ramTotal() || 8192, 256));

  function validate(w: number, u: number, c: number): string | null {
    if (c <= 0) return "critical must be greater than zero";
    if (u < c) return "unload must be ≥ critical";
    if (w < u) return "warn must be ≥ unload";
    return null;
  }

  // Reactively run validation whenever edits change
  $effect(() => {
    validationError = validate(warnEdit, unloadEdit, criticalEdit);
  });

  let dirty = $derived(
    warnEdit !== thresholds().warn_mb ||
      unloadEdit !== thresholds().unload_mb ||
      criticalEdit !== thresholds().critical_mb,
  );

  async function handleSaveThresholds() {
    const err = validate(warnEdit, unloadEdit, criticalEdit);
    if (err) {
      validationError = err;
      return;
    }
    saving = true;
    saveError = null;
    try {
      await invoke("governor_set_thresholds", {
        tier: effectiveTier(),
        warnMb: warnEdit,
        unloadMb: unloadEdit,
        criticalMb: criticalEdit,
      });
    } catch (e) {
      saveError = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  type OverrideChoice = "auto" | HardwareTier;

  let overrideChoice = $state<OverrideChoice>("auto");
  let restartHint = $state<boolean>(false);
  let overrideError = $state<string | null>(null);

  $effect(() => {
    if (effectiveTier() === detectedTier()) {
      overrideChoice = "auto";
    } else {
      overrideChoice = effectiveTier();
    }
  });

  async function handleOverrideChange(e: Event) {
    const v = (e.currentTarget as HTMLSelectElement).value as OverrideChoice;
    overrideChoice = v;
    overrideError = null;
    try {
      await invoke("set_tier_override", {
        tier: v === "auto" ? null : v,
      });
      restartHint = true;
    } catch (err) {
      overrideError = err instanceof Error ? err.message : String(err);
    }
  }

  // Helper for computing track fill percentage dynamically
  function getPercent(val: number, min: number, max: number): number {
    const denom = max - min;
    if (denom <= 0) return 0;
    return Math.max(0, Math.min(100, ((val - min) / denom) * 100));
  }
</script>

<section class="threshold-controls" id="threshold-controls">
  <header class="section-header">
    <span class="section-title">Thresholds — {effectiveTier()}</span>
  </header>

  <div class="slider-row">
    <label class="slider-label" for="warn-mb">
      Warn
      <span class="slider-value">{warnEdit} MB</span>
    </label>
    <input
      id="warn-mb"
      type="range"
      min={Math.max(criticalEdit + 2, 32)}
      max={sliderMax}
      step="32"
      bind:value={warnEdit}
      style="background: linear-gradient(to right, var(--status-warn-text) {getPercent(
        warnEdit,
        Math.max(criticalEdit + 2, 32),
        sliderMax,
      )}%, var(--border-subtle) {getPercent(
        warnEdit,
        Math.max(criticalEdit + 2, 32),
        sliderMax,
      )}%);"
    />
  </div>

  <div class="slider-row">
    <label class="slider-label" for="unload-mb">
      Unload
      <span class="slider-value">{unloadEdit} MB</span>
    </label>
    <input
      id="unload-mb"
      type="range"
      min={Math.max(criticalEdit + 1, 16)}
      max={Math.max(warnEdit, 32)}
      step="16"
      bind:value={unloadEdit}
      style="background: linear-gradient(to right, var(--status-warn-text) {getPercent(
        unloadEdit,
        Math.max(criticalEdit + 1, 16),
        Math.max(warnEdit, 32),
      )}% , var(--border-subtle) {getPercent(
        unloadEdit,
        Math.max(criticalEdit + 1, 16),
        Math.max(warnEdit, 32),
      )}%);"
    />
  </div>

  <div class="slider-row">
    <label class="slider-label" for="critical-mb">
      Critical
      <span class="slider-value">{criticalEdit} MB</span>
    </label>
    <input
      id="critical-mb"
      type="range"
      min="1"
      max={Math.max(unloadEdit - 1, 8)}
      step="8"
      bind:value={criticalEdit}
      style="background: linear-gradient(to right, var(--status-warn-text) {getPercent(
        criticalEdit,
        1,
        Math.max(unloadEdit - 1, 8),
      )}% , var(--border-subtle) {getPercent(
        criticalEdit,
        1,
        Math.max(unloadEdit - 1, 8),
      )}%);"
    />
  </div>

  {#if validationError}
    <p class="inline-error" role="alert">{validationError}</p>
  {/if}
  {#if saveError}
    <p class="inline-error" role="alert">{saveError}</p>
  {/if}

  <div class="override-row">
    <label class="override-label" for="tier-override"> Tier override </label>
    <select
      id="tier-override"
      value={overrideChoice}
      onchange={handleOverrideChange}
    >
      <option value="auto">Auto-detect ({detectedTier()})</option>
      <option value="minimal">Minimal</option>
      <option value="standard">Standard</option>
      <option value="full">Full</option>
    </select>
  </div>

  {#if restartHint}
    <p class="hint" role="status">
      Restart Heimdall to apply the new tier across every subsystem.
    </p>
  {/if}
  {#if overrideError}
    <p class="inline-error" role="alert">{overrideError}</p>
  {/if}

  {#if dirty}
    <button
      type="button"
      class="save-btn"
      disabled={saving || validationError !== null}
      onclick={handleSaveThresholds}
    >
      {#if saving}
        Saving…
      {:else}
        ✓ Apply
      {/if}
    </button>
  {/if}
</section>

<style>
  .threshold-controls {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    padding: var(--space-md);
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-lg);
  }

  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: var(--space-xs);
    border-bottom: 0.5px solid var(--border-subtle);
  }

  .section-title {
    font-family: var(--font-ui);
    font-size: 9px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  .save-btn {
    display: block;
    width: 100%;
    text-align: center;
    background: var(--gold-bg);
    border: 0.5px solid var(--border-warm);
    color: var(--gold-primary);
    border-radius: var(--radius-md);
    padding: 8px;
    font-family: var(--font-ui);
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    cursor: pointer;
    transition: opacity 0.15s;
    margin-top: 8px;
  }

  .save-btn:hover:not(:disabled) {
    opacity: 0.85;
  }

  .save-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .slider-row {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }

  .slider-label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--text-dim);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .slider-value {
    color: var(--text-primary);
    font-variant-numeric: tabular-nums;
  }

  input[type="range"] {
    -webkit-appearance: none;
    appearance: none;
    width: 100%;
    height: 4px;
    border-radius: 2px;
    outline: none;
    margin: 8px 0;
    accent-color: var(--status-warn-text);
  }

  input[type="range"]::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #ffffff;
    cursor: pointer;
    border: none;
    margin-top: 0px;
  }

  input[type="range"]::-moz-range-thumb {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #ffffff;
    cursor: pointer;
    border: none;
  }

  input[type="range"]:focus-visible {
    outline: 1px solid var(--status-warn-text);
    outline-offset: 2px;
  }

  .inline-error {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--accent-red);
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    background: var(--status-danger-bg);
    border: 0.5px solid var(--accent-red);
    border-radius: var(--radius-sm);
  }

  .override-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    border-top: 0.5px solid var(--border-subtle);
    padding-top: 12px;
    margin-top: 12px;
  }

  .override-label {
    font-family: var(--font-ui);
    font-size: 9px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  select {
    -webkit-appearance: none;
    -moz-appearance: none;
    appearance: none;
    background: var(--bg-elevated) !important;
    background-color: var(--bg-elevated) !important;
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
    color: var(--text-dim) !important;
    font-family: var(--font-ui);
    font-size: 10px;
    padding: 4px 24px 4px 10px;
    outline: none;
    cursor: pointer;
    background-image: url("data:image/svg+xml;charset=UTF-8,%3csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238a8fa8' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3e%3cpolyline points='6 9 12 15 18 9'%3e%3c/polyline%3e%3c/svg%3e");
    background-repeat: no-repeat;
    background-position: right 8px center;
    background-size: 10px;
  }

  select option {
    background: var(--bg-elevated) !important;
    background-color: var(--bg-elevated) !important;
    color: var(--text-dim) !important;
  }

  select:focus-visible {
    border-color: var(--status-warn-text);
  }

  .hint {
    font-family: var(--font-ui);
    font-size: 10px;
    color: var(--gold-primary);
    margin: 0;
    padding: var(--space-xs) var(--space-sm);
    background: var(--gold-bg);
    border: 0.5px solid var(--gold-dim);
    border-radius: var(--radius-sm);
  }
</style>
