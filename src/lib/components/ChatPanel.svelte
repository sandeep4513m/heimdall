<!-- src/lib/components/ChatPanel.svelte -->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import { open } from '@tauri-apps/plugin-dialog';
  import { readFile } from '@tauri-apps/plugin-fs';
  import Icon from './icons/Icon.svelte';
  import { iconSend2, iconRobot, iconLoader2, iconAlertCircle, iconPhoto, iconPlus, iconX, iconBook2 } from './icons/index';
  import ModelSelector from './ModelSelector.svelte';
  import MemoryToggle from './chat/MemoryToggle.svelte';
  import MemoryIndicator from './chat/MemoryIndicator.svelte';
  import type { ModelCapabilities } from '$lib/types/model';
  import { ragStore } from '$lib/stores/rag.svelte';
  import { memoryStore } from '$lib/stores/memory.svelte';
  import type { MemoryUsedEvent } from '$lib/types/memory';

  // ── Race-safe generation counter (module-scoped) ───────────────────────────
  let capRequestGen = 0;

  // ── Types ──────────────────────────────────────────────────────────────────

  interface OllamaHealth {
    online: boolean;
    version: string | null;
  }

  interface OllamaModel {
    name: string;
    size: number;
    digest: string;
    modified_at: string;
    capability: string;
  }

  interface StreamTokenEvent {
    conversation_id: string;
    token: string;
    done: boolean;
    tokens_used: number | null;
  }

  interface StreamThinkingEvent {
    conversation_id: string;
    content: string;
    done: boolean;
  }

  interface ChatMessage {
    id: string;
    role: 'user' | 'assistant' | 'system';
    content: string;
    tokens_used: number | null;
    images?: string[];
    thinking?: string;
    memoryUsed?: string;
  }

  interface Conversation {
    id: string;
    title: string | null;
    model: string | null;
    created_at: number;
    updated_at: number;
  }

  interface BackendMessage {
    id: string;
    conversation_id: string | null;
    role: 'user' | 'assistant' | 'system';
    content: string | null;
    input_type: string | null;
    tokens_used: number | null;
    images: string | null; // JSON array of base64 strings
    thinking: string | null; // thinking block content
    created_at: number;
  }

  interface UserPreferences {
    default_chat_model: string | null;
    default_vision_model: string | null;
    ollama_url: string;
  }

  // ── State ──────────────────────────────────────────────────────────────────

  type Tab = 'chat' | 'history';

  let activeTab = $state<Tab>('chat');
  let health = $state<OllamaHealth>({ online: false, version: null });
  let models = $state<OllamaModel[]>([]);
  let selectedModel = $state<string>('');
  let userHasDefaultModel = $state<boolean>(false);
  let messages = $state<ChatMessage[]>([]);
  let inputText = $state('');
  let isStreaming = $state(false);
  let streamingContent = $state('');
  let tokensUsed = $state<number | null>(null);
  let conversationId = $state<string>(crypto.randomUUID());
  let conversationCreated = $state(false);
  let errorMessage = $state<string | null>(null);
  let conversations = $state<Conversation[]>([]);
  let pendingImage = $state<string | null>(null); // base64 image ready to send
  let pendingImagePreview = $state<string | null>(null); // data URL for thumbnail
  let activeCollections = $state<string[]>([]);
  let showCollectionPicker = $state(false);
  let showChatOverflow = $state(false);
  let memoryEnabled = $state<boolean>(true);

  // ── Memory transparency (per-turn injection capture) ──────────────────────
  // `chat://memory_used` arrives just before chat_stream begins. We hold the
  // text until the assistant message lands (token done=true), then attach it
  // to that message so the user can expand a "Memory used" badge.
  let lastMemoryUsed = $state<string | null>(null);
  // Per-message expand toggle for the memory badge.
  let expandedMemoryUsed = $state<Set<string>>(new Set());

  // ── Thinking block streaming state ─────────────────────────────────────────
  let streamingThinking = $state('');
  let isThinking = $state(false);
  let thinkingStartTime = $state<number>(0);
  let thinkingDuration = $state<number>(0); // seconds
  // Track which messages have their thinking block expanded
  let expandedThinking = $state<Set<string>>(new Set());

  let messagesEnd = $state<HTMLDivElement>(undefined!);
  let inputEl = $state<HTMLTextAreaElement>(undefined!);
  let unlistenToken: UnlistenFn | null = null;
  let unlistenThinking: UnlistenFn | null = null;
  let unlistenMemoryUsed: UnlistenFn | null = null;
  let openConvUnsubscribe: (() => void) | null = null;

  // Image size cap — refuse files this large to protect WebKit from OOM.
  // The 1024 px resize pipeline can spike to ~3x the source size in transient
  // RAM (Uint8Array → Blob → decoded RGBA → canvas → toDataURL).
  // See AUDIT P3-B6.
  const MAX_IMAGE_BYTES = 10 * 1024 * 1024; // 10 MB

  // Scroll throttling. Streaming thinking models emit ~30 tokens/sec; calling
  // scrollIntoView({behavior:'smooth'}) on each one cancels and restarts an
  // animated scroll, pegging the GPU. We throttle to ≤10 Hz and use 'auto'
  // (jump-scroll) while streaming; a final 'smooth' settle on done.
  // See AUDIT P3-A2.
  let scrollPending = false;
  let lastScrollAt = 0;
  const SCROLL_MIN_INTERVAL_MS = 100;

  // ── Model capabilities state ────────────────────────────────────────────────
  let selectedModelInfo = $state<ModelCapabilities | null>(null);
  let capabilitiesLoading = $state(false);

  // ── Race-safe $effect to fetch capabilities when selectedModel changes ─────
  $effect(() => {
    const model = selectedModel;
    if (!model) {
      selectedModelInfo = null;
      capabilitiesLoading = false;
      return;
    }

    const myGen = ++capRequestGen;
    selectedModelInfo = null;
    capabilitiesLoading = true;

    invoke<ModelCapabilities>('get_model_capabilities', { modelName: model })
      .then((caps) => {
        if (myGen !== capRequestGen) return; // stale — discard
        selectedModelInfo = caps;
        capabilitiesLoading = false;
      })
      .catch((err) => {
        if (myGen !== capRequestGen) return; // stale — discard
        console.warn('[Heimdall] get_model_capabilities failed, using fallback:', err);
        selectedModelInfo = {
          model_name: model,
          digest: '',
          completion: true,
          vision: false,
          thinking: false,
          tools: false,
          embedding: false,
          capability_source: 'heuristic',
          raw_capabilities: [],
          family: null,
          parameter_size: null,
          quantization_level: null,
          detected_at: 0,
          updated_at: 0,
        };
        capabilitiesLoading = false;
        // Surface non-blocking error indicator
        errorMessage = `Could not detect capabilities for ${model} — using conservative defaults.`;
      });
  });

  // ── Derived ────────────────────────────────────────────────────────────────

  let showImageButton = $derived(selectedModelInfo?.vision === true);
  let supportsThinking = $derived(selectedModelInfo?.thinking === true);
  let isEmbeddingModel = $derived(selectedModelInfo?.embedding === true);
  let supportsTools = $derived(selectedModelInfo?.tools === true);

  let canSend = $derived(
    (inputText.trim().length > 0 || pendingImage !== null) && !isStreaming && health.online && selectedModel !== ''
  );

  // ── Getter for non-reactive closures ─────────────────────────────────────
  //
  // Svelte 5's $state rune rewrites reads into live signal accesses ONLY
  // inside reactive contexts (template, $effect, $derived). Plain JS
  // closures — like the Tauri event listener callbacks below — capture the
  // value at creation time and never update.
  //
  // This getter ensures those closures always read the CURRENT value of
  // conversationId, regardless of when they were created.
  // See: https://svelte.dev/docs/svelte/$state#Passing-state-into-functions
  function getConversationId() { return conversationId; }

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  onMount(async () => {
    // Register listeners FIRST, synchronously before any async init.
    // If we await the heavy init first, an early unmount can leak the
    // listener forever. See AUDIT P3-B1.
    try {
      unlistenToken = await listen<StreamTokenEvent>('chat://token', (event) => {
        const payload = event.payload;
        if (payload.conversation_id !== getConversationId()) return;

        streamingContent += payload.token;

        if (payload.done) {
          tokensUsed = payload.tokens_used;
          messages = [...messages, {
            id: crypto.randomUUID(),
            role: 'assistant',
            content: streamingContent,
            tokens_used: payload.tokens_used,
            thinking: streamingThinking || undefined,
            memoryUsed: lastMemoryUsed ?? undefined,
          }];
          streamingContent = '';
          streamingThinking = '';
          lastMemoryUsed = null;
          isStreaming = false;
          isThinking = false;
          // Final settle is smooth.
          scrollToBottom(true);
        }
      });

      unlistenMemoryUsed = await listen<MemoryUsedEvent>('chat://memory_used', (event) => {
        const payload = event.payload;
        if (payload.conversation_id !== getConversationId()) return;
        // Capture the exact memory text injected for THIS turn. It's
        // attached to the assistant message when the token stream completes.
        lastMemoryUsed = payload.memory_text;
      });

      unlistenThinking = await listen<StreamThinkingEvent>('chat://thinking', (event) => {
        const payload = event.payload;
        if (payload.conversation_id !== getConversationId()) return;

        if (payload.done) {
          // Thinking complete — record duration
          isThinking = false;
          thinkingDuration = Math.round((Date.now() - thinkingStartTime) / 1000);
        } else {
          // Thinking content arriving
          if (!isThinking) {
            isThinking = true;
            thinkingStartTime = Date.now();
          }
          streamingThinking += payload.content;
          // Throttled jump-scroll during streaming.
          scrollToBottom(false);
        }
      });
    } catch (e) {
      console.error('[Heimdall] Listener registration failed:', e);
    }

    // Then heavy init. If anything below throws, the listeners are already
    // cleanly registered (or registration failed and was logged).
    try {
      await checkHealth();
      await loadModels();
      await loadUserPreferences();
      await loadLastConversation();
      await loadConversations();
      await ragStore.loadCollections();
      await memoryStore.loadFacts();
      await memoryStore.loadSettings();
      await memoryStore.startListening();
    } catch (e) {
      console.error('[Heimdall] Mount initialisation failed:', e);
    }

    // Provenance pills (Memory panel → "from {conversation}") dispatch this
    // CustomEvent. Switch the chat to that conversation.
    const openConvHandler = async (ev: Event) => {
      const detail = (ev as CustomEvent<{ conversationId: string }>).detail;
      if (!detail?.conversationId) return;
      // Refresh the conversations cache to ensure we know about it.
      try {
        await loadConversations();
      } catch {}
      const target = conversations.find((c) => c.id === detail.conversationId);
      if (target) {
        await switchConversation(target);
      }
    };
    window.addEventListener('heimdall:open-conversation', openConvHandler);
    openConvUnsubscribe = () => window.removeEventListener('heimdall:open-conversation', openConvHandler);
  });

  onDestroy(() => {
    if (unlistenToken) unlistenToken();
    if (unlistenThinking) unlistenThinking();
    if (unlistenMemoryUsed) unlistenMemoryUsed();
    if (openConvUnsubscribe) openConvUnsubscribe();
  });

  // ── Functions ──────────────────────────────────────────────────────────────

  async function checkHealth() {
    try {
      health = await invoke<OllamaHealth>('check_ollama_health');
    } catch {
      health = { online: false, version: null };
    }
  }

  async function loadModels() {
    try {
      models = await invoke<OllamaModel[]>('list_models');
    } catch {
      models = [];
    }
  }

  async function loadUserPreferences() {
    try {
      const prefs = await invoke<UserPreferences>('get_user_preferences');
      if (prefs.default_chat_model && models.some(m => m.name === prefs.default_chat_model)) {
        selectedModel = prefs.default_chat_model;
        userHasDefaultModel = true;
      } else if (models.length > 0 && !selectedModel) {
        const chatModel = models.find(m => m.capability !== 'embedding');
        selectedModel = chatModel?.name ?? models[0].name;
      }
    } catch {
      // Fallback: pick first non-embedding model
      if (models.length > 0 && !selectedModel) {
        const chatModel = models.find(m => m.capability !== 'embedding');
        selectedModel = chatModel?.name ?? models[0].name;
      }
    }
  }

  async function loadLastConversation() {
    try {
      const conversations = await invoke<Conversation[]>('list_conversations');
      if (conversations.length === 0) return;

      const latest = conversations[0]; // Already sorted by updated_at DESC
      conversationId = latest.id;
      conversationCreated = true;

      // Restore the model used in that conversation only if the user hasn't explicitly
      // set a default model preference.
      if (!userHasDefaultModel && latest.model && models.some(m => m.name === latest.model)) {
        selectedModel = latest.model!;
      }

      const backendMessages = await invoke<BackendMessage[]>('get_messages', {
        conversationId: latest.id,
      });

      messages = backendMessages
        .filter(m => m.role !== 'system')
        .map(m => backendToChat(m));

      try {
        const collections = await invoke<string[]>('get_active_collections', { conversationId: latest.id });
        activeCollections = collections || [];
      } catch {
        activeCollections = [];
      }

      try {
        memoryEnabled = await memoryStore.getConversationMemory(latest.id);
      } catch {
        memoryEnabled = true;
      }

      if (messages.length > 0) {
        scrollToBottom(true);
      }
    } catch {
      // Silently fall back to fresh conversation
    }
  }

  async function sendMessage() {
    if (!canSend) return;

    const userContent = inputText.trim() || (pendingImage ? '[image]' : '');
    inputText = '';
    errorMessage = null;

    // Capture and clear pending image (raw base64, no prefix)
    const imageToSend = pendingImage;
    pendingImage = null;
    pendingImagePreview = null;

    // Ensure a conversation record exists in the DB before sending
    if (!conversationCreated) {
      try {
        const conv = await invoke<{ id: string }>('create_conversation', {
          model: selectedModel,
        });
        conversationId = conv.id;
        conversationCreated = true;
        
        if (activeCollections.length > 0) {
          invoke('set_active_collections', { conversationId, collections: activeCollections }).catch(console.error);
        }
      } catch (e) {
        errorMessage = `Failed to create conversation: ${e}`;
        return;
      }
    }

    // Auto-title on first user message (first 40 chars)
    const isFirstMessage = messages.length === 0;

    // Store data URL for display in chat bubble
    const userMsg: ChatMessage = {
      id: crypto.randomUUID(),
      role: 'user',
      content: userContent,
      tokens_used: null,
      images: imageToSend ? [`data:image/jpeg;base64,${imageToSend}`] : undefined,
    };
    messages = [...messages, userMsg];
    scrollToBottom(true);

    if (isFirstMessage) {
      const title = userContent.length > 40
        ? userContent.slice(0, 40) + '…'
        : userContent;
      invoke('update_conversation_title', { conversationId, title }).catch(() => {});
    }

    isStreaming = true;
    streamingContent = '';

    // Build message history for Ollama — only include images in the LAST user message
    // Vision models don't need images repeated in history, and it bloats the request
    const lastUserIdx = messages.length - 1; // The message we just added
    const ollamaMessages = messages.map((m, idx) => {
      let imgs: string[] | null = null;
      if (idx === lastUserIdx && m.images && m.images.length > 0) {
        imgs = m.images.map(dataUrl => {
          const commaIdx = dataUrl.indexOf(',');
          return commaIdx >= 0 ? dataUrl.slice(commaIdx + 1) : dataUrl;
        });
      }
      return {
        role: m.role,
        content: m.content,
        images: imgs,
      };
    });

    try {
      await invoke<string>('chat_stream', {
        conversationId,
        model: selectedModel,
        messages: ollamaMessages,
        options: null,
        context: {
          rag_collections: activeCollections.length > 0 ? activeCollections : null,
          memory_enabled: memoryEnabled,
        },
      });
    } catch (e) {
      isStreaming = false;
      streamingContent = '';
      errorMessage = String(e);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  function scrollToBottom(smooth: boolean = false) {
    const now = Date.now();

    // Smooth scroll = final settle, always honour it.
    if (smooth) {
      requestAnimationFrame(() => {
        messagesEnd?.scrollIntoView({ behavior: 'smooth' });
      });
      lastScrollAt = now;
      return;
    }

    // Throttled jump-scroll path. If a frame is already pending, drop;
    // if we scrolled within the last interval, defer.
    if (scrollPending) return;

    const elapsed = now - lastScrollAt;
    if (elapsed < SCROLL_MIN_INTERVAL_MS) {
      scrollPending = true;
      setTimeout(() => {
        scrollPending = false;
        requestAnimationFrame(() => {
          messagesEnd?.scrollIntoView({ behavior: 'auto' });
        });
        lastScrollAt = Date.now();
      }, SCROLL_MIN_INTERVAL_MS - elapsed);
      return;
    }

    requestAnimationFrame(() => {
      messagesEnd?.scrollIntoView({ behavior: 'auto' });
    });
    lastScrollAt = now;
  }

  function newChat() {
    // Trigger extraction for the current conversation before starting a new one
    if (conversationCreated && conversationId) {
      memoryStore.extract(conversationId).catch(() => {});
    }
    messages = [];
    conversationId = crypto.randomUUID();
    conversationCreated = false;
    streamingContent = '';
    streamingThinking = '';
    isStreaming = false;
    isThinking = false;
    thinkingDuration = 0;
    tokensUsed = null;
    errorMessage = null;
    pendingImage = null;
    pendingImagePreview = null;
    activeCollections = [];
    memoryEnabled = true;
    // Clear expanded-thinking set so it doesn't leak across sessions.
    // See AUDIT P3-B4.
    expandedThinking = new Set();
    expandedMemoryUsed = new Set();
    lastMemoryUsed = null;
  }

  // ── Message content parsing ────────────────────────────────────────────────

  interface ContentSegment {
    type: 'text' | 'code';
    content: string;
    lang?: string;
  }

  function parseContent(raw: string): ContentSegment[] {
    const segments: ContentSegment[] = [];
    const codeBlockRegex = /```(\w*)\n?([\s\S]*?)```/g;
    let lastIndex = 0;
    let match: RegExpExecArray | null;

    while ((match = codeBlockRegex.exec(raw)) !== null) {
      // Text before this code block
      if (match.index > lastIndex) {
        const text = raw.slice(lastIndex, match.index);
        if (text.trim()) segments.push({ type: 'text', content: text });
      }
      // The code block itself
      segments.push({ type: 'code', content: match[2].replace(/\n$/, ''), lang: match[1] || undefined });
      lastIndex = match.index + match[0].length;
    }

    // Remaining text after last code block
    if (lastIndex < raw.length) {
      const text = raw.slice(lastIndex);
      if (text.trim()) segments.push({ type: 'text', content: text });
    }

    // If no code blocks found, return the whole thing as text
    if (segments.length === 0) {
      segments.push({ type: 'text', content: raw });
    }

    return segments;
  }

  async function selectModel(model: string) {
    selectedModel = model;
    userHasDefaultModel = true;
    // Persist the selection as the user's default
    try {
      await invoke('set_default_model', { modelName: model });
    } catch {
      // Non-critical — preference just won't persist
    }
  }

  async function pickImage() {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif'] }],
      });

      if (!selected) return;

      const filePath = selected;

      const fileBytes = await readFile(filePath);

      // Reject oversize images before they hit the WebKit decode pipeline.
      // A 50 MB phone JPEG can spike transient RAM to 150+ MB through
      // canvas decode + toDataURL, risking OOM on a 4 GB box.
      // See AUDIT P3-B6.
      if (fileBytes.byteLength > MAX_IMAGE_BYTES) {
        const mb = (fileBytes.byteLength / (1024 * 1024)).toFixed(1);
        errorMessage = `Image too large (${mb} MB). Resize below 10 MB before attaching, or wait for Phase 4 which moves resize to Rust.`;
        return;
      }

      const blob = new Blob([fileBytes]);
      const dataUrl = await resizeImageBlob(blob, 1024);

      // Extract pure base64 (strip data:image/...;base64, prefix)
      const base64 = dataUrl.split(',')[1];
      pendingImage = base64;
      pendingImagePreview = dataUrl;
    } catch (e) {
      console.error('[Heimdall] Image pick failed:', e);
      errorMessage = `Failed to attach image: ${e}`;
    }
  }

  function clearPendingImage() {
    pendingImage = null;
    pendingImagePreview = null;
  }

  function resizeImageBlob(blob: Blob, maxSize: number): Promise<string> {
    return new Promise((resolve, reject) => {
      const objectUrl = URL.createObjectURL(blob);
      const img = new Image();

      img.onload = () => {
        let { width, height } = img;

        // Scale down if larger than maxSize
        if (width > maxSize || height > maxSize) {
          if (width > height) {
            height = Math.round(height * (maxSize / width));
            width = maxSize;
          } else {
            width = Math.round(width * (maxSize / height));
            height = maxSize;
          }
        }

        const canvas = document.createElement('canvas');
        canvas.width = width;
        canvas.height = height;
        const ctx = canvas.getContext('2d')!;
        ctx.drawImage(img, 0, 0, width, height);

        const dataUrl = canvas.toDataURL('image/jpeg', 0.85);

        // Revoke the blob URL once the canvas has copied its pixels.
        // Without this, WebKit pins the original file's bytes for the
        // app's entire lifetime. See AUDIT P3-B5.
        URL.revokeObjectURL(objectUrl);

        resolve(dataUrl);
      };
      img.onerror = () => {
        URL.revokeObjectURL(objectUrl);
        reject(new Error('Failed to load image'));
      };
      img.src = objectUrl;
    });
  }

  /** Convert a backend message to a frontend ChatMessage, parsing images JSON */
  function backendToChat(m: BackendMessage): ChatMessage {
    let images: string[] | undefined;
    if (m.images) {
      try {
        const raw: string[] = JSON.parse(m.images);
        // Convert raw base64 to data URLs for display
        images = raw.map(b64 => `data:image/jpeg;base64,${b64}`);
      } catch {
        images = undefined;
      }
    }
    return {
      id: m.id,
      role: m.role as 'user' | 'assistant',
      content: m.content ?? '',
      tokens_used: m.tokens_used,
      images,
      thinking: m.thinking ?? undefined,
    };
  }

  /** Toggle thinking block expand/collapse for a message */
  function toggleThinking(msgId: string) {
    const next = new Set(expandedThinking);
    if (next.has(msgId)) {
      next.delete(msgId);
    } else {
      next.add(msgId);
    }
    expandedThinking = next;
  }

  /** Toggle "Memory used" expand/collapse for a message */
  function toggleMemoryUsed(msgId: string) {
    const next = new Set(expandedMemoryUsed);
    if (next.has(msgId)) {
      next.delete(msgId);
    } else {
      next.add(msgId);
    }
    expandedMemoryUsed = next;
  }

  async function loadConversations() {
    try {
      conversations = await invoke<Conversation[]>('list_conversations');
    } catch {
      conversations = [];
    }
  }

  async function switchConversation(conv: Conversation) {    // Trigger extraction for the current conversation before switching
    if (conversationCreated && conversationId) {
      memoryStore.extract(conversationId).catch(() => {});
    }
    conversationId = conv.id;
    conversationCreated = true;
    errorMessage = null;
    streamingContent = '';
    streamingThinking = '';
    isStreaming = false;
    isThinking = false;
    expandedThinking = new Set();
    expandedMemoryUsed = new Set();
    lastMemoryUsed = null;

    if (conv.model && models.some(m => m.name === conv.model)) {
      selectedModel = conv.model!;
    }

    try {
      const collections = await invoke<string[]>('get_active_collections', { conversationId: conv.id });
      activeCollections = collections || [];
    } catch {
      activeCollections = [];
    }

    try {
      memoryEnabled = await memoryStore.getConversationMemory(conv.id);
    } catch {
      memoryEnabled = true;
    }

    try {
      const backendMessages = await invoke<BackendMessage[]>('get_messages', {
        conversationId: conv.id,
      });
      messages = backendMessages
        .filter(m => m.role !== 'system')
        .map(m => backendToChat(m));
    } catch {
      messages = [];
    }

    activeTab = 'chat';
    scrollToBottom(true);
  }

  async function deleteConversation(id: string) {
    try {
      await invoke('delete_conversation', { conversationId: id });
      if (id === conversationId) {
        newChat();
      }
      await loadConversations();
    } catch {
      // Non-critical
    }
  }

  async function openHistoryTab() {
    activeTab = 'history';
    await loadConversations();
  }

  async function cancelStream() {
    try {
      await invoke('cancel_chat_stream', { conversationId });
    } catch (e) {
      console.error(e);
    }
  }

  async function toggleMemory() {
    memoryEnabled = !memoryEnabled;
    if (conversationCreated) {
      await memoryStore.setConversationMemory(conversationId, memoryEnabled);
    }
  }

  async function reExtract() {
    showChatOverflow = false;
    if (!conversationCreated || !conversationId) return;
    await memoryStore.extract(conversationId);
  }

  async function addCollection(name: string) {
    if (!activeCollections.includes(name)) {
      activeCollections = [...activeCollections, name].sort();
      if (conversationCreated) {
        invoke('set_active_collections', { conversationId, collections: activeCollections }).catch(console.error);
      }
    }
    showCollectionPicker = false;
  }

  async function removeCollection(name: string) {
    activeCollections = activeCollections.filter(c => c !== name);
    if (conversationCreated) {
      invoke('set_active_collections', { conversationId, collections: activeCollections }).catch(console.error);
    }
  }
