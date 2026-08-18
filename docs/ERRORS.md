# Heimdall — Error Log

Every non-trivial bug gets one entry here.

## Format

Each entry follows this template, top to bottom:

```
## YYYY-MM-DD | Module | One-line summary
**When:** Single-line trigger condition
**Symptom:** What the user sees
**Severity:** Blocker / Significant / Minor

### Root Cause
Single paragraph or short list explaining why it happened.

### Fix
Concrete change(s) that closed the bug. Reference exact files and lines
where useful.

### Prevention
What we'd do differently. Lint, test, doc, or convention to prevent
recurrence.
```

If the same error string can come from two unrelated causes, both
entries stay separate but the index below disambiguates.

---

## Index of confusable errors

- **"WebKit encountered an internal error" (blank screen)** — see two
  entries below dated 2026-05-19. Cause depends on whether it appears
  on first launch (port race) or after a code change (onMount unhandled
  rejection).

---

## 2026-05-23 | Frontend+Backend | Streaming tokens silently dropped — two compounding bugs

**When:** Sending any message after app launch (especially on second+
launch with existing conversations in DB)
**Symptom:** Two manifestations: (1) Entire stream invisible — UI shows
spinner forever, but reload reveals the full response. (2) After fix #1,
last few characters of every response cut off mid-word.
**Severity:** Blocker — complete or partial data loss on every chat

### Root Cause

**Bug 1 — Stale closure in Svelte 5 event listener (frontend):**

`conversationId` is declared with `$state<string>()`. The Tauri event
listeners (`chat://token` and `chat://thinking`) are registered in
`onMount` as plain JavaScript arrow functions passed to the Tauri
`listen()` API. These closures are **outside** Svelte 5's reactive
compilation context — the compiler does not rewrite reads inside them
into live signal accesses. They capture the value of `conversationId`
at registration time (a throwaway UUID), not a live reference.

After registration, `loadLastConversation()` overwrites `conversationId`
with the real DB id. Every subsequent token event is compared against the
stale UUID and silently dropped via the guard:
`if (payload.conversation_id !== conversationId) return;`

**Bug 2 — Tag-parser lookahead buffer not emitted at stream end (backend):**

For non-thinking models, the streaming code uses a tag-parser fallback
that holds back up to 7 bytes in `tag_buf` as lookahead for `<think>`
tag detection. When the stream ends (`parsed.done`), the code flushed
`tag_buf` into `answer_content` (persisted to DB) but never emitted a
`chat://token` event for those final bytes. The frontend never received
the last 1–7 characters of the response.

### Fix

**Bug 1:** Added a `getConversationId()` getter function. Both listener
guards now call `getConversationId()` instead of reading `conversationId`
directly. The getter reads the live `$state` value at call time. This is
the canonical Svelte 5 pattern documented at:
https://svelte.dev/docs/svelte/$state#Passing-state-into-functions

**Bug 2:** In the `parsed.done` flush block in `ollama_client.rs`, added
a `chat://token` emit of the remaining `tag_buf` content (with
`done: false`) immediately before the final `done: true` event. The
frontend now receives every character before the stream is finalized.

### Prevention

- **Svelte 5 rule:** Never read `$state` variables directly inside
  closures passed to external APIs (Tauri `listen()`, `setTimeout`,
  `addEventListener`, etc). Always use a getter function or wrap in
  `$effect`. Document this in `agents.md` conventions.
- **Streaming rule:** Any buffering mechanism in the stream parser must
  flush its buffer to ALL consumers (both the return value AND the event
  emitter) at stream end. Add a comment at every buffer site noting
  "flush on done" as a requirement.

---

## 2026-05-23 | Frontend | Conditional rendering kills Tauri streaming events

**When:** Streaming a response from a model, switching to Settings or
Governor panel, and then switching back to Chat
**Symptom:** The response stream freezes and stops updating. The backend
is still generating, but the UI never receives the tokens.
**Severity:** Significant — data loss on panel switch

