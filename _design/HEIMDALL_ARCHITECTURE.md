# HEIMDALL v1.0
## Architecture, Vision & Engineering Specification
**Author: Sandeep + Claude**
**Started: 2026**
**Philosophy: What if Linus Torvalds had an AI assistant to build software?**

---

## THE MISSION STATEMENT

Heimdall is the fastest, most resource-efficient local AI desktop application ever built for Linux. It runs on hardware that every other tool ignores. It treats 4GB RAM not as a limitation but as the design target. Everything above that is a bonus. Heimdall adapts to what the machine has — it never fights the machine.

Heimdall is not a wrapper. It is not a pretty face on Ollama. It is a complete system: a conversation engine, a knowledge engine, a resource governor, and a multimodal interface — all running locally, all owned by the user, all optimized to consume the minimum resources possible.

Built by one developer and one AI. Documented like a spacecraft. Engineered like a kernel module.

---

## CORE PRINCIPLES — NEVER VIOLATE THESE

1. **Hardware first.** Every feature is designed for 4GB RAM, no GPU. Better hardware gets better experience automatically. Worse hardware never breaks the experience.
2. **Heimdall is invisible.** The Rust backend targets under 40 MB resident at idle (the part we control). The model and the WebView need the rest of the RAM. Heimdall gets out of the way.
3. **No external servers.** No ChromaDB process. No Qdrant daemon. No separate Python process. Everything that Heimdall needs runs inside Heimdall or inside Ollama. One app, one process tree. **Zero network calls on cold launch** — fonts ship bundled, no CDN.
4. **Adaptive behavior.** Heimdall detects available RAM, VRAM, and CPU at startup and configures itself accordingly. A 4GB machine and a 32GB machine both run Heimdall — they just get different capability tiers automatically.
5. **Everything is documented.** Every architectural decision has a reason written down. Every module has a doc comment. The DEVLOG is updated with every meaningful change.

---

## SYSTEM ARCHITECTURE OVERVIEW

```
┌─────────────────────────────────────────────────────────────────┐
│                         HEIMDALL                                 │
│                                                                  │
│  ┌─────────────┐   ┌──────────────┐   ┌─────────────────────┐  │
│  │   SVELTE    │   │  TAURI CORE  │   │   RUST MODULES      │  │
│  │  FRONTEND   │◄──►  (IPC BRIDGE)│◄──►                     │  │
│  │             │   │              │   │  • ollama_client     │  │
│  │  • Chat UI  │   │              │   │  • governor          │  │
│  │  • Governor │   │              │   │  • rag_engine        │  │
│  │  • RAG UI   │   │              │   │  • input_processor   │  │
│  │  • Settings │   │              │   │  • shortcut_manager  │  │
│  └─────────────┘   └──────────────┘   │  • adaptive_config   │  │
│                                        └─────────────────────┘  │
│                                                 │                │
│                    ┌────────────────────────────┘                │
│                    ▼                                             │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                  LOCAL DATA LAYER                        │   │
│  │                                                          │   │
│  │  ~/.heimdall/                                            │   │
│  │  ├── db/           SQLite (chat history, metadata)       │   │
│  │  ├── vectors/      Embedded vector store (usearch)       │   │
│  │  ├── knowledge/    Raw ingested files                     │   │
│  │  └── config.toml   App configuration                     │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                  │
└──────────────────────────────┬──────────────────────────────────┘
                               │ HTTP (localhost only)
                               ▼
                    ┌─────────────────────┐
                    │       OLLAMA        │
                    │  (model inference)  │
                    │  localhost:11434    │
                    └─────────────────────┘
```

---

## MODULE SPECIFICATIONS

### MODULE 1: ADAPTIVE CONFIG ENGINE
**File:** `src-tauri/src/adaptive_config.rs`
**Purpose:** Heimdall's brain that runs at startup. Detects hardware and sets capability tiers.

**How it works:**
On startup, before any UI renders, this module reads:
- Total RAM
- Available RAM
- VRAM (if GPU exists via sysfs or nvidia-smi)
- CPU core count
- Disk space at ~/.heimdall/

It assigns one of three tiers:

```
TIER 1 — Minimal (< 6GB RAM, no GPU)
  - RAG: disabled by default, user can enable with warning
  - Embedding: use smallest possible model (nomic-embed-text or all-minilm)
  - Chunk size: 256 tokens max
  - Vector store: memory-mapped, max 10,000 vectors
  - Auto-unload: aggressive (unload after 2 min idle)
  - UI: full functionality, no visual degradation

TIER 2 — Standard (6-16GB RAM, optional GPU)
  - RAG: enabled, moderate chunk sizes
  - Embedding: nomic-embed-text
  - Chunk size: 512 tokens
  - Vector store: up to 100,000 vectors
  - Auto-unload: moderate (unload after 10 min idle)

TIER 3 — Full (16GB+ RAM, GPU available)
  - RAG: fully enabled
  - Embedding: best available model
  - Chunk size: 1024 tokens
  - Vector store: unlimited
  - Auto-unload: conservative (user preference)
```

This tier is stored in config and re-evaluated on every launch. User can override any setting manually. The tier is shown in the UI so the user always knows what mode they are in.

---

### MODULE 2: OLLAMA CLIENT
**File:** `src-tauri/src/ollama_client.rs`
**Purpose:** All communication with Ollama. Clean, typed, no raw HTTP strings scattered across the codebase.

**Responsibilities:**
- List available models with their capabilities (text, vision, embedding, audio)
- Detect model type from model metadata (Ollama returns this in model info)
- Stream chat completions token by token via Tauri events to frontend
- Send multimodal requests (text + base64 image)
- Pull/delete models
- Check Ollama health and version
- Handle Ollama being offline gracefully (show clear error, retry button)

**Model capability detection:**
```rust
pub enum ModelCapability {
    TextOnly,
    Vision,        // accepts image input
    Embedding,     // for RAG, not chat
    Audio,         // whisper-style models
    Multimodal,    // text + image + more
}
```

Heimdall reads the model family and template from Ollama's model info endpoint to determine capability. This drives what input options appear in the UI. If a text-only model is selected, the image upload button disappears. If a vision model is selected, it appears. The UI adapts to the model.

**Streaming:**
Every chat completion streams via a Tauri event channel. The frontend receives token events and appends them in real time. This is non-negotiable for responsiveness on slow hardware — the user sees output immediately, not after a 30-second wait.

---

### MODULE 3: INPUT PROCESSOR
**File:** `src-tauri/src/input_processor.rs`
**Purpose:** Takes raw user input (text, image files, audio files) and prepares them for the model.

**Text input:** Passed directly. Trimmed, validated, injected with system prompt and memory context.

**Image input:**
- Accepts: PNG, JPG, WEBP, GIF
- Resizes to max 1024px on longest side before encoding (reduces token usage dramatically)
- Converts to base64
- Passes to Ollama vision endpoint
- On low-memory tier: warns user if image is large, offers to resize further

**Audio input:**
- Accepts: WAV, MP3, OGG, FLAC
- Transcribes locally using a Whisper model via Ollama (whisper model must be pulled)
- Transcription result becomes text input to the chat model
- Shows transcription to user before sending so they can edit it
- On Tier 1 hardware: transcription is optional, user warned about RAM usage

**The pipeline:**
```
User input
    │
    ▼
Input type detection
    │
    ├── Text ──────────────────────────► inject context ──► Ollama
    │
    ├── Image ──► resize ──► base64 ──► inject context ──► Ollama (vision)
    │
    └── Audio ──► Whisper transcribe ──► show user ──► inject context ──► Ollama
```

---

### MODULE 4: RAG ENGINE
**File:** `src-tauri/src/rag_engine.rs`
**Purpose:** The knowledge layer. Ingest anything. Retrieve fast. Work on 4GB RAM.

**This is the hardest module. It must be designed perfectly.**

**Vector Store Choice: usearch**
Not ChromaDB. Not Qdrant. Not Weaviate. Those are servers. Heimdall uses `usearch` — a Rust-native, embedded, memory-mapped vector similarity search library. It runs inside the Heimdall process. It requires zero setup. It works on 4GB RAM. On Tier 1, it memory-maps the index to disk so RAM usage stays low. On Tier 3, it loads fully into RAM for speed.

**Embedding Model:**
`nomic-embed-text` via Ollama for all tiers. It is small (274MB), fast, and produces high quality 768-dimension embeddings. On Tier 1, it is loaded only when a RAG operation is needed and unloaded immediately after. The Governor handles this automatically.

**Ingestion Pipeline:**

