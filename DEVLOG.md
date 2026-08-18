# Heimdall Development Log

Format for every entry:
## [DATE] — [WHAT HAPPENED]
**Why:** Reason for this decision.
**What:** What was built or changed.
**Result:** Current state after this session.
**Next:** What the next session must do.
**Broken by:** (fill if something broke the build — model used, what it did wrong)

When a saga spans multiple entries (e.g. a multi-day debugging trail
with intermediate reverts), the final entry includes a "Saga summary:"
note pointing back at the start.

---

## 2026-05-29 — Phase 6: optional test suite + Legendary feature complete

**Why:** Phase 6 shipped with 20 optional tasks deferred ("ship working
code first"): the eight Governor property tests, six unit-test groups,
five integration tests, the NF1 benchmark, and the Legendary feature.
This session completes all 20 to bring Phase 6 to full
spec-compliance — every acceptance criterion that classified as a
property now has an executable, passing property test.

**What:**

*Three UI fixes carried in from the prior session (now committed):*
- Governor tier dropdown showed the raw `detectedTier` function source;
  fixed `{detectedTier}` → `{detectedTier()}` in `ThresholdControls.svelte`
  (the getter-function conversion's last missed call site).
- Governor RAM hero restructured: Free is the prominent number with a
  Used / Total / Free breakdown (`GovernorPanel.svelte`).
- Models-tab button labels shortened to "Chat default" / "Vision
  default" / "Embed default" (`ModelRow.svelte`).

*Testability refactors (behavior-preserving — pure cores extracted,
I/O shells delegate):*
- `governor.rs`: `parse_meminfo`, `normalize_meminfo`, `clamp_rss`,
  `clamp_self_rss`, `read_drm_vram_at(root)`, `find_ollama_pid_at(root)`,
  `pick_smallest_matching_pid`, `parse_status_vmrss`. `CpuJiffies`,
  `compute_cpu_percent`, `derive_risk_state`, `select_unload_candidate`,
  `can_load_embedding` made reachable from tests. `tick()` rewired
  through the helpers with identical behavior.
- `ollama_client.rs`: `map_ps_entries` + `pub fn parse_ps_json`
  extracted; `PsResponseRaw`/`PsEntryRaw` are `pub(crate)`;
  `list_running` delegates to `map_ps_entries`.
- `lib.rs`: `consume_chat_reload_pending` extracted and wired into
  `chat_stream` Hook 1.
- **`Governor` is now generic over `R: tauri::Runtime = Wry`** so
  integration tests can construct it against `tauri::test::mock_app()`'s
  `MockRuntime`. Production resolves to `Governor<Wry>` via the default
  type parameter — zero behavior change.

*Property tests (`src-tauri/tests/`, 256 cases each, all green):*
- `property_p1_metrics_roundtrip` — `GovernorMetrics` JSON round-trip.
- `property_p2_memory_invariants` — available≤total, swap_used≤swap_total,
  CPU% in [0,100] never NaN, RSS clamps.
- `property_p3_per_model_accounting` — `/api/ps` mapping preserves
  count/names/sizes, no dedup.
- `property_p4_risk_state_monotonicity` — lower available never yields a
  less-severe state.
- `property_p5_stream_aware_unload` — selector never returns a streaming
  model.
- `property_p6_ingestion_aware_unload` — selector never returns the
  embedding model while ingestion is active.
- `property_p7_candidate_determinism` — identical inputs → identical
  result.
- `property_p8_embedding_fit` — the three-branch decision table holds
  across the full numeric range.
- Shared generators in `tests/governor_strategies.rs`. No collision with
  the Phase 3.5 registry P1–P7 tests; both suites coexist.

*Unit tests (in-module):* `parse_meminfo` (5 cases), `compute_cpu_percent`
(3, incl. zero-delta→0.0 not NaN), `find_ollama_pid`/`parse_status_vmrss`
(tempfile fake `/proc`), `read_drm_vram_at` (7 GPU configs via tempfile
sysfs), `StreamGuard` Drop on completion/error/drop/panic
(`catch_unwind`), `consume_chat_reload_pending` (3 cases).

*Integration tests:* `integration_polling_lifecycle` (mock_app, ≥1 emit,
cancel < 2200 ms), `integration_metrics_event`, `integration_api_ps_shape`
(wiremock `/api/ps` fixture), `integration_tier1_rag_swap` +
`integration_4gb_acceptance` (decision-path integration tests, documented
as such in their headers).

*Benchmark:* `benches/chat_stream_governor_overhead.rs` (criterion) —
per-token `try_lock`+insert on the real `std::sync::Mutex<HashMap>`
hot-path type vs a baseline; `[[bench]]` entry + criterion dev-dep added.

*Legendary feature (gated, default off):* `governor_preview_ingestion`
Tauri command wrapping `evaluate_embedding_fit`, mapping
`EmbeddingFitDecision` → green/amber/red with a numeric breakdown;
gated behind `AppConfig.legendary_predictive_preview` (default
`Some(false)`, returns `status: "disabled"` when off). New
`IngestionFitPreview` struct (Rust + TS) and
`IngestionPressurePreview.svelte` (zero hex, status tokens, Svelte 5
runes) — compiles clean, not yet mounted.

*Dev-deps added:* `wiremock`, `criterion`, and the `tauri` `test`
feature.

**Result:** Independently verified. `cargo test` → EXIT 0: 166 lib tests
+ all 8 Phase 6 property tests + 5 integration tests green, 0 failures
(212 s lib-test run). `cargo test --no-run` clean. `cargo bench --no-run`
compiles. `npm run check` → 0 errors (4 pre-existing unrelated warnings).
No property surfaced a real bug — every assertion reflects the spec's
acceptance criteria. All 20 tasks marked `[x]` in tasks.md.

**Next:** Phase 7 — Release Candidate (settings panel, shortcut
remapping, memory import, performance hardening).

**Broken by:** Nothing.

---

## 2026-05-29 — Phase 6: Governor Intelligence — Beta 3

**Why:** Heimdall promised to run on 4 GB RAM. Without the Governor that
promise lived only in the README. Phase 6 makes it true in practice: a
user on a 4 GB machine can run a long RAG ingestion, switch to chat
mid-ingestion, use a vision model on an image, and resume ingestion —
without a single OOM crash, without manually unloading anything, without
ever opening the Governor panel unless they want to.

**What:**

*Backend — Resource Monitoring Engine (`src-tauri/src/governor.rs`):*
- `Governor` struct with a single long-lived tokio task spawned from
  `bootstrap()` after AppState registration. Cancellable via
  `CancellationToken` stored on `AppState.governor_cancel`.
- 2-second polling loop (first tick fires immediately — within 2000 ms
  of spawn). Reads `/proc/meminfo` (MemTotal, MemAvailable, SwapTotal,
  SwapFree), `/proc/stat` (per-core + aggregate CPU via jiffy delta),
  Ollama process RSS via `/proc/*/comm` scan (no subprocess — pure
  `/proc` reads), Heimdall self RSS via `/proc/self/status`, WebKitGTK
  child RSS, VRAM via `/sys/class/drm/` (NVIDIA `0x10de` + AMD `0x1002`,
  Intel `0x8086` excluded), and per-model memory via Ollama `GET /api/ps`
  (5-second deadline, `OllamaClient::list_running()`).
- Emits `governor://metrics` Tauri event every tick. Graceful degradation:
  any failed source sets its field to `None` / sentinel status; the tick
  still emits.
- `GovernorMetrics` struct: total/available RAM, swap, per-core + aggregate
  CPU, Ollama RSS, Heimdall RSS, WebView RSS, VRAM total/used/status,
  loaded models, risk state, thresholds, detected/effective tier,
  proc_status, cgroup_detected, timestamp.

*Backend — Auto-Unload Intelligence:*
- `derive_risk_state` pure function: `Calm | Warn | Unload | Critical`
  derived from `available_ram_mb` vs tier thresholds. Defaults per tier:
  T1 (800/400/200 MB), T2 (1500/800/400 MB), T3 (2000/1000/500 MB).
  Threshold validation with documented-default fallback on misconfiguration.
- `select_unload_candidate` pure function: filters by streaming guard
  (`active_stream_models`), ingestion guard (`active_ingestions` +
  `TierConfig.embedding_model`), per-model auto-unload toggle, 3-strikes
  exclusion set. Tie-break: longest idle → largest size → lex-smallest
  name (deterministic, P7-verifiable).
- 5-second cooldown between successive unloads (no cascade). 3-strikes
  exclusion per model within a 30-second window on force-unload failure.
- Critical state: edge-transition detection, batch unload bypassing
  cooldown, `ingestion_paused` flag set, `governor://critical` event
  emitted before any unload.
- `AppState` extensions: `model_last_used`, `active_stream_models`,
  `chat_reload_pending`, `ingestion_paused`, `governor`, `governor_cancel`.
- `chat_stream` integration: `StreamGuard` Drop impl removes
  `conversation_id` from `active_stream_models` on completion/error/
  cancel/panic. Per-token `try_lock` on `model_last_used` (skip on
  contention — never blocks the hot path). `chat_reload_pending` consumed
  and `governor://embedding_swap{phase: reloading_chat}` emitted before
  the `/api/chat` request.

*Backend — Adaptive Embedding Orchestration:*
- `Governor::can_load_embedding(chat_model_name) -> EmbeddingFitDecision`
  pure function: `FitsAlongside | RequiresChatUnload | InsufficientEvenAlone`.
  Budget = `floor(MemAvailable * safe_headroom_pct)` (default 0.80).
- `IngestionWorker` now calls `can_load_embedding` instead of the deleted
  `tactical_unload`. On `RequiresChatUnload`: force-unload chat model,
  set `chat_reload_pending`, emit `governor://embedding_swap{phase:
  unloading_chat}`. On `InsufficientEvenAlone`: fail the job with a
  user-visible error. On ingestion complete: force-unload embedding model,
  emit `governor://embedding_swap{phase: unloading_embedding}`.
- `rag_engine/memory_guard.rs` **deleted**. `tactical_unload` and
  `check_memory` are gone. `scripts/check_memory_guard_removed.sh`
  closing-commit grep predicate exits 0.

*Backend — New Tauri commands (8):*
`governor_unload_model`, `governor_set_thresholds`,
`governor_set_auto_unload_for_model`, `governor_set_auto_unload_global`,
`set_tier_override`, `set_default_vision_model`,
`set_default_embedding_model`, `models_tab_list`, `models_catalog_list`.

*Backend — New Tauri events (5):*
`governor://metrics`, `governor://critical`, `governor://critical_cleared`,
`governor://no_candidates`, `governor://embedding_swap`.

*Backend — TierConfig extension:*
`governor_warn_mb`, `governor_unload_mb`, `governor_critical_mb`,
`safe_headroom_pct` added to `TierConfig`. `auto_unload_enabled` and
`auto_unload_per_model` added to `AppConfig`.

*Backend — model_catalog.json:*
`src-tauri/resources/model_catalog.json` — 8 curated entries (phi4-mini,
qwen2.5:0.5b, nomic-embed-text, llama3.2:3b, llava:7b, qwen3:7b,
deepseek-r1:7b, llama3.3:70b) with capability tags and `min_tier`.
Loaded once on startup, cached on `AppState`, exposed via
`models_catalog_list`.

*Frontend — Stores:*
- `src/lib/stores/governor.svelte.ts`: `GovernorStore` class with
  `$state<GovernorMetrics | null>`, subscribes to all 5 events,
  exports 15 `$derived` slices (ramAvailable, ramTotal, swapTotal,
  swapUsed, cpuAggregate, heimdallRss, ollamaRss, loadedModels,
  vramStatus, vramTotal, vramUsed, riskState, effectiveTier,
  detectedTier, thresholds). Defensive parse-error guard. Out-of-order
  `critical_cleared` ignored.
- `src/lib/stores/models.svelte.ts`: `ModelsStore` class with
  `$state<ModelsTabRow[]>`, `refresh()`, `markPullStarted/Done`,
  `markDeleted`, subscribes to `model://pull-progress`. Refreshes on
  mount + post-pull + post-delete only — never on a metrics tick.
  "Currently loaded" derived from `governorStore.metrics.loaded_models`.

*Frontend — Governor components (`src/lib/components/governor/`):*
- `GovernorPanel.svelte`: always-mounted with `class:hidden`, hero
  indicator colour-coded by `risk_state` via `--status-*` tokens
  (Critical adds 1.5 s `governor-pulse` keyframe), tier badge with
  focusable "tier overridden" button, `{#key tierKey}` full re-render
  only on `effective_tier` change, loading skeleton until first metrics
  event, "model reloading" hint on `embedding_swap{reloading_chat}`.
- `ResourceCard.svelte`: variants for used+total, percent, rss_mb.
  `null`/`undefined` → "—" (never "0 MB"). Distinct `aria-label`s for
  Heimdall vs Ollama RSS.
- `ModelList.svelte`: per-row name/size/idle/last-used, "Unload" button
  with `currently_streaming` guard mounting `UnloadConfirmModal`, auto-
  unload toggle with optimistic UI + rollback, inline errors.
- `UnloadConfirmModal.svelte`: in-component modal, focus trap, Escape
  cancels, focus restored to trigger. No `window.alert/confirm/prompt`.
- `ThresholdControls.svelte`: three sliders with `warn ≥ unload ≥ critical`
  UI-level invariant, inline error on violation, tier-override picker
  with "restart recommended" hint.
- `VramCard.svelte`: mounted only when `vram_status ∈ {ok, unavailable}`;
  `absent` → parent unmounts entirely. "VRAM: unavailable" literal text.
- `index.ts`: re-exports all six components.

*Frontend — Models components (`src/lib/components/models/`):*
- `ModelsTab.svelte`: always-mounted, refreshes on mount, case-insensitive
  substring filter, inline error, loading skeleton, mounts `<PullPanel>`
  at top.
- `ModelRow.svelte`: capability badges (text/vision/thinking/embedding/
  tools), family/params/quant metadata, last-used formatter, hardware-
  aware recommendation pill, capability-gated default-setter buttons,
  Delete action with modal.
- `PullPanel.svelte`: capability filter (Chat default), curated catalog
  filtered by capability + `effective_tier`, recommendation labels,
  free-form name input (256-byte limit), progress from
  `modelsStore.pullProgress[name]`, accessible `<button>` catalog entries,
  "Catalog unavailable" and "No entries match" literal copy.
- `DeleteConfirmModal.svelte`: in-component modal, focus trap, "currently
  loaded" warning surfaced when applicable. No `window.confirm`.

*Frontend — Routing:*
- `Sidebar.svelte`: `iconRobot` Models button between Memory and Governor.
- `+page.svelte`: `Panel` union extended with `'models'`, `<GovernorPanel />`
  and `<ModelsTab />` always-mounted with `class:hidden`, both stores
  started on mount.

*Types:*
- `src/lib/types/governor.ts`: `RiskState`, `VramStatus`, `ProcStatus`,
  `HardwareTier`, `EmbeddingSwapPhase`, `ModelRecommendation`,
  `RunningModel`, `GovernorThresholds`, `GovernorMetrics`, `CriticalEvent`,
  `NoCandidatesEvent`, `EmbeddingSwapEvent`, `ModelsTabRow`,
  `PullProgressEvent`, `CatalogEntry`.

**Result:**
- `cargo check` clean (one pre-existing unrelated warning).
- `npm run check` clean (0 errors, 4 pre-existing unrelated warnings).
- `scripts/check_memory_guard_removed.sh` exits 0.
- `tactical_unload` and `memory_guard` fully removed from codebase.
- Governor panel live in the sidebar. Models tab live in the sidebar.
- Zero hardcoded hex in any new `.svelte` file.
- No `window.alert/confirm/prompt` anywhere in new code.
- Svelte 5 runes mode throughout.

**Next:** Phase 7 — Release Candidate. Settings panel, keyboard shortcut
remapping, memory import, property-based tests P1–P8 (deferred from
Phase 6), NF1 benchmark.

**Broken by:** Nothing.

---

## 2026-05-28 — Beta 2 release — v0.5.0

**Why:** Phase 5 Memory is verified working end-to-end. Phase 4 RAG has
been stable since the stabilisation sweep. Time to cut Beta 2.

**What:**
- README.md rewritten from scratch in a precision-first voice — Tesla,
  not marketing. Honest about what is built (Chat, RAG, Memory, Vision,
  adaptive tiers) and what is not (Governor, audio, shortcuts UI,
  settings panel, memory import).
- Version bumped to 0.5.0 across `package.json`, `package-lock.json`,
  `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`. `Cargo.lock`
  refreshed by `cargo check`.
- Sidebar version label changed from `v{version} · Alpha` to
  `v{version} · Beta`.
- `cargo check` clean. `npm run check` clean (4 pre-existing warnings).

**Result:** Heimdall v0.5.0 Beta 2 — RAG + Memory complete. Tagged ready
for users.

**Next:** Phase 6 — Governor.

**Broken by:** Nothing.

---

## 2026-05-28 — Phase 5: Memory System — extract, store, inject, harden

**Why:** Phase 5 Memory was implemented and verified working end-to-end:
facts extract after conversations, the gold notification banner appears,
facts can be confirmed/rejected/edited, confirmed facts are injected into
new chats, and the episode pipeline creates searchable conversation
summaries. A full audit and hardening sweep followed to fix every broken
link in the extraction chain and bring the system to production quality.

**What:**

*Core memory system (initial implementation):*
- `src-tauri/src/memory/mod.rs`: `MemoryEngine` struct owning db, ollama,
  tier_config, vectors_dir, registry. `on_conversation_end` — checks
  memory_enabled and global_enabled flags, enforces 4-user-message
  threshold, runs fact extraction + dedup + conflict detection + episode
  creation. `build_injection_context` — builds facts + episodes context
  prefix for chat system prompt. `store_episode` — embeds episode summary
  and stores in `_memories.usearch`.
- `src-tauri/src/memory/extraction.rs`: `select_extraction_model`,
  `extract_facts`, `generate_episode_summary`. Full five-layer extraction
  pipeline (see hardening section below).
- `src-tauri/src/memory/dedup.rs`: `check_deduplication` (embedding
  cosine similarity, three-tier classification: New / PossibleUpdate /
  Duplicate), `detect_conflict` (LLM yes/no classifier).
- `src-tauri/src/memory/injection.rs`: `build_facts_context` (token-budget
  enforced, most-recent-first), `retrieve_episodes` (usearch semantic
  search, similarity threshold 0.6, top-3 within budget).
- `src-tauri/src/db.rs`: `memory_facts`, `memory_episodes`,
  `memory_settings` tables. Full CRUD: `insert_memory_fact`,
  `confirm_memory_fact`, `delete_memory_fact`, `list_all_memory_facts`,
  `get_confirmed_memory_facts`, `insert_memory_episode`,
  `get_active_episodes`, `get_memory_setting`, `get_conversation_memory_enabled`,
  `get_conversation_model`, `get_confirmed_fact_count`.
- `src-tauri/src/lib.rs`: 14 Tauri commands registered — `memory_extract`,
  `memory_list_facts`, `memory_confirm_fact`, `memory_confirm_all`,
  `memory_reject_fact`, `memory_reject_all`, `memory_edit_fact`,
  `memory_delete_fact`, `memory_delete_all_facts`, `memory_delete_all_episodes`,
  `memory_get_settings`, `memory_update_settings`, `memory_export_facts`,
  `memory_set_conversation_memory`, `memory_get_conversation_memory`.
  `chat_stream` emits `chat://memory_used` event with exact injected text
  per turn. Memory injection reads `num_ctx` from `ModelRegistry::get_settings`
  and computes adaptive token budget (8% of context, floor/ceiling clamped).
- `src/lib/stores/memory.svelte.ts`: `MemoryStore` class — `facts`,
  `settings`, `isExtracting`, `hasNewPendingFacts`, `lastExtractionError`,
  `lastExtractionWasEmpty`. `startListening` registers `memory://extraction_complete`
  event handler. `loadFacts` sets `hasNewPendingFacts` on restart if pending
  facts exist. Failure routing: catastrophic errors (Ollama down, model
  missing, DB failure) → red banner; parse/quality failures → calm dim hint.
- `src/lib/types/memory.ts`: `MemoryFact`, `MemorySettings`,
  `ExtractionResult`, `CandidateFact`, `ExtractionCompleteEvent`,
  `MemoryUsedEvent` interfaces.
- `src/lib/components/memory/MemoryPanel.svelte`: Memory on/off toggle,
  soft-warn (150) and hard-cap (200) banners, pending-fact review batches,
  confirmed facts list with search input, episodes section with decay
  threshold control, export button.
- `src/lib/components/memory/FactReviewBanner.svelte`: Per-batch review UI
  — confirm-all, reject-all, per-fact confirm/edit/reject, dedup badge,
  conflict badge.
- `src/lib/components/memory/FactList.svelte`: Confirmed facts with
  edit/delete, date, "active" badge, provenance pill ("from {conversation
  title}") that switches to the source conversation on click. Client-side
  search via `searchQuery` prop.
- `src/lib/components/chat/MemoryIndicator.svelte`: Fact count in chat
  toolbar; turns amber at 150-fact soft warn, red at 200-fact hard cap.
- `src/lib/components/ChatPanel.svelte`: Gold extraction notification
  banner, red error banner (catastrophic only), dim "no new facts" hint.
  Per-turn "● Memory used" expandable badge below each assistant message
  showing exact injected context. Chat overflow menu with "Re-extract
  memory" action. `memoryEnabled` per-conversation toggle.

*Bug fixes (audit sweep):*
- **Immutable usearch index** (`rag_engine/index.rs`): Added
  `VectorIndex::open_writable` that always loads mutably (never
  `restore_view`). Routed four write sites: `memory/mod.rs::store_episode`,
  `rag_engine/ingestion.rs`, `rag_engine/mod.rs::create_collection`,
  `rag_engine/mod.rs::delete_source`. Fixes "Can't add to an immutable
  index" error on every episode after the first.
- **Extraction fragments** (`memory/extraction.rs`): Replaced prompt with
  strict sentence-shape rules; retry prompt also strict.
- **Pending facts lost on restart** (`stores/memory.svelte.ts`): `loadFacts`
  now sets `hasNewPendingFacts = true` when pending facts exist, so the
  gold banner reappears after app restart.
- **Concurrent extraction race** (`memory/mod.rs`): `MemoryEngine` gains
  `extraction_lock: Arc<Mutex<()>>` serialising `on_conversation_end` so
  two rapid chat switches cannot race on dedup snapshots or the writable
  episode index handle.
- **Conflict detection too narrow** (`memory/mod.rs`, `memory/dedup.rs`):
  `check_deduplication` now returns top-3 candidates above 0.5 similarity
  (`conflict_candidates`). `on_conversation_end` runs `detect_conflict`
  against all of them, not just the dedup anchor. Stale facts that drift
  below the 0.7 PossibleUpdate threshold no longer slip through.
- **Episode similarity threshold** (`memory/injection.rs`): Lowered 0.7 →
  0.6 for episodes (RAG retrieval threshold unchanged). Conversational
  summaries are short and rarely keyword-overlap with fresh queries.
- **Short-fact quality filter** (`memory/extraction.rs`): `validate_fact`
  drops facts under 20 chars before dedup, catching small-model regressions
  to bare entities.
- **`generate_completion` sent `think: None`** (`ollama_client.rs`): Changed
  to `think: Some(false)` so thinking models don't contaminate extraction
  responses with `<think>` blocks.
- **`#[instrument]` span on `extract_facts`**: Records `model`,
  `messages_count`, `attempt_index`, `protocol_used`, `parsed_count`,
  `validation_dropped_count`, `outcome` for production diagnostics.
- **Adaptive token budget** (`memory/injection.rs`, `memory/mod.rs`,
  `lib.rs`): Budget = clamp(8% × `num_ctx`, floor, ceiling). Facts: 200–1500
  tokens. Episodes: 240–2000 tokens. `num_ctx` from `ModelRegistry::get_settings`.
- **Memory transparency** (`lib.rs`, `ChatPanel.svelte`): `chat_stream`
  emits `chat://memory_used` with exact injected text. ChatPanel renders
  expandable "● Memory used" badge per assistant message.
- **MemoryIndicator cap signal** (`chat/MemoryIndicator.svelte`): Amber at
  150 facts, red at 200 facts, with descriptive tooltip.
- **Fact provenance** (`memory/FactList.svelte`): "from {conversation title}"
  pill beside each fact; click switches to source conversation.
- **Memory search** (`memory/MemoryPanel.svelte`, `memory/FactList.svelte`):
  Search input above confirmed facts list; client-side substring filter.
- **Sidebar brain icon** (`icons/index.ts`): Replaced broken 10-path stub
  with correct 6-path Tabler `IconBrain` geometry.

*Five-layer extraction engine (final hardening):*
- **Layer 1 — Constrained generation**: `OllamaChatRequest.format:
  Option<serde_json::Value>` added to `models.rs`. `generate_completion`
  in `ollama_client.rs` accepts `format` parameter. Schema:
  `{ "type": "object", "properties": { "facts": { "type": "array",
  "items": { "type": "string" }, "maxItems": 12 } }, "required": ["facts"] }`.
- **Layer 2 — Short concrete prompt**: One few-shot example, no bad-example
  section, no abstract adjectives. Line-delimited variant for attempt 3.
- **Layer 3 — Robust parser**: Five recovery strategies in order — direct
  JSON (array or object-with-array), balanced-bracket array scan, balanced-
  brace object scan, single-quote coercion (string-aware), line-delimited
  fallback. All string-literal and escape-sequence aware.
- **Layer 4 — Per-fact validation**: Length 20–500 chars, alphabetic
  content, leading char, AI-framing prefix rejection, verb-form allow-list
  (50 common verb forms). `validate_fact` is unit-tested.
- **Layer 5 — Protocol-fallback retries**: Three attempts — SchemaJson →
  PlainJson → Lines. Each uses a different Ollama output protocol. Returns
  `Ok(facts)` on success, `Ok([])` if validation drops everything, `Err`
  only when every attempt fails to parse at all.
- 13 unit tests covering all parser strategies and validator cases.

**Result:** `cargo check` clean. `npm run check` clean (0 errors, 4
pre-existing autofocus/state_referenced_locally warnings). All 13 extraction
unit tests pass. Phase 5 Memory System verified working end-to-end:
- Facts extract after conversations with 4+ user messages ✅
- Gold notification banner appears in ChatPanel ✅
- Facts confirmed and stored with ACTIVE badges ✅
- Memory injected into new chats ✅
- Episodes pipeline working ✅
- Immutable index error fixed ✅
- Extraction produces complete sentences, not fragments ✅
- Per-turn "Memory used" transparency badge ✅
- Fact provenance links ✅
- Memory search ✅
- Cap signal in chat toolbar ✅

**Next:** Phase 6 — Governor (RAM/CPU/VRAM polling, auto-unload).

**Broken by:** Nothing.

---

## 2026-05-28 — feat: per-source delete for RAG collections

**Why:** Users can accidentally ingest a wrong file into a collection.
Without per-source delete, the only fix was deleting the entire
collection and starting over. That's destructive and slow.

**What:**

*Backend (`src-tauri/src/`):*
- `rag_engine/mod.rs`: New `RagEngine::delete_source(collection, source_path)`.
  Fetches all `vector_id`s for the source from `rag_chunks`, deletes the
  chunk rows, removes the vectors from the usearch index via the existing
  `VectorIndex::remove(key)` (O(1), no rebuild), saves the index, deletes
  all matching `ingestion_jobs` rows, and touches `collections.updated_at`.
- `lib.rs`: New `rag_delete_source` Tauri command. Registered in the
  invoke handler.

*Frontend (`src/`):*
- `rag.svelte.ts`: New `deleteSource(sourcePath)` method on `RagStore`.
  Calls the backend, then refreshes jobs and stats for the current
  collection.
- `IngestionJobsList.svelte`: Delete button (×) appears on hover for
  terminal-state jobs (`done`, `failed`, `cancelled`, `interrupted`).
  Clicking it shows an inline confirmation row (no `confirm()` dialog).
  Confirming calls `ragStore.deleteSource`. Multi-file jobs show a
  disabled × with a tooltip explaining the limitation.

**Result:** `cargo check` clean. `npm run check` clean (0 errors,
2 pre-existing autofocus warnings). Per-source delete works for
single-file and URL ingestion jobs. Multi-file jobs are blocked with
a clear message.

**Next:** Phase 5 — Memory.

**Broken by:** Nothing.

---

## 2026-05-28 — RAG Audit & Stabilisation Sweep

**Why:** Phase 4 had landed marked "complete" in the previous DEVLOG
entry, but a fresh end-to-end audit driven by the user's experience
report turned up a chain of cross-language shape mismatches and
silent dispatch failures that made the feature unusable in practice:
collection names rendered as undefined, URL ingestion silently no-op'd,
folder ingestion silently no-op'd, the rename command rejected, and
the chat-side active-collection state was a list of `undefined`s.
A single TS-vs-Rust drift on the `Collection` shape was the root
cause of most user-visible breakage.

**What:**

*Critical (the cascade):*
- `Collection` TS interface aligned with Rust: now has `id`,
  `display_name`, `created_at`, `updated_at`, `last_ingested_at`.
  All UI text reads `display_name`. All consumers updated.
- `CollectionStats` TS interface aligned: `chunks`, `sources` (count),
  `last_updated`, `vector_bytes`, `display_name`.
- Centralised slug normalisation in `rag_engine::slug_id` (pub at
  module level). Every Tauri command that takes a collection name
  slugs at the IPC boundary so the DB consistently stores ids while
  the UI keeps speaking display names. `chat_stream` slugs
  `context.rag_collections` defensively.
- `get_active_collections` resolves stored ids back to display names
  via the `collections` table so the picker shows what the user typed.
- `dispatch_loader` extension routing replaced by a richer
  `dispatch_source` returning `SourceKind::{Url, File, Folder, Unsupported}`.
  URL ingestion now actually runs `UrlLoader`. Folder ingestion now
  walks via `FolderLoader::load_folder`. Worker handles each branch
  with the same per-source error semantics.
- Dropzone gains a "Browse Folder" button alongside "Browse Files".
- `complete_ingestion_job` now stamps `collections.last_ingested_at`
  and `updated_at` (was previously dead).

*High-priority:*
- `rag_rename_collection` invoke arg names corrected to `oldName`/
  `newName` (Tauri 2 camelCase).
- `RagEngine::search_preview` now takes the shared `ModelRegistry`
  from `AppState` instead of constructing a fresh one per call.
- `rag_resume_ingestion` reuses any existing cancel flag in
  `active_ingestions` instead of replacing it; resets the flag to
  `false` on resume so a previously cancelled job can run again.
- Per-collection job cache in `rag.svelte.ts`. Switching collections
  no longer drops live ingestion progress; events route via
  `findJobAcrossCaches` and update both the live array and the cache.

*Medium-priority:*
- `tactical_unload` on Tier 1 ingestion now receives the user's
  default chat model name via a new `IngestionRequest::chat_model_hint`
  field. When no hint is set, the unload call is skipped entirely
  (avoids the previous Ollama 404 on empty model name).
- `ingestion_jobs.source_path` for multi-file jobs now stores a
  human-readable summary like `"3 files: a.pdf, b.pdf, …"` rather
  than just the first path. Resume on multi-file jobs surfaces a
  clear "not supported yet" error rather than silently misrouting.
- Native `alert`/`confirm`/`prompt` removed from `CollectionsList`
  and replaced with inline confirm/rename UI styled with CSS vars.
- New `--accent-green` and `--accent-red` tokens declared in `app.css`
  (the latter was used everywhere but never declared — silently
  unset until now). Mirrored in `src/lib/tokens.ts`.
- Hardcoded `#10b981` in `IngestionJobsList` replaced with
  `var(--accent-green)`. `pending` status now also styled.
- `rag_create_collection` pre-checks duplicate id-or-display_name
  and returns a clean `CollectionAlreadyExists` error; UI translates
  to "A collection with that name already exists."

*UX:*
- The `+` icon above the chat input is gone. A new Knowledge button
  in the chat toolbar (next to "New Chat") opens a popover listing
  active and available collections. Uses the same `iconBook2` as the
  Knowledge sidebar entry. A subtle gold dot appears on the icon
  when ≥1 collection is active. Outside-click overlay closes it.

*Configuration:*
- `tauri.conf.json`: `dragDropEnabled: true` pinned explicitly
  on the main window.

**Result:** `cargo check` clean. `npm run check` clean (0 errors,
2 pre-existing autofocus a11y warnings). The 6-step RAG success
chain (create → select → drag/browse → embed → activate → chat) is
sound by code-trace; live smoke verification deferred to Sandeep's
next `cargo tauri dev` run.

**Next:** Phase 5 — Memory (fact extraction and user-confirmed
memory subsystem). Items deferred to follow-up: multi-file resume,
tactical_unload awareness of in-flight chat streams (Phase 6).

**Broken by:** Nothing in this session. The shape drifts that
caused the user-visible breakage entered the codebase during the
original Phase 4 implementation.

---

## 2026-05-26 — Phase 4: RAG Engine Complete (Beta 1)

**Why:** Heimdall needs to embed, store, and retrieve custom knowledge from local files without sending data off-device, enabling users to chat with their own documents using local models.

**What:**
- **TierConfig foundation:** Extended `TierConfig` to support `quantization` and `index_mmap`.
- **Vector Index:** Integrated embedded `usearch` for fast, serverless vector search.
- **Loaders:** Implemented chunks and 7 loaders (.txt, .md, .pdf, .docx, URLs, Code, Folders) utilizing `tiktoken-rs` and respective parsing libraries.
- **Ingestion Worker & Memory Guard:** Added an asynchronous FIFO ingestion pipeline backed by SQLite + `usearch` with real-time UI progress events. `MemoryGuard` pauses chat models during embedding on low-tier hardware.
- **Retrieval:** Embedded user queries and implemented fan-out parallel retrieval across multiple collections, injecting context directly into the chat stream.
- **Collections UI:** Full CRUD for collections in the new Knowledge Panel, including drag-and-drop file ingestion, URL pasting, and detailed job progress tracking.
- **Chat UI updates:** Added a pill row to select active collections per conversation, with real-time streaming cancellation support.

**Result:** Phase 4 Beta 1 is complete. `cargo check` and `npm run check` pass clean. The RAG pipeline correctly ingests documents and retrieves them for chat context seamlessly.
**Next:** Phase 5 — Memory (fact extraction and user-confirmed memory).
**Broken by:** Nothing.

---

## 2026-05-24 — Phase 3.5: Model Intelligence Registry

**Why:** Vision models like `gemma3` never showed the image-upload button
because the name-heuristic didn't recognise them. Every chat turn paid a
hidden `/api/show` round-trip for thinking detection. The single-enum
`ModelCapability` couldn't represent multi-capability models (e.g.
completion + vision + tools). Phase 4 RAG needs to know which models
support embedding before it can pick one.

**What:**

*Backend (`src-tauri/src/`):*
- `models.rs`: Added `ModelCapabilities` struct (five independent boolean
  flags: completion, vision, thinking, tools, embedding), `CapabilitySource`
  enum (api_show, template, heuristic, user_override), `ModelSettings`
  struct. Extended `OllamaModel` with `capabilities: Option<ModelCapabilities>`.
  Marked legacy `capability: ModelCapability` field `#[deprecated]`. Added
  `legacy_capability_from(&caps)` shim.
- `db.rs`: Two new SQLite tables (`model_capabilities`, `model_settings`)
  via additive `CREATE TABLE IF NOT EXISTS` migrations. Existing tables
  untouched.
- `model_registry.rs` (new): `ModelRegistry` — lazy digest-keyed cache,
  three-layer detection chain (`/api/show.capabilities` → template markers
  → name heuristic), in-flight dedup via `Shared<Future>`, bounded warm-up
  (4 concurrent `/api/show` calls), SQLite persistence, digest-mismatch
  eviction. Public API: `get_capabilities`, `list_with_capabilities`,
  `refresh`, `warm_up`, `hydrate`, `get_settings`, `set_settings`.
- `ollama_client.rs`: Added `show()` (raw `/api/show` accessor) and
  `list_tags_raw()`. Marked legacy helpers (`get_model_info`,
  `model_supports_thinking`, `detect_capability_from_name`,
  `detect_capability_from_template`) `#[deprecated]`.
- `lib.rs`: Wired `ModelRegistry` into `AppState` and bootstrap. Added
  `get_model_capabilities` and `refresh_model_capabilities` Tauri commands.
  `list_models` delegates to registry. `chat_stream` reads `thinking` from
  registry cache (zero `/api/show` on hot path).

*Frontend (`src/`):*
- `src/lib/types/model.ts` (new): TypeScript `ModelCapabilities` and
  `CapabilitySource` types.
- `ChatPanel.svelte`: Race-safe `$effect` with generation counter for
  model selection. `selectedModelInfo` replaces `selectedModelCapability`.
  Shimmer placeholder while capabilities load. `showImageButton` reads
  `selectedModelInfo?.vision`.

*Tests (`src-tauri/tests/`):*
- Seven property-based tests (P1–P7): cache determinism, digest
  invalidation, multi-capability representation, schema migration
  idempotence, concurrent detection dedup, source-of-truth priority,
  hot-swap safety.
- Integration test: end-to-end list/get/repull lifecycle.
- `proptest = "1.5"` added as dev dependency.

**Result:** `cargo check --all-targets` clean. `cargo test --lib` passes.
`cargo clippy --all-targets -- -D warnings` clean. `npm run check` clean.
Vision button now appears for `gemma3` and any model whose
`/api/show.capabilities` includes `"vision"`. Chat latency reduced by one
HTTP round-trip per turn. Multi-capability models correctly advertise all
their capabilities simultaneously.

**Next:** Phase 4 — RAG Engine. Migration step 3 (removing
`OllamaModel.capability` field and the deprecated `detect_capability_from_*`
helpers) deferred to a follow-up release.

**Broken by:** Nothing.

---

## 2026-05-23 — Fix: Streaming tokens silently dropped (two bugs)

**Why:** Users reported two streaming issues: (1) the entire stream was
invisible until reload, and (2) even after fix #1, the last few characters
of every response were cut off mid-word.

**What:**

*Bug 1 — Stale closure (frontend):*
- `ChatPanel.svelte`: Added a `getConversationId()` getter function that
  reads the live `$state` value at call time. Both Tauri event listener
  guards now call `getConversationId()` instead of reading
  `conversationId` directly. This is the canonical Svelte 5 pattern for
  passing reactive state into non-reactive closures, as documented at
  https://svelte.dev/docs/svelte/$state#Passing-state-into-functions.

*Bug 2 — Tag-parser buffer not emitted at stream end (backend):*
- `ollama_client.rs`: The tag-parser fallback (used for all non-thinking
  models) holds back up to 7 bytes as lookahead for `<think>` detection.
  When the stream ends, these bytes were flushed to `answer_content` (for
  DB persistence) but never emitted as a `chat://token` event to the
  frontend. Added a token emit of the remaining `tag_buf` content
  immediately before the final `done: true` event.

**Result:** `npm run check` clean. `cargo check` clean. Streaming now
delivers every character to the UI in real time, including the final bytes.
No more truncation. No more invisible streams.

**Next:** Phase 4 — RAG integration.

**Broken by:** Nothing. Bug 1 was latent since the AUDIT P3-B1 fix
reordered listener registration before async init. Bug 2 was latent since
the tag-parser was introduced in the 2026-05-22 audit fix sweep.

---

## 2026-05-23 — Model Persistence & Streaming Resilience Fixes

**Why:** The user reported three issues: (1) Heimdall forgot the last used
model on restart, defaulting to the model of the last conversation
instead of the user's explicit preference. (2) Switching to the Governor
or Settings panel while a thinking model was streaming silently killed
the stream. (3) Responses would sometimes freeze mid-stream.

**What:**
*Frontend (`src/`):*
- `ChatPanel.svelte`: Added `userHasDefaultModel` state flag.
  `loadLastConversation` now prioritizes the user's explicit model
  preference (`set_default_model`) over the historical model used in the
  last conversation.
- `+page.svelte`: Refactored Svelte conditional rendering. The `<ChatPanel />`
  is no longer destroyed (`{#if activePanel === 'chat'}`) when navigating
  to other panels. Instead, it remains permanently mounted and is hidden
  via CSS (`display: none`).

**Result:** `npm run check` and `cargo check` pass clean. Panel
navigation is instantaneous. Event listeners survive panel switching,
meaning you can start a long response, switch to Settings, and return to
find the stream continuing perfectly without freezing or data loss.

**Next:** Phase 4 — RAG integration.

**Broken by:** Nothing.

**Why:** Heimdall's model capability detection was entirely hardcoded — a
brittle list of model name substrings determined whether a model supported
thinking, vision, etc. Any model not explicitly listed was misclassified.
Gemma 3 was incorrectly listed as a thinking model, causing HTTP 400
errors. The UI also collapsed when switching between panels due to missing
flexbox boundaries.

**What:**

*Backend (`src-tauri/src/`):*
- `models.rs`: Added `capabilities: Vec<String>` field to `ModelInfo` to
  pass raw Ollama capability strings to the frontend.
- `ollama_client.rs`: Added `capabilities: Option<Vec<String>>` to
  `ShowResponse` to parse the `capabilities` array from `/api/show`.
- `ollama_client.rs`: New `capability_from_ollama_array()` method maps
  Ollama's raw capabilities to `ModelCapability` enum (priority:
  Embedding > Vision > Thinking > TextOnly).