### Root Cause
`+page.svelte` used `{#if activePanel === 'chat'}` to conditionally render
the ChatPanel. In Svelte, `{#if}` completely destroys the component when
the condition is false. This triggered `onDestroy`, which unregistered
the `chat://token` Tauri event listeners. When the user switched back, a
brand new `ChatPanel` mounted and fetched from the DB, but since the
response was still in-flight and not fully committed to DB, the stream
was lost.

### Fix
Changed conditional rendering to CSS visibility. The `<ChatPanel />` is
now ALWAYS mounted, wrapped in a `<div class:hidden={activePanel !== 'chat'}>`.
This ensures event listeners stay active in the background, and Svelte
preserves the component's state (messages array, selected model, etc.).

### Prevention
In single-page desktop apps (Tauri/Electron), components that manage
long-running I/O (like websockets or streaming IPC events) should
generally remain mounted for the lifetime of the application to prevent
dropped events. Use CSS `display: none` to hide them instead of framework
unmounting logic.

**When:** Using any model not in the hardcoded name list (e.g. a custom
HuggingFace model, or Gemma 3 incorrectly listed as "thinking")
**Symptom:** HTTP 400 "does not support thinking" for non-thinking models
misclassified as thinking, or missing image button for unlisted vision
models
**Severity:** Blocker — wrong capability detection cascades into chat
failures

### Root Cause
`detect_capability_from_name()` used hardcoded substring matching against
a fixed list of model names. Any model not in the list was misclassified.
Gemma 3 was incorrectly included in the thinking list. Custom or
HuggingFace models with no name match defaulted to `TextOnly` regardless
of actual capabilities.

### Fix
Three-layer dynamic capability detection:
1. Query Ollama's `/api/show` for the `capabilities` array (authoritative,
   available since Ollama ~v0.5).
2. Fall back to template inspection (`{{ .Think }}`, `{{ .Images }}`).
3. Fall back to name heuristic (last resort for old Ollama).

Added `model_supports_thinking()` async method and a retry-on-400 safety
net in `chat_stream()`: if Ollama rejects `think: true`, automatically
retry without it. The user never sees an error.

Removed `gemma3`/`gemma-3` from the thinking model name list.

### Prevention
Never rely solely on model name heuristics for capability detection. Use
the provider's API to query capabilities dynamically. Keep name heuristics
only as a last-resort fallback. The retry safety net ensures even wrong
guesses self-correct at runtime.

---

## 2026-05-23 | Frontend | UI layout collapses when switching panels

**When:** Clicking Settings gear in sidebar, then clicking back to Chat
**Symptom:** Chat panel shrinks — titlebar and sidebar icons shift off the
top of the viewport, layout is broken until page reload
**Severity:** Significant — broken layout

### Root Cause
CSS flexbox default `min-height: auto` and `min-width: auto` on nested
flex containers (`.app-body`, `.main-panel`). When Svelte unmounts one
panel and mounts another (e.g. Settings → Chat), the flex height
calculations break because flex items with `overflow: visible` grow to
content size instead of respecting the parent's bounds. The
`.placeholder-panel` used `height: 100%` which doesn't participate in
flex sizing the same way as `flex: 1`.

### Fix
- `.app-body`: added `min-height: 0`
- `.main-panel`: added `min-width: 0` and `min-height: 0`
- `.placeholder-panel`: changed from `height: 100%` to `flex: 1;
  min-height: 0`
- `.app-root`: changed from `100vw/100vh` to `100%/100%` to avoid
  scrollbar-induced layout shifts
- `.chat-panel`: added `min-width: 0` to prevent horizontal blowout

### Prevention
In nested flex layouts, always set `min-height: 0` and `min-width: 0`
on flex children that contain scrollable or dynamically-sized content.
The browser default `auto` causes content to push the flex item beyond
its allocated space. Comment the constraint in CSS.

---

## 2026-05-22 | Audit | Pre-alpha audit fix sweep — 39 findings

