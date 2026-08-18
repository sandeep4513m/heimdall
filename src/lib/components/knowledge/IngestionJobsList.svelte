<script lang="ts">
  import { ragStore } from '$lib/stores/rag.svelte';
  import { invoke } from '@tauri-apps/api/core';
  import Icon from '../icons/Icon.svelte';
  import { iconX } from '../icons/index';
  import type { IngestionJob } from '$lib/types/rag';

  // Tracks which job id is awaiting delete confirmation.
  let pendingDeleteJobId = $state<string | null>(null);
  let deleteError = $state<string | null>(null);

  async function cancelJob(job: IngestionJob) {
    try {
      await invoke('rag_cancel_ingestion', { jobId: job.id });
      job.status = 'cancelled';
    } catch (e) {
      console.error(e);
    }
  }

  async function resumeJob(job: IngestionJob) {
    try {
      await invoke('rag_resume_ingestion', { jobId: job.id });
      job.status = 'running';
    } catch (e) {
      console.error(e);
    }
  }

  function startDelete(jobId: string) {
    pendingDeleteJobId = jobId;
    deleteError = null;
  }

  function cancelDelete() {
    pendingDeleteJobId = null;
    deleteError = null;
  }

  async function confirmDelete(job: IngestionJob) {
    pendingDeleteJobId = null;
    deleteError = null;
    try {
      await ragStore.deleteSource(job.source_path ?? '');
    } catch (e) {
      deleteError = typeof e === 'string' ? e : 'Failed to delete source.';
    }
  }

  /// Returns true if this job's source can be individually deleted.
  /// Multi-file jobs store a label like "3 files: a.pdf, …" — the backend
  /// can't match that against rag_chunks.source_path, so deletion is blocked.
  function isDeletable(job: IngestionJob): boolean {
    if (!job.source_path) return false;
    return !job.source_path.includes(' files:');
  }

  function formatTime(timestamp: number) {
    return new Date(timestamp * 1000).toLocaleString();
  }

  // Terminal states where delete is offered.
  const TERMINAL = new Set(['done', 'failed', 'cancelled', 'interrupted']);
</script>