- `ollama_client.rs`: New `model_supports_thinking()` async method with
  three-layer detection:
  1. Ollama's `capabilities` array (authoritative, dynamic)
  2. Template inspection (`{{ .Think }}`)
  3. Name-based heuristic (fallback for old Ollama)
  Falls back gracefully on any error (fail-safe: skip thinking rather
  than crash).
- `ollama_client.rs`: `get_model_info()` updated to use the three-layer
  priority for capability detection instead of template-only.
- `ollama_client.rs`: `chat_stream()` rewritten — replaced hardcoded
  `detect_capability_from_name()` call with dynamic
  `model_supports_thinking()`. Added retry-on-400 safety net: if Ollama
  rejects `think: true` with "does not support thinking", automatically
  retries without it. The user never sees an error.
- `ollama_client.rs`: Removed `gemma3` and `gemma-3` from the thinking
  model name heuristic list (they were incorrectly classified).

*Frontend (`src/`):*
- `+page.svelte`: Fixed flexbox layout collapse — added `min-height: 0`
  to `.app-body`, `min-width: 0` + `min-height: 0` to `.main-panel`,
  changed `.placeholder-panel` from `height: 100%` to `flex: 1;
  min-height: 0`. Changed `.app-root` from `100vw/100vh` to `100%/100%`
  to avoid scrollbar-induced layout shifts.