**When:** Pre-alpha audit run before tagging v0.3.0
**Symptom:** Multiple — see the audit findings document for the full list
**Severity:** A:9, B:14, C:16

### Root Cause
Single audit pass against the entire codebase identified accumulated
issues across seven categories: build/runtime sanity, backend correctness,
frontend memory shape, documentation truth, repo hygiene, future-proofing,
and README quality. Listed and remediated in one fix sweep before
tagging the alpha.

### Fix
One commit applied 26 A-severity, 41 B-severity, 33 C-severity
remediations. Highlights: UTF-8 boundary fix in stream parsing,
file-rolling logger to `~/.heimdall/logs/`, atomic-failure placeholder
on stream errors, bundled fonts (no CDN call), conversation-switch lock
during stream, image size cap, blob URL revoke, IPC forward-compat for
Phase 4–6, mobile-platform icon cleanup, doc reconciliation across
agents.md / CONTEXT.md / spec, README rewrite. See `DEVLOG.md` for the
session record.

### Prevention
Run a full audit at every phase boundary, not just before alpha. Each
finding has a stable ID (P{pass}-{severity}{n}) for traceability.
Cleanup conventions added to `.gitignore` (`*_tmp.txt`, `audit_pass*.txt`)
to prevent debris recurrence.

---

## 2026-05-21 | Backend | Gemma 4 thinking not appearing in ThinkingBlock

**When:** Using Gemma 4 or any native-thinking model that depends on the
Ollama `think: true` request flag
**Symptom:** Gemma 4 silently discards reasoning tokens; no thinking
block renders in the UI even though the model produces reasoning
**Severity:** Blocker for thinking-block feature

### Root Cause
Ollama requires `"think": true` in the request body to enable the native
`message.thinking` streaming field for certain models (Gemma 4 in
particular). Without the flag explicitly set, Gemma silently drops
reasoning tokens. DeepSeek R1 happened to work because it falls back to
emitting legacy `<think>` tags inside `message.content`, which the tag
parser handled.

### Fix
- `models.rs::OllamaChatRequest` — added `think: Option<bool>` field.
- `ollama_client.rs::chat_stream` — sets `think: Some(true)` on every
  outgoing request.

### Prevention
Always explicitly request thinking tokens via the provider's documented
API parameter rather than relying on default behaviour. Different model
families default differently. Document required flags in the source
near the call site.

---

## 2026-05-21 | Backend | Native `message.thinking` field silently dropped by serde

**When:** Using `deepseek-r1:1.5b` or any thinking model in Heimdall
v0.3.0 pre-fix
**Symptom:** Model produces thinking output in `ollama run` but Heimdall
shows only the final answer — no thinking block, no reasoning visible
**Severity:** Blocker for thinking-block feature

### Root Cause
Ollama v0.9+ natively parses `<think>` tags on the server and streams
reasoning tokens in a dedicated `message.thinking` JSON field, separate
from `message.content`. Heimdall's `OllamaChatMessage` struct only
declared `role`, `content`, and `images`. Serde's `#[derive(Deserialize)]`
silently dropped the `thinking` field at deserialisation. The tag parser
then searched for `<think>` in `message.content` — but content was empty
during the thinking phase because Ollama had already extracted the tags
server-side.

### Fix
- `models.rs::OllamaChatMessage` — added
  `#[serde(default, skip_serializing_if = "Option::is_none")] thinking: Option<String>`.
- `ollama_client.rs::chat_stream` — rewrote the streaming loop with a
  hybrid: native `message.thinking` is the primary path; the existing
  `<think>` tag parser stays as fallback for older Ollama versions or
  non-Ollama endpoints.

### Prevention
When consuming a streaming API, log a sample raw chunk during development
and verify the actual JSON shape matches the struct definition. Pin and
document the Ollama version Heimdall is tested against.

---

## 2026-05-20 | Frontend/CSS | UI shrinks to top — input bar floats, empty state not centred

**When:** After a clean app restart following CSS overflow tweaks
**Symptom:** Chat panel content bunches into the top ~40% of the window
instead of filling the viewport
**Severity:** Significant — broken layout

