/// db.rs — SQLite database layer for Heimdall
///
/// Manages the SQLite database at ~/.heimdall/db/heimdall.db.
/// Handles schema creation, migrations, and all CRUD operations
/// for conversations, messages, memory facts, RAG chunks, and ingestion jobs.
///
/// Uses sqlx with async/await. All queries are compile-time checked.
/// Connection pool is shared across the application via AppState.

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{sqlite::{SqliteConnectOptions, SqlitePoolOptions}, SqlitePool};
use std::path::Path;
use std::str::FromStr;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::models::{Conversation, IngestionJob, MemoryEpisode, MemoryFact, Message, RagChunk};

// ---------------------------------------------------------------------------
// Pool initialisation
// ---------------------------------------------------------------------------

/// Initialise the SQLite connection pool and run all migrations.
///
/// Creates the database file and parent directories if they do not exist.
/// Returns a connection pool ready for use.
#[instrument(skip_all)]
pub async fn init_pool(db_path: &Path) -> Result<SqlitePool> {
    // Ensure the parent directory exists
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create db directory: {}", parent.display()))?;
    }

    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

    let connect_options = SqliteConnectOptions::from_str(&db_url)
        .with_context(|| format!("Invalid SQLite URL: {}", db_url))?
        .pragma("foreign_keys", "ON")
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "NORMAL")
        .pragma("busy_timeout", "5000")
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(connect_options)
        .await
        .with_context(|| format!("Failed to open SQLite database at {}", db_path.display()))?;

    run_migrations(&pool).await?;

    info!("Database initialised at {}", db_path.display());
    Ok(pool)
}

// ---------------------------------------------------------------------------
// Schema migrations
// ---------------------------------------------------------------------------

