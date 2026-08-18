# Heimdall — Technical Decisions

Every major technical choice is recorded here with reasoning.
Once written, entries are never deleted — only appended.

---

## Tauri 2 + Svelte over Electron + React
**Decision date:** Project start
**Why Tauri:** Uses OS WebView (WebKitGTK on Linux), not bundled Chromium.
Idle RAM under 60MB vs 150-300MB for Electron. Native Linux feel.
**Why Svelte:** Compiles to vanilla JS. No virtual DOM. Smallest bundle of any
frontend framework. Fastest rendering on low-end hardware.
**Rejected:** Electron (too heavy), Flutter (immature Linux), Qt (UI ceiling too low)

## Rust over Go/Python for backend
**Decision date:** Project start
**Why Rust:** Memory safe without GC. Zero-cost abstractions. No GC pauses
during inference. Perfect match for Tauri. Rich crate ecosystem for all needed
libraries (sqlx, sysinfo, reqwest, usearch).
**Rejected:** Go (GC pauses), Python (too slow, separate process needed)

## usearch over ChromaDB/Qdrant for vector storage
**Decision date:** Architecture design
**Why usearch:** Embedded Rust crate. Runs inside Heimdall process. Zero setup.
Memory-maps to disk on low-RAM systems. No separate server process.
**Rejected:** ChromaDB (Python server), Qdrant (separate binary), Weaviate (too heavy)

## SQLite over PostgreSQL for chat/metadata storage
**Decision date:** Architecture design
**Why SQLite:** Embedded, zero setup, single file at ~/.heimdall/db/heimdall.db.
sqlx gives async + compile-time query checking. Perfect for local desktop app.
**Rejected:** PostgreSQL (requires server), plain files (no query capability)

## CSS Custom Properties over hardcoded hex
**Decision date:** Phase 1
**What:** All component styles use var(--token-name) from src/app.css. Zero hardcoded hex values in any .svelte file style block. Ever.
**Why:** Single source of truth. Change one value in app.css, entire app updates. Enforced from first component.
**How:** src/app.css defines all tokens as CSS variables. Imported once in app.html. All components use var() only.

## Phase 2 split into 2A and 2B
**Decision date:** Phase 2 entry (2026-05-18)
**Why:** Backend and UI built separately so each session ships something complete and testable. Never have half-working UI waiting on unfinished backend.

## Beta release strategy adopted
**Decision date:** Phase 1
**Why:** Each major feature ships as a beta. Real users find real bugs before next layer builds on top. Linus released 0.01 first. Same principle.

## @tabler/icons-svelte replaced with native icon system
**Decision date:** Phase 3 / Alpha 1 (2026-05-21)
**Problem:** `@tabler/icons-svelte@3.44.0` is a Svelte 4 library. Its `.svelte` files use `<svelte:element this={tag}>` inside `{#each}`, `$$props`, `$$restProps`, and `<slot />`. In Svelte 5.55.7+ runes mode, these legacy patterns cause a fatal `next_sibling_getter` DOM reconciliation crash — black screen, zero rendering.
**Immediate fix:** `+layout.svelte` converted from `<slot />` to `{@render children()}`. All `@tabler/icons-svelte` imports replaced with `src/lib/components/icons/` — a Svelte 5 native icon system using `$props()` and static `<path>` elements.
**Permanent status:** Fix is already in place. The `@tabler/icons-svelte` dependency remains in `package.json` but is no longer imported anywhere. It should be removed from `package.json` before Beta 1.
**Options considered:** `unplugin-icons` (too much config), `svelte-hero-icons` (wrong icon set), `svelte-tabler` v2 (Svelte 5 runes-compatible but adds external dep). Chose inline SVG path data — zero dependencies, 14 icons, ~3KB total.
**Risk if reverted:** Will crash immediately on any Svelte 5.55+ build. The incompatibility is structural, not a bug that will be patched upstream.

## @tabler/icons-svelte fully removed from package.json
**Decision date:** 2026-05-21
**What:** Removed `@tabler/icons-svelte` from `package.json` dependencies. The native icon system (`src/lib/components/icons/`) was already in place and all imports already pointed to it. The package was dead weight.
**Why:** Zero imports reference it. Keeping it in `package.json` adds 5000+ unused `.svelte` files to `node_modules`, slows installs, and risks accidental re-import.
**Result:** Clean dependency tree. Build verified.