</script>

<svelte:window onkeydown={(e) => {
  if (e.key === 'Escape' && isStreaming) {
    cancelStream();
  }
}} />

<div class="chat-panel">

  <!-- Tab strip -->
  <div class="tabs">
    <button
      class="tab"
      class:active={activeTab === 'chat'}
      onclick={() => activeTab = 'chat'}
    >Chat</button>
    <button
      class="tab"
      class:active={activeTab === 'history'}
      onclick={openHistoryTab}
    >History</button>
  </div>

  <!-- Chat tab content -->
  {#if activeTab === 'chat'}
    <!-- Model bar: selector + new chat + knowledge + status -->
    <div class="model-bar">
      <div class="model-bar-left">
        <ModelSelector
          {models}
          {selectedModel}
          onSelect={selectModel}
        />
        <button
          class="new-chat-btn"
          onclick={newChat}
          title="New Chat (Ctrl+N)"
          aria-label="New chat"
        >
          <Icon paths={iconPlus} size={14} stroke={1.5} />
        </button>

        <!-- Knowledge collections — toolbar entry point. Replaces the
             previous pill-row + sits beside the model selector and
             new-chat button so it matches existing toolbar visual style. -->
        <div class="knowledge-anchor">
          <button
            class="new-chat-btn knowledge-btn"
            class:has-active={activeCollections.length > 0}
            onclick={(e) => { e.stopPropagation(); showCollectionPicker = !showCollectionPicker; }}
            title={activeCollections.length > 0
              ? `Knowledge (${activeCollections.length} active)`
              : 'Attach knowledge collections'}
            aria-label="Knowledge collections"
            aria-expanded={showCollectionPicker}
          >
            <Icon paths={iconBook2} size={14} stroke={1.5} />
          </button>

          {#if showCollectionPicker}
            <!-- Outside-click capture: a transparent overlay below the
                 popover but above page content. Clicking it closes. -->
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="knowledge-popover-overlay" onclick={() => showCollectionPicker = false}></div>

            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="knowledge-popover" onclick={(e) => e.stopPropagation()}>
              {#if ragStore.collections.length === 0}
                <div class="kn-empty">No collections yet. Create one in Knowledge.</div>
              {:else}
                {#if activeCollections.length > 0}
                  <div class="kn-section-label">Active</div>
                  {#each activeCollections as colName}
                    <div class="kn-row active-row">
                      <span class="kn-name" title={colName}>{colName}</span>
                      <button
                        class="kn-remove"
                        onclick={() => removeCollection(colName)}
                        title="Remove from this chat"
                        aria-label="Remove from this chat"
                      >
                        <Icon paths={iconX} size={10} stroke={2} />
                      </button>
                    </div>
                  {/each}
                  <div class="kn-divider"></div>
                {/if}

                <div class="kn-section-label">Available</div>
                {#each ragStore.collections.filter(c => !activeCollections.includes(c.display_name)) as avail}
                  <button class="kn-row available-row" onclick={() => addCollection(avail.display_name)}>
                    <span class="kn-name" title={avail.display_name}>{avail.display_name}</span>
                    <Icon paths={iconPlus} size={10} stroke={2} />
                  </button>
                {:else}
                  <div class="kn-empty kn-empty-inline">All collections active for this chat.</div>
                {/each}
              {/if}
            </div>
          {/if}
        </div>

        <!-- Chat overflow menu -->
        <div class="chat-overflow-anchor">
          <button
            class="new-chat-btn"
            onclick={(e) => { e.stopPropagation(); showChatOverflow = !showChatOverflow; }}
            title="More options"
            aria-label="More chat options"
            aria-expanded={showChatOverflow}
          >
            ···
          </button>

          {#if showChatOverflow}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="chat-overflow-overlay" onclick={() => showChatOverflow = false}></div>

            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="chat-overflow-menu" onclick={(e) => e.stopPropagation()}>
              <button
                class="overflow-item"
                onclick={reExtract}
                disabled={memoryStore.isExtracting || !conversationCreated}
                title="Re-run memory extraction for this conversation"
              >
                {#if memoryStore.isExtracting}
                  Extracting…
                {:else}
                  Re-extract memory
                {/if}
              </button>
            </div>
          {/if}
        </div>
      </div>
      <div class="model-bar-right">
        <MemoryIndicator />
        <MemoryToggle enabled={memoryEnabled} onToggle={toggleMemory} />
        {#if tokensUsed}
          <span class="token-counter" title="Tokens used in last response">
            {tokensUsed} tok
          </span>
        {/if}
        <span class="status-dot" class:online={health.online} class:offline={!health.online}
          title={health.online ? `Ollama ${health.version ?? ''}` : 'Ollama offline'}
        ></span>
      </div>
    </div>

    <!-- Memory extraction notification -->
    {#if memoryStore.hasNewPendingFacts}
      <div class="memory-notification">
        <span class="memory-notif-text">
          {memoryStore.pendingFacts.length} new {memoryStore.pendingFacts.length === 1 ? 'fact' : 'facts'} extracted — review in Memory panel
        </span>
        <button class="memory-notif-dismiss" onclick={() => memoryStore.dismissNewFacts()} aria-label="Dismiss">
          ×
        </button>
      </div>
    {/if}

    <!-- Memory extraction error banner -->
    {#if memoryStore.lastExtractionError}
      <div class="memory-notification memory-notification-error">
        <span class="memory-notif-text">
          Memory extraction failed — {memoryStore.lastExtractionError}
        </span>
        <button
          class="memory-notif-dismiss"
          onclick={() => memoryStore.dismissExtractionError()}
          aria-label="Dismiss extraction error"
        >
          ×
        </button>
      </div>
    {/if}

    <!-- Memory extraction empty hint (ran but found no facts) -->
    {#if memoryStore.lastExtractionWasEmpty}
      <div class="memory-notification memory-notification-empty">
        <span class="memory-notif-text">
          Memory extraction found no new facts in this conversation
        </span>
        <button
          class="memory-notif-dismiss"
          onclick={() => memoryStore.dismissExtractionEmpty()}
          aria-label="Dismiss"
        >
          ×
        </button>
      </div>
    {/if}

    <!-- Message list -->
    <div class="messages-area">

      {#if !health.online}
        <div class="empty-state">
          <Icon paths={iconAlertCircle} size={32} stroke={1.2} />
          <p class="empty-title">Ollama is not running</p>
          <p class="empty-sub">Start Ollama to begin chatting</p>
        </div>
      {:else if messages.length === 0 && !isStreaming}
        <div class="empty-state">
          <Icon paths={iconRobot} size={32} stroke={1.2} />
          <p class="empty-title">New conversation</p>
          <p class="empty-sub">Send a message to begin</p>
        </div>
      {:else}
        {#each messages as msg (msg.id)}
          <div class="message" class:user={msg.role === 'user'} class:assistant={msg.role === 'assistant'}>
            <div class="msg-avatar" class:user={msg.role === 'user'} class:ai={msg.role === 'assistant'}>
              {#if msg.role === 'user'}
                U
              {:else}
                <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
                  <polygon points="7,1 13,7 7,13 1,7" fill="none" stroke="currentColor" stroke-width="0.7"/>
                  <circle cx="7" cy="7" r="2" fill="currentColor"/>
                </svg>
              {/if}
            </div>
            <div class="msg-content">
              {#if msg.role === 'assistant'}
                {#if msg.thinking}
                  <div class="think-block">
                    <button class="think-header" onclick={() => toggleThinking(msg.id)}>
                      <span class="think-diamond">◆</span>
                      <span class="think-label">Thought for a moment</span>
                      <span class="think-chevron" class:expanded={expandedThinking.has(msg.id)}>
                        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                          <path d="M6 9l6 6l6 -6" />
                        </svg>
                      </span>
                    </button>
                    {#if expandedThinking.has(msg.id)}
                      <pre class="think-content">{msg.thinking}</pre>
                    {/if}
                  </div>
                {/if}
                {#each parseContent(msg.content) as segment}
                  {#if segment.type === 'code'}
                    <pre class="msg-code">{segment.content}</pre>
                  {:else}
                    <span class="msg-text">{segment.content}</span>
                  {/if}
                {/each}
                {#if msg.memoryUsed}
                  <div class="memory-used-block">
                    <button
                      class="memory-used-header"
                      onclick={() => toggleMemoryUsed(msg.id)}
                      title="Show the exact memory context sent to the model for this turn"
                    >
                      <span class="memory-used-dot">●</span>
                      <span class="memory-used-label">Memory used</span>
                      <span class="memory-used-chevron" class:expanded={expandedMemoryUsed.has(msg.id)}>
                        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                          <path d="M6 9l6 6l6 -6" />
                        </svg>
                      </span>
                    </button>
                    {#if expandedMemoryUsed.has(msg.id)}
                      <pre class="memory-used-content">{msg.memoryUsed}</pre>
                    {/if}
                  </div>
                {/if}
              {:else}
                {#if msg.images && msg.images.length > 0}
                  <div class="msg-images">
                    {#each msg.images as imgSrc}
                      <img src={imgSrc} alt="Attached" class="msg-thumb" />
                    {/each}
                  </div>
                {/if}
                <span class="msg-text">{msg.content}</span>
              {/if}
            </div>
          </div>
        {/each}

        <!-- Streaming message -->
        {#if isStreaming && (streamingContent || streamingThinking)}
          <div class="message assistant">
            <div class="msg-avatar ai">
              <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
                <polygon points="7,1 13,7 7,13 1,7" fill="none" stroke="currentColor" stroke-width="0.7"/>
                <circle cx="7" cy="7" r="2" fill="currentColor"/>
              </svg>
            </div>
            <div class="msg-content">
              {#if streamingThinking}
                <div class="think-block" class:live={isThinking}>
                  <div class="think-header">
                    <span class="think-diamond">◆</span>
                    <span class="think-label">
                      {#if isThinking}
                        Thinking…
                      {:else}
                        Thought for {thinkingDuration}s
                      {/if}
                    </span>
                  </div>
                  {#if isThinking}
                    <pre class="think-content">{streamingThinking}<span class="cursor-blink">▌</span></pre>
                  {/if}
                </div>
              {/if}
              {#if streamingContent}
                <pre class="msg-text">{streamingContent}<span class="cursor-blink">▌</span></pre>
              {/if}
            </div>
          </div>
        {:else if isStreaming}
          <div class="message assistant">
            <div class="msg-avatar ai streaming-icon">
              <Icon paths={iconLoader2} size={14} stroke={1.5} />
            </div>
            <div class="msg-content">
              <span class="thinking-placeholder">Thinking…</span>
            </div>
          </div>
        {/if}
      {/if}

      <!-- Error display -->
      {#if errorMessage}
        <div class="error-banner">
          <Icon paths={iconAlertCircle} size={14} stroke={1.5} />
          <span>{errorMessage}</span>
        </div>
      {/if}

      <div bind:this={messagesEnd}></div>
    </div>

    <!-- Active collections are now selected from the toolbar's Knowledge
         button; no pill row above the input. -->

    <!-- Input bar -->
    <div class="input-bar">
      {#if capabilitiesLoading}
        <div class="input-action shimmer" aria-label="Loading capabilities"></div>
      {:else if showImageButton}
        <button class="input-action" title="Attach image" aria-label="Attach image" onclick={pickImage}>
          <Icon paths={iconPhoto} size={16} stroke={1.5} />
        </button>
      {/if}

      <div class="input-wrapper">
        {#if pendingImagePreview}
          <div class="pending-image">
            <img src={pendingImagePreview} alt="Attached" class="pending-thumb" />
            <button class="pending-remove" onclick={clearPendingImage} aria-label="Remove image">
              <Icon paths={iconX} size={12} stroke={2} />
            </button>
          </div>
        {/if}

        <textarea
          bind:this={inputEl}
          bind:value={inputText}
          onkeydown={handleKeydown}
          placeholder={health.online ? 'Ask Heimdall…' : 'Ollama offline'}
          disabled={!health.online || isStreaming}
          rows={1}
          class="input-field"
          aria-label="Message input"
        ></textarea>
      </div>

      <div class="send-container">
        {#if isStreaming}
          <button class="stop-btn" onclick={cancelStream} title="Stop (Escape)" aria-label="Stop generating">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
              <rect x="6" y="6" width="12" height="12" />
            </svg>
          </button>
        {:else}
          <button
            class="send-btn"
            onclick={sendMessage}
            disabled={!canSend}
            title="Send (Enter)"
            aria-label="Send message"
          >
            <Icon paths={iconSend2} size={16} stroke={1.5} />
          </button>
        {/if}
      </div>
    </div>
  {:else if activeTab === 'history'}
    <div class="history-panel">
      {#if conversations.length === 0}
        <div class="empty-state">
          <p class="empty-title">No conversations yet</p>
          <p class="empty-sub">Start chatting to build history</p>
        </div>
      {:else}
        <div class="history-list">
          {#each conversations as conv (conv.id)}
            <div
              class="history-item"
              class:active={conv.id === conversationId}
              role="button"
              tabindex="0"
              onclick={() => switchConversation(conv)}
              onkeydown={(e) => { if (e.key === 'Enter') switchConversation(conv); }}
            >
              <div class="history-item-content">
                <span class="history-title">{conv.title ?? 'New Chat'}</span>
                <span class="history-meta">{conv.model ?? ''}</span>
              </div>
              <button
                class="history-delete"
                onclick={(e) => { e.stopPropagation(); deleteConversation(conv.id); }}
                title="Delete conversation"
                aria-label="Delete conversation"
              >×</button>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}

</div>

<style>
  .chat-panel {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
  }

  /* ── Tab strip ──────────────────────────── */
  .tabs {
    display: flex;
    background: var(--bg-titlebar);
    border-bottom: 0.5px solid var(--border-subtle);
    padding: 0 var(--space-md);
    flex-shrink: 0;
  }

  .tab {
    padding: 9px 14px;
    font-family: var(--font-ui);
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--text-ghost);
    cursor: pointer;
    border: none;
    background: transparent;
    border-bottom: 1.5px solid transparent;
    margin-bottom: -0.5px;
    transition: color 0.15s;
  }
  .tab:hover:not(.active) {
    color: var(--text-dim);
  }
  .tab.active {
    color: var(--gold-primary);
    border-bottom-color: var(--gold-primary);
  }

  /* ── Model bar (below tabs, chat only) ─── */
  .model-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-sm) var(--space-lg);
    border-bottom: 0.5px solid var(--border-subtle);
    flex-shrink: 0;
    background: var(--bg-app);
    position: relative;
    z-index: 10;
  }

  .model-bar-left {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .model-bar-right {
    display: flex;
    align-items: center;
    gap: var(--space-md);
  }

  .new-chat-btn {
    width: 28px;
    height: 28px;
    border-radius: var(--radius-md);
    border: 0.5px solid var(--border-subtle);
    background: var(--bg-elevated);
    color: var(--text-dim);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
  }
  .new-chat-btn:hover {
    color: var(--gold-primary);
    border-color: var(--border-warm);
  }

  .token-counter {
    font-size: 10px;
    color: var(--text-ghost);
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.05em;
  }

  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .status-dot.online {
    background: var(--status-ok-text);
    box-shadow: 0 0 4px var(--status-ok-text);
  }
  .status-dot.offline {
    background: var(--status-danger-text);
  }


  /* ── History panel ──────────────────────── */
  .history-panel {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  .history-list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-sm) 0;
  }

  .history-list::-webkit-scrollbar {
    width: 4px;
  }
  .history-list::-webkit-scrollbar-track {
    background: transparent;
  }
  .history-list::-webkit-scrollbar-thumb {
    background: var(--border-subtle);
    border-radius: var(--radius-pill);
  }

  .history-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 10px var(--space-lg);
    border: none;
    background: transparent;
    cursor: pointer;
    text-align: left;
    border-bottom: 0.5px solid var(--border-subtle);
    transition: background 0.12s;
  }
  .history-item:hover {
    background: var(--bg-elevated);
  }
  .history-item.active {
    background: var(--gold-bg);
    border-left: 2px solid var(--gold-primary);
  }

  .history-item-content {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .history-title {
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .history-item.active .history-title {
    color: var(--gold-primary);
  }

  .history-meta {
    font-size: 10px;
    color: var(--text-ghost);
    letter-spacing: 0.03em;
  }

  .history-delete {
    width: 22px;
    height: 22px;
    border-radius: var(--radius-md);
    border: none;
    background: transparent;
    color: var(--text-ghost);
    font-size: 14px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    opacity: 0;
    transition: opacity 0.12s, color 0.12s;
  }
  .history-item:hover .history-delete {
    opacity: 1;
  }
  .history-delete:hover {
    color: var(--status-danger-text);
    background: var(--status-danger-bg);
  }

  /* ── Messages area ──────────────────────── */
  .messages-area {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-xl) var(--space-xl) 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-lg);
    min-height: 0;
  }

  .messages-area::-webkit-scrollbar {
    width: 4px;
  }
  .messages-area::-webkit-scrollbar-track {
    background: transparent;
  }
  .messages-area::-webkit-scrollbar-thumb {
    background: var(--border-subtle);
    border-radius: var(--radius-pill);
  }

  .empty-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-sm);
    color: var(--text-ghost);
  }
  .empty-title {
    font-size: 14px;
    color: var(--text-dim);
  }
  .empty-sub {
    font-size: 11px;
    color: var(--text-ghost);
  }

  /* ── Message rows ─────────────────────── */
  .message {
    display: flex;
    gap: var(--space-md);
    align-items: flex-start;
  }

  .msg-avatar {
    width: 26px;
    height: 26px;
    border-radius: var(--radius-md);
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    margin-top: 1px;
    font-size: 11px;
    font-weight: 500;
  }
  .msg-avatar.user {
    background: var(--bg-elevated);
    color: var(--text-dim);
    border: 0.5px solid var(--border-dim);
  }
  .msg-avatar.ai {
    background: var(--gold-bg);
    color: var(--gold-primary);
    border: 0.5px solid var(--border-warm);
  }

  .msg-content {
    min-width: 0;
    max-width: 560px;
  }

  .msg-images {
    display: flex;
    gap: var(--space-sm);
    margin-bottom: 6px;
  }

  .msg-thumb {
    width: 120px;
    height: 90px;
    object-fit: cover;
    border-radius: var(--radius-md);
    border: 0.5px solid var(--border-subtle);
  }

  .msg-text {
    font-family: var(--font-ui);
    font-size: 12px;
    line-height: 1.7;
    color: var(--text-muted);
    white-space: pre-wrap;
    word-break: break-word;
    margin: 0;
  }
  .message.assistant .msg-text {
    color: var(--text-secondary);
  }

  .msg-code {
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: 8px 12px;
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-code);
    margin-top: 6px;
    letter-spacing: 0.02em;
    white-space: pre;
    overflow-x: auto;
    line-height: 1.6;
  }

  .cursor-blink {
    animation: blink 0.8s step-end infinite;
    color: var(--gold-primary);
  }
  @keyframes blink {
    0%, 100% { opacity: 1; }
    50% { opacity: 0; }
  }

  .streaming-icon {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  .thinking-placeholder {
    font-size: 12px;
    color: var(--text-ghost);
    font-style: italic;
  }

  /* ── Thinking blocks ────────────────────── */
  .think-block {
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    background: var(--bg-surface);
    margin-bottom: var(--space-sm);
    overflow: hidden;
  }
  .think-block.live {
    border-color: var(--border-warm);
  }

  .think-header {
    display: flex;
    align-items: center;
    gap: var(--space-xs, 4px);
    padding: 6px 10px;
    background: transparent;
    border: none;
    cursor: pointer;
    width: 100%;
    text-align: left;
    font-family: var(--font-ui);
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-dim);
    transition: color 0.12s;
  }
  .think-header:hover {
    color: var(--text-secondary);
  }

  .think-diamond {
    color: var(--gold-primary);
    font-size: 10px;
  }

  .think-label {
    flex: 1;
  }

  .think-chevron {
    display: flex;
    align-items: center;
    color: var(--gold-primary);
    transition: transform 0.15s;
  }
  .think-chevron.expanded {
    transform: rotate(180deg);
  }

  .think-content {
    padding: 0 10px 8px;
    font-family: var(--font-ui);
    font-size: 11px;
    line-height: 1.6;
    color: var(--text-ghost);
    white-space: pre-wrap;
    word-break: break-word;
    margin: 0;
    max-height: 300px;
    overflow-y: auto;
    border-top: 0.5px solid var(--border-subtle);
  }

  .think-content::-webkit-scrollbar {
    width: 3px;
  }
  .think-content::-webkit-scrollbar-thumb {
    background: var(--border-subtle);
    border-radius: var(--radius-pill);
  }

  /* ── Memory used (per-turn transparency) ─ */
  .memory-used-block {
    margin-top: var(--space-sm);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    overflow: hidden;
    align-self: flex-start;
    max-width: 100%;
  }

  .memory-used-header {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    width: 100%;
    padding: 4px 8px;
    background: transparent;
    border: none;
    cursor: pointer;
    color: var(--text-dim);
    font-family: var(--font-ui);
    font-size: 10px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    transition: color 0.1s, background 0.1s;
  }

  .memory-used-header:hover {
    color: var(--gold-primary);
    background: var(--gold-bg);
  }

  .memory-used-dot {
    color: var(--gold-primary);
    font-size: 8px;
    line-height: 1;
  }

  .memory-used-label {
    flex: 1;
    text-align: left;
  }

  .memory-used-chevron {
    color: var(--gold-dim);
    display: inline-flex;
    transition: transform 0.15s ease;
  }

  .memory-used-chevron.expanded {
    transform: rotate(180deg);
  }

  .memory-used-content {
    padding: 0 10px 8px;
    font-family: var(--font-ui);
    font-size: 11px;
    line-height: 1.6;
    color: var(--text-ghost);
    white-space: pre-wrap;
    word-break: break-word;
    margin: 0;
    max-height: 240px;
    overflow-y: auto;
    border-top: 0.5px solid var(--border-subtle);
  }

  .memory-used-content::-webkit-scrollbar {
    width: 3px;
  }
  .memory-used-content::-webkit-scrollbar-thumb {
    background: var(--border-subtle);
    border-radius: var(--radius-pill);
  }

  /* ── Error banner ───────────────────────── */
  .error-banner {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    background: var(--status-danger-bg);
    border: 0.5px solid var(--status-danger-text);
    border-radius: var(--radius-md);
    color: var(--status-danger-text);
    font-size: 12px;
  }

  /* ── Knowledge collections (toolbar dropdown) ────────────────
   *  The Knowledge button replaces the previous pill-row / + icon.
   *  Visual style matches .new-chat-btn so it reads as a peer in the
   *  toolbar. The dropdown uses var(--shadow-popover) and the same
   *  surface tokens as ModelSelector for consistency.                */

  .knowledge-anchor {
    position: relative;
    display: flex;
    align-items: center;
  }

  .knowledge-btn {
    /* Inherits the .new-chat-btn base style; this rule layers on top. */
    position: relative;
  }

  /* Active-state dot — appears top-right when ≥1 collection is active. */
  .knowledge-btn.has-active::after {
    content: '';
    position: absolute;
    top: 4px;
    right: 4px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--gold-primary);
    box-shadow: var(--shadow-elevated);
  }

  .knowledge-popover-overlay {
    position: fixed;
    inset: 0;
    background: transparent;
    z-index: 99;
  }

  .knowledge-popover {
    position: absolute;
    top: calc(100% + var(--space-xs));
    left: 0;
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: var(--space-xs);
    box-shadow: var(--shadow-popover);
    display: flex;
    flex-direction: column;
    min-width: 220px;
    max-width: 280px;
    z-index: 100;
  }

  .kn-section-label {
    font-family: var(--font-ui);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-ghost);
    padding: var(--space-xs) var(--space-sm);
  }

  .kn-divider {
    height: 0.5px;
    background: var(--border-subtle);
    margin: var(--space-xs) 0;
  }

  .kn-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-sm);
    background: transparent;
    border: none;
    color: var(--text-base);
    font-family: var(--font-ui);
    font-size: 12px;
    padding: var(--space-sm);
    border-radius: var(--radius-sm);
    text-align: left;
    cursor: pointer;
    width: 100%;
  }

  .kn-row.available-row:hover {
    background: var(--bg-app);
    color: var(--gold-primary);
  }

  .kn-row.active-row {
    cursor: default;
  }

  .kn-name {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .kn-remove {
    background: transparent;
    border: none;
    color: var(--text-ghost);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2px;
    border-radius: 50%;
  }
  .kn-remove:hover {
    color: var(--accent-red);
    background: var(--bg-app);
  }

  .kn-empty {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
    padding: var(--space-md);
    text-align: center;
  }
  .kn-empty-inline {
    padding: var(--space-sm) var(--space-md);
  }

  /* ── Chat overflow menu ─────────────────── */
  .chat-overflow-anchor {
    position: relative;
  }

  .chat-overflow-overlay {
    position: fixed;
    inset: 0;
    z-index: 99;
  }

  .chat-overflow-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 100;
    background: var(--bg-elevated);
    border: 0.5px solid var(--border-dim);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-popover);
    min-width: 160px;
    overflow: hidden;
  }

  .overflow-item {
    display: block;
    width: 100%;
    text-align: left;
    font-family: var(--font-ui);
    font-size: 11px;
    padding: var(--space-sm) var(--space-md);
    background: transparent;
    border: none;
    color: var(--text-base);
    cursor: pointer;
    transition: background 0.1s, color 0.1s;
  }

  .overflow-item:hover:not(:disabled) {
    background: var(--bg-app);
    color: var(--gold-primary);
  }

  .overflow-item:disabled {
    color: var(--text-ghost);
    cursor: default;
  }

  /* ── Input bar ──────────────────────────── */
  .input-bar {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-md) var(--space-lg) 14px;
    border-top: 0.5px solid var(--border-subtle);
    background: var(--bg-titlebar);
    flex-shrink: 0;
  }

  .input-action {
    width: 32px;
    height: 32px;
    border-radius: var(--radius-md);
    border: 0.5px solid var(--border-subtle);
    background: var(--bg-elevated);
    color: var(--text-dim);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
    flex-shrink: 0;
  }
  .input-action:hover {
    color: var(--gold-primary);
    border-color: var(--border-warm);
  }

  .input-action.shimmer {
    width: 32px;
    height: 32px;
    border-radius: var(--radius-sm);
    border: none;
    background: linear-gradient(
      90deg,
      var(--color-bg-elev, var(--bg-elevated)) 25%,
      var(--color-bg-hover, var(--bg-surface)) 50%,
      var(--color-bg-elev, var(--bg-elevated)) 75%
    );
    background-size: 200% 100%;
    animation: shimmer 1.5s infinite;
  }

  @keyframes shimmer {
    0% { background-position: 200% 0; }
    100% { background-position: -200% 0; }
  }

  .input-wrapper {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: var(--space-xs, 4px);
    min-width: 0;
  }

  .pending-image {
    position: relative;
    display: inline-block;
    width: 48px;
    height: 48px;
    margin-bottom: 2px;
  }

  .pending-thumb {
    width: 48px;
    height: 48px;
    object-fit: cover;
    border-radius: var(--radius-md);
    border: 0.5px solid var(--border-subtle);
  }

  .pending-remove {
    position: absolute;
    top: -4px;
    right: -4px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: none;
    background: var(--bg-elevated);
    color: var(--text-dim);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    box-shadow: var(--shadow-elevated);
  }
  .pending-remove:hover {
    color: var(--status-danger-text);
  }

  .input-field {
    flex: 1;
    resize: none;
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    background: var(--bg-surface);
    color: var(--text-dim);
    font-family: var(--font-ui);
    font-size: 11px;
    padding: var(--space-sm) 14px;
    line-height: 1.5;
    min-height: 36px;
    max-height: 120px;
    outline: none;
    letter-spacing: 0.04em;
    transition: border-color 0.15s;
  }
  .input-field::placeholder {
    color: var(--text-ghost);
  }
  .input-field:focus {
    border-color: var(--border-dim);
  }
  .input-field:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .send-btn {
    width: 32px;
    height: 32px;
    border-radius: var(--radius-lg);
    border: 0.5px solid var(--border-warm);
    background: var(--gold-bg);
    color: var(--gold-primary);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background 0.15s, opacity 0.15s;
    flex-shrink: 0;
  }
  .send-btn:hover:not(:disabled) {
    background: var(--border-warm);
  }
  .send-btn:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .send-container {
    display: flex;
    align-items: center;
  }

  .stop-btn {
    width: 32px;
    height: 32px;
    border-radius: var(--radius-lg);
    border: 0.5px solid var(--border-subtle);
    background: var(--bg-surface);
    color: var(--text-dim);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
    flex-shrink: 0;
  }
  .stop-btn:hover {
    background: var(--accent-red);
    color: var(--bg-app);
    border-color: var(--accent-red);
  }

  .memory-notification {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-xs) var(--space-md);
    background: var(--gold-bg);
    border-bottom: 0.5px solid var(--gold-dim);
    flex-shrink: 0;
  }

  .memory-notification-error {
    background: var(--status-danger-bg);
    border-color: var(--accent-red);
    color: var(--accent-red);
  }

  .memory-notification-empty {
    background: var(--bg-elevated);
    border-color: var(--border-dim);
    color: var(--text-dim);
  }

  .memory-notif-text {
    font-size: 11px;
    color: var(--gold-primary);
    letter-spacing: 0.02em;
  }

  .memory-notif-dismiss {
    font-family: var(--font-ui);
    font-size: 14px;
    color: var(--text-dim);
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 0 var(--space-xs);
    line-height: 1;
  }

  .memory-notif-dismiss:hover {
    color: var(--text-primary);
  }
</style>
