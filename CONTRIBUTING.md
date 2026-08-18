# Contributing to Heimdall
=========================

Heimdall is in alpha. The architecture is intentional and documented.
Read before you write.


Before opening a PR
-------------------

  * _design/HEIMDALL_ARCHITECTURE.md — the full spec. If your change
    contradicts it, open an issue first.
  * CONTEXT.md — current phase and what is in scope right now.
  * docs/ERRORS.md — bugs already fixed. Do not reintroduce them.
  * docs/DECISIONS.md — why things are the way they are.


The Three Laws
--------------

These are not suggestions.

1. Zero hardcoded hex values.
   All colours come from CSS custom properties in src/app.css.
   That file is the single source of truth. Add a token there if you
   need a new colour. Never write #rrggbb or rgba() in a component file.

2. One thing per PR.
   One component, one module, or one bug fix. A new Tauri command and
   the frontend call that uses it is one feature — that is fine.
   Combining unrelated backend and frontend changes is not.

3. Every PR updates the docs.
   Add a feature → update CONTEXT.md.
   Fix a non-trivial bug → add it to docs/ERRORS.md.
   Make a design decision → add it to docs/DECISIONS.md.


Code style
----------

Rust:
  * cargo fmt before committing.
  * cargo clippy -- -D warnings must pass clean on Ubuntu.
  * Fedora: clippy may report E0514 against tauri_build due to a
    Fedora-side packaging quirk. Workaround: cargo clean, then rebuild.
  * All public functions get a doc comment (///).
  * Errors propagate via anyhow::Result. No .unwrap() in production paths.
  * No println!. Use tracing::info!, tracing::warn!, tracing::error!.

Svelte / TypeScript:
  * No hardcoded hex or rgba literals. CSS variables only.
  * No console.log. console.error is acceptable in catch blocks.
  * Svelte 5 runes only — $state, $derived, $props. No legacy stores.
  * All interactive elements need aria-label if they have no visible text.

SQL:
  * Use sqlx::query_as::<_, T>(). Never sqlx::query_as!() — it requires
    DATABASE_URL at compile time and we do not use that.
  * New tables go in db.rs::run_migrations(). Idempotent, IF NOT EXISTS.


Running locally
---------------

    npm install
    cargo tauri dev       # dev mode, hot reload
    cargo tauri build     # production build

DATABASE_URL is not required to compile. See docs/DECISIONS.md for why.


Filing issues
-------------

Bug reports need: OS and version, Ollama version, model name, exact error
or screenshot, and steps to reproduce.

Feature requests: check CONTEXT.md first. If it is already planned for a
phase, comment on the roadmap instead of opening a new issue.

Security issues: use GitHub's private vulnerability reporting. Not a
public issue.


Bumping the version
-------------------

Four files must stay in sync. Do all four in the same commit:

  1. package.json → version
  2. package-lock.json → run npm install to sync
  3. src-tauri/Cargo.toml → version
  4. src-tauri/tauri.conf.json → version

Then tag:

    git tag -a vX.Y.Z -m "vX.Y.Z"
    git push origin vX.Y.Z


What is out of scope for Alpha
------------------------------

PRs for the following will be closed until the relevant phase opens:

  * RAG / knowledge base (Phase 4)
  * Memory extraction (Phase 5)
  * Resource governor (Phase 6)
  * Shortcut remapping UI (Phase 7)
  * Windows or macOS support


AI agents
---------

If you are an LLM or AI coding assistant, read agents.md before doing
anything else. It contains the reading order, the Three Laws, known
gotchas, and build instructions. Skipping it will cause you to break
things that have already been fixed.
