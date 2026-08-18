<script lang="ts">
  import { ragStore } from '$lib/stores/rag.svelte';
  import IngestionDropzone from './IngestionDropzone.svelte';
  import IngestionJobsList from './IngestionJobsList.svelte';
  import RetrievalPreview from './RetrievalPreview.svelte';

  function formatBytes(bytes: number): string {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const units = ['B', 'KB', 'MB', 'GB'];
    const i = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(k)));
    return `${(bytes / Math.pow(k, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
  }
</script>

<div class="collection-detail">
  {#if ragStore.selectedCollection}
    <div class="header">
      <h2>{ragStore.selectedCollection.display_name}</h2>
    </div>

    <div class="stats-row">
      <div class="stat-card">
        <span class="label">Chunks</span>
        <span class="val">{ragStore.collectionStats?.chunks ?? 0}</span>
      </div>
      <div class="stat-card">
        <span class="label">Sources</span>
        <span class="val">{ragStore.collectionStats?.sources ?? 0}</span>
      </div>
      <div class="stat-card">
        <span class="label">Index Size</span>
        <span class="val">{formatBytes(ragStore.collectionStats?.vector_bytes ?? 0)}</span>
      </div>
    </div>

    <IngestionDropzone />

    <IngestionJobsList />

    <RetrievalPreview />
  {/if}
</div>

<style>
  .collection-detail {
    padding: var(--space-xl);
    display: flex;
    flex-direction: column;
    gap: var(--space-xl);
    max-width: 800px;
    margin: 0 auto;
    width: 100%;
  }
  .header h2 {
    font-family: var(--font-brand);
    font-size: 24px;
    color: var(--text-base);
    margin: 0;
  }
  .stats-row {
    display: flex;
    gap: var(--space-md);
  }
  .stat-card {
    flex: 1;
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: var(--space-md) var(--space-lg);
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }
  .label {
    font-family: var(--font-ui);
    font-size: 11px;
    text-transform: uppercase;
    color: var(--text-ghost);
    letter-spacing: 0.05em;
  }
  .val {
    font-family: var(--font-ui);
    font-size: 20px;
    color: var(--gold-primary);
    font-weight: 600;
  }
</style>
