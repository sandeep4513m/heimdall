// src/lib/stores/governor.svelte.ts
//
// Svelte 5 runes store for the Phase 6 Governor. One `$state` slot for
// the latest `GovernorMetrics` snapshot, one for the current
// "embedding swap" hint (used by ChatPanel to render the
// "model reloading" indicator), and a flag tracking whether we are
// currently inside a Critical pressure event.
//
// Re-renders are granular: components import the `$derived` slices they
// care about rather than the whole metrics object, so a per-tick update
// only re-renders the text nodes that actually changed (Req 11.7, 11.8).
// A single full re-render on `effective_tier` change is gated by
// `GovernorPanel.svelte` via `{#key tierKey}` (Req 11.9).
//
// Defensive parse-error: a malformed `governor://metrics` payload logs
// one console warn and keeps the last good state (P1 makes this
// impossible in practice, but the guard is the defence-in-depth).
//
// Out-of-order `critical_cleared` arriving without a matching `critical`
// is ignored; the next metrics tick is the source of truth.

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  GovernorMetrics,
  CriticalEvent,
  NoCandidatesEvent,
  EmbeddingSwapEvent,
  RiskState,
  HardwareTier,
  VramStatus,
  RunningModel,
  GovernorThresholds,
} from '$lib/types/governor';

const DEFAULT_THRESHOLDS: GovernorThresholds = {
  warn_mb: 0,
  unload_mb: 0,
  critical_mb: 0,
};

class GovernorStore {
  /// Latest metrics snapshot. `null` until the first
  /// `governor://metrics` event arrives (within ~2 s of bootstrap).
  metrics = $state<GovernorMetrics | null>(null);

  /// Phase identifier of the most recent `embedding_swap` event, or
  /// `null` when none is active. ChatPanel reads this to render the
  /// "model reloading" indicator next to the input bar (Req 10.10).
  swapPhase = $state<EmbeddingSwapEvent | null>(null);

  /// True between `governor://critical` and `governor://critical_cleared`.
  /// Out-of-order `cleared` arriving without a matching `critical` is
  /// ignored — `inCritical` is also re-derived implicitly from
  /// `metrics.risk_state` on the next tick.
  inCritical = $state<boolean>(false);

  /// Detail of the last `governor://critical` event, surfaced as a
  /// banner. Cleared on `critical_cleared`.
  lastCritical = $state<CriticalEvent | null>(null);

  /// Detail of the last `governor://no_candidates` event. Stale entries
  /// are cleared on the next tick whose risk_state is Calm or Warn.
  lastNoCandidates = $state<NoCandidatesEvent | null>(null);

  private unlisteners: UnlistenFn[] = [];
  private subscribed = false;

  /// Subscribe to all five Governor events. Idempotent — calling twice
  /// is a no-op. Call once from `+page.svelte` (or another long-lived
  /// component) so the store keeps receiving events even when the
  /// Governor panel itself is hidden.
  async startListening(): Promise<void> {
    if (this.subscribed) return;
    this.subscribed = true;

    const u1 = await listen<GovernorMetrics>('governor://metrics', (e) => {
      try {
        // P1 guarantees the payload round-trips, but we still validate
        // the bare shape so a backend bug never wipes the panel.
        const p = e.payload;
        if (
          !p ||
          typeof p.total_ram_mb !== 'number' ||
          typeof p.available_ram_mb !== 'number' ||
          !Array.isArray(p.loaded_models)
        ) {
          // eslint-disable-next-line no-console
          console.warn('governor://metrics: malformed payload, keeping last good state');
          return;
        }
        this.metrics = p;
        // Auto-clear stale `no_candidates` once pressure abates.
        if (p.risk_state === 'calm' || p.risk_state === 'warn') {
          this.lastNoCandidates = null;
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn('governor://metrics: parse error, keeping last good state', err);
      }
    });

    const u2 = await listen<CriticalEvent>('governor://critical', (e) => {
      this.inCritical = true;
      this.lastCritical = e.payload;
    });

    const u3 = await listen<null>('governor://critical_cleared', () => {
      // Out-of-order: ignore a clear that arrives without a matching
      // critical. The next metrics tick is the source of truth either
      // way.
      if (!this.inCritical) return;
      this.inCritical = false;
      this.lastCritical = null;
    });

    const u4 = await listen<NoCandidatesEvent>('governor://no_candidates', (e) => {
      this.lastNoCandidates = e.payload;
    });

    const u5 = await listen<EmbeddingSwapEvent>('governor://embedding_swap', (e) => {
      this.swapPhase = e.payload;
      // Reload signal clears as soon as the next chat token starts
      // streaming — ChatPanel calls `clearSwapPhase()` for that.
    });

    this.unlisteners.push(u1, u2, u3, u4, u5);
  }

  /// Tear down every event listener. Rarely needed because the store is
  /// process-wide, but provided for symmetry with `memoryStore`.
  stopListening(): void {
    for (const u of this.unlisteners) {
      try {
        u();
      } catch {
        /* already removed */
      }
    }
    this.unlisteners = [];
    this.subscribed = false;
  }

  /// ChatPanel calls this once the first token of the assistant
  /// response arrives, dismissing the "model reloading" indicator.
  clearSwapPhase(): void {
    this.swapPhase = null;
  }
}

export const governorStore = new GovernorStore();

// ── Granular `$derived` slices ──────────────────────────────────────
// Svelte 5 module files cannot export `$derived` values directly
// (derived_invalid_export). Export getter functions instead so
// components call e.g. `ramAvailable()` to read the current value.
// The getters are reactive — Svelte tracks the `governorStore.metrics`
// dependency through the function call just as it would through a
// direct `$derived` reference.

export function ramAvailable(): number {
  return governorStore.metrics?.available_ram_mb ?? 0;
}
export function ramTotal(): number {
  return governorStore.metrics?.total_ram_mb ?? 0;
}
export function swapTotal(): number {
  return governorStore.metrics?.swap_total_mb ?? 0;
}
export function swapUsed(): number {
  return governorStore.metrics?.swap_used_mb ?? 0;
}
export function cpuAggregate(): number {
  return governorStore.metrics?.cpu_aggregate_percent ?? 0;
}
export function loadedModels(): RunningModel[] {
  return governorStore.metrics?.loaded_models ?? [];
}
export function vramStatus(): VramStatus {
  return governorStore.metrics?.vram_status ?? 'absent';
}
export function vramTotal(): number | null {
  return governorStore.metrics?.vram_total_mb ?? null;
}
export function vramUsed(): number | null {
  return governorStore.metrics?.vram_used_mb ?? null;
}
export function riskState(): RiskState {
  return governorStore.metrics?.risk_state ?? 'calm';
}
export function effectiveTier(): HardwareTier {
  return governorStore.metrics?.effective_tier ?? 'minimal';
}
export function detectedTier(): HardwareTier {
  return governorStore.metrics?.detected_tier ?? 'minimal';
}
export function thresholds(): GovernorThresholds {
  return governorStore.metrics?.thresholds ?? DEFAULT_THRESHOLDS;
}
