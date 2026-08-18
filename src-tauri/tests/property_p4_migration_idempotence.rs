//! property_p4_migration_idempotence.rs — Property test P4 (task 5.5).
//!
//! **Property 4: Schema Migration Idempotence**
//!
//! **Validates: Requirements 4.1, 4.1.a, 4.1.b, 4.1.c, 4.2, 4.2.a, 4.2.b**
//!
//! Strategy: arbitrary small populations of seed rows in
//! `conversations`, `messages`, `memory_facts`, `rag_chunks`, and
//! `ingestion_jobs` (0..=5 rows per table via `proptest::collection::vec`).
//!
//! Predicate: snapshot `sqlite_master` rows, `PRAGMA table_info(<table>)`,
//! row counts, and per-row payloads for the five existing tables;
//! invoke `db::run_migrations` once (creating all seven tables), seed,
//! snapshot, invoke `db::run_migrations` a second time, re-snapshot.
//!
//! Assertion: snapshot before == snapshot after for the five existing
//! tables (schema and contents identical); `model_capabilities` and
//! `model_settings` exist after both runs with identical schema
//! introspection results between run 1 and run 2; both invocations
//! return `Ok(())`.
//!
//! Note: `db::run_migrations` is `pub`-exposed for this test (and only
//! this test) so we can drive the migration directly against a single
//! pool rather than spinning a fresh `init_pool` between runs. That
//! keeps the comparison honest — the same connection observes both
//! migration invocations and any drift would surface immediately.

#![allow(clippy::too_many_arguments)]

use std::path::PathBuf;

use heimdall_lib::db;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Tables under test
// ---------------------------------------------------------------------------

/// The five tables that must be preserved bit-for-bit across re-runs of
/// `db::run_migrations` per Requirement 4.1.a / 4.1.b.
const PRE_EXISTING_TABLES: &[&str] = &[
    "conversations",
    "messages",
    "memory_facts",
    "rag_chunks",
    "ingestion_jobs",
];

/// The two new tables introduced by this spec; both must exist after run
/// 1 and after run 2 with identical schema introspection per
/// Requirement 4.1.c / 4.2.b.
const NEW_TABLES: &[&str] = &["model_capabilities", "model_settings"];

// ---------------------------------------------------------------------------
// Seed-row generators
//
// Tiny hand-rolled strategies that emit values of the right shape for
// each table's columns. The schema is INTEGER timestamps + short TEXT
// keys + a handful of nullable TEXT/INTEGER fields, which we generate
// with deliberately small alphabets so shrinking is fast and
// counterexamples are readable.
// ---------------------------------------------------------------------------

fn prop_id() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9]{4,12}").expect("id regex compiles")
}

fn prop_short_text() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9 _\\-]{0,32}").expect("short-text regex compiles")
}

fn prop_opt_text() -> impl Strategy<Value = Option<String>> {
    proptest::option::of(prop_short_text())
}

fn prop_role() -> impl Strategy<Value = String> {
    prop_oneof![Just("user"), Just("assistant"), Just("system")].prop_map(String::from)
}

fn prop_input_type() -> impl Strategy<Value = Option<String>> {
    proptest::option::of(prop_oneof![
        Just("text".to_string()),
        Just("image".to_string()),
        Just("file".to_string()),
    ])
}

fn prop_status() -> impl Strategy<Value = Option<String>> {
    proptest::option::of(prop_oneof![
        Just("pending".to_string()),
        Just("running".to_string()),
        Just("done".to_string()),
        Just("failed".to_string()),
    ])
}

fn prop_timestamp() -> impl Strategy<Value = i64> {
    0_i64..2_700_000_000_i64
}

fn prop_opt_timestamp() -> impl Strategy<Value = Option<i64>> {
    proptest::option::of(prop_timestamp())
}

fn prop_opt_i64() -> impl Strategy<Value = Option<i64>> {
    proptest::option::of(0_i64..100_000_i64)
}