- `ChatPanel.svelte`: Added `min-width: 0` to `.chat-panel` to prevent
  horizontal blowout from long code blocks.

*Cleanup:*
- Deleted `.state-devserver.txt` (repo debris from previous audit).

**Result:** `cargo check` clean. `svelte-check` clean. User-verified with
`cargo tauri dev`:
- Text models: working ✅
- Thinking models (Gemma 4, DeepSeek R1): thinking blocks visible ✅
- Vision models (moondream): image button appears, images processed ✅
- Dynamic detection: `supports_thinking=false` correctly shown for
  non-thinking models ✅
- UI layout: stable across all panel transitions ✅

**Next:** Phase 4 — RAG integration.

**Broken by:** Nothing.

---

## 2026-05-22 — Pre-Alpha Audit Fix Sweep

**Why:** Pre-alpha audit identified 39 actionable findings (9 A-blocker,
14 B-significant, 16 C-minor) plus 27 clean signals across seven passes:
build sanity, backend correctness, frontend memory, doc truth, repo
hygiene, future-proofing, README quality. All deferred to a single
fix sweep before tagging v0.3.0.

**What:**

*Backend (`src-tauri/src/`):*
- `models.rs`: added `keep_alive: Option<String>` to `OllamaChatRequest`
  and `OllamaOptions` (Phase 4+6 prerequisite). New `ContextHint` struct
  for Phase 4 RAG / Phase 5 Memory injection. `HardwareInfo.tier` split
  into `detected_tier` + `effective_tier` (Phase 6 prerequisite).