```
Source file/URL
    │
    ▼
Document Loader (by type)
    │
    ├── PDF ──────► pdf-extract (Rust crate) ──► raw text
    ├── TXT/MD ───► read directly
    ├── DOCX ─────► docx-rs ──► raw text
    ├── Images ───► vision model description ──► raw text
    ├── URLs ─────► headless HTTP fetch ──► HTML strip ──► raw text
    ├── Code ─────► read directly, preserve structure
    └── Folders ──► recurse, process each file by type
    │
    ▼
Text Chunker
    │
    ├── Tier 1: 256 token chunks, 32 token overlap
    ├── Tier 2: 512 token chunks, 64 token overlap
    └── Tier 3: 1024 token chunks, 128 token overlap
    │
    ▼
Embedding Generator (nomic-embed-text via Ollama)
    │
    ▼
usearch vector store ──► stored in ~/.heimdall/vectors/
    │
SQLite metadata ──► stored in ~/.heimdall/db/
(chunk text, source file, page number, timestamp)
```

**Retrieval Pipeline:**

```
User query
    │
    ▼
Query embedding (nomic-embed-text)
    │
    ▼
usearch similarity search (top-k, k=5 on Tier 1, k=10 on Tier 3)
    │
    ▼
Fetch chunk text from SQLite by chunk IDs
    │
    ▼
Re-rank by relevance score (cosine similarity threshold: 0.7)
    │
    ▼
Inject into prompt as context
    │
    ▼
Ollama inference
```

**Knowledge Collections:**
Users organize knowledge into named collections — "Work Documents", "Python Reference", "My Notes". Each collection is a separate vector namespace. When chatting, user selects which collections are active. This prevents irrelevant knowledge from polluting context and reduces retrieval time on low-end hardware.

**Memory vs RAG distinction:**
- **RAG** = external knowledge the user ingests. Static documents. Retrieved by semantic similarity.
- **Memory** = things the model learned about the user from conversation. Stored as structured facts in SQLite. Injected as a short system prompt prefix. Example: "User's name is Sandeep. User prefers Python. User is building Heimdall."

Memory extraction happens automatically after each conversation using a small, fast model. The extracted facts are reviewed by the user before saving — nothing is stored without confirmation.

---

### MODULE 5: GOVERNOR
**File:** `src-tauri/src/governor.rs`
**Purpose:** System stability guardian. The most important feature on low-end hardware.

**Monitoring (every 2 seconds):**
- RAM total / used / available (via `/proc/meminfo`)
- CPU usage per core (via `/proc/stat`)
- VRAM if GPU exists (via `/sys/class/drm/` or `nvidia-smi`)
- Swap usage
- Ollama process memory consumption
- Heimdall process memory consumption

**Automatic actions:**

```
RAM available < 800MB  ──► WARNING: suggest unloading idle models
RAM available < 400MB  ──► AUTO-UNLOAD: unload longest-idle model, notify user
RAM available < 200MB  ──► CRITICAL: unload all models, pause ingestion, alert
```

Thresholds are configurable. Auto-unload can be disabled by the user (with a warning that they accept stability risks).

**Model idle tracking:**
Every model gets a last-used timestamp. Governor knows which model has been idle longest and targets that one first for unload.

**Adaptive embedding:**
When a RAG operation needs the embedding model but RAM is tight, Governor checks if the chat model can be temporarily offloaded. If yes, it orchestrates: unload chat model → run embedding → reload chat model. This is transparent to the user except for a small status indicator.

---

### MODULE 6: SHORTCUT MANAGER
**File:** `src-tauri/src/shortcut_manager.rs`
**Purpose:** Global and in-app keyboard shortcuts for power users.

Shortcuts stored in `config.toml`. Editable from the UI. Registered with Tauri's global shortcut API.

**Default shortcuts:**
```
Ctrl+N          New chat
Ctrl+M          Switch model
Ctrl+G          Open Governor
Ctrl+B          Toggle sidebar
Ctrl+K          Open knowledge base
Ctrl+Shift+I    Ingest file/folder
Ctrl+Enter      Send message
Escape          Cancel streaming response
```

---

## DATA LAYER

**Location:** `~/.heimdall/`

```
~/.heimdall/
├── config.toml           App config, user preferences, tier override
├── db/
│   └── heimdall.db       SQLite: chats, messages, memory facts, ingestion metadata
├── vectors/
│   ├── default.usearch   Default knowledge collection vector index
│   └── [name].usearch    Named collections
├── knowledge/
│   └── [collection]/     Raw ingested files (kept for re-ingestion if needed)
└── logs/
    └── heimdall.log      Rotating log, max 10MB
```