// ---------------------------------------------------------------------------
// Per-table seed structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ConversationSeed {
    id: String,
    title: Option<String>,
    model: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn prop_conversation_seed() -> impl Strategy<Value = ConversationSeed> {
    (
        prop_id(),
        prop_opt_text(),
        prop_opt_text(),
        prop_timestamp(),
        prop_timestamp(),
    )
        .prop_map(
            |(id, title, model, created_at, updated_at)| ConversationSeed {
                id,
                title,
                model,
                created_at,
                updated_at,
            },
        )
}

#[derive(Debug, Clone)]
struct MessageSeed {
    id: String,
    /// Index into the seeded `conversations` vector (or `None` for a
    /// NULL `conversation_id`). Resolved to the actual id at insert
    /// time so generated messages always satisfy the FK constraint.
    conversation_idx: Option<usize>,
    role: String,
    content: Option<String>,
    input_type: Option<String>,
    tokens_used: Option<i64>,
    images: Option<String>,
    thinking: Option<String>,
    created_at: i64,
}

/// Strategy for a `MessageSeed` whose `conversation_idx` is bounded by
/// the supplied `conversation_count`. When the bound is zero, every
/// generated message has a NULL `conversation_id`.
fn prop_message_seed(conversation_count: usize) -> impl Strategy<Value = MessageSeed> {
    let idx_strategy: BoxedStrategy<Option<usize>> = if conversation_count == 0 {
        Just(None).boxed()
    } else {
        proptest::option::of(0..conversation_count).boxed()
    };
    (
        prop_id(),
        idx_strategy,
        prop_role(),
        prop_opt_text(),
        prop_input_type(),
        prop_opt_i64(),
        prop_opt_text(),
        prop_opt_text(),
        prop_timestamp(),
    )
        .prop_map(
            |(
                id,
                conversation_idx,
                role,
                content,
                input_type,
                tokens_used,
                images,
                thinking,
                created_at,
            )| MessageSeed {
                id,
                conversation_idx,
                role,
                content,
                input_type,
                tokens_used,
                images,
                thinking,
                created_at,
            },
        )
}

#[derive(Debug, Clone)]
struct MemoryFactSeed {
    id: String,
    fact: String,
    source_conversation_id: Option<String>,
    confirmed_by_user: bool,
    created_at: i64,
}

fn prop_memory_fact_seed() -> impl Strategy<Value = MemoryFactSeed> {
    (
        prop_id(),
        prop_short_text(),
        prop_opt_text(),
        any::<bool>(),
        prop_timestamp(),
    )
        .prop_map(
            |(id, fact, source_conversation_id, confirmed_by_user, created_at)| MemoryFactSeed {
                id,
                fact,
                source_conversation_id,
                confirmed_by_user,
                created_at,
            },
        )
}

#[derive(Debug, Clone)]
struct RagChunkSeed {
    id: String,
    collection: String,
    source_path: String,
    chunk_index: i64,
    content: String,
    token_count: i64,
    vector_id: Option<i64>,
    created_at: i64,
}

fn prop_rag_chunk_seed() -> impl Strategy<Value = RagChunkSeed> {
    (
        prop_id(),
        prop_short_text(),
        prop_short_text(),
        0_i64..1_000_i64,
        prop_short_text(),
        0_i64..10_000_i64,
        prop_opt_i64(),
        prop_timestamp(),
    )
        .prop_map(
            |(
                id,
                collection,
                source_path,
                chunk_index,
                content,
                token_count,
                vector_id,
                created_at,
            )| RagChunkSeed {
                id,
                collection,
                source_path,
                chunk_index,
                content,
                token_count,
                vector_id,
                created_at,
            },
        )
}

#[derive(Debug, Clone)]
struct IngestionJobSeed {
    id: String,
    source_path: Option<String>,
    collection: Option<String>,
    status: Option<String>,
    chunks_total: i64,
    chunks_done: i64,
    error: Option<String>,
    created_at: i64,
    completed_at: Option<i64>,
}