## Thinking blocks: dynamic detection over hardcoded model list
**Decision date:** 2026-05-21
**What:** Thinking block support detects `<think>` tags in the token stream dynamically. `ModelCapability::Thinking` is assigned by name heuristic (deepseek-r1, qwen3, qwq) for UI hints, but the parser handles any model that emits `<think>` tags regardless of capability classification.
**Why:** New thinking models appear frequently. Hardcoding a list means constant updates. Dynamic detection is future-proof — if a model emits `<think>`, we parse it. The capability enum is only for UI hints (e.g. showing a "thinking" badge in the model selector).
**Rejected:** Hardcoded model list only (brittle), separate Ollama API field (doesn't exist yet), always-on parsing without capability hint (no way to show UI badge before first response).

## Thinking block: single generic Icon.svelte + path arrays over one-file-per-icon
**Decision date:** 2026-05-21 (reaffirmed from prior session)
**What:** The icon system uses one `Icon.svelte` component that takes `paths: string[]` rather than individual `IconMessage2.svelte`, `IconCpu.svelte` etc. files.
**Why:** Less file proliferation (2 files vs 14+), same zero-dependency result, easier to add new icons (just add a path array export). The generic component is 30 lines and handles all SVG rendering uniformly.

## Thinking blocks: native Ollama field over client-side tag parsing
**Decision date:** 2026-05-21
**What:** Primary thinking block detection reads Ollama's native `message.thinking` field. The client-side `<think>` tag parser is retained as a fallback for older Ollama versions or non-Ollama endpoints.
**Why:** Ollama v0.9+ parses `<think>` tags server-side and streams reasoning in a separate JSON field. Reading this field directly is simpler, more reliable, and avoids the complexity of stateful tag parsing over a partial token stream. The tag parser stays because Heimdall should work with any Ollama version.
**Rejected:** Tag parser only (missed native field entirely — root cause of the bug), native field only (breaks with older Ollama).

## Thinking blocks: strip reasoning from multi-turn history
**Decision date:** 2026-05-21
**What:** Past AI reasoning blocks are NOT included when sending conversation history to Ollama. Only the clean `content` (final answer) is sent.
**Why:** (1) DeepSeek's own paper recommends against including `<think>` blocks in history — the model performs better with fresh reasoning each turn. (2) Thinking blocks can be thousands of tokens, exhausting context windows on consumer hardware (Heimdall targets 4GB RAM minimum). (3) The final answer already encodes the distilled reasoning.
**Rejected:** Always include reasoning (wastes context, can confuse models), hardware-adaptive inclusion (quality argument doesn't hold regardless of RAM).

## Bundle fonts locally instead of fetching from Google
**Decision date:** Pre-alpha audit (2026-05-22)
**Problem:** `app.html` was fetching Cinzel and JetBrains Mono from
`fonts.googleapis.com` on every cold launch. Heimdall's identity is
"no data leaves your machine" — phoning home to Google CDN on every
start contradicted the README and gave Google the launch IP, User-Agent,
and timing.
**Decision:** Bundle the same Google-Fonts WOFF2 subsets locally under
`static/fonts/` (latin + latin-ext for both fonts, ~96 KB total). Both
fonts ship under SIL Open Font License — redistribution is allowed.
**Implementation:** `src/app.html` no longer references `fonts.googleapis.com`.
`src/app.css` declares `@font-face` with relative `src: url('/fonts/...')`
plus `unicode-range` hints matching Google's upstream subset structure.
**Result:** Zero network calls at launch. Identical visual output.

## File logging to `~/.heimdall/logs/heimdall.log`
**Decision date:** Pre-alpha audit (2026-05-22)
**Problem:** `tracing_subscriber::fmt().init()` only wrote to stdout.
Tauri apps launched from a `.desktop` entry have no stdout, so every
diagnostic was vanishing. The architecture spec promised a rotating log
at `~/.heimdall/logs/heimdall.log`. The directory was created; the file
was never written.
**Decision:** Use `tracing-appender = "0.2"` daily-rolling file appender
plus a non-blocking writer, composed with the existing stdout layer.
The `WorkerGuard` is held for the process lifetime by `run()`.
**Result:** `~/.heimdall/logs/heimdall.log` now receives all `info!`/
`warn!`/`error!` output. `.desktop` launches produce debuggable artefacts.

## Atomic-failure asymmetry in `chat_stream`
**Decision date:** Pre-alpha audit (2026-05-22)
**Problem:** Original `chat_stream` persisted the user message *before*
calling Ollama. If streaming failed mid-flight, the user message stayed
persisted with no assistant reply — orphan messages on reload, duplicate
user messages on retry.
**Options considered:**
- (a) Save user message *only* after streaming completes — loses record
  that the user even tried.
- (b) Save user message immediately, save a placeholder assistant error
  message on failure — clutters history with errors but is honest.
- (c) Wrap the whole flow in a transaction — can't easily transact
  across an HTTP stream.
**Decision:** (b). On stream failure or empty-response, persist a
placeholder assistant message with content `"⚠️ Stream failed: <reason>"`
or `"⚠️ Model returned an empty response..."`. History reads honestly;
the user can scroll up and see what broke.

## Forward-compat IPC additions for Phase 4–6
**Decision date:** Pre-alpha audit (2026-05-22)
**What:** Locked in API surface during the audit fix sweep so Phase 4
and Phase 6 don't re-break the IPC contract:
- `OllamaOptions::keep_alive: Option<String>` — Phase 6 sends `"0s"` to
  unload models; Phase 4 sends `"5m"` to keep embedding model warm
  during ingestion.
- `HardwareInfo` split: `detected_tier` (what the box is) +
  `effective_tier` (what the user overrode to). Phase 6 Governor panel
  shows both honestly.
- `ContextHint` struct: `rag_collections: Option<Vec<String>>`,
  `memory_enabled: Option<bool>`. Passed to `chat_stream` as a no-op
  today; Phase 4 fills the first, Phase 5 fills the second.
- New `get_tier_config` Tauri command returning the active `TierConfig`
  for the future Knowledge Base UI and Governor panel.
**Why now:** Adding parameters to existing commands later means breaking
the frontend IPC contract. Adding them now (with frontend passing
`None`/`null`) means Phase 4–6 can fill them in cleanly without
co-ordinated frontend changes.

## UTF-8 boundary safety in stream parsing
**Decision date:** Pre-alpha audit (2026-05-22)
**Problem:** `OllamaClient::chat_stream` was decoding each TCP chunk
with `std::str::from_utf8(&chunk)`. TCP chunks split at byte boundaries,
not Unicode boundaries — a multi-byte CJK character or emoji straddling
two chunks would crash the stream with "Stream chunk is not valid UTF-8."
A Hindi/Chinese/Japanese/Korean/Arabic/emoji conversation could break
randomly.
**Decision:** Accumulate raw bytes in a `Vec<u8>`. Split on `\n`
(Ollama's NDJSON format guarantees newline-terminated objects). Decode
UTF-8 only on each complete line, where the boundary is guaranteed safe.
**Result:** Streams never abort due to chunk boundaries; non-ASCII
conversations work reliably.

## Tag-parser fallback buffer cap
**Decision date:** Pre-alpha audit (2026-05-22)
**Problem:** The `<think>` tag-parser fallback (used when Ollama doesn't
populate the native `message.thinking` field) accumulated content into
`tag_buf` until it found `</think>`. A malformed model emitting `<think>`
without ever closing it would grow the buffer without bound.
**Decision:** Cap `tag_buf` at 1 MB. On overflow, force-close the thinking
block (flush whatever was in it as thinking content, emit `done: true`),
log a warning, and pass all subsequent content straight through as answer
text.
**Result:** No memory leak from malformed thinking responses. Generous
1 MB cap accommodates any well-behaved model.

## Move handoff doc to `docs/history/`
**Decision date:** Pre-alpha audit (2026-05-22)
**What:** Moved `_design/heimdall_handoff_to_agent.md` to
`docs/history/heimdall_handoff_to_agent.md` with a header note marking
it as the original vision document, no longer operational.
**Why:** The handoff was written pre-Phase 0 and contained instructions
that have since been reversed (mandate of `@tabler/icons-svelte`,
"do not write code yet," etc). Keeping it next to the active spec
risked agents following stale instructions. Moving it preserves the
project's origin record without confusing operational reading.
**Replaces:** `docs/ARCHITECTURE.md` (deleted; was a 30-line stub
that self-contradicted within four lines). The release plan and
module status moved into a "Current Implementation Status" appendix
in `_design/HEIMDALL_ARCHITECTURE.md`.