**SQLite Schema (key tables):**

```sql
-- Conversations
CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    title TEXT,
    model TEXT,
    created_at INTEGER,
    updated_at INTEGER
);

-- Messages
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    conversation_id TEXT,
    role TEXT,          -- 'user' | 'assistant' | 'system'
    content TEXT,
    input_type TEXT,    -- 'text' | 'image' | 'audio'
    tokens_used INTEGER,
    created_at INTEGER,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id)
);

-- Memory facts
CREATE TABLE memory_facts (
    id TEXT PRIMARY KEY,
    fact TEXT,
    source_conversation_id TEXT,
    confirmed_by_user INTEGER DEFAULT 0,
    created_at INTEGER
);

-- RAG chunks
CREATE TABLE rag_chunks (
    id TEXT PRIMARY KEY,
    collection TEXT,
    source_path TEXT,
    chunk_index INTEGER,
    content TEXT,
    token_count INTEGER,
    vector_id INTEGER,  -- references usearch internal ID
    created_at INTEGER
);

-- Ingestion jobs
CREATE TABLE ingestion_jobs (
    id TEXT PRIMARY KEY,
    source_path TEXT,
    collection TEXT,
    status TEXT,        -- 'pending' | 'running' | 'done' | 'failed'
    chunks_total INTEGER,
    chunks_done INTEGER,
    error TEXT,
    created_at INTEGER,
    completed_at INTEGER
);
```

---

## FRONTEND PANELS

### Panel 1: Chat
- Model selector (shows capability icons: text/vision/audio)
- Active knowledge collections selector (pill badges, toggleable)
- Memory indicator (shows how many memory facts are active)
- Message list with streaming
- Input bar: text field + image upload button (appears only for vision models) + audio record/upload button (appears only when Whisper is available)
- Token counter (live, shows context window usage)

### Panel 2: Knowledge Base
- Collection manager (create, rename, delete collections)
- Ingestion interface: drag-and-drop files, paste URL, select folder
- Ingestion progress (live progress bar per job)
- Collection stats: document count, chunk count, last updated
- Search bar to test retrieval (shows what chunks would be returned for a query)

### Panel 3: Memory
- List of all stored memory facts
- Each fact: text, source conversation link, confirmed/unconfirmed status
- Confirm, edit, or delete individual facts
- Toggle memory on/off per conversation

### Panel 4: Governor
- Resource cards (RAM, CPU, VRAM, Swap)
- Loaded models list with unload buttons
- Auto-unload threshold sliders
- Current hardware tier display
- Heimdall's own memory consumption (the app must be honest about itself)

### Panel 5: Settings
- Model management (pull, delete models)
- Default model per input type
- Hardware tier override
- Shortcut manager
- RAG settings (chunk size override, embedding model choice)
- Theme (only dark, because this is a serious tool)
- About: version, Sandeep's name, build date

---

## DEVLOG SYSTEM

Every meaningful development event is recorded in `DEVLOG.md` at the project root. This is not a changelog. It is an engineering journal.

**Format:**

```markdown
## [DATE] — [WHAT CHANGED]

**Why:** The reason this decision was made.
**What:** What was built or changed.
**How:** Key technical decisions.
**Result:** What works now that did not before.
**Next:** What this unlocks or requires next.
```

The agent maintains this file. Every session that produces working code ends with a DEVLOG entry. This creates a complete record of how Heimdall was built — every decision, every pivot, every lesson.

Additionally, each module has its own `MODULE_[NAME].md` in a `docs/` folder explaining its design, its API surface (Tauri commands it exposes), and known limitations.

---

## BUILD ORDER

This is the sequence. Do not deviate.