fn prop_ingestion_job_seed() -> impl Strategy<Value = IngestionJobSeed> {
    (
        prop_id(),
        prop_opt_text(),
        prop_opt_text(),
        prop_status(),
        0_i64..10_000_i64,
        0_i64..10_000_i64,
        prop_opt_text(),
        prop_timestamp(),
        prop_opt_timestamp(),
    )
        .prop_map(
            |(
                id,
                source_path,
                collection,
                status,
                chunks_total,
                chunks_done,
                error,
                created_at,
                completed_at,
            )| IngestionJobSeed {
                id,
                source_path,
                collection,
                status,
                chunks_total,
                chunks_done,
                error,
                created_at,
                completed_at,
            },
        )
}

// ---------------------------------------------------------------------------
// Aggregate seed population
//
// One generator that builds an entire seed set with up to 5 rows per
// table. `messages` is generated *after* `conversations` so its
// conversation references can be resolved against the just-generated
// pool of conversation ids without violating the FK constraint.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SeedPopulation {
    conversations: Vec<ConversationSeed>,
    messages: Vec<MessageSeed>,
    memory_facts: Vec<MemoryFactSeed>,
    rag_chunks: Vec<RagChunkSeed>,
    ingestion_jobs: Vec<IngestionJobSeed>,
}

fn prop_seed_population() -> impl Strategy<Value = SeedPopulation> {
    proptest::collection::vec(prop_conversation_seed(), 0..=5).prop_flat_map(|conversations| {
        let conv_count = conversations.len();
        (
            Just(conversations),
            proptest::collection::vec(prop_message_seed(conv_count), 0..=5),
            proptest::collection::vec(prop_memory_fact_seed(), 0..=5),
            proptest::collection::vec(prop_rag_chunk_seed(), 0..=5),
            proptest::collection::vec(prop_ingestion_job_seed(), 0..=5),
        )
            .prop_map(
                |(conversations, messages, memory_facts, rag_chunks, ingestion_jobs)| {
                    // Deduplicate by id within each table — the
                    // generator can produce repeats and SQLite's PK
                    // constraint would reject the second one. Keeping
                    // the first occurrence is enough for the property:
                    // we assert idempotence over whatever rows
                    // actually persist.
                    SeedPopulation {
                        conversations: dedup_by_id(conversations, |c| c.id.clone()),
                        messages: dedup_by_id(messages, |m| m.id.clone()),
                        memory_facts: dedup_by_id(memory_facts, |m| m.id.clone()),
                        rag_chunks: dedup_by_id(rag_chunks, |c| c.id.clone()),
                        ingestion_jobs: dedup_by_id(ingestion_jobs, |j| j.id.clone()),
                    }
                },
            )
    })
}

fn dedup_by_id<T, F: Fn(&T) -> String>(rows: Vec<T>, key: F) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|row| seen.insert(key(row)))
        .collect()
}

// ---------------------------------------------------------------------------
// Snapshot machinery
//
// A "snapshot" captures everything we need to compare for equality:
//
//   1. `sqlite_master` rows (every CREATE statement currently in the DB)
//   2. `PRAGMA table_info(<table>)` for each of the seven tables
//   3. row counts per table
//   4. row payloads (per-table, deterministically ordered by primary key)
//
// All four sub-snapshots are stored as `String` so equality is a
// straightforward string compare. Determinism is achieved by sorting
// `sqlite_master` rows by `name`, sorting each table's rows by `id`,
// and rendering each row through a fixed column order.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct DbSnapshot {
    /// Verbatim contents of `sqlite_master`, sorted by `name` for
    /// deterministic ordering.
    sqlite_master: String,
    /// One entry per table. Captures `PRAGMA table_info`.
    table_infos: Vec<(String, String)>,
    /// Row counts for each of the seven tables.
    counts: Vec<(String, i64)>,
    /// Row contents for each of the seven tables, ordered by id.
    rows: Vec<(String, String)>,
}