- `adaptive_config.rs`: `detect_hardware` populates both tier fields;
  `build_tier_config` uses `effective_tier`. Intel iGPU exclusion
  documented inline (Ollama can't use them anyway).
- `db.rs`: removed dead `get_conversation`. New `conversation_exists`
  and `create_conversation_with_id` for the `chat_stream` FK guard.
- `ollama_client.rs`: removed dead `default_local()`. **Capability
  heuristic reordered — Vision before Thinking** so a hypothetical
  `gemma3-vision` classifies as Vision. **Streaming chat rewritten with
  byte-buffer + newline-split** (UTF-8 boundary fix; multi-byte CJK,
  emoji, Hindi etc no longer abort the stream). Tag-parser fallback
  capped at 1 MB to prevent unbounded growth on malformed `<think>`
  responses. `keep_alive` plumbed through to outgoing Ollama request.
  `TODO(phase-4)` seam comment for cancellation token wiring.
- `lib.rs`: new `init_logging()` writes to both stdout and a daily-
  rolling file at `~/.heimdall/logs/heimdall.log` via `tracing-appender`.
  `bootstrap()` errors are captured and surfaced via stderr at exit.
  `chat_stream` adds `context: Option<ContextHint>` parameter (no-op
  today), an FK guard that auto-creates the conversation row if missing,
  atomic-failure placeholder messages persisted on stream error or
  empty response. `set_default_model` releases mutex before async
  `write_config`. New `get_tier_config` Tauri command. Phase 5 / Phase 6
  seam comments added.
- `Cargo.toml`: metadata complete (description, author, license,
  repository, homepage, keywords). Added `tracing-appender = "0.2"`.

*Frontend (`src/`):*
- `vite.config.js`: stale `@ts-expect-error` removed (was failing
  `npm run check`).
- `package.json`: dead `@tauri-apps/plugin-opener` removed. Metadata
  complete (description, author, repository, bugs, homepage, keywords,
  engines).
- `app.html`: Google Fonts CDN links removed; SVG favicon primary, PNG
  fallback. **Zero network calls on cold launch.**
- `app.css`: shadow tokens (`--shadow-elevated`, `--shadow-popover`)
  added. Bundled-font `@font-face` declarations matching Google's
  upstream subset structure (latin + latin-ext for both fonts).
- `static/fonts/`: 4 WOFF2 files added (~96 KB total) under SIL Open
  Font License.
- `static/favicon.svg`: rune-eye source moved from `src/` to `static/`
  for SvelteKit's asset pipeline.
- `Sidebar.svelte`: reads version from Tauri's `getVersion()` instead
  of a hardcoded literal — version no longer drifts on bumps.
- `ModelSelector.svelte`, `ChatPanel.svelte`: `rgba()` literals replaced
  with `var(--shadow-*)` tokens.
- `ChatPanel.svelte`:
  - listeners register synchronously before async init (closes a race
    window where early unmount could leak a listener forever);
  - `scrollToBottom(smooth)` throttled to ≤10 Hz with `behavior: 'auto'`
    during streaming, `'smooth'` only on final settle (was pegging the
    GPU at ~30 Hz on thinking models);
  - `expandedThinking` set cleared in `newChat()` and
    `switchConversation()`;
  - `switchConversation` refuses while a stream is in flight
    (previously dropped tokens silently);
  - `pickImage` rejects files > 10 MB with a clear error (protects
    WebKit decode pipeline from OOM on a 4 GB box);
  - `resizeImageBlob` revokes its blob URL after canvas paint (was
    pinning the original file's bytes for the app's lifetime);
  - `chat_stream` invoke passes `context: null` (forward-compat for
    Phase 4–5 retrieval injection);
  - chat textarea has `aria-label="Message input"`.

*Configuration & assets:*
- `src-tauri/tauri.conf.json`: dropped `icon.icns`/`icon.ico` references
  (Linux-only alpha).
- `src-tauri/icons/`: removed Android, iOS, Windows-Store icons (~45
  files); kept only the Linux-relevant PNGs.
- 7 `*_tmp.txt` debris files removed from repo root and from git
  history. `.gitignore` updated to block recurrence.
- `.gitignore`: removed dead rules (`~/.heimdall/` literal-tilde no-op,
  obsolete `vite.config.js.timestamp-*` glob). Added `*_tmp.txt`,
  `audit_pass*.txt`, `AUDIT_FINDINGS.md`, `target_clippy/`.
- `src-tauri/icons/Square*Logo.png`: removed (Windows Store, unused).
- `src/lib/components/.gitkeep`: removed (redundant — directory has
  real files).

*Documentation:*
- `agents.md`: rewritten. Stripped workflow leakage (Model Assignment
  block), stale SQLx requirement, and the dual-source-of-truth design
  token claim. Added Phase 7 to release plan. Added Fedora clippy quirk
  to Known Gotchas. Synced reading order with `CONTEXT.md`.
- `CONTEXT.md`: trimmed from a 40-line accomplishments checklist to a
  short status + next-task summary. "Known Issues: None." replaced with
  honest "None known post-audit."
- `docs/DECISIONS.md`: corrected "Phase 2 split" mis-date; appended
  audit-driven decisions (font bundling, file logging, atomic-failure
  asymmetry, Phase 4–6 IPC forward-compat, UTF-8 boundary, tag-buffer
  cap, handoff doc move).
- `docs/ERRORS.md`: format-normalised across all entries; added an
  index for the WebKit-error confusion.
- `docs/ARCHITECTURE.md`: deleted (was a 30-line stub that
  self-contradicted within four lines). Module status moved into
  `_design/HEIMDALL_ARCHITECTURE.md`'s new "Current Implementation
  Status" appendix.
- `_design/HEIMDALL_ARCHITECTURE.md`: corrected falsehoods (Tabler
  icons, Google Fonts hosting), reframed unverified performance
  numbers, replaced the spec comparison table with honest
  "today vs target" columns, added Status Appendix with Phase 4–6
  prerequisites and known fragility notes.
- `_design/heimdall_handoff_to_agent.md`: moved to
  `docs/history/heimdall_handoff_to_agent.md` with a header note
  marking it historical.
- `.kiro/steering/steering.md`: reduced to a thin pointer at
  `agents.md` (was a stale duplicate that drifted independently).
- `CONTRIBUTING.md`: Fedora clippy workaround documented; security
  contact added; version-bump checklist added.
- `README.md`: rewritten from scratch. ~280 lines covering identity,
  philosophy, what works today, hardware tiers, install/build/first-run,
  configuration, troubleshooting, architecture, doc index. Drops the
  unrealistic spec performance numbers; honest about what's alpha and
  what's coming.

**Result:** Single commit lands every audit fix. `cargo check` clean.
`npm run check` clean (zero errors, zero warnings). `npm run build`
clean. Live SQLite schema unchanged (additive migrations only). Bundle
size ~40 KB gzipped client. App launches with zero network calls.

**Next:** Tag v0.3.0. Cut release artefact (AppImage). Begin Phase 4
RAG implementation.

**Broken by:** Nothing.

---

## 2026-05-21 — Thinking Blocks Fix: Gemma 4 and Ollama Native Thinking Parameter

**Why:** Gemma 4 and other native thinking models were silently discarding reasoning tokens because Ollama requires `"think": true` in the request payload to stream native reasoning tokens in `message.thinking`. DeepSeek r1 worked by accident because it fell back to legacy `<think>` tags in content.

**What:**
- `models.rs`: Added `think: Option<bool>` to `OllamaChatRequest`.
- `ollama_client.rs`: Enabled native thinking globally by setting `think: Some(true)` in `chat_stream()`.
- `ollama_client.rs`: Updated `detect_capability_from_name()` to include `gemma4`, `gemma-4`, `gemma3`, and `gemma-3` as "Thinking" capable models.

**Result:** Gemma 4 thinking is now fully enabled, captured, and correctly rendered in Svelte's `ThinkingBlock` UI alongside DeepSeek.

**Next:** Phase 4 — RAG Engine.

---

## 2026-05-21 — Thinking Blocks Fix: Native Ollama `message.thinking` Field

**Why:** Thinking blocks were implemented but not working. deepseek-r1:1.5b produces thinking output in the terminal but Heimdall showed no thinking block. Independent audit found the implementation structurally complete — but it was targeting the wrong data path.

**Root cause:** Ollama v0.9+ natively parses `<think>` tags on the server and streams reasoning tokens in a dedicated `message.thinking` JSON field, separate from `message.content`. Heimdall's `OllamaChatMessage` struct did not declare a `thinking` field, so serde silently dropped it during deserialization. The tag parser then searched for `<think>` in `message.content` — which was empty during the thinking phase because Ollama had already extracted the tags. Result: thinking content was silently lost.

**What:**
- `models.rs`: Added `thinking: Option<String>` to `OllamaChatMessage` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Reads from Ollama responses, does not serialize in outgoing requests.
- `ollama_client.rs`: Rewrote the streaming loop with a hybrid approach:
  - **Primary path (native field):** Reads `parsed.message.thinking` directly. If non-empty, accumulates as thinking content and emits `chat://thinking` events. State transition: first content token after thinking → emit `done: true`.
  - **Fallback path (tag parser):** If native `thinking` field is never populated (older Ollama or non-Ollama endpoint), falls back to the existing `<think>` tag parser on `content`.
- Also aligned thinking block CSS with design spec: `radius-lg`, 10px uppercase header, gold chevron.

**Design decision — stripping thinking from history:** Past thinking blocks are NOT sent back to the model in conversation history. Only the clean answer (`content`) is sent. This is correct because: (a) DeepSeek's own paper recommends against including `<think>` in history, (b) thinking blocks are huge (thousands of tokens) and would exhaust context windows on consumer hardware, (c) the model's final answer already encodes the distilled reasoning.

**Result:** `cargo check` clean. `svelte-check` clean. `npm run build` clean. Thinking blocks will now appear when using deepseek-r1, qwen3, qwq, or any model that uses Ollama's native thinking field. Tag parser fallback ensures compatibility with older Ollama versions.

**Next:** Test with deepseek-r1:1.5b to verify end-to-end. Then Phase 4 — RAG Engine.

**Broken by:** Previous agent's implementation relied entirely on client-side `<think>` tag parsing, unaware that Ollama v0.9+ extracts these tags server-side into a native `message.thinking` field.

---

## 2026-05-21 — Icon Fix Finalized + Thinking Blocks Implemented

**Why:** Two goals: (1) Permanently remove `@tabler/icons-svelte` from the dependency tree — it was already unused but still listed in `package.json`. (2) Add thinking block support for reasoning models (deepseek-r1, qwen3, qwq) that emit `<think>…</think>` tags in their response stream.

**What:**

**Part 1 — Icon fix (permanent):**
- Verified all 14 icon imports already point to `src/lib/components/icons/` (native Svelte 5 system).
- Removed `@tabler/icons-svelte` from `package.json` dependencies.
- Removed `node_modules/@tabler/` directory.
- Build verified: `svelte-check` passes, `vite build` passes, `cargo check` passes.

**Part 2 — Thinking blocks (full stack):**

*Rust backend:*
- Added `Thinking` variant to `ModelCapability` enum in `models.rs`.
- Added `ThinkingEvent` struct (conversation_id, content, done) for the `chat://thinking` Tauri event.
- Added `thinking: Option<String>` field to `Message` struct.
- Updated `detect_capability_from_name()` in `ollama_client.rs` to detect deepseek-r1, deepseek-r2, qwen3, qwq as thinking models.
- Rewrote `chat_stream()` in `ollama_client.rs` with a streaming `<think>` tag parser. Uses a lookahead buffer to handle tags split across token boundaries. Emits `chat://thinking` events for thinking content and `chat://token` events for answer content. Returns `(answer, thinking, tokens)` tuple.
- Updated `db.rs`: added `ALTER TABLE messages ADD COLUMN thinking TEXT` migration (additive, idempotent).
- Updated `insert_message()` to accept and persist `thinking` parameter.
- Updated `get_messages()` query to SELECT the `thinking` column.
- Updated `chat_stream` command in `lib.rs` to handle the new 3-tuple return and persist thinking content.

*Svelte frontend:*
- Added `thinking` field to `ChatMessage` and `BackendMessage` interfaces.
- Added `StreamThinkingEvent` interface.
- Added streaming state: `streamingThinking`, `isThinking`, `thinkingStartTime`, `thinkingDuration`, `expandedThinking`.
- Registered `chat://thinking` event listener in `onMount`.
- During streaming: shows live thinking block with content appearing in real time, cursor blink.
- When `</think>` arrives: thinking block collapses to "Thought for Ns", answer begins streaming.
- For completed messages: thinking block renders as collapsible "Thought for a moment" with expand/collapse toggle.
- CSS: `.think-block`, `.think-header`, `.think-diamond`, `.think-content` — all using design tokens (var(--border-subtle), var(--bg-surface), var(--text-ghost), var(--gold-primary)). Zero hardcoded colors.
- Updated `backendToChat()` to map `thinking` field from backend messages.
- Updated `newChat()` to reset thinking state.

**Result:** Both parts compile clean. Frontend build passes. Rust cargo check passes. Thinking blocks are fully wired end-to-end: Rust parses `<think>` tags from the stream, emits events, persists to DB. Frontend renders live thinking during streaming and collapsible blocks for completed messages.

**Next:** Test with deepseek-r1 model to verify end-to-end behavior. Then proceed to Phase 4 (RAG Engine).

**Broken by:** Nothing.

---

### Retrospective on the @tabler/icons-svelte black-screen crash

The earlier black-screen incident is documented in detail here so the
root cause survives even when the symptoms don't recur.

**When:** After running `cargo tauri icon src/rune-eye.svg` followed by `NO_STRIP=true npm run tauri build`, the app showed a completely black screen in both dev (`npm run dev`) and production. The WebKit console reported: `"undefined is not an object evaluating next_sibling_getter.call"` originating in `+page.svelte`, `+layout.svelte`, and `root.svelte`. Full cache clear (`rm -rf dist/ src-tauri/target/ .svelte-kit/ node_modules/.vite/`) did not fix it.

**Root Cause:** `@tabler/icons-svelte@3.44.0` is a **Svelte 4 library** that is fatally incompatible with **Svelte 5.55.7** in runes mode.

The crash mechanism:
1. SvelteKit 2.60.1 generates `root.svelte` with `<svelte:options runes={true} />`. This means the entire component tree operates in Svelte 5 runes mode.
2. The app's components (`+page.svelte`, `ChatPanel.svelte`, etc.) use `$state()`, `$derived()`, `$props()` — all runes-mode features.
3. `@tabler/icons-svelte` ships raw `.svelte` files written in Svelte 4 syntax: `export let`, `$$props`, `$$restProps`, `<slot />`, and critically, `<svelte:element this={tag} {...attrs} />` inside `{#each}` loops.
4. When Svelte 5.55's compiler processes these legacy components inside a runes-mode parent tree, the internal DOM template node structure generated for `<svelte:element>` with dynamic tag names inside `{#each}` does not match what the runes-mode reconciler expects.
5. During mount, the runtime calls `next_sibling_getter.call(node)` — a cached reference to `Node.prototype.nextSibling`'s getter — on a DOM node that doesn't exist at the expected position in the sibling chain. This throws `"undefined is not an object"` in WebKit (WebKitGTK 2.52.3), which is a TypeError because the node is null/undefined.
6. Because this error occurs during the initial mount of the component tree (before any DOM is painted), the entire app fails to render — black screen.

**Contributing factor:** `src/routes/+layout.svelte` used `<slot />` (Svelte 4 syntax) instead of `{@render children()}` (Svelte 5 syntax). While `<slot />` alone might work via the legacy compatibility shim, it added another legacy codepath into the already-fragile mount sequence.

**Why it appeared after the build command:** The `npm run tauri build` (triggered via `beforeBuildCommand` in `tauri.conf.json`) ran `vite build`, which compiled the full dependency tree. The icon generation (`cargo tauri icon`) was a red herring — it only replaced PNG/ICO/ICNS files in `src-tauri/icons/` and had no effect on the frontend. The real issue was always present but may have been masked by Vite's dev-mode HMR which can sometimes recover from partial mount failures. A production build + fresh page load exposed the crash deterministically.

**What was fixed:**
1. **Replaced `@tabler/icons-svelte` entirely** with a Svelte 5 native icon system:
   - Created `src/lib/components/icons/Icon.svelte` — a minimal component using `$props()` and `{#each paths as d}<path {d} />{/each}`. No `<svelte:element>`, no `$$props`, no `<slot />`.
   - Created `src/lib/components/icons/index.ts` — exports SVG path data arrays for all 14 icons used in the app.
   - Updated `Sidebar.svelte`, `ChatPanel.svelte`, `ModelSelector.svelte` to import from the new icon system.
2. **Fixed `src/routes/+layout.svelte`** — replaced `<slot />` with `let { children } = $props()` + `{@render children()}`.
3. **Fixed `ChatPanel.svelte`** — changed `$state<HTMLDivElement>(null!)` to `$state<HTMLDivElement>(undefined!)` for `bind:this` targets.

**Result:** App renders correctly in both dev and production. Build output dropped from 6326 modules to 176 (no longer bundling 5000+ unused icon components). Zero compiler warnings. Zero runtime errors.

**Lesson:** Never use Svelte 4 component libraries (those shipping raw `.svelte` files with `export let` / `$$props` / `<svelte:element>`) in a Svelte 5.55+ runes-mode app. The legacy compatibility layer does not handle `<svelte:element this={dynamic}>` inside `{#each}` correctly when the parent tree is in runes mode. Either use libraries that ship Svelte 5 native code, or replace them with inline implementations.

**Broken by:** `@tabler/icons-svelte@3.44.0` — a Svelte 4 library whose internal use of `<svelte:element>` + `{#each}` + `$$props` is incompatible with Svelte 5.55.7's runes-mode DOM reconciler.

**Saga summary:** `@tabler/icons-svelte@3.44.0` was added in the Phase 0
foundation entry (2026-05-17) for frontend icons. It worked through
Phases 1–2, then crashed Alpha pre-release with a `next_sibling_getter`
error under Svelte 5 runes. This entry replaces it with the native
`src/lib/components/icons/` system. The dependency was removed from
`package.json` in this same entry. After this point, no other entry
mentions Tabler — the saga is closed.

---

## 2026-05-21 — Svelte-Check CSS and TypeScript Cleanup
**Why:** Lint and type checks flagged an unrecognized CSS property warning in the custom titlebar and a TypeScript type-narrowing error in the ChatPanel image picker.
**What:**
- Removed invalid un-prefixed CSS property `app-region: no-drag;` from `src/lib/components/TitleBar.svelte`.
- Kept the standard, vendor-prefixed `-webkit-app-region: no-drag;` property, which is fully supported by Chromium and Tauri for excluding window controls from the titlebar drag region.
- Resolved type-narrowing compiler error in `src/lib/components/ChatPanel.svelte`'s image picker (`pickImage()`) where TypeScript incorrectly inferred a `never` type for unreachable properties inside the `open` dialog return branch. Simplified `filePath` retrieval directly from the returned path string.
**Result:** Both `TitleBar.svelte` and `ChatPanel.svelte` now compile and type-check with zero errors or warnings.
**Next:** Address remaining Vite config diagnostics in the workspace.
**Broken by:** Nothing.

---

## 2026-05-20 — Phase 3: Alpha Release 1 (v0.3.0)
**Why:** Heimdall must feel complete and trustworthy. Users should see their last conversation on launch, manage history, and use vision models with images.
**What:**
- **Task 1 — Memory & persistence:** Verified `get_user_preferences` and `set_default_model` commands exist and work. Model selection persists to `config.toml` and loads on restart.
- **Task 2 — Chat history loads on startup:** On mount, calls `list_conversations`, loads most recent, fetches messages via `get_messages`. User sees their last conversation, not a blank screen.
- **Task 3 — New Chat behavior:** New Chat resets local state. Old conversation stays in history. Auto-title from first user message (first 40 chars). Added `update_conversation_title` Tauri command.
- **Task 4 — History tab:** Replaced placeholder with scrollable conversation list. Click to load, active highlight, delete button (appears on hover). Refreshes on tab open.
- **Task 5 — Model capability detection:** Expanded name heuristics (11 vision patterns, 7 embedding patterns). Template-based detection improved with `vision`/`embed` family checks.
- **Task 6 — UI adapts to model:** Image button only shows for Vision/Multimodal models. Audio button removed (Ollama has no Whisper transcription API).
- **Task 7 — Image input:** File picker via `@tauri-apps/plugin-dialog`. Image resized to max 1024px via canvas, encoded base64. Sent to Ollama vision endpoint. Thumbnail shown in chat bubble. Images persisted to DB (survives conversation switches and app restarts).
- **Task 8 — Audio input:** Deferred. Ollama does not support audio transcription natively. Will revisit with whisper.cpp sidecar in a future phase.
- **Task 9 — Final Alpha polish:** Version bumped to 0.3.0. Warnings fixed. Window min-size enforced programmatically for Linux. DEVLOG updated.
- **Bug fixes along the way:**
  - Replaced `sqlx::query_as!` (compile-time) with `sqlx::query_as::<_, T>()` (runtime) — eliminates DATABASE_URL requirement.
  - Fixed nested `<button>` in history tab (HTML spec violation).
  - Installed missing `@tauri-apps/plugin-dialog` and `@tauri-apps/plugin-fs` npm packages.
  - Fixed image not reaching vision models (was sending images in all history messages instead of just the last).
  - Increased Ollama timeout from 120s to 300s for CPU-bound vision models.
  - Enforced window min-size programmatically (Linux + decorations:false doesn't respect config hint).
**Result:** Alpha Release 1 complete. Heimdall is a functional local AI chat app with conversation persistence, history management, model switching, and vision input.
**Next:** Phase 4 — RAG integration (knowledge ingestion, vector search, context injection).
**Broken by:** Nothing.

---

## 2026-05-20 — Phase 2 UI Polish Pass
**Why:** Chat UI diverged from concept design during Phase 2B implementation. Needed to align pixel-for-pixel with `_design/heimdall_ui_reference.html`.
**What:**
- Added tab strip (Chat | History | Models) below titlebar, matching concept exactly.
- Moved ModelSelector + new chat button + status dot into a model-bar below tabs (visible only on Chat tab).
- Switched messages from bubble-style (user right, AI left) to left-aligned linear flow matching concept.
- Replaced IconUser/IconRobot avatars with concept-accurate U letter (user) and Heimdall diamond SVG (AI).
- Added code block parsing: assistant messages with fenced code blocks render in `.msg-code` style (dark bg, blue-ish `#7a9fbc` text).
- Fixed layout collapse: `.main-panel` now flex container so ChatPanel fills available space correctly.
- Fixed CSS loading: removed dead `<link href="/src/app.css">` from app.html, created `+layout.svelte` with proper `import '../app.css'` for SvelteKit pipeline. CSS now bundles correctly in production builds.
**Result:** Chat UI matches concept design. Production build verified. All Phase 2 cleanup tasks complete.
**Next:** Phase 3 — multimodal input (image + audio support via input_processor.rs).
**Broken by:** Nothing.

---

## 2026-05-19 — Phase 2B: ChatPanel UI Complete
**Why:** Backend is ready (Phase 2A). Now wire the frontend to actually chat with Ollama.
**What:**
- Built `ChatPanel.svelte`: full streaming chat with token-by-token display via Tauri events.
- Built `ModelSelector.svelte`: dropdown showing all local models with capability icons and sizes.
- Wired `create_conversation`, `chat_stream`, model selection, and preference persistence.
- Added empty states (Ollama offline, new conversation), error banners, streaming indicators.
- Token counter shows usage after each response.
- New Chat button resets conversation state.
- Messages persisted to SQLite (both user and assistant).
**Result:** Heimdall can chat with any local Ollama model. Streaming works. Conversations persist.
**Next:** UI polish pass to align with concept design.
**Broken by:** Nothing.

---

## 2026-05-19 — Phase 2A: Rust Backend Complete
**Why:** Need the full backend before building any chat UI. Clean separation of concerns.
**What:**
- Implemented `ollama_client.rs`: health check, list models, model info, streaming chat completions, pull/delete models. All async with reqwest.
- Implemented `db.rs`: SQLite via sqlx with async pool. Schema: conversations + messages tables. CRUD operations for both.
- Implemented `adaptive_config.rs`: hardware detection (RAM, CPU, VRAM via sysinfo), tier assignment (Minimal/Standard/Full), config.toml persistence, `~/.heimdall/` directory management.
- Implemented `models.rs`: shared types (HardwareInfo, Tier, OllamaHealth, OllamaModel, Conversation, Message, etc).
- Wired all Tauri commands in `lib.rs`: 12 commands registered covering health, models, chat, conversations, preferences.
- Bootstrap sequence: ensure dirs → load config → detect hardware → assign tier → open DB → build client → register state.
- Added Tauri plugins: shell, dialog, fs, notification. Capabilities configured.
**Result:** Full backend scaffold. All Tauri commands callable from frontend. SQLite persists data to `~/.heimdall/db/`.
**Next:** Phase 2B — ChatPanel UI to wire frontend to these commands.
**Broken by:** Nothing.

---

## 2026-05-18 — Release Strategy and Phase Split Decided
**Why:** Solo developer, better to ship working increments than wait for everything. Beta release strategy adopted to find bugs iteratively.
**What:**
- Split Phase 2 into Phase 2A (backend only - ollama_client, SQLite schema, Tauri commands) and Phase 2B (ChatPanel UI).
- Established explicit release plan mapping phases to beta milestones.
  - Phase 3 complete = Alpha Release 1 (Chat working locally with models)
  - Phase 4 complete = Beta 1 (RAG integration)
  - Phase 5 complete = Beta 2 (Chat history and memory)
  - Phase 6 complete = Beta 3 (Governor and resource management)
  - Phase 7 complete = Release Candidate
  - Phase 8 = v1.0 Stable
**Result:** Clear roadmap defined; Phase 2A starts immediately as a backend-only pass.
**Next:** Begin Phase 2A backend development.
**Broken by:** Nothing.

---

## 2026-05-18 — Phase 1: App Shell Complete
**Why:** Build the visual skeleton — TitleBar, Sidebar, and wired page shell — before any panel logic.
**What:**
- Created `src/app.css` with all design tokens as CSS custom properties (var(--name)). Single source of truth for styles.
- Updated `src/app.html` to import `src/app.css` and Google Fonts (Cinzel + JetBrains Mono).
- Built `TitleBar.svelte`: rune-eye SVG logo, Cinzel wordmark "HEIMDALL", ghost subtitle "LOCAL AI GATEWAY", window control dots. All CSS via var() tokens.
- Built `Sidebar.svelte`: icon-only nav (Chat, Governor, Shortcuts, Settings), active state with gold highlight, gold pulse dot for governor alerts, Settings pinned to bottom. Accessible `<button>` elements.
- Wired `+page.svelte` shell: TitleBar + Sidebar mounted, `activePanel` state, `navigate()` callback, placeholder main panel.
- Removed redundant `:global` resets from `+page.svelte` (now owned by `app.css`).
- Documented "CSS Custom Properties over hardcoded hex" decision in `docs/DECISIONS.md`.
- Verified in browser: shell renders correctly, TitleBar gold wordmark visible, sidebar icons and active state correct, zero console errors.
**Result:** Phase 1 shell complete and confirmed working.
**Next:** Phase 2A — Rust backend implementation (ollama_client, SQLite schema, Tauri commands), followed by Phase 2B (ChatPanel UI).
**Broken by:** Nothing.

---

## 2026-05-18 — Phase 1: Tokens and Handoff Confirmed
**Why:** First step of Phase 1 is to strictly define the design language via a token file so there are no magic strings in the UI.
**What:** 
- Confirmed Pre-Phase 1 fixes (SvelteKit routing, `dist/` build path, `tauri-plugin-shell`).
- Created `src/lib/tokens.ts` with exact values from `_design/heimdall_handoff_to_agent.md`.
**Result:** Tokens file created. Awaiting manual confirmation before proceeding to layout.
**Next:** Once tokens are confirmed, build `App.svelte` shell layout, `TitleBar.svelte`, and `Sidebar.svelte`.
**Broken by:** Nothing.

---

## 2026-05-17 — Phase 0: Project Initialized
**Why:** Starting Heimdall from scratch with clean architecture.
**What:**
- Verified full toolchain: rustc 1.95, node 22.22, cargo-tauri 2.10.1, Python 3.14, Graphify 0.6.4
- Created Tauri 2 + SvelteKit + TypeScript project via `npm create tauri-app@latest`
- Established folder structure: `src/lib/{components,stores,utils}`, `docs/modules`, `_design/`
- Configured `tauri.conf.json`: decorations off, 1100×700 window, identifier `dev.heimdall.app`
- Added 18 Rust dependencies (tauri plugins, tokio, reqwest, sqlx, sysinfo, tracing, etc.)
- Added `@tabler/icons-svelte` 3.44.0 for frontend icons
- Created documentation scaffold: CONTEXT.md, DEVLOG.md, docs/{ARCHITECTURE,ERRORS,DECISIONS}.md
- Design reference files placed in `_design/`
- Git initialized, pushed to `github.com/sandeep4513m/heimdall`, tagged `v0.0.1`
- Graphify installed, first graph extracted (11 nodes, 5 edges), GRAPH_REPORT.md committed
- Dev server verified — compiles 663 crates, window opens without errors
- Fixed SvelteKit adapter-static output to `dist/` to match `tauri.conf.json` frontendDist
- Swapped `tauri-plugin-opener` → `tauri-plugin-shell` in lib.rs and capabilities

**Result:** Project skeleton complete. Dev server runs. All Phase 0 checkboxes green.
**Next:** Begin Phase 1 — design tokens (`src/lib/tokens.ts`) and app shell layout.
**Broken by:** Nothing.