```
PHASE 0 — FOUNDATION
  [ ] Tauri + Svelte project initialized fresh
  [ ] Design token file created (tokens.ts)
  [ ] Folder structure established
  [ ] SQLite schema created and migrated
  [ ] config.toml structure defined
  [ ] DEVLOG.md initialized

PHASE 1 — CORE ENGINE
  [ ] adaptive_config.rs — hardware detection and tier assignment
  [ ] ollama_client.rs — all Ollama communication, streaming
  [ ] governor.rs — resource monitoring (polling, no auto-actions yet)

PHASE 2 — BASIC UI (CHAT WORKS)
  [ ] App shell (titlebar, sidebar, layout)
  [ ] Chat panel (messages, streaming, model selector)
  [ ] Input bar (text only first)
  [ ] SQLite chat persistence

PHASE 3 — MULTIMODAL INPUT
  [ ] input_processor.rs — image resizing and encoding
  [ ] Image input in chat UI (vision models only)
  [ ] input_processor.rs — audio transcription via Whisper
  [ ] Audio input in chat UI

PHASE 4 — RAG ENGINE
  [ ] rag_engine.rs — usearch integration, embedding pipeline
  [ ] Document loaders (PDF, TXT, MD, DOCX, URL, code, folders)
  [ ] Knowledge Base panel UI
  [ ] Ingestion progress UI
  [ ] RAG retrieval wired into chat pipeline

PHASE 5 — MEMORY
  [ ] Memory fact extraction after conversations
  [ ] Memory review UI
  [ ] Memory injection into chat system prompt

PHASE 6 — GOVERNOR INTELLIGENCE
  [ ] Auto-unload logic with configurable thresholds
  [ ] Adaptive embedding (orchestrate model swap for RAG on low RAM)
  [ ] Governor alerts and notifications

PHASE 7 — POLISH AND OPTIMIZATION
  [ ] Shortcut manager fully wired
  [ ] Settings panel complete
  [ ] Performance profiling (Heimdall must stay under 80MB idle)
  [ ] Error handling pass (every Rust unwrap replaced)
  [ ] DEVLOG complete
  [ ] All module docs complete

PHASE 8 — v1.0 RELEASE
  [ ] AppImage build for Linux
  [ ] README.md (installation, usage, hardware requirements)
  [ ] GitHub release with tags
```

---

## TECHNOLOGY STACK

```
Frontend:    Svelte 5 (runes mode) + TypeScript + SvelteKit
Desktop:     Tauri 2.x (Rust)
Database:    SQLite via sqlx (async, runtime queries via query_as::<_, T>)
Vectors:     usearch (Rust crate, embedded, memory-mapped) — Phase 4
HTTP:        reqwest (async HTTP client for Ollama API)
Sysinfo:     sysinfo (Rust crate for cross-platform system metrics)
PDF:         TBD at Phase 4 entry — pdf-extract is unmaintained,
             evaluate `pdf` or `lopdf` instead
Config:      toml (Rust crate, serde-deserialised)
Logging:     tracing + tracing-subscriber + tracing-appender
             (stdout + daily-rolling file at ~/.heimdall/logs/heimdall.log)
Icons:       Native Svelte 5 (src/lib/components/icons/), path data
             arrays. Tabler-derived geometry. Zero dependencies.
Fonts:       JetBrains Mono + Cinzel, bundled under SIL Open Font
             License in static/fonts/. No CDN call on launch.
```

**Why these and not others:**
- `usearch` over ChromaDB: no server, no Python, embedded in process, works on 4GB RAM
- `sqlx` runtime queries over the `query_as!` macro: no DATABASE_URL requirement, simpler CI/CD
- `sysinfo` over raw /proc parsing: cross-platform Rust crate, well-maintained
- `reqwest` for Ollama: async, streaming support via bytes stream
- Native icon system over `@tabler/icons-svelte`: latter ships Svelte 4
  components that crash in Svelte 5 runes mode. See `docs/ERRORS.md`.

---

## PERFORMANCE TARGETS

These are design targets, not measured guarantees. The alpha hasn't
been profiled end-to-end yet; numbers below are stated as goals to
hit by v1.0. Where current behaviour is known to break a target, it's
flagged.

```
App startup time:                 < 1.5 seconds to first render (target, unmeasured)
Idle RAM, Rust process only:      < 40 MB
Idle RAM, total (incl. WebKit):   < 130 MB on Linux (WebKitGTK floor is 50–80 MB)
Heimdall IPC overhead per token:  < 5 ms
First-token latency:              dominated by Ollama; Heimdall adds ~5 ms
RAG retrieval time:               < 500ms for top-5 results (Tier 1) — Phase 4 target
Ingestion speed:                  > 50 pages/minute (PDF, Tier 1) — Phase 4 target
Image preprocessing:              < 300ms for inputs ≤ 5 MB (current, WebKit canvas).
                                  Phase 4's input_processor moves resize to Rust.
Audio transcription:              real-time factor < 1.0x — Phase 5+ target
```

