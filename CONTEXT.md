# Heimdall — Current Context

## Status

**v0.6.0 Beta 3 — Governor Intelligence complete.** Phase 6 ships the
resource-management subsystem that makes Heimdall trustworthy on real
4 GB hardware. The Governor polls system resources every 2 seconds,
auto-unloads idle models under memory pressure, orchestrates
embedding/chat swaps on Tier 1, and exposes a live Governor panel and
a full Models management tab.

What this build delivers:
- Streaming chat against any local Ollama model with stream cancellation.
- Conversation history persists across restarts; last session loads on launch.
- Vision input for vision-capable models (image resized to 1024 px, 10 MB cap).
- Thinking-model support: native `message.thinking` field plus `<think>` tag fallback.
- **Model Intelligence Registry** — single source of truth for model capabilities.
- **RAG Engine** — embedded usearch vector database, `tiktoken-rs` chunker,
  loaders for .txt/.md/.pdf/.docx/code/folders/URLs.
- **Knowledge Panel** — collections CRUD with inline confirm/rename (no
  `alert/confirm/prompt`), browse-files + browse-folder + drag-drop +
  paste-URL ingestion paths, live progress that survives navigation.
- **Chat Knowledge attach** — toolbar button (book icon) opens a
  popover to toggle active collections per conversation. A subtle
  gold dot marks the button when ≥1 collection is active.
- **Memory System** — fact extraction, user-confirmed memory, episode
  summaries, injection into chat context. See details below.
- **Governor Intelligence** — 2-second polling loop reading `/proc/meminfo`,
  `/proc/stat`, Ollama process RSS, Heimdall self RSS, VRAM via
  `/sys/class/drm/`, and per-model memory via Ollama `/api/ps`.
  Auto-unloads longest-idle model under pressure with stream-aware and
  ingestion-aware safety guards. Adaptive embedding orchestration on
  Tier 1 (transparent chat/embedding swap). Governor Panel with live
  risk-state hero, ResourceCards, ModelList with per-model unload +
  auto-unload toggle, ThresholdControls, VramCard.
- **Models Tab** — lists all local Ollama models with capability badges,
  hardware-aware recommendations, pull (curated catalog + free-form),
  delete, and set-default actions.
- Adaptive hardware detection (RAM, VRAM, CPU) driving TierConfig limits.
- Bundled fonts, local SQLite persistence, daily-rolling logs.

## Memory System — Current Functional State

All of the following are verified working:

- **Extraction**: After a conversation ends (new chat or switch), facts are
  extracted using a five-layer pipeline — schema-constrained generation
  (Ollama `format` parameter), short concrete prompt with one few-shot
  example, robust multi-strategy parser, per-fact validation (length +
  verb-form + AI-framing rejection), three-attempt protocol-fallback
  (SchemaJson → PlainJson → Lines). Works reliably on phi4-mini and larger.
- **Review**: Gold notification banner in ChatPanel. FactReviewBanner in
  Memory panel groups pending facts by batch. Per-fact confirm/edit/reject.
  Confirm-all / reject-all. Dedup badge ("similar to existing"). Conflict
  badge ("conflicts with existing").
- **Storage**: Confirmed facts stored in `memory_facts` (SQLite). Hard cap
  200 facts. Soft warn at 150 (amber indicator in chat toolbar). Hard cap
  signal turns red.
- **Injection**: Confirmed facts injected as system message at position 0
  on every chat turn. Token budget adaptive: 8% of model's `num_ctx`,
  clamped 200–1500 tokens for facts, 240–2000 for episodes.
- **Episodes**: Conversation summaries embedded via nomic-embed-text and
  stored in `_memories.usearch`. Retrieved semantically (cosine ≥ 0.6,
  top-3) on the first message of a new conversation.
- **Transparency**: Per-turn "● Memory used" expandable badge below each
  assistant message shows exact injected context. Fact provenance pill
  ("from {conversation title}") links back to source conversation.
- **Search**: Search input in Memory panel filters confirmed facts by
  substring client-side.
- **Per-conversation toggle**: Memory can be disabled per-conversation via
  the MemoryToggle in the chat toolbar. Disabling gates both injection and
  extraction.
- **Export**: Export button in Memory panel writes confirmed facts as JSON.
- **Re-extract**: Chat overflow menu (···) has "Re-extract memory" action
  for manual retry.

## Current Task

Phase 6 complete (including the full optional test suite). Beta 3 shipped.

The 20 deferred Phase 6 optional tasks are now done: all eight Governor
property tests (P1–P8), the unit tests for `/proc` parsers / VRAM walk /
PID resolution / `StreamGuard` / `chat_reload_pending`, the five
integration tests, the NF1 chat-stream latency benchmark, and the
gated "predictive pre-unload preview" Legendary feature.

## Next Task

**Phase 7 — Release Candidate.**

Settings panel, keyboard shortcut remapping, memory import, and
performance hardening.

## Known Issues

- Resume isn't supported for multi-file ingestion jobs yet
  (single-file resume works). Surfaces a clear error message.
- Per-source delete on multi-file jobs is blocked (the job stores a
  label, not individual paths). Delete the collection and re-ingest
  as a workaround.
- Memory import (counterpart to export) is deferred to a follow-up session.
- Fact quality/confidence scoring deferred to a follow-up session.
- The "predictive pre-unload preview" Legendary feature ships behind the
  `legendary_predictive_preview` config flag (default off); the preview
  component is not yet mounted into the Knowledge panel.

## Agent Instructions

Read `agents.md` first every session. Then this file. Then
`graphify-out/GRAPH_REPORT.md` if it exists. Then `DEVLOG.md` last entry.
Then `_design/HEIMDALL_ARCHITECTURE.md` only if touching the spec.
Only then begin work.
