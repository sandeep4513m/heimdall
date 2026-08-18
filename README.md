# Heimdall
Local-first AI gateway with persistent memory and RAG, built on Ollama. 
No cloud, no API keys, no data leaving your machine.

## Features
- Persistent memory across conversations (confirmed facts + episodic recall)
- RAG knowledge base — drag in files/folders, query against them
- Multi-model support via Ollama, switch models mid-session
- Resource-aware model governor — auto-unloads idle models based on system load
- Ships as .deb / .rpm

## Stack
Tauri 2 · Rust · SvelteKit / Svelte 5 · SQLite · usearch · Ollama

## Running locally
Requires Ollama running locally.

npm install
npm run tauri dev

## Status
v0.6.0 Beta 3 — actively developed, solo project.