/// Apply all schema migrations in order.
///
/// Each migration is idempotent — safe to run on an existing database.
/// New tables and columns are added here as the schema evolves.
///
/// Marked `pub` so integration tests (notably the property test for
/// schema-migration idempotence in `tests/property_p4_migration_idempotence.rs`)
/// can drive the migration directly against a controlled pool without
/// going through `init_pool` and creating a fresh connection each call.
pub async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    // Conversations table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS conversations (
            id          TEXT PRIMARY KEY,
            title       TEXT,
            model       TEXT,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create conversations table")?;

    // Messages table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS messages (
            id                  TEXT PRIMARY KEY,
            conversation_id     TEXT,
            role                TEXT NOT NULL CHECK(role IN ('user','assistant','system')),
            content             TEXT,
            input_type          TEXT,
            tokens_used         INTEGER,
            created_at          INTEGER NOT NULL,
            FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create messages table")?;

    // Index for fast conversation message retrieval
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_messages_conversation_id
         ON messages(conversation_id, created_at);",
    )
    .execute(pool)
    .await
    .context("Failed to create messages index")?;

    // Migration: add images column to messages (stores JSON array of base64 strings)
    sqlx::query(
        "ALTER TABLE messages ADD COLUMN images TEXT;"
    )
    .execute(pool)
    .await
    .ok(); // Ignore error if column already exists

    // Migration: add thinking column to messages (stores <think> block content)
    sqlx::query(
        "ALTER TABLE messages ADD COLUMN thinking TEXT;"
    )
    .execute(pool)
    .await
    .ok(); // Ignore error if column already exists

    // Memory facts table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS memory_facts (
            id                      TEXT PRIMARY KEY,
            fact                    TEXT NOT NULL,
            source_conversation_id  TEXT,
            confirmed_by_user       INTEGER NOT NULL DEFAULT 0,
            created_at              INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create memory_facts table")?;

    // RAG chunks table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS rag_chunks (
            id          TEXT PRIMARY KEY,
            collection  TEXT NOT NULL,
            source_path TEXT NOT NULL,
            chunk_index INTEGER NOT NULL,
            content     TEXT NOT NULL,
            token_count INTEGER NOT NULL DEFAULT 0,
            vector_id   INTEGER,
            created_at  INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create rag_chunks table")?;

    // Index for collection-scoped chunk lookups
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_rag_chunks_collection
         ON rag_chunks(collection);",
    )
    .execute(pool)
    .await
    .context("Failed to create rag_chunks index")?;

    // Ingestion jobs table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ingestion_jobs (
            id              TEXT PRIMARY KEY,
            source_path     TEXT,
            collection      TEXT,
            status          TEXT,
            chunks_total    INTEGER NOT NULL DEFAULT 0,
            chunks_done     INTEGER NOT NULL DEFAULT 0,
            error           TEXT,
            created_at      INTEGER NOT NULL,
            completed_at    INTEGER
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create ingestion_jobs table")?;

    // Model capabilities table — authoritative cache for what each locally
    // available Ollama model can do. One row per model_name; the digest
    // column drives invalidation when the user re-pulls the model.
    // Capability flags are INTEGER 0/1 (mapped to bool in Rust). The
    // capability_source column records which detection layer produced the
    // row (api_show | template | heuristic | user_override) so the future
    // Models tab can show provenance. raw_capabilities holds the verbatim
    // JSON array from /api/show.capabilities for forward compatibility
    // with new capability vocabulary terms.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS model_capabilities (
            model_name          TEXT PRIMARY KEY,
            digest              TEXT NOT NULL,
            completion          INTEGER NOT NULL DEFAULT 1,
            vision              INTEGER NOT NULL DEFAULT 0,
            thinking            INTEGER NOT NULL DEFAULT 0,
            tools               INTEGER NOT NULL DEFAULT 0,
            embedding           INTEGER NOT NULL DEFAULT 0,
            capability_source   TEXT NOT NULL DEFAULT 'heuristic',
            raw_capabilities    TEXT,
            family              TEXT,
            parameter_size      TEXT,
            quantization_level  TEXT,
            detected_at         INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create model_capabilities table")?;

    // Index for fast digest comparison during list_with_capabilities()
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_model_caps_digest
         ON model_capabilities(model_name, digest);",
    )
    .execute(pool)
    .await
    .context("Failed to create model_capabilities index")?;

    // Model settings table — per-model override values for chat options.
    // Empty in this release; the future Models tab populates it. All
    // override columns are nullable so a partial override (e.g., only
    // temperature) is valid; chat reads this as an override layer above
    // OllamaOptions defaults.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS model_settings (
            model_name          TEXT PRIMARY KEY,
            temperature         REAL,
            num_ctx             INTEGER,
            top_p               REAL,
            top_k               INTEGER,
            system_prompt       TEXT,
            default_keep_alive  TEXT,
            updated_at          INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create model_settings table")?;

    // Collections table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS collections (
            id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            last_ingested_at INTEGER
        );"
    )
    .execute(pool)
    .await
    .context("Failed to create collections table")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_collections_display_name ON collections(display_name);"
    )
    .execute(pool)
    .await
    .context("Failed to create collections index")?;

    // Migration: add active_rag_collections column to conversations (stores JSON array of collection ids)
    sqlx::query(
        "ALTER TABLE conversations ADD COLUMN active_rag_collections TEXT;"
    )
    .execute(pool)
    .await
    .ok(); // Ignore error if column already exists

    // -------------------------------------------------------------------------
    // Phase 5: Memory System migrations
    // -------------------------------------------------------------------------

    // Memory episodes table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS memory_episodes (
            id                      TEXT PRIMARY KEY,
            summary                 TEXT NOT NULL,
            source_conversation_id  TEXT,
            vector_id               INTEGER,
            created_at              INTEGER NOT NULL,
            decayed                 INTEGER NOT NULL DEFAULT 0,
            restored                INTEGER NOT NULL DEFAULT 0
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create memory_episodes table")?;

    // Memory settings table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS memory_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .execute(pool)
    .await
    .context("Failed to create memory_settings table")?;

    // Insert default settings (idempotent)
    sqlx::query(
        "INSERT OR IGNORE INTO memory_settings (key, value) VALUES ('global_enabled', 'true');"
    )
    .execute(pool)
    .await
    .context("Failed to insert default memory setting: global_enabled")?;

    sqlx::query(
        "INSERT OR IGNORE INTO memory_settings (key, value) VALUES ('decay_threshold_days', '90');"
    )
    .execute(pool)
    .await
    .context("Failed to insert default memory setting: decay_threshold_days")?;

    // Migration: add dedup_status column to memory_facts
    sqlx::query("ALTER TABLE memory_facts ADD COLUMN dedup_status TEXT DEFAULT 'new';")
        .execute(pool)
        .await
        .ok(); // Ignore error if column already exists

    // Migration: add conflict_with_id column to memory_facts
    sqlx::query("ALTER TABLE memory_facts ADD COLUMN conflict_with_id TEXT;")
        .execute(pool)
        .await
        .ok(); // Ignore error if column already exists

    // Migration: add update_hint_id column to memory_facts
    sqlx::query("ALTER TABLE memory_facts ADD COLUMN update_hint_id TEXT;")
        .execute(pool)
        .await
        .ok(); // Ignore error if column already exists

    // Migration: add batch_id column to memory_facts
    sqlx::query("ALTER TABLE memory_facts ADD COLUMN batch_id TEXT;")
        .execute(pool)
        .await
        .ok(); // Ignore error if column already exists

    // Migration: add memory_enabled column to conversations
    sqlx::query("ALTER TABLE conversations ADD COLUMN memory_enabled INTEGER DEFAULT 1;")
        .execute(pool)
        .await
        .ok(); // Ignore error if column already exists

    // Index for episode retrieval by age
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_memory_episodes_created_at ON memory_episodes(created_at);"
    )
    .execute(pool)
    .await
    .context("Failed to create memory_episodes index")?;

    info!("Schema migrations complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Conversation operations
// ---------------------------------------------------------------------------

/// Insert a new conversation record and return it.
pub async fn create_conversation(pool: &SqlitePool, model: &str) -> Result<Conversation> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO conversations (id, title, model, created_at, updated_at)
         VALUES (?, 'New Chat', ?, ?, ?);",
    )
    .bind(&id)
    .bind(model)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .context("Failed to insert conversation")?;

    Ok(Conversation {
        id,
        title: Some("New Chat".to_string()),
        model: Some(model.to_string()),
        created_at: now,
        updated_at: now,
    })
}