### Root Cause
Conflict between `overflow: hidden` and `overflow: visible` in nested
flex layouts. `.main-panel` and `.chat-panel` both had
`overflow: hidden`, which is required for flex children to size
correctly (flex items with `overflow: visible` grow to content size
instead of filling available space). Changing one to `visible` (to
fix a clipped ModelSelector dropdown) broke the height calculation.

### Fix
- `.main-panel` (`+page.svelte`) — keep `overflow: hidden`; outer
  container needs to constrain the layout.
- `.chat-panel` (`ChatPanel.svelte`) — use `min-height: 0` instead of
  `overflow: hidden`. Same sizing behaviour without the clipping.
- `.model-bar` — `position: relative; z-index: 10` so the dropdown
  paints above the messages area.

### Prevention
In nested flex layouts, `overflow: hidden` does double duty: it clips
content (intended) and implicitly sets `min-height: 0` (side effect).
When you need the sizing without the clipping, use `min-height: 0`
explicitly. Comment the choice in the CSS.

---

## 2026-05-20 | Frontend | Vision model POST fails with `llava:7b` on CPU

**When:** Sending an image to `llava:7b` (4.6 GB model running on 100% CPU)
**Symptom:** Error banner "POST /api/chat failed"; model never responds
**Severity:** Blocker for vision feature

### Root Cause
Two compounding issues:

1. **Request payload too large.** The frontend was sending the full base64
   image inside *every* message in the conversation history. Vision
   models only need the image in the current (last) user message.
   Repeating ~500 KB of base64 across many messages bloated the request
   and could exceed context limits or cause OOM on CPU-only systems.
2. **Timeout too short.** The reqwest client had a 120-second timeout.
   Vision models on CPU (no GPU offload) can take 2–5 minutes for a
   single image, especially when RAM is tight and the system is
   swapping.

### Fix
- Frontend: only include `images` in the last user message; previous
  messages get `images: null` (and per `OllamaChatMessage`'s
  `skip_serializing_if`, this is omitted from the wire entirely).
- Backend: `OllamaClient::new` sets a 300-second timeout.
- UI: error message rewritten to "model may be loading or request timed
  out" instead of generic "POST failed".

### Prevention
- Never repeat large binary payloads across all messages in any chat
  history send. Comment the constraint at the build-message site.
- Vision models on CPU need generous timeouts (5+ minutes). Document
  that the 300-second ceiling is hardware-driven, not a network value.

---

## 2026-05-20 | Frontend | "Failed to resolve import @tauri-apps/plugin-dialog"

**When:** Running `cargo tauri dev` after wiring image input
**Symptom:** Vite import-analysis error — `@tauri-apps/plugin-dialog`
or `@tauri-apps/plugin-fs` cannot be found
**Severity:** Blocker — app won't load

### Root Cause
Tauri v2 plugins ship in two parts: a Rust crate (in `Cargo.toml`) and
an npm package (in `package.json`). The Rust crates were installed and
registered in `lib.rs`. The frontend npm bindings were not.

### Fix
```bash
npm install @tauri-apps/plugin-dialog @tauri-apps/plugin-fs
```

### Prevention
When adding any Tauri plugin to `Cargo.toml`, also add the matching npm
package. Verify both `node_modules/@tauri-apps/plugin-<name>` and
`Cargo.lock` references exist after installation.

---

## 2026-05-20 | Frontend | `<button>` cannot be a child of `<button>`

**When:** Opening the History tab
**Symptom:** Vite HMR red overlay: "node_invalid_placement" at
`ChatPanel.svelte:582`
**Severity:** Significant — breaks History tab rendering

### Root Cause
HTML spec forbids nesting interactive elements. A `<button>` (delete)
inside another `<button>` (the conversation row) caused the browser to
"repair" the DOM by moving the inner button outside, which broke
Svelte's reactivity assumptions.

