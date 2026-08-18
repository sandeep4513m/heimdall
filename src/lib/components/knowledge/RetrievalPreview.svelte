<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { ragStore } from '$lib/stores/rag.svelte';
  import type { ChunkPreview } from '$lib/types/rag';

  let query = $state('');
  let results = $state<ChunkPreview[]>([]);
  let isSearching = $state(false);
  let error = $state<string | null>(null);

  async function handleSearch() {
    if (!query.trim() || !ragStore.selectedCollection) return;
    isSearching = true;
    error = null;
    try {
      results = await invoke<ChunkPreview[]>('rag_search_preview', {
        name: ragStore.selectedCollection.display_name,
        query: query.trim(),
        k: 5,
      });
    } catch (e) {
      console.error(e);
      error = e as string;
    } finally {
      isSearching = false;
    }
  }
</script>

<div class="preview-container">
  <h3>Test Retrieval</h3>
  <div class="search-box">
    <input 
      type="text" 
      bind:value={query}
      placeholder="Type a test query..."
      onkeydown={(e) => e.key === 'Enter' && handleSearch()}
      maxlength="500"
    />
    <button onclick={handleSearch} disabled={isSearching || !query.trim()}>
      {isSearching ? 'Searching...' : 'Search'}
    </button>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <div class="results">
    {#each results as res}
      <div class="result-card">
        <div class="result-meta">
          <span class="source">{res.source_path}</span>
          <span class="score">Score: {res.score.toFixed(3)}</span>
        </div>
        <div class="content">{res.content}</div>
      </div>
    {/each}
  </div>
</div>

<style>
  .preview-container {
    margin-top: var(--space-xl);
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
  }
  h3 {
    font-family: var(--font-brand);
    font-size: 14px;
    margin: 0;
    color: var(--text-base);
  }
  .search-box {
    display: flex;
    gap: var(--space-sm);
  }
  .search-box input {
    flex: 1;
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    color: var(--text-base);
    font-family: var(--font-ui);
    font-size: 13px;
    padding: var(--space-sm) var(--space-md);
    border-radius: var(--radius-sm);
  }
  .search-box button {
    background: var(--gold-primary);
    color: var(--bg-app);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0 var(--space-lg);
    font-family: var(--font-ui);
    font-size: 13px;
    cursor: pointer;
  }
  .search-box button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .error {
    color: var(--accent-red);
    font-family: var(--font-ui);
    font-size: 12px;
  }
  .results {
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
  }
  .result-card {
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: var(--space-md);
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }
  .result-meta {
    display: flex;
    justify-content: space-between;
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
  }
  .score {
    color: var(--gold-primary);
  }
  .content {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-base);
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