/// Insert a conversation with a caller-provided id.
///
/// Used by `chat_stream`'s FK guard: when the frontend sends a
/// conversation_id that has no matching row, we create the row here with
/// that exact id rather than generating a new one. This keeps the
/// conversation_id stable across the failure-recovery boundary.
pub async fn create_conversation_with_id(
    pool: &SqlitePool,
    id: &str,
    model: &str,
) -> Result<Conversation> {
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO conversations (id, title, model, created_at, updated_at)
         VALUES (?, 'New Chat', ?, ?, ?);",
    )
    .bind(id)
    .bind(model)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .context("Failed to insert conversation with id")?;

    Ok(Conversation {
        id: id.to_string(),
        title: Some("New Chat".to_string()),
        model: Some(model.to_string()),
        created_at: now,
        updated_at: now,
    })
}

/// Fetch all conversations ordered by most recently updated.
pub async fn list_conversations(pool: &SqlitePool) -> Result<Vec<Conversation>> {
    let rows = sqlx::query_as::<_, Conversation>(
        "SELECT id, title, model, created_at, updated_at
         FROM conversations
         ORDER BY updated_at DESC;"
    )
    .fetch_all(pool)
    .await
    .context("Failed to list conversations")?;

    Ok(rows)
}

/// Update the title of a conversation.
pub async fn update_conversation_title(
    pool: &SqlitePool,
    id: &str,
    title: &str,
) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "UPDATE conversations SET title = ?, updated_at = ? WHERE id = ?;",
    )
    .bind(title)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await
    .context("Failed to update conversation title")?;

    Ok(())
}

/// Return whether a conversation row exists for the given id.
///
/// Used by `chat_stream` as an FK guard before inserting messages — if the
/// frontend has somehow lost track of the row (race after delete, app
/// restart with a stale conversation_id), we create the row on the fly
/// rather than failing with a foreign key violation.
pub async fn conversation_exists(pool: &SqlitePool, id: &str) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM conversations WHERE id = ? LIMIT 1;"
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("Failed to check conversation existence")?;

    Ok(row.is_some())
}

