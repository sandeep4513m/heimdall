<!-- src/lib/components/Sidebar.svelte -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import Icon from './icons/Icon.svelte';
  import { iconMessage2, iconCpu, iconKeyboard, iconSettings, iconBook2, iconBrain, iconRobot } from './icons/index';

  interface Props {
    activePanel: 'chat' | 'knowledge' | 'memory' | 'models' | 'governor' | 'shortcuts' | 'settings';
    onNavigate: (panel: 'chat' | 'knowledge' | 'memory' | 'models' | 'governor' | 'shortcuts' | 'settings') => void;
    governorAlert?: boolean;
  }

  let { activePanel, onNavigate, governorAlert = false }: Props = $props();

  // Read version from Tauri at mount so we never lie when the
  // package.json / Cargo.toml / tauri.conf.json get bumped.
  // See AUDIT P5-B5.
  let version = $state<string>('');
  onMount(async () => {
    try {
      version = await getVersion();
    } catch {
      // Fallback (e.g. if Tauri is unavailable in some test harness).
      version = '';
    }
  });
</script>

<nav class="sidebar">

  <button
    id="nav-chat"
    class="nav-btn"
    class:active={activePanel === 'chat'}
    onclick={() => onNavigate('chat')}
    title="Chat"
    aria-label="Chat"
  >
    <Icon paths={iconMessage2} size={18} stroke={1.5} />
  </button>

  <button
    id="nav-knowledge"
    class="nav-btn"
    class:active={activePanel === 'knowledge'}
    onclick={() => onNavigate('knowledge')}
    title="Knowledge (Ctrl+K)"
    aria-label="Knowledge"
  >
    <Icon paths={iconBook2} size={18} stroke={1.5} />
  </button>

  <button
    id="nav-memory"
    class="nav-btn"
    class:active={activePanel === 'memory'}
    onclick={() => onNavigate('memory')}
    title="Memory (Ctrl+M)"
    aria-label="Memory"
  >
    <Icon paths={iconBrain} size={18} stroke={1.5} />
  </button>

  <button
    id="nav-models"
    class="nav-btn"
    class:active={activePanel === 'models'}
    onclick={() => onNavigate('models')}
    title="Models"
    aria-label="Models"
  >
    <Icon paths={iconRobot} size={18} stroke={1.5} />
  </button>

  <button
    id="nav-governor"
    class="nav-btn"
    class:active={activePanel === 'governor'}
    onclick={() => onNavigate('governor')}
    title="Governor"
    aria-label="Governor"
  >
    <Icon paths={iconCpu} size={18} stroke={1.5} />
    {#if governorAlert}
      <span class="nav-dot" aria-hidden="true"></span>
    {/if}
  </button>

  <button
    id="nav-shortcuts"
    class="nav-btn"
    class:active={activePanel === 'shortcuts'}
    onclick={() => onNavigate('shortcuts')}
    title="Shortcuts"
    aria-label="Shortcuts"
  >
    <Icon paths={iconKeyboard} size={18} stroke={1.5} />
  </button>

  <span class="nav-spacer"></span>

  {#if version}
    <span class="version-indicator" aria-label="Version {version} Beta">
      v{version} · Beta
    </span>
  {/if}

  <button
    id="nav-settings"
    class="nav-btn"
    class:active={activePanel === 'settings'}
    onclick={() => onNavigate('settings')}
    title="Settings"
    aria-label="Settings"
  >
    <Icon paths={iconSettings} size={18} stroke={1.5} />
  </button>

</nav>

<style>
  .sidebar {
    width: 52px;
    background: var(--bg-titlebar);
    border-right: 0.5px solid var(--border-subtle);
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: var(--space-lg) 0;
    gap: var(--space-sm);
    flex-shrink: 0;
  }

  .nav-btn {
    width: 36px;
    height: 36px;
    border-radius: var(--radius-lg);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    border: 0.5px solid transparent;
    background: transparent;
    color: var(--text-ghost);
    position: relative;
    transition: color 0.15s, background 0.15s, border-color 0.15s;
  }

  .nav-btn:hover:not(.active) {
    color: var(--text-dim);
  }

  .nav-btn.active {
    background: var(--bg-elevated);
    border-color: var(--border-subtle);
    color: var(--gold-primary);
  }

  .nav-spacer {
    flex: 1;
  }

  .version-indicator {
    font-family: var(--font-ui);
    font-size: 8px;
    color: var(--text-ghost);
    letter-spacing: 0.05em;
    writing-mode: vertical-rl;
    text-orientation: mixed;
    transform: rotate(180deg);
    opacity: 0.5;
    user-select: none;
    margin-bottom: var(--space-sm);
  }

  .nav-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--gold-primary);
    position: absolute;
    top: 6px;
    right: 6px;
    animation: pulse 1.5s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%       { opacity: 0.3; }
  }
</style>