---

## WHAT MAKES HEIMDALL DIFFERENT

| Feature | Open WebUI | Hollama | Enchanted | **Heimdall (alpha)** | **Heimdall (v1.0 target)** |
|---|---|---|---|---|---|
| Linux-first | No (web) | Partial | No (macOS) | **Yes** | **Yes** |
| 4 GB RAM target | No | No | No | **Yes** | **Yes** |
| Embedded RAG (no server) | No | No | No | not yet (Phase 4) | **Yes** |
| Adaptive hardware tiers | No | No | No | **Yes** | **Yes** |
| Multimodal input | Partial | No | Partial | text + vision | text + vision + audio |
| Memory system | No | No | No | not yet (Phase 5) | **Yes** |
| Resource governor | No | No | No | not yet (Phase 6) | **Yes** |
| Under 130 MB total idle (Linux) | No | No | No | target, unmeasured | **Yes** |
| Zero network calls on launch | No | No | No | **Yes** | **Yes** |

The right column is the v1.0 promise. The middle column is what
this alpha actually delivers. Heimdall does not over-promise its
current state.

---

## CLOSING NOTE

Sandeep is building this. One developer. With AI assistance. In the open.

The goal is not to compete with cloud AI. The goal is to give Linux users — especially those without expensive hardware — a tool that respects their machine, respects their privacy, and respects their intelligence.

Every line of code in Heimdall should be understandable by the person who wrote it. Every decision should have a reason. Every module should do one thing well.

That is the Linus standard. That is the Heimdall standard.

**The watchman does not sleep. The watchman sees everything. The watchman lets nothing unnecessary through.**

Build accordingly.


---

## APPENDIX A — Current Implementation Status

This appendix replaces the deleted `docs/ARCHITECTURE.md` stub. It tracks
divergences between the spec above and the code that actually exists.
Updated at every phase boundary.

### Release & Phase Milestones

| Phase | Focus / Deliverable | Release Milestone | Status |
|-------|---------------------|-------------------|--------|
| 0 | Foundation: Tauri + Svelte scaffold, dirs, configs | — | complete |
| 1 | App shell: TitleBar, Sidebar, layout, design tokens | — | complete |
| 2A | Backend: ollama_client, db, adaptive_config, Tauri commands | Backend Scaffold | complete |
| 2B | ChatPanel UI, streaming, model selector | Development Build | complete |
| 3 | Local chat polish, history, vision, thinking blocks | **Alpha Release 1** | code complete; pre-alpha audit merged |
| 4 | RAG Engine (rag_engine.rs, vector store, ingestion) | Beta 1 | complete |
| 5 | Memory (fact extraction, user-confirmed) | Beta 2 | not started |
| 6 | Governor (resource polling, auto-unload, adaptive embedding) | Beta 3 | not started |
| 7 | Shortcut Manager UI; Settings panel; polish | Release Candidate | not started |
| 8 | AppImage; final optimisation; signed bundle | v1.0 Stable | not started |

### Module Status

| Module | Target Phase | Status | File |
|--------|--------------|--------|------|
| `adaptive_config` | 2A | complete; Phase 6 will read `effective_tier` | `src-tauri/src/adaptive_config.rs` |
| `ollama_client` | 2A | complete; Phase 4 calls `embed()`; Phase 6 sends `keep_alive: "0s"` | `src-tauri/src/ollama_client.rs` |
| `db` | 2A | complete; tables exist for all phases (rag_chunks, ingestion_jobs, memory_facts) | `src-tauri/src/db.rs` |
| `input_processor` | 3 → 4 | **diverged** — Phase 3 image resize lives in `ChatPanel.svelte` (WebKit canvas). Phase 4 moves it to Rust per spec (handles ≤ 10 MB cap, audio prep, etc). | not yet at `src-tauri/src/input_processor.rs` |
| `rag_engine` | 4 | complete | `src-tauri/src/rag_engine.rs` |
| `governor` | 6 | not started | `src-tauri/src/governor.rs` |
| `shortcut_manager` | 7 | not started | `src-tauri/src/shortcut_manager.rs` |

### Phase 4–6 Prerequisites & Notes

The pre-alpha audit landed the IPC contract changes Phase 4–6 need
without filling them in. Seam comments in the source flag where each
phase picks up.

**Phase 4 Prerequisites (already in place):**
- `OllamaClient::embed(model, text) -> Vec<f32>` — implemented and
  unused. Phase 4's first chunk-embed call exercises it.