/// Touch the updated_at timestamp on a conversation (called after each message).
pub async fn touch_conversation(pool: &SqlitePool, id: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query("UPDATE conversations SET updated_at = ? WHERE id = ?;")
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to touch conversation")?;

    Ok(())
}

/// Delete a conversation and all its messages (CASCADE handles messages).
pub async fn delete_conversation(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM conversations WHERE id = ?;")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete conversation")?;

    Ok(())
}

/// Return the model name stored on a conversation row.
/// Returns None if the conversation does not exist or has no model set.
pub async fn get_conversation_model(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT model FROM conversations WHERE id = ? LIMIT 1;")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("Failed to fetch conversation model")?;
    Ok(row.and_then(|(m,)| m))
}

// ---------------------------------------------------------------------------
// Message operations
// ---------------------------------------------------------------------------

/// Insert a message into a conversation.
///
/// Also touches the parent conversation's updated_at timestamp.
pub async fn insert_message(
    pool: &SqlitePool,
    conversation_id: &str,
    role: &str,
    content: &str,
    input_type: &str,
    tokens_used: Option<i64>,
    images: Option<&str>,
    thinking: Option<&str>,
) -> Result<Message> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO messages
            (id, conversation_id, role, content, input_type, tokens_used, images, thinking, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?);",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(role)
    .bind(content)
    .bind(input_type)
    .bind(tokens_used)
    .bind(images)
    .bind(thinking)
    .bind(now)
    .execute(pool)
    .await
    .context("Failed to insert message")?;

    touch_conversation(pool, conversation_id).await?;

    Ok(Message {
        id,
        conversation_id: Some(conversation_id.to_string()),
        role: role.to_string(),
        content: Some(content.to_string()),
        input_type: Some(input_type.to_string()),
        tokens_used,
        images: images.map(|s| s.to_string()),
        thinking: thinking.map(|s| s.to_string()),
        created_at: now,
    })
}

/// Fetch all messages for a conversation in chronological order.
pub async fn get_messages(pool: &SqlitePool, conversation_id: &str) -> Result<Vec<Message>> {
    let rows = sqlx::query_as::<_, Message>(
        "SELECT id, conversation_id, role, content, input_type, tokens_used, images, thinking, created_at
         FROM messages
         WHERE conversation_id = ?
         ORDER BY created_at ASC;"
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await
    .context("Failed to fetch messages")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Memory fact operations
// ---------------------------------------------------------------------------

/// Insert a new memory fact (unconfirmed by default).
pub async fn insert_memory_fact(
    pool: &SqlitePool,
    fact: &str,
    source_conversation_id: Option<&str>,
    dedup_status: Option<&str>,
    conflict_with_id: Option<&str>,
    update_hint_id: Option<&str>,
    batch_id: Option<&str>,
) -> Result<MemoryFact> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO memory_facts (id, fact, source_conversation_id, confirmed_by_user, created_at, dedup_status, conflict_with_id, update_hint_id, batch_id)
         VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?);",
    )
    .bind(&id)
    .bind(fact)
    .bind(source_conversation_id)
    .bind(now)
    .bind(dedup_status.unwrap_or("new"))
    .bind(conflict_with_id)
    .bind(update_hint_id)
    .bind(batch_id)
    .execute(pool)
    .await
    .context("Failed to insert memory fact")?;

    Ok(MemoryFact {
        id,
        fact: fact.to_string(),
        source_conversation_id: source_conversation_id.map(str::to_string),
        confirmed_by_user: false,
        created_at: now,
        dedup_status: Some(dedup_status.unwrap_or("new").to_string()),
        conflict_with_id: conflict_with_id.map(str::to_string),
        update_hint_id: update_hint_id.map(str::to_string),
        batch_id: batch_id.map(str::to_string),
    })
}

/// Confirm a memory fact (user approved it).
pub async fn confirm_memory_fact(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("UPDATE memory_facts SET confirmed_by_user = 1 WHERE id = ?;")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to confirm memory fact")?;

    Ok(())
}

/// Delete a memory fact.
pub async fn delete_memory_fact(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM memory_facts WHERE id = ?;")
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to delete memory fact")?;

    Ok(())
}