async fn snapshot(pool: &SqlitePool) -> DbSnapshot {
    let sqlite_master = snapshot_sqlite_master(pool).await;

    let all_tables: Vec<&str> = PRE_EXISTING_TABLES
        .iter()
        .chain(NEW_TABLES.iter())
        .copied()
        .collect();

    let mut table_infos = Vec::new();
    let mut counts = Vec::new();
    let mut rows = Vec::new();

    for &table in &all_tables {
        table_infos.push((table.to_string(), snapshot_table_info(pool, table).await));
        counts.push((table.to_string(), snapshot_count(pool, table).await));
        rows.push((table.to_string(), snapshot_rows(pool, table).await));
    }

    DbSnapshot {
        sqlite_master,
        table_infos,
        counts,
        rows,
    }
}

/// Read every row from `sqlite_master` (excluding the SQLite-internal
/// `sqlite_*` rows which can be re-ordered or auto-generated) and
/// return a deterministic string rendering. The columns captured are
/// `type, name, tbl_name, sql` — enough to detect any drift in the
/// CREATE statements between runs.
async fn snapshot_sqlite_master(pool: &SqlitePool) -> String {
    let rows = sqlx::query(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_master
         WHERE name NOT LIKE 'sqlite_%'
         ORDER BY type, name;",
    )
    .fetch_all(pool)
    .await
    .expect("sqlite_master query succeeds");

    let mut out = String::new();
    for row in rows {
        let r#type: String = row.get(0);
        let name: String = row.get(1);
        let tbl_name: String = row.get(2);
        let sql: Option<String> = row.try_get(3).ok();
        out.push_str(&format!(
            "{}|{}|{}|{}\n",
            r#type,
            name,
            tbl_name,
            sql.unwrap_or_default()
        ));
    }
    out
}

/// Render `PRAGMA table_info(<table>)` as a deterministic string. The
/// pragma returns one row per column with `cid, name, type, notnull,
/// dflt_value, pk` — every field the design requires us to compare
/// per Requirement 4.1.a.
async fn snapshot_table_info(pool: &SqlitePool, table: &str) -> String {
    // PRAGMA table_info doesn't accept bound parameters; the table
    // name comes from a hard-coded constant list, so direct
    // interpolation is safe here.
    let query = format!("PRAGMA table_info({});", table);
    let rows = sqlx::query(&query)
        .fetch_all(pool)
        .await
        .expect("PRAGMA table_info succeeds");

    let mut out = String::new();
    for row in rows {
        let cid: i64 = row.get(0);
        let name: String = row.get(1);
        let r#type: String = row.get(2);
        let notnull: i64 = row.get(3);
        let dflt_value: Option<String> = row.try_get(4).ok();
        let pk: i64 = row.get(5);
        out.push_str(&format!(
            "{}|{}|{}|{}|{}|{}\n",
            cid,
            name,
            r#type,
            notnull,
            dflt_value.unwrap_or_default(),
            pk
        ));
    }
    out
}

async fn snapshot_count(pool: &SqlitePool, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {};", table);
    let row = sqlx::query(&query)
        .fetch_one(pool)
        .await
        .expect("COUNT query succeeds");
    row.get::<i64, _>(0)
}