### Fix
Outer `<button>` became `<div role="button" tabindex="0">` with explicit
`onclick` and `onkeydown` handlers. Inner delete `<button>` stays a
real button (it's now the only interactive child).

### Prevention
Never nest `<button>` inside `<button>`, `<a>` inside `<a>`, or any
interactive element inside another. For compound clickable rows that
contain other interactive elements, use `<div role="button">` with
keyboard handling. svelte-check warns; respect it.

---

## 2026-05-20 | Backend | "set DATABASE_URL to use query macros online" compile error

**When:** Running `cargo tauri dev` after modifying `lib.rs`
**Symptom:** Six compile errors in `db.rs` — every `sqlx::query_as!()`
macro call fails
**Severity:** Blocker — won't compile

### Root Cause
`sqlx::query_as!()` is a compile-time-checked macro. At build time it
connects to a real SQLite database to verify the SQL is valid and column
types match. This requires either a live database (via `DATABASE_URL`
env var) or a pre-generated offline cache (`.sqlx/` from
`cargo sqlx prepare`). Heimdall had neither. Earlier builds succeeded
only because Rust's incremental cache had the macro results from a prior
build with a valid DB; modifying `lib.rs` invalidated that cache.

### Fix
Replaced every `sqlx::query_as!()` (compile-time checked) with
`sqlx::query_as::<_, T>()` (runtime mapped via `FromRow`). All structs
already derived `sqlx::FromRow`, so the mapping is type-safe at runtime.

### Trade-off
- Lost: compile-time SQL verification (typos in column names won't be
  caught until runtime).
- Gained: no external tooling, no `.env` file, no `cargo sqlx prepare`
  step, simpler CI/CD.

### Prevention
**Never use `sqlx::query_as!()` in this project.** Always use
`sqlx::query_as::<_, T>()`. Captured in `docs/DECISIONS.md`. SQL
correctness is now verified by integration tests and the migration
system.

---

## 2026-05-19 | Frontend | "WebKit encountered an internal error" — onMount unhandled rejection crash

**When:** After adding `loadLastConversation()` to `ChatPanel.svelte`'s
`onMount`
**Symptom:** Blank white screen with only "WebKit encountered an internal
error"
**Severity:** Blocker — entire UI dead
**Note:** Distinct from the dev-mode port race below (same symptom,
different cause). See the index at the top of this file.

### Root Cause
An unhandled promise rejection inside `onMount` crashed the component
mount sequence. On Linux (WebKitGTK), an uncaught rejection during
component init causes the WebView to show its internal error page
instead of degrading gracefully.

### Fix
Wrapped the `onMount` body in a top-level `try/catch`. Even if any
init function throws, the component still renders and event listeners
still register. Each individual async init function additionally
catches its own errors for defence in depth.

### Prevention
Always wrap `onMount` bodies in `try/catch` in Tauri/WebKitGTK apps.
Each async init function should catch and handle its own errors
independently. Convention captured in `agents.md` Three Laws.

---

## 2026-05-19 | Frontend | "WebKit encountered an internal error" — dev mode port race

**When:** Running `cargo tauri dev` when Vite hasn't finished binding
its port yet
**Symptom:** Blank white screen with only "WebKit encountered an internal
error"
**Severity:** Blocker — entire UI dead
**Note:** Distinct from the onMount rejection crash above (same symptom,
different cause). See the index at the top of this file.

### Root Cause
In dev mode, `tauri.conf.json`'s `beforeDevCommand` starts Vite, but if
Vite hasn't bound `http://localhost:1420` by the time WebKit tries to
load, the WebView fails to initialise. Also occurs when `dist/` is stale
or `bootstrap()` panics (SQLite locked, malformed `config.toml`).

### Fix
- Use `cargo tauri dev` exclusively. It runs `beforeDevCommand` in the
  right order.
- Or run `npm run dev` in one terminal first, then `cargo tauri dev` in
  another.

### Prevention
Never start the Tauri binary manually in dev mode. If the error appears
after a clean `cargo tauri dev`, wait 3–5 seconds and reload. CI-style
release-build does not hit this race.