<div class="jobs-list">
  <h3>Recent Ingestions</h3>

  {#if deleteError}
    <div class="delete-error">{deleteError}</div>
  {/if}

  {#if ragStore.ingestionJobs.length === 0}
    <p class="empty">No files have been added to this collection yet.</p>
  {:else}
    {#each ragStore.ingestionJobs as job (job.id)}
      <div class="job-item" class:confirming={pendingDeleteJobId === job.id}>
        {#if pendingDeleteJobId === job.id}
          <!-- Inline delete confirmation row -->
          <div class="confirm-row">
            <span class="confirm-text">Remove this source and its chunks?</span>
            <div class="confirm-actions">
              <button class="action-btn destructive" onclick={() => confirmDelete(job)}>Remove</button>
              <button class="action-btn" onclick={cancelDelete}>
                <Icon paths={iconX} size={12} stroke={2} />
              </button>
            </div>
          </div>
        {:else}
          <div class="job-main">
            <span class="source" title={job.source_path || 'Unknown'}>{job.source_path || 'Unknown'}</span>
            <div class="job-right">
              <span class="status {job.status}">{job.status}</span>
              {#if TERMINAL.has(job.status ?? '')}
                {#if isDeletable(job)}
                  <button
                    class="delete-btn"
                    onclick={() => startDelete(job.id)}
                    title="Remove this source from the collection"
                    aria-label="Remove source"
                  >
                    <Icon paths={iconX} size={12} stroke={2} />
                  </button>
                {:else}
                  <button
                    class="delete-btn disabled"
                    disabled
                    title="Multi-file jobs can't be individually deleted yet. Delete the collection and re-ingest."
                    aria-label="Cannot remove multi-file source"
                  >
                    <Icon paths={iconX} size={12} stroke={2} />
                  </button>
                {/if}
              {/if}
            </div>
          </div>

          {#if job.status === 'running' || job.status === 'pending'}
            <div class="progress-bar">
              <div class="fill" style:width="{job.chunks_total > 0 ? (job.chunks_done / job.chunks_total) * 100 : 0}%"></div>
            </div>
            <div class="job-meta">
              <span>{job.chunks_done} / {job.chunks_total} chunks</span>
              {#if job.status === 'running'}
                <button class="action-btn" onclick={() => cancelJob(job)}>Cancel</button>
              {/if}
            </div>
          {:else}
            <div class="job-meta">
              <span>{job.chunks_done} chunks embedded</span>
              <span>{formatTime(job.created_at)}</span>
              {#if job.status === 'paused_low_memory' || job.status === 'interrupted' || job.status === 'failed'}
                <button class="action-btn" onclick={() => resumeJob(job)}>Resume</button>
              {/if}
            </div>
            {#if job.error}
              <div class="error-text">{job.error}</div>
            {/if}
          {/if}
        {/if}
      </div>
    {/each}
  {/if}
</div>

<style>
  .jobs-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
    margin-top: var(--space-xl);
  }
  h3 {
    font-family: var(--font-brand);
    font-size: 14px;
    margin: 0;
    color: var(--text-base);
  }
  .empty {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-ghost);
  }
  .delete-error {
    padding: var(--space-sm) var(--space-md);
    border: 0.5px solid var(--accent-red);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    color: var(--accent-red);
    font-family: var(--font-ui);
    font-size: 11px;
  }
  .job-item {
    background: var(--bg-surface);
    border: 0.5px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: var(--space-md);
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }
  .job-item.confirming {
    border-color: var(--accent-red);
  }
  .job-main {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-sm);
  }
  .job-right {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    flex-shrink: 0;
  }
  .source {
    font-family: var(--font-ui);
    font-size: 13px;
    color: var(--text-base);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
    min-width: 0;
  }
  .status {
    font-family: var(--font-ui);
    font-size: 11px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    text-transform: uppercase;
    font-weight: 600;
    white-space: nowrap;
  }
  .status.running { background: var(--bg-elevated); color: var(--gold-primary); }
  .status.done { background: var(--bg-elevated); color: var(--accent-green); }
  .status.failed { background: var(--bg-elevated); color: var(--accent-red); }
  .status.pending,
  .status.cancelled,
  .status.interrupted,
  .status.paused_low_memory { background: var(--bg-elevated); color: var(--text-ghost); }

  .delete-btn {
    background: transparent;
    border: none;
    color: var(--text-ghost);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 2px;
    border-radius: var(--radius-sm);
    opacity: 0;
    transition: opacity 0.15s, color 0.15s;
  }
  .job-item:hover .delete-btn {
    opacity: 1;
  }
  .delete-btn:hover {
    color: var(--accent-red);
  }
  .delete-btn.disabled {
    cursor: not-allowed;
    opacity: 0.3;
  }
  .delete-btn.disabled:hover {
    color: var(--text-ghost);
  }

  /* Inline confirmation */
  .confirm-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-sm);
  }
  .confirm-text {
    font-family: var(--font-ui);
    font-size: 12px;
    color: var(--text-base);
    flex: 1;
    min-width: 0;
  }
  .confirm-actions {
    display: flex;
    gap: var(--space-xs);
    flex-shrink: 0;
  }

  .progress-bar {
    height: 4px;
    background: var(--bg-elevated);
    border-radius: 2px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--gold-primary);
    transition: width 0.2s;
  }
  .job-meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--text-ghost);
  }
  .action-btn {
    background: transparent;
    border: 0.5px solid var(--text-ghost);
    color: var(--text-ghost);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    cursor: pointer;
    font-size: 11px;
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .action-btn:hover {
    color: var(--text-base);
    border-color: var(--text-base);
  }
  .action-btn.destructive {
    color: var(--accent-red);
    border-color: var(--accent-red);
  }
  .action-btn.destructive:hover {
    background: var(--accent-red);
    color: var(--bg-app);
  }
  .error-text {
    font-family: var(--font-ui);
    font-size: 11px;
    color: var(--accent-red);
  }
</style>