/// Render every row in `<table>` as a deterministic string. Ordered by
/// `id` (every table here uses `id TEXT PRIMARY KEY` except
/// `model_capabilities` and `model_settings` which use `model_name`;
/// both are empty across the run so this code path is exercised only
/// for the five existing tables).
///
/// Each column is rendered into a `String` regardless of its on-disk
/// type via SQLite's loose typing — `row.get::<String, _>` works for
/// INTEGER and REAL columns too because sqlx coerces. This keeps the
/// snapshot logic table-agnostic.
async fn snapshot_rows(pool: &SqlitePool, table: &str) -> String {
    // Decide a stable order: every table here has an `id` column
    // except the two new ones which use `model_name`.
    let order_col = match table {
        "model_capabilities" | "model_settings" => "model_name",
        _ => "id",
    };
    let query = format!("SELECT * FROM {} ORDER BY {};", table, order_col);
    let rows = sqlx::query(&query)
        .fetch_all(pool)
        .await
        .expect("SELECT * succeeds");

    let mut out = String::new();
    for row in rows {
        let mut cells = Vec::new();
        for col_idx in 0..row.len() {
            // Try INTEGER, then REAL, then TEXT, then NULL — covering
            // every type SQLite affixes to a column in this schema.
            let cell = if let Ok(v) = row.try_get::<Option<i64>, _>(col_idx) {
                format!("i:{:?}", v)
            } else if let Ok(v) = row.try_get::<Option<f64>, _>(col_idx) {
                format!("f:{:?}", v)
            } else if let Ok(v) = row.try_get::<Option<String>, _>(col_idx) {
                format!("s:{:?}", v)
            } else {
                "?".to_string()
            };
            cells.push(cell);
        }
        out.push_str(&cells.join("|"));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Pool / file helpers
// ---------------------------------------------------------------------------

struct TempDbGuard {
    path: PathBuf,
}

impl Drop for TempDbGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut wal = self.path.clone();
        wal.set_extension("db-wal");
        let _ = std::fs::remove_file(&wal);
        let mut shm = self.path.clone();
        shm.set_extension("db-shm");
        let _ = std::fs::remove_file(&shm);
    }
}