/// Fetch all confirmed memory facts (used for context injection).
pub async fn get_confirmed_memory_facts(pool: &SqlitePool) -> Result<Vec<MemoryFact>> {
    let rows = sqlx::query_as::<_, MemoryFact>(
        "SELECT id, fact, source_conversation_id,
                confirmed_by_user,
                created_at,
                dedup_status, conflict_with_id, update_hint_id, batch_id
         FROM memory_facts
         WHERE confirmed_by_user = 1
         ORDER BY created_at ASC;"
    )
    .fetch_all(pool)
    .await
    .context("Failed to fetch confirmed memory facts")?;

    Ok(rows)
}

/// Fetch all memory facts (confirmed and unconfirmed) for the Memory panel.
pub async fn list_all_memory_facts(pool: &SqlitePool) -> Result<Vec<MemoryFact>> {
    let rows = sqlx::query_as::<_, MemoryFact>(
        "SELECT id, fact, source_conversation_id,
                confirmed_by_user,
                created_at,
                dedup_status, conflict_with_id, update_hint_id, batch_id
         FROM memory_facts
         ORDER BY created_at DESC;"
    )
    .fetch_all(pool)
    .await
    .context("Failed to list memory facts")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// RAG chunk operations
// ---------------------------------------------------------------------------

/// Insert a RAG chunk record after embedding.
pub async fn insert_rag_chunk(
    pool: &SqlitePool,
    collection: &str,
    source_path: &str,
    chunk_index: i64,
    content: &str,
    token_count: i64,
    vector_id: Option<i64>,
) -> Result<RagChunk> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO rag_chunks
            (id, collection, source_path, chunk_index, content, token_count, vector_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?);",
    )
    .bind(&id)
    .bind(collection)
    .bind(source_path)
    .bind(chunk_index)
    .bind(content)
    .bind(token_count)
    .bind(vector_id)
    .bind(now)
    .execute(pool)
    .await
    .context("Failed to insert RAG chunk")?;

    Ok(RagChunk {
        id,
        collection: collection.to_string(),
        source_path: source_path.to_string(),
        chunk_index,
        content: content.to_string(),
        token_count,
        vector_id,
        created_at: now,
    })
}

/// Fetch RAG chunks by their IDs (used after vector similarity search).
pub async fn get_rag_chunks_by_ids(
    pool: &SqlitePool,
    ids: &[String],
) -> Result<Vec<RagChunk>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }

    // Build a parameterised IN clause
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let query_str = format!(
        "SELECT id, collection, source_path, chunk_index, content, token_count, vector_id, created_at
         FROM rag_chunks
         WHERE id IN ({});",
        placeholders
    );

    let mut query = sqlx::query_as::<_, RagChunk>(&query_str);
    for id in ids {
        query = query.bind(id);
    }

    let rows = query
        .fetch_all(pool)
        .await
        .context("Failed to fetch RAG chunks by IDs")?;

    Ok(rows)
}

/// Fetch RAG chunks by their vector_ids (usearch integer keys).
///
/// Used by the retrieval pipeline: vector search returns (u64 key, score)
/// pairs; this query maps those keys back to chunk rows in a given collection.
pub async fn get_rag_chunks_by_vector_ids(
    pool: &SqlitePool,
    vector_ids: &[i64],
    collection: &str,
) -> Result<Vec<RagChunk>> {
    if vector_ids.is_empty() {
        return Ok(vec![]);
    }

    // Build a parameterised IN clause.
    let placeholders = vector_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let query_str = format!(
        "SELECT id, collection, source_path, chunk_index, content, token_count, vector_id, created_at
         FROM rag_chunks
         WHERE collection = ? AND vector_id IN ({});",
        placeholders
    );

    let mut query = sqlx::query_as::<_, RagChunk>(&query_str).bind(collection);
    for vid in vector_ids {
        query = query.bind(vid);
    }

    let rows = query
        .fetch_all(pool)
        .await
        .context("Failed to fetch RAG chunks by vector IDs")?;

    Ok(rows)
}

