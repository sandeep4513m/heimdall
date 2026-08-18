<!-- src/routes/+page.svelte -->
<script lang="ts">
  import { onMount } from 'svelte';
  import TitleBar from '$lib/components/TitleBar.svelte';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import ChatPanel from '$lib/components/ChatPanel.svelte';
  import KnowledgePanel from '$lib/components/knowledge/KnowledgePanel.svelte';
  import MemoryPanel from '$lib/components/memory/MemoryPanel.svelte';
  import GovernorPanel from '$lib/components/governor/GovernorPanel.svelte';
  import ModelsTab from '$lib/components/models/ModelsTab.svelte';
  import { governorStore } from '$lib/stores/governor.svelte';
  import { modelsStore } from '$lib/stores/models.svelte';

  type Panel = 'chat' | 'knowledge' | 'memory' | 'models' | 'governor' | 'shortcuts' | 'settings';

  let activePanel = $state<Panel>('chat');

  function navigate(panel: Panel) {
    activePanel = panel;
  }

  // Provenance pills in MemoryPanel dispatch a `heimdall:open-conversation`
  // CustomEvent. We switch back to chat; ChatPanel handles the actual
  // conversation switch via its own listener for the same event.
  onMount(() => {
    // Phase 6: kick off the Governor + Models event subscriptions once
    // at the top of the tree so live state survives navigation between
    // panels (Req 11.2, 13.1, 13.9).
    void governorStore.startListening();
    void modelsStore.startListening();

    const handler = () => {
      activePanel = 'chat';
    };
    window.addEventListener('heimdall:open-conversation', handler);
    return () => window.removeEventListener('heimdall:open-conversation', handler);
  });
</script>

<svelte:window onkeydown={(e) => {
  if (e.ctrlKey && e.key === 'k') {
    e.preventDefault();
    navigate('knowledge');
  }
  if (e.ctrlKey && e.shiftKey && e.key === 'M') {
    e.preventDefault();
    navigate('memory');
  }
}} />

<div class="app-root">

  <TitleBar />

  <div class="app-body">
    <Sidebar {activePanel} onNavigate={navigate} governorAlert={false} />

    <main class="main-panel">
      <!-- ChatPanel is ALWAYS mounted to preserve streaming state -->
      <div class="panel-container" class:hidden={activePanel !== 'chat'}>
        <ChatPanel />
      </div>

      <!-- KnowledgePanel is also ALWAYS mounted to preserve drag/drop state and progress -->
      <div class="panel-container" class:hidden={activePanel !== 'knowledge'}>
        <KnowledgePanel />
      </div>

      <!-- MemoryPanel is ALWAYS mounted to preserve pending review state -->
      <div class="panel-container" class:hidden={activePanel !== 'memory'}>
        <MemoryPanel />
      </div>

      <!-- ModelsTab is ALWAYS mounted so in-flight pulls and the
           cached row list survive navigation (Req 13.1). -->
      <div class="panel-container" class:hidden={activePanel !== 'models'}>
        <ModelsTab />
      </div>

      <!-- GovernorPanel is ALWAYS mounted so live metrics keep updating
           even while another panel is on top (Req 11.2). -->
      <div class="panel-container" class:hidden={activePanel !== 'governor'}>
        <GovernorPanel />
      </div>

      {#if activePanel === 'shortcuts'}
        <div class="placeholder-panel">
          <h2 class="placeholder-title">Shortcuts</h2>
          <p class="placeholder-body">Keyboard shortcuts are first-class in Heimdall. New chat with Ctrl+N, switch model with Ctrl+M, send with Enter, cancel a streaming reply with Escape — and remap any of them to whatever your hands already know.</p>
          <p class="placeholder-note">Defaults are wired. The remap UI lands with the Release Candidate.</p>
        </div>
      {:else if activePanel === 'settings'}
        <div class="placeholder-panel">
          <h2 class="placeholder-title">Settings</h2>
          <p class="placeholder-body">Heimdall reads <code>~/.heimdall/config.toml</code> on launch. For Alpha you can edit it directly: Ollama URL, default chat model, hardware tier override, embedding model.</p>
          <p class="placeholder-note">A graphical settings panel ships with the Release Candidate. The format of config.toml will not change.</p>
        </div>
      {/if}
    </main>
  </div>

</div>

<style>
  .app-root {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .app-body {
    display: flex;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .main-panel {
    flex: 1;
    display: flex;
    flex-direction: column;
    background: var(--bg-app);
    overflow: hidden;
    min-width: 0;
    min-height: 0;
  }

  .panel-container {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
  }

  .panel-container.hidden {
    display: none;
  }

  .placeholder-panel {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    flex: 1;
    min-height: 0;
    max-width: 480px;
    margin: 0 auto;
    padding: var(--space-xl);
    gap: var(--space-md);
    text-align: center;
  }

  .placeholder-title {
    font-family: var(--font-brand);
    font-size: 18px;
    font-weight: 600;
    letter-spacing: 0.1em;
    color: var(--gold-primary);
    margin: 0;
  }

  .placeholder-body {
    font-family: var(--font-ui);
    font-size: 12px;
    line-height: 1.8;
    color: var(--text-dim);
    margin: 0;
  }

  .placeholder-body code {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-code);
    background: var(--bg-surface);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    border: 0.5px solid var(--border-subtle);
  }

  .placeholder-note {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
    margin: 0;
    letter-spacing: 0.03em;
  }
</style>
