<!--
  src/lib/components/knowledge/IngestionPressurePreview.svelte

  Predictive ingestion-pressure preview.

  Renders a traffic-light summary of whether the embedding model can load
  for an upcoming ingestion, driven by the gated `governor_preview_ingestion`
  Tauri command (`IngestionFitPreview`). Three live states:

    - green → "Ready to ingest"     (FitsAlongside)
    - amber → "Chat model will swap" (RequiresChatUnload)
    - red   → "Not enough memory"    (InsufficientEvenAlone)

  Plus a `disabled` fall-through (feature flag off) which renders nothing.

  This component is flag-gated and optional — it is NOT mounted into
  KnowledgePanel by default. It compiles clean under `npm run check`.

  Three Laws: zero hardcoded hex. Every colour comes from `src/app.css`
  via the `--status-ok-*`, `--status-warn-*`, and `--status-danger-*`
  custom properties. Svelte 5 runes mode.
-->
<script lang="ts">
  import type { IngestionFitPreview, IngestionFitStatus } from '$lib/types/governor';

  // Optional `preview` prop: when omitted the component renders its idle
  // placeholder. Parents pass the result of `governor_preview_ingestion`.
  let { preview = null }: { preview?: IngestionFitPreview | null } = $props();

  // Human-readable headline per status.
  const HEADLINES: Record<IngestionFitStatus, string> = {
    green: 'Ready to ingest',
    amber: 'Chat model will make room',
    red: 'Not enough memory',
    disabled: '',
  };

  let status = $derived<IngestionFitStatus>(preview?.status ?? 'disabled');
  let headline = $derived(HEADLINES[status]);

  function fmtMb(v: number | null | undefined): string {
    if (v === null || v === undefined) return '—';
    if (v >= 1024) return `${(v / 1024).toFixed(1)} GB`;
    return `${v} MB`;
  }
</script>

{#if preview && status !== 'disabled'}
  <div
    class="pressure-preview"
    class:is-green={status === 'green'}
    class:is-amber={status === 'amber'}
    class:is-red={status === 'red'}
    role="status"
    aria-live="polite"
  >
    <div class="dot" aria-hidden="true"></div>
    <div class="body">
      <span class="headline">{headline}</span>
      <span class="detail">
        embedding {fmtMb(preview.embedding_mb)}
        {#if preview.chat_mb > 0}
          · chat {fmtMb(preview.chat_mb)}
        {/if}
        · budget {fmtMb(preview.budget_mb)} of {fmtMb(preview.available_mb)}
      </span>
    </div>
  </div>
{/if}

<style>
  .pressure-preview {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    padding: var(--space-md);
    border-radius: var(--radius-md);
    border: 0.5px solid var(--border-subtle);
    background: var(--bg-elevated);
  }

  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    margin-top: 3px;
    border-radius: var(--radius-sm);
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    min-width: 0;
  }

  .headline {
    font-family: var(--font-ui);
    font-size: 13px;
    letter-spacing: 0.01em;
  }

  .detail {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  /* ── Status variants — colours strictly from app.css tokens ───────── */
  .is-green {
    border-color: var(--status-ok-border);
    background: var(--status-ok-bg);
  }
  .is-green .dot {
    background: var(--status-ok-text);
  }
  .is-green .headline {
    color: var(--status-ok-text);
  }

  .is-amber {
    border-color: var(--status-warn-border);
    background: var(--status-warn-bg);
  }
  .is-amber .dot {
    background: var(--status-warn-text);
  }
  .is-amber .headline {
    color: var(--status-warn-text);
  }

  .is-red {
    background: var(--status-danger-bg);
  }
  .is-red .dot {
    background: var(--status-danger-text);
  }
  .is-red .headline {
    color: var(--status-danger-text);
  }
</style>