/// Delete all RAG chunks for a given collection (used when re-ingesting).
pub async fn delete_rag_chunks_for_collection(
    pool: &SqlitePool,
    collection: &str,
) -> Result<u64> {
    let result = sqlx::query("DELETE FROM rag_chunks WHERE collection = ?;")
        .bind(collection)
        .execute(pool)
        .await
        .context("Failed to delete RAG chunks")?;

    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Ingestion job operations
// ---------------------------------------------------------------------------

/// Create a new ingestion job in 'pending' state.
pub async fn create_ingestion_job(
    pool: &SqlitePool,
    source_path: &str,
    collection: &str,
) -> Result<IngestionJob> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO ingestion_jobs
            (id, source_path, collection, status, chunks_total, chunks_done, created_at)
         VALUES (?, ?, ?, 'pending', 0, 0, ?);",
    )
    .bind(&id)
    .bind(source_path)
    .bind(collection)
    .bind(now)
    .execute(pool)
    .await
    .context("Failed to create ingestion job")?;

    Ok(IngestionJob {
        id,
        source_path: Some(source_path.to_string()),
        collection: Some(collection.to_string()),
        status: Some("pending".to_string()),
        chunks_total: 0,
        chunks_done: 0,
        error: None,
        created_at: now,
        completed_at: None,
    })
}

/// Update ingestion job progress.
pub async fn update_ingestion_progress(
    pool: &SqlitePool,
    id: &str,
    chunks_total: i64,
    chunks_done: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE ingestion_jobs
         SET chunks_total = ?, chunks_done = ?, status = 'running'
         WHERE id = ?;",
    )
    .bind(chunks_total)
    .bind(chunks_done)
    .bind(id)
    .execute(pool)
    .await
    .context("Failed to update ingestion progress")?;

    Ok(())
}

/// Mark an ingestion job as done. Also stamps `last_ingested_at` on the
/// owning collection so the Knowledge UI reflects "last ingested" times.
pub async fn complete_ingestion_job(pool: &SqlitePool, id: &str) -> Result<()> {
    let now = Utc::now().timestamp();

    // Read the job's collection so we can stamp the collection row too.
    let collection: Option<String> = sqlx::query_scalar(
        "SELECT collection FROM ingestion_jobs WHERE id = ?;"
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("Failed to read job collection")?;

    sqlx::query(
        "UPDATE ingestion_jobs
         SET status = 'done', completed_at = ?
         WHERE id = ?;",
    )
    .bind(now)
    .bind(id)
    .execute(pool)
    .await
    .context("Failed to complete ingestion job")?;

    if let Some(coll_id) = collection {
        // Best-effort — a missing collection (race after delete) is not fatal.
        let _ = sqlx::query(
            "UPDATE collections SET last_ingested_at = ?, updated_at = ? WHERE id = ?;",
        )
        .bind(now)
        .bind(now)
        .bind(&coll_id)
        .execute(pool)
        .await;
    }

    Ok(())
}

/// Mark an ingestion job as failed with an error message.
pub async fn fail_ingestion_job(pool: &SqlitePool, id: &str, error: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "UPDATE ingestion_jobs
         SET status = 'failed', error = ?, completed_at = ?
         WHERE id = ?;",
    )
    .bind(error)
    .bind(now)
    .bind(id)
    .execute(pool)
    .await
    .context("Failed to mark ingestion job as failed")?;

    Ok(())
}

/// Fetch all ingestion jobs ordered by most recent first.
pub async fn list_ingestion_jobs(pool: &SqlitePool) -> Result<Vec<IngestionJob>> {
    let rows = sqlx::query_as::<_, IngestionJob>(
        "SELECT id, source_path, collection, status, chunks_total, chunks_done,
                error, created_at, completed_at
         FROM ingestion_jobs
         ORDER BY created_at DESC;"
    )
    .fetch_all(pool)
    .await
    .context("Failed to list ingestion jobs")?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Memory episode operations
// ---------------------------------------------------------------------------

/// Insert a new memory episode and return it.
pub async fn insert_memory_episode(
    pool: &SqlitePool,
    summary: &str,
    source_conversation_id: Option<&str>,
    vector_id: Option<i64>,
) -> Result<MemoryEpisode> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO memory_episodes (id, summary, source_conversation_id, vector_id, created_at, decayed, restored)
         VALUES (?, ?, ?, ?, ?, 0, 0);",
    )
    .bind(&id)
    .bind(summary)
    .bind(source_conversation_id)
    .bind(vector_id)
    .bind(now)
    .execute(pool)
    .await
    .context("Failed to insert memory episode")?;

    Ok(MemoryEpisode {
        id,
        summary: summary.to_string(),
        source_conversation_id: source_conversation_id.map(str::to_string),
        vector_id,
        created_at: now,
        decayed: false,
        restored: false,
    })
}

