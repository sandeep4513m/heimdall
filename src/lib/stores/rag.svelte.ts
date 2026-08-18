import { invoke } from '@tauri-apps/api/core';
import type {
  Collection,
  CollectionStats,
  IngestionJob,
  IngestionProgressEvent,
  IngestionCompleteEvent,
} from '$lib/types/rag';

class RagStore {
  collections = $state<Collection[]>([]);
  selectedCollection = $state<Collection | null>(null);
  collectionStats = $state<CollectionStats | null>(null);
  ingestionJobs = $state<IngestionJob[]>([]);

  /// Per-collection job cache so ingestion progress survives the user
  /// navigating away from a collection mid-ingestion. Keyed by collection.id.
  /// On `selectCollection`, we restore from this cache and refresh from DB.
  private jobsByCollectionId = new Map<string, IngestionJob[]>();

  async loadCollections() {
    this.collections = await invoke<Collection[]>('rag_list_collections');
  }

  async selectCollection(collection: Collection | null) {
    this.selectedCollection = collection;
    this.collectionStats = null;

    if (collection) {
      // Restore cached jobs first so the UI never goes blank during the
      // round-trip; then refresh from DB.
      this.ingestionJobs = this.jobsByCollectionId.get(collection.id) ?? [];

      try {
        this.collectionStats = await invoke<CollectionStats>('rag_collection_stats', {
          name: collection.display_name,
        });
      } catch {
        this.collectionStats = null;
      }
      try {
        const jobs = await invoke<IngestionJob[]>('rag_list_ingestion_jobs', {
          collection: collection.display_name,
        });
        this.ingestionJobs = jobs;
        this.jobsByCollectionId.set(collection.id, jobs);
      } catch {
        // Keep cached jobs if the refresh failed.
      }
    } else {
      this.ingestionJobs = [];
    }
  }

  async createCollection(displayName: string) {
    const col = await invoke<Collection>('rag_create_collection', { name: displayName });
    await this.loadCollections();
    await this.selectCollection(col);
  }

  async renameCollection(oldDisplayName: string, newDisplayName: string) {
    const col = await invoke<Collection>('rag_rename_collection', {
      oldName: oldDisplayName,
      newName: newDisplayName,
    });
    await this.loadCollections();
    if (this.selectedCollection?.id === col.id) {
      await this.selectCollection(col);
    }
  }

  async deleteCollection(displayName: string) {
    await invoke('rag_delete_collection', { name: displayName });
    await this.loadCollections();
    if (this.selectedCollection?.display_name === displayName) {
      await this.selectCollection(null);
    }
  }

  /// Delete a single source from the currently selected collection.
  ///
  /// Calls the backend, then refreshes the job list and stats so the UI
  /// reflects the removal immediately. The per-collection cache is also
  /// updated so navigating away and back doesn't resurrect the deleted row.
  async deleteSource(sourcePath: string) {
    if (!this.selectedCollection) return;
    await invoke<number>('rag_delete_source', {
      collection: this.selectedCollection.display_name,
      sourcePath,
    });
    // Refresh jobs and stats for this collection.
    await this.reloadJobs();
    try {
      this.collectionStats = await invoke<CollectionStats>('rag_collection_stats', {
        name: this.selectedCollection.display_name,
      });
    } catch {
      /* non-fatal */
    }
  }

  async reloadJobs() {
    if (!this.selectedCollection) return;
    try {
      const jobs = await invoke<IngestionJob[]>('rag_list_ingestion_jobs', {
        collection: this.selectedCollection.display_name,
      });
      this.ingestionJobs = jobs;
      this.jobsByCollectionId.set(this.selectedCollection.id, jobs);
    } catch {
      // Keep stale state on failure.
    }
  }

  /// Find which collection a job belongs to, scanning both the live list
  /// and the cache. Used so we can update jobs the user isn't currently
  /// looking at without losing them.
  private findJobAcrossCaches(jobId: string): { collectionId: string; job: IngestionJob } | null {
    for (const [collectionId, jobs] of this.jobsByCollectionId.entries()) {
      const job = jobs.find((j) => j.id === jobId);
      if (job) return { collectionId, job };
    }
    const live = this.ingestionJobs.find((j) => j.id === jobId);
    if (live && this.selectedCollection) {
      return { collectionId: this.selectedCollection.id, job: live };
    }
    return null;
  }

  handleProgress(event: IngestionProgressEvent) {
    const found = this.findJobAcrossCaches(event.job_id);
    if (!found) return;
    found.job.chunks_done = event.chunks_done;
    found.job.chunks_total = event.chunks_total;
    found.job.status = event.status;

    // If this job belongs to the currently selected collection, update the
    // live array reference so reactivity fires.
    if (this.selectedCollection?.id === found.collectionId) {
      const idx = this.ingestionJobs.findIndex((j) => j.id === event.job_id);
      if (idx >= 0) {
        this.ingestionJobs[idx] = { ...found.job };
      }
    }
  }

  handleComplete(event: IngestionCompleteEvent) {
    const found = this.findJobAcrossCaches(event.job_id);
    if (!found) return;
    found.job.status = event.success ? 'done' : 'failed';
    found.job.error = event.error;
    found.job.completed_at = Date.now() / 1000;

    if (this.selectedCollection?.id === found.collectionId) {
      const idx = this.ingestionJobs.findIndex((j) => j.id === event.job_id);
      if (idx >= 0) {
        this.ingestionJobs[idx] = { ...found.job };
      }
      // Refresh stats so the chunk count updates.
      invoke<CollectionStats>('rag_collection_stats', {
        name: this.selectedCollection.display_name,
      })
        .then((stats) => (this.collectionStats = stats))
        .catch(() => {
          /* non-fatal */
        });
    }
  }
}

export const ragStore = new RagStore();
