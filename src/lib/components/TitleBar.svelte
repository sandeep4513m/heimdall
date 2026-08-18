<!-- src/lib/components/TitleBar.svelte -->
<script lang="ts">
  import { tokens } from '$lib/tokens';
  import { getCurrentWindow } from '@tauri-apps/api/window';

  const appWindow = getCurrentWindow();

  function minimize() {
    appWindow.minimize();
  }

  function toggleMaximize() {
    appWindow.toggleMaximize();
  }

  function close() {
    appWindow.close();
  }
</script>

<header class="titlebar" data-tauri-drag-region>

  <!-- Logo left -->
  <div class="logo-wrap">
    <svg width="28" height="28" viewBox="0 0 28 28" fill="none" xmlns="http://www.w3.org/2000/svg">
      <!-- Outer diamond -->
      <polygon points="14,2 26,14 14,26 2,14"
        fill="none" stroke={tokens.gold.primary} stroke-width="0.8" opacity="0.4"/>
      <!-- Inner diamond -->
      <polygon points="14,5 23,14 14,23 5,14"
        fill="none" stroke={tokens.gold.primary} stroke-width="0.6" opacity="0.6"/>
      <!-- Outer ring -->
      <ellipse cx="14" cy="14" rx="5" ry="5"
        fill="none" stroke={tokens.gold.primary} stroke-width="1"/>
      <!-- Center dot -->
      <ellipse cx="14" cy="14" rx="2" ry="2"
        fill={tokens.gold.primary}/>
      <!-- Cardinal lines -->
      <line x1="14" y1="2"  x2="14" y2="7"  stroke={tokens.gold.primary} stroke-width="1" opacity="0.8"/>
      <line x1="14" y1="21" x2="14" y2="26" stroke={tokens.gold.primary} stroke-width="1" opacity="0.8"/>
      <line x1="2"  y1="14" x2="7"  y2="14" stroke={tokens.gold.primary} stroke-width="1" opacity="0.8"/>
      <line x1="21" y1="14" x2="26" y2="14" stroke={tokens.gold.primary} stroke-width="1" opacity="0.8"/>
    </svg>

    <div class="logo-text-wrap">
      <div class="logo-wordmark">HEIMDALL</div>
      <div class="logo-sub">Local AI Gateway</div>
    </div>
  </div>

  <!-- Window controls — no-drag zone so clicks register -->
  <div class="win-controls">
    <button class="wc wc-r" title="Close" aria-label="Close window" onclick={close}></button>
    <button class="wc wc-y" title="Minimise" aria-label="Minimise window" onclick={minimize}></button>
    <button class="wc wc-g" title="Maximise" aria-label="Maximise window" onclick={toggleMaximize}></button>
  </div>

</header>

<style>
  .titlebar {
    display: flex;
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
    padding: 0 var(--space-lg);
    height: 38px;
    background: var(--bg-titlebar);
    border-bottom: 0.5px solid var(--border-subtle);
    flex-shrink: 0;
    user-select: none;
  }

  .logo-wrap {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .logo-text-wrap {
    display: flex;
    flex-direction: column;
  }

  .logo-wordmark {
    font-family: var(--font-brand);
    font-size: 15px;
    font-weight: 600;
    letter-spacing: 0.15em;
    color: var(--gold-primary);
    line-height: 1;
  }

  .logo-sub {
    font-family: var(--font-ui);
    font-size: 9px;
    letter-spacing: 0.3em;
    color: var(--text-ghost);
    text-transform: uppercase;
    margin-top: 2px;
  }

  .win-controls {
    display: flex;
    gap: 7px;
    -webkit-app-region: no-drag;
  }

  .wc {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    cursor: pointer;
    transition: filter 0.15s;
    padding: 0;
    border: none;
  }
  .wc:hover { filter: brightness(1.5); }

  /* Window control colors are UI chrome — not semantic tokens.
     These are intentional fixed values per macOS-style dot convention. */
  .wc-r { background: #3a1a1a; border: 0.5px solid #6b2a2a; }
  .wc-y { background: #3a2d1a; border: 0.5px solid #7a5a1a; }
  .wc-g { background: #1a2e1a; border: 0.5px solid #2a6a2a; }
</style>