/// Get the total number of memory episodes.
pub async fn get_memory_episode_count(pool: &SqlitePool) -> Result<u64> {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(id) FROM memory_episodes;"
    )
    .fetch_one(pool)
    .await
    .context("Failed to count memory episodes")?;

    Ok(count.0 as u64)
}

/// Delete all memory episodes. Returns the number of rows deleted.
pub async fn delete_all_memory_episodes(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM memory_episodes;")
        .execute(pool)
        .await
        .context("Failed to delete all memory episodes")?;

    Ok(result.rows_affected())
}

/// Delete all memory facts. Returns the number of rows deleted.
pub async fn delete_all_memory_facts(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("DELETE FROM memory_facts;")
        .execute(pool)
        .await
        .context("Failed to delete all memory facts")?;

    Ok(result.rows_affected())
}

/// Get a memory setting by key.
pub async fn get_memory_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT value FROM memory_settings WHERE key = ?;"
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .context("Failed to get memory setting")?;

    Ok(row.map(|r| r.0))
}

/// Set a memory setting (upsert).
pub async fn set_memory_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO memory_settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .context("Failed to set memory setting")?;

    Ok(())
}

/// Get the count of confirmed memory facts.
pub async fn get_confirmed_fact_count(pool: &SqlitePool) -> Result<u64> {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(id) FROM memory_facts WHERE confirmed_by_user = 1;"
    )
    .fetch_one(pool)
    .await
    .context("Failed to count confirmed memory facts")?;

    Ok(count.0 as u64)
}

/// Update the text of a memory fact.
pub async fn update_memory_fact_text(pool: &SqlitePool, id: &str, text: &str) -> Result<()> {
    sqlx::query("UPDATE memory_facts SET fact = ? WHERE id = ?;")
        .bind(text)
        .bind(id)
        .execute(pool)
        .await
        .context("Failed to update memory fact text")?;

    Ok(())
}

/// Set the memory_enabled flag for a conversation.
pub async fn set_conversation_memory_enabled(
    pool: &SqlitePool,
    conv_id: &str,
    enabled: bool,
) -> Result<()> {
    let val: i64 = if enabled { 1 } else { 0 };
    sqlx::query("UPDATE conversations SET memory_enabled = ? WHERE id = ?;")
        .bind(val)
        .bind(conv_id)
        .execute(pool)
        .await
        .context("Failed to set conversation memory_enabled")?;

    Ok(())
}

/// Get the memory_enabled flag for a conversation. Defaults to true if NULL.
pub async fn get_conversation_memory_enabled(pool: &SqlitePool, conv_id: &str) -> Result<bool> {
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT memory_enabled FROM conversations WHERE id = ?;"
    )
    .bind(conv_id)
    .fetch_optional(pool)
    .await
    .context("Failed to get conversation memory_enabled")?;

    // Default to true if the row doesn't exist or the column is NULL
    Ok(match row {
        Some((Some(val),)) => val != 0,
        _ => true,
    })
}

/// Get active (non-decayed or restored) episodes.
///
/// An episode is active if:
/// - Its age in days is less than `decay_threshold_days`, OR
/// - Its `restored` flag is true.
pub async fn get_active_episodes(
    pool: &SqlitePool,
    decay_threshold_days: u32,
) -> Result<Vec<MemoryEpisode>> {
    let now = Utc::now().timestamp();
    let threshold_seconds = decay_threshold_days as i64 * 86400;
    let cutoff = now - threshold_seconds;

    let rows = sqlx::query_as::<_, MemoryEpisode>(
        "SELECT id, summary, source_conversation_id, vector_id, created_at, decayed, restored
         FROM memory_episodes
         WHERE created_at >= ? OR restored = 1
         ORDER BY created_at DESC;"
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await
    .context("Failed to fetch active memory episodes")?;

    Ok(rows)
}
