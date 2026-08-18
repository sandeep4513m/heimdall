<!-- src/lib/components/ModelSelector.svelte -->
<script lang="ts">
  import Icon from './icons/Icon.svelte';
  import { iconChevronDown, iconMessage2, iconPhoto, iconVolume, iconDatabase } from './icons/index';

  interface OllamaModel {
    name: string;
    size: number;
    digest: string;
    modified_at: string;
    capability: string;
  }

  interface Props {
    models: OllamaModel[];
    selectedModel: string;
    onSelect: (model: string) => void;
  }

  let { models, selectedModel, onSelect }: Props = $props();

  let open = $state(false);
  let dropdownEl: HTMLDivElement;

  function toggle() {
    open = !open;
  }

  function select(name: string) {
    onSelect(name);
    open = false;
  }

  function handleClickOutside(e: MouseEvent) {
    if (open && dropdownEl && !dropdownEl.contains(e.target as Node)) {
      open = false;
    }
  }

  function formatSize(bytes: number): string {
    const gb = bytes / (1024 * 1024 * 1024);
    if (gb >= 1) return `${gb.toFixed(1)} GB`;
    const mb = bytes / (1024 * 1024);
    return `${mb.toFixed(0)} MB`;
  }

  function capabilityIcon(cap: string): string[] {
    switch (cap) {
      case 'vision': return iconPhoto;
      case 'audio': return iconVolume;
      case 'embedding': return iconDatabase;
      default: return iconMessage2;
    }
  }
</script>

<svelte:window onclick={handleClickOutside} />

<div class="model-selector" bind:this={dropdownEl}>
  <button class="selector-trigger" onclick={toggle} aria-expanded={open} aria-haspopup="listbox">
    {#if selectedModel}
      <span class="model-name">{selectedModel}</span>
    {:else}
      <span class="model-name placeholder">No model</span>
    {/if}
    <Icon paths={iconChevronDown} size={12} stroke={1.5} />
  </button>

  {#if open}
    <div class="dropdown" role="listbox" aria-label="Select model">
      {#if models.length === 0}
        <div class="dropdown-empty">No models available</div>
      {:else}
        {#each models as model (model.digest)}
          {@const iconPaths = capabilityIcon(model.capability)}
          <button
            class="dropdown-item"
            class:active={model.name === selectedModel}
            onclick={() => select(model.name)}
            role="option"
            aria-selected={model.name === selectedModel}
          >
            <span class="item-icon">
              <Icon paths={iconPaths} size={12} stroke={1.5} />
            </span>
            <span class="item-name">{model.name}</span>
            <span class="item-size">{formatSize(model.size)}</span>
          </button>
        {/each}
      {/if}
    </div>
  {/if}
</div>

<style>
  .model-selector {
    position: relative;
  }

  .selector-trigger {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-xs) var(--space-sm);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    font-family: var(--font-ui);
    font-size: 12px;
    cursor: pointer;
    transition: border-color 0.15s;
    max-width: 220px;
  }
  .selector-trigger:hover {
    border-color: var(--border-dim);
  }

  .model-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .model-name.placeholder {
    color: var(--text-ghost);
  }

  .dropdown {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    min-width: 240px;
    max-height: 280px;
    overflow-y: auto;
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-dim);
    border-radius: var(--radius-lg);
    padding: var(--space-xs);
    z-index: 100;
    box-shadow: var(--shadow-popover);
  }

  .dropdown::-webkit-scrollbar {
    width: 4px;
  }
  .dropdown::-webkit-scrollbar-thumb {
    background: var(--border-subtle);
    border-radius: var(--radius-pill);
  }

  .dropdown-empty {
    padding: var(--space-md);
    text-align: center;
    color: var(--text-ghost);
    font-size: 12px;
  }

  .dropdown-item {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    width: 100%;
    padding: var(--space-sm) var(--space-sm);
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-secondary);
    font-family: var(--font-ui);
    font-size: 12px;
    cursor: pointer;
    transition: background 0.1s;
    text-align: left;
  }
  .dropdown-item:hover {
    background: var(--bg-surface);
  }
  .dropdown-item.active {
    background: var(--gold-bg);
    color: var(--gold-primary);
  }

  .item-icon {
    display: flex;
    align-items: center;
    color: var(--text-ghost);
    flex-shrink: 0;
  }
  .dropdown-item.active .item-icon {
    color: var(--gold-dim);
  }

  .item-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .item-size {
    font-size: 10px;
    color: var(--text-ghost);
    flex-shrink: 0;
  }
</style>
