<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { ragStore } from '$lib/stores/rag.svelte';
  import type { IngestionProgressEvent, IngestionCompleteEvent } from '$lib/types/rag';
  import CollectionsList from './CollectionsList.svelte';
  import CollectionDetail from './CollectionDetail.svelte';

  let unlistenProgress: UnlistenFn;
  let unlistenComplete: UnlistenFn;

  onMount(async () => {
    ragStore.loadCollections();

    unlistenProgress = await listen<IngestionProgressEvent>('rag://ingestion-progress', (event) => {
      ragStore.handleProgress(event.payload);
    });

    unlistenComplete = await listen<IngestionCompleteEvent>('rag://ingestion-complete', (event) => {
      ragStore.handleComplete(event.payload);
    });
  });

  onDestroy(() => {
    if (unlistenProgress) unlistenProgress();
    if (unlistenComplete) unlistenComplete();
  });
</script>

<div class="knowledge-panel">
  <div class="left-rail">
    <CollectionsList />
  </div>
  <div class="detail-view">
    {#if ragStore.selectedCollection}
      <CollectionDetail />
    {:else}
      <div class="empty-state">
        <h3 class="empty-title">Knowledge Collections</h3>
        <p class="empty-body">Select a collection or create a new one to start giving your models custom knowledge.</p>
      </div>
    {/if}
  </div>
</div>

<style>
  .knowledge-panel {
    display: flex;
    flex-direction: row;
    height: 100%;
    width: 100%;
    overflow: hidden;
  }
  .left-rail {
    width: 250px;
    border-right: 0.5px solid var(--border-subtle);
    background: var(--bg-surface);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
  }
  .detail-view {
    flex: 1;
    display: flex;
    flex-direction: column;
    background: var(--bg-app);
    overflow-y: auto;
  }
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--text-dim);
    text-align: center;
    padding: var(--space-xl);
  }
  .empty-title {
    font-family: var(--font-brand);
    font-size: 18px;
    color: var(--gold-primary);
    margin-bottom: var(--space-sm);
  }
  .empty-body {
    font-family: var(--font-ui);
    font-size: 13px;
    max-width: 400px;
  }
</style>