- `OllamaOptions::keep_alive: Option<String>` — Phase 4 sets `"5m"`
  during ingestion to keep the embedding model warm; sets `None`
  (default) during chat.
- `chat_stream` accepts `context: Option<ContextHint>` with
  `rag_collections: Option<Vec<String>>`. Phase 4 fills this; Rust
  retrieves chunks and prepends a system message before forwarding to
  Ollama. Frontend already passes `null`.
- `db::insert_rag_chunk`, `get_rag_chunks_by_ids`,
  `delete_rag_chunks_for_collection` exist and are tested via the
  schema migration.
- `db::create_ingestion_job`, `update_ingestion_progress`,
  `complete_ingestion_job`, `fail_ingestion_job`, `list_ingestion_jobs`
  exist.
- `~/.heimdall/vectors/` and `~/.heimdall/knowledge/` directories are
  created at startup by `ensure_dirs()`.
- `TierConfig` carries per-tier `chunk_size_tokens`,
  `chunk_overlap_tokens`, `max_vectors`, `rag_top_k`, `embedding_model`.
  All values match the spec.

**Phase 4 Crate Validation:**
- `usearch` C++ build is **unverified** on this Linux toolchain.
  Phase 4's first task is `cargo add usearch && cargo check`.
- `pdf-extract` (named in the spec) is unmaintained on crates.io. At
  Phase 4 entry, evaluate `pdf` (lower-level, more flexible) or
  `lopdf` (higher-level, less low-level control) as alternatives.

**Phase 4 Architecture Note — RAG on 4 GB without Phase 6 is fragile.**
On a Tier 1 (Minimal) machine, adding `nomic-embed-text` (~274 MB) on
top of the chat model leaves ~1.3 GB of headroom. Without Phase 6's
auto-unload, ingestion runs require the user to manually unload the
chat model first. Phase 4 release should ship with a "Tier 1: manual
model management required" warning until Phase 6 lands. The README's
Hardware section already states this honestly.

**Phase 5 Prerequisites (already in place):**
- `db::insert_memory_fact`, `confirm_memory_fact`, `delete_memory_fact`,
  `get_confirmed_memory_facts`, `list_all_memory_facts` exist.
- `chat_stream`'s `ContextHint::memory_enabled` field carries the
  injection signal.

**Phase 5 Deferrals:**
- `extract_memory_facts(conversation_id)` Tauri command not yet wired.
  Seam comment in `lib.rs` near the command list.

**Phase 6 Prerequisites (already in place):**
- `HardwareInfo` carries both `detected_tier` and `effective_tier`. The
  Governor panel will show both ("you have a 4 GB box; you've overridden
  to Standard").
- `OllamaOptions::keep_alive` allows force-unload via `"0s"`.
- `get_tier_config` Tauri command surfaces the active per-tier config.

**Phase 6 Deferrals:**
- Resource polling loop. Seam comment in `bootstrap()` after AppState
  registration.
- `model_last_used: Mutex<HashMap<String, i64>>` in AppState. Phase 6
  first commit adds it; not in alpha to avoid a hot-path mutex
  acquisition with no consumer.
- Stream cancellation (`CancellationToken` keyed by conversation_id +
  `cancel_chat_stream` command). `TODO(phase-4)` comment in
  `chat_stream` flags the seam. **Phase 4 lands this** — RAG-driven
  rambling is a real user-cancel scenario that Phase 4 must handle.

### Documented Divergences from Spec

- **Performance numbers** in the section above are reframed as targets,
  not guarantees. The spec's original "<80 MB idle" was unrealistic on
  Linux because WebKitGTK alone is 50–80 MB; the Rust process target
  is now stated separately at < 40 MB.
- **Tabler icons** were reversed at Phase 3 alpha pre-release after a
  Svelte 5 runes-mode crash. Native icon system used instead. See
  `docs/ERRORS.md` and `docs/DECISIONS.md`.
- **Google Fonts CDN loading** was reversed at the pre-alpha audit.
  Both fonts now ship bundled under OFL in `static/fonts/`. Spec
  text above reflects the bundled-fonts decision.
- **Audio input** deferred from Phase 3 to "Phase 5+". Ollama doesn't
  support audio transcription natively; will revisit with whisper.cpp
  sidecar after the core Phase 4–6 modules ship.
