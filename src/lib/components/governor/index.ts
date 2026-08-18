// src/lib/components/governor/index.ts
//
// Re-exports for the Phase 6 Governor component cluster. Components
// are mounted directly from `+page.svelte` (Sidebar wiring lands in
// Batch 5); these re-exports also let downstream consumers (tests,
// Storybook, future panels) import from a single path.

export { default as GovernorPanel } from './GovernorPanel.svelte';
export { default as ResourceCard } from './ResourceCard.svelte';
export { default as VramCard } from './VramCard.svelte';
export { default as ModelList } from './ModelList.svelte';
export { default as UnloadConfirmModal } from './UnloadConfirmModal.svelte';
export { default as ThresholdControls } from './ThresholdControls.svelte';
