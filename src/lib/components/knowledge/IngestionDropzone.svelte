<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { invoke } from '@tauri-apps/api/core';
  import { onMount, onDestroy } from 'svelte';
  import { ragStore } from '$lib/stores/rag.svelte';

  let isDragging = $state(false);
  let urlInput = $state('');
  let unlistenDragDrop: UnlistenFn;
  let unlistenDragEnter: UnlistenFn;
  let unlistenDragLeave: UnlistenFn;

  onMount(async () => {
    unlistenDragDrop = await listen<{ paths: string[] }>('tauri://drag-drop', async (event) => {
      isDragging = false;
      if (event.payload.paths && event.payload.paths.length > 0 && ragStore.selectedCollection) {
        try {
          await invoke('rag_ingest_paths', {
            collection: ragStore.selectedCollection.display_name,
            paths: event.payload.paths,
          });
          ragStore.reloadJobs();
        } catch (e) {
          console.error(e);
        }
      }
    });
    unlistenDragEnter = await listen('tauri://drag-enter', () => {
      isDragging = true;
    });
    unlistenDragLeave = await listen('tauri://drag-leave', () => {
      isDragging = false;
    });
  });

  onDestroy(() => {
    if (unlistenDragDrop) unlistenDragDrop();
    if (unlistenDragEnter) unlistenDragEnter();
    if (unlistenDragLeave) unlistenDragLeave();
  });

  async function handleBrowseFiles() {
    if (!ragStore.selectedCollection) return;
    const selected = await open({
      multiple: true,
      directory: false,
    });
    if (selected) {
      const paths = Array.isArray(selected) ? selected : [selected];
      try {
        await invoke('rag_ingest_paths', {
          collection: ragStore.selectedCollection.display_name,
          paths,
        });
        ragStore.reloadJobs();
      } catch (e) {
        console.error(e);
      }
    }
  }

  async function handleBrowseFolder() {
    if (!ragStore.selectedCollection) return;
    const selected = await open({
      multiple: false,
      directory: true,
    });
    if (selected) {
      const path = Array.isArray(selected) ? selected[0] : selected;
      try {
        await invoke('rag_ingest_paths', {
          collection: ragStore.selectedCollection.display_name,
          paths: [path],
        });
        ragStore.reloadJobs();
      } catch (e) {
        console.error(e);
      }
    }
  }

  async function handleUrl() {
    if (!urlInput.trim() || !ragStore.selectedCollection) return;
    try {
      await invoke('rag_ingest_url', {
        collection: ragStore.selectedCollection.display_name,
        url: urlInput.trim(),
      });
      urlInput = '';
      ragStore.reloadJobs();
    } catch (e) {
      console.error(e);
    }
  }

  function handleDrop(e: DragEvent) {
    e.preventDefault();
    isDragging = false;
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div 
  class="dropzone" 
  class:dragging={isDragging}
  ondragover={(e) => { e.preventDefault(); isDragging = true; }}
  ondragleave={() => isDragging = false}
  ondrop={handleDrop}
>
  <div class="drop-content">
    <p>Drag files or a folder here to add to knowledge</p>
    <p class="sub">or</p>
    <div class="browse-row">
      <button class="browse-btn" onclick={handleBrowseFiles}>Browse Files</button>
      <button class="browse-btn" onclick={handleBrowseFolder}>Browse Folder</button>
    </div>
  </div>
  <div class="url-bar">
    <input 
      type="url" 
      placeholder="Or paste a URL..." 
      bind:value={urlInput}
      onkeydown={(e) => e.key === 'Enter' && handleUrl()}
    />
    <button onclick={handleUrl} disabled={!urlInput.trim()}>Add URL</button>
  </div>
</div>

<style>
  .dropzone {
    border: 1px dashed var(--border-subtle);
    border-radius: var(--radius-md);
    padding: var(--space-xl);
    text-align: center;
    background: var(--bg-surface);
    transition: all 0.2s;
    display: flex;
    flex-direction: column;
    gap: var(--space-lg);
  }
  .dropzone.dragging {
    border-color: var(--gold-primary);
    background: var(--bg-elevated);
  }
  .drop-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-sm);
  }
  p {
    font-family: var(--font-ui);
    font-size: 14px;
    color: var(--text-base);
    margin: 0;
  }
  p.sub {
    font-size: 12px;
    color: var(--text-ghost);
  }
  .browse-row {
    display: flex;
    gap: var(--space-sm);
    flex-wrap: wrap;
    justify-content: center;
  }
  .browse-btn {
    background: transparent;
    border: 0.5px solid var(--text-ghost);
    color: var(--text-base);
    padding: var(--space-sm) var(--space-lg);
    border-radius: var(--radius-sm);
    cursor: pointer;
    font-family: var(--font-ui);
    font-size: 13px;
  }
  .browse-btn:hover {
    border-color: var(--gold-primary);
    color: var(--gold-primary);
  }
  .url-bar {
    display: flex;
    gap: var(--space-sm);
    max-width: 400px;
    margin: 0 auto;
    width: 100%;
  }
  .url-bar input {
    flex: 1;
    background: var(--bg-app);
    border: 0.5px solid var(--border-subtle);
    color: var(--text-base);
    font-family: var(--font-ui);
    font-size: 13px;
    padding: var(--space-sm) var(--space-md);
    border-radius: var(--radius-sm);
  }
  .url-bar button {
    background: var(--gold-primary);
    color: var(--bg-app);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0 var(--space-md);
    font-family: var(--font-ui);
    font-size: 13px;
    cursor: pointer;
  }
  .url-bar button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