/// Open a SQLite pool against a unique temp-file path with the same
/// pragmas `db::init_pool` would apply. We bypass `init_pool` itself
/// so the test can drive `run_migrations` directly twice on the same
/// pool — that is the property under test.
async fn open_pool(path: &PathBuf) -> SqlitePool {
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let opts = SqliteConnectOptions::from_str(&url)
        .expect("sqlite url parses")
        .pragma("foreign_keys", "ON")
        .pragma("journal_mode", "WAL")
        .pragma("synchronous", "NORMAL")
        .pragma("busy_timeout", "5000")
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
        .expect("pool connect succeeds")
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

async fn seed(pool: &SqlitePool, pop: &SeedPopulation) {
    for c in &pop.conversations {
        sqlx::query(
            "INSERT INTO conversations (id, title, model, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?);",
        )
        .bind(&c.id)
        .bind(&c.title)
        .bind(&c.model)
        .bind(c.created_at)
        .bind(c.updated_at)
        .execute(pool)
        .await
        .expect("insert conversation");
    }

    for m in &pop.messages {
        let conv_id: Option<String> = m
            .conversation_idx
            .and_then(|idx| pop.conversations.get(idx).map(|c| c.id.clone()));
        sqlx::query(
            "INSERT INTO messages
                (id, conversation_id, role, content, input_type,
                 tokens_used, images, thinking, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?);",
        )
        .bind(&m.id)
        .bind(&conv_id)
        .bind(&m.role)
        .bind(&m.content)
        .bind(&m.input_type)
        .bind(m.tokens_used)
        .bind(&m.images)
        .bind(&m.thinking)
        .bind(m.created_at)
        .execute(pool)
        .await
        .expect("insert message");
    }

    for f in &pop.memory_facts {
        sqlx::query(
            "INSERT INTO memory_facts
                (id, fact, source_conversation_id, confirmed_by_user, created_at)
             VALUES (?, ?, ?, ?, ?);",
        )
        .bind(&f.id)
        .bind(&f.fact)
        .bind(&f.source_conversation_id)
        .bind(if f.confirmed_by_user { 1_i64 } else { 0_i64 })
        .bind(f.created_at)
        .execute(pool)
        .await
        .expect("insert memory_fact");
    }

    for r in &pop.rag_chunks {
        sqlx::query(
            "INSERT INTO rag_chunks
                (id, collection, source_path, chunk_index, content,
                 token_count, vector_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?);",
        )
        .bind(&r.id)
        .bind(&r.collection)
        .bind(&r.source_path)
        .bind(r.chunk_index)
        .bind(&r.content)
        .bind(r.token_count)
        .bind(r.vector_id)
        .bind(r.created_at)
        .execute(pool)
        .await
        .expect("insert rag_chunk");
    }

    for j in &pop.ingestion_jobs {
        sqlx::query(
            "INSERT INTO ingestion_jobs
                (id, source_path, collection, status, chunks_total,
                 chunks_done, error, created_at, completed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?);",
        )
        .bind(&j.id)
        .bind(&j.source_path)
        .bind(&j.collection)
        .bind(&j.status)
        .bind(j.chunks_total)
        .bind(j.chunks_done)
        .bind(&j.error)
        .bind(j.created_at)
        .bind(j.completed_at)
        .execute(pool)
        .await
        .expect("insert ingestion_job");
    }
}

// ---------------------------------------------------------------------------
// Property body
//
// Plain `#[test]` (not `#[tokio::test]`) so we can drive an explicit
// `TestRunner` and reuse a single tokio runtime across all generated
// cases. Each case opens a fresh temp-file SQLite database, runs the
// migration once, seeds, snapshots, runs the migration again,
// re-snapshots, and asserts equality.
// ---------------------------------------------------------------------------

#[test]
fn p4_run_migrations_is_idempotent() {
    // Single tokio runtime drives every case; cheaper than spinning a
    // runtime per `prop_assert!`.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    let mut runner = TestRunner::new(ProptestConfig {
        cases: 32,
        ..ProptestConfig::default()
    });

    let result = runner.run(&prop_seed_population(), |pop| {
        rt.block_on(async {
            // Fresh temp-file DB per case so each property
            // invocation starts from an empty schema.
            let mut path = std::env::temp_dir();
            path.push(format!("heimdall-p4-{}.db", Uuid::new_v4()));
            let _guard = TempDbGuard { path: path.clone() };

            let pool = open_pool(&path).await;

            // Run 1: must succeed and create all seven tables.
            let r1 = db::run_migrations(&pool).await;
            prop_assert!(r1.is_ok(), "first run_migrations failed: {:?}", r1);

            // Seed 0..=5 rows per existing table.
            seed(&pool, &pop).await;

            // Snapshot after run 1 + seed.
            let snap_before = snapshot(&pool).await;

            // Run 2: must also succeed, must not change anything.
            let r2 = db::run_migrations(&pool).await;
            prop_assert!(r2.is_ok(), "second run_migrations failed: {:?}", r2);

            // Snapshot after run 2.
            let snap_after = snapshot(&pool).await;

            // Assertion: full snapshot equality covers every
            // sub-clause of Requirements 4.1.* and 4.2.* — schema
            // introspection (sqlite_master + PRAGMA table_info),
            // row counts, and row payloads for all seven tables.
            prop_assert_eq!(
                &snap_before.sqlite_master,
                &snap_after.sqlite_master,
                "sqlite_master drifted between run 1 and run 2"
            );
            prop_assert_eq!(
                &snap_before.table_infos,
                &snap_after.table_infos,
                "PRAGMA table_info drifted between run 1 and run 2"
            );
            prop_assert_eq!(
                &snap_before.counts,
                &snap_after.counts,
                "row counts drifted between run 1 and run 2"
            );
            prop_assert_eq!(
                &snap_before.rows,
                &snap_after.rows,
                "row contents drifted between run 1 and run 2"
            );

            // Sanity: confirm new tables exist (Requirement 4.1.c
            // + 4.2.b). The table_info entries above already
            // capture this implicitly — non-existent tables would
            // produce empty PRAGMA output and the equality holds —
            // so we tighten by asserting each new table's
            // table_info is non-empty.
            for table in NEW_TABLES {
                let info = snap_after
                    .table_infos
                    .iter()
                    .find(|(name, _)| name == table)
                    .map(|(_, info)| info.as_str())
                    .unwrap_or("");
                prop_assert!(
                    !info.is_empty(),
                    "expected new table `{}` to exist after run 2",
                    table
                );
            }

            pool.close().await;
            Ok(())
        })
    });

    result.expect("property holds for all generated cases");
}
