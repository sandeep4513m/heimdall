/// governor.rs — Phase 6 resource-management subsystem
///
/// One long-lived tokio task polling system resources every 2 seconds,
/// derived from `/proc/meminfo`, `/proc/stat`, `/proc/[pid]/status`,
/// `/sys/class/drm/`, and Ollama's `GET /api/ps` endpoint. The polling
/// task emits a structured `GovernorMetrics` snapshot on every tick and
/// (in Run 3) acts on memory pressure via auto-unload — all without
/// holding any lock across an `.await` and without spawning a dedicated
/// thread.
///
/// ## Cancellation
///
/// The polling loop accepts a `CancellationToken` as its sole termination
/// signal. Cancellation lands within ~2200 ms in the common case (the
/// `tokio::time::sleep(2 s)` wake-up path), within 5 s in the worst case
/// if a `list_running` HTTP call is in flight (bounded by its 5 s
/// deadline). Per Req 1.4, the loop exits cleanly within 2200 ms of a
/// cancel arriving while the loop is in `sleep`; the worst case of a
/// cancel arriving mid-tick during the HTTP call is bounded by the
/// `OllamaClient::list_running` timeout. Tightening to a hard 2.2 s by
/// inserting `cancel.cancelled()` checks inside `tick()` is a v1.1
/// candidate.
///
/// ## Locking discipline
///
/// The polling task **never holds a lock across an `.await`** other than
/// `tokio::time::sleep` and the lock-free synchronous `try_lock()` calls
/// on the shared maps. The candidate selector path reads
/// `active_streams` (`tokio::Mutex` → `try_lock().ok()`),
/// `active_stream_models` (`std::sync::Mutex` → `try_lock()`),
/// `active_ingestions` (`tokio::Mutex` → `try_lock().ok()`), and
/// `model_last_used` (`std::sync::Mutex` → `try_lock()`). On contention
/// the polling tick uses pessimistic-on-contention semantics so a
/// wrongful unload is impossible (a missed unload is recoverable; a
/// wrongful unload during streaming is not). The loop body itself
/// takes locks during the `tick()` builder when evaluating pressure.
///
/// Design ref: `.kiro/specs/governor-intelligence/design.md` →
/// "Backend — `src-tauri/src/governor.rs`".
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::adaptive_config::AppConfig;
use crate::models::{
    GovernorMetrics, GovernorThresholds, HardwareInfo, HardwareTier, ProcStatus,
    RiskState, RunningModel, TierConfig, VramStatus, EmbeddingFitDecision,
};
use crate::ollama_client::OllamaClient;

// ---------------------------------------------------------------------------
// Top-level type
// ---------------------------------------------------------------------------

/// The Phase 6 Governor.
///
/// Owns the polling-loop task and every piece of cross-cutting state the
/// loop needs to read or write. `Arc`-clone fields are shared with
/// `AppState` so the chat-stream hot path, the ingestion worker, and the
/// frontend command handlers can read/mutate the same maps.
///
/// Note on field visibility: most fields are crate-private so subsequent
/// runs can fill the loop body without exposing internals to callers.
/// `app_handle` is held so the loop can `emit` events without taking a
/// reference at call time.
pub struct Governor<R: tauri::Runtime = tauri::Wry> {
    // ── External I/O ──────────────────────────────────────────────────────
    /// HTTP client for Ollama (`/api/ps`, `/api/generate keep_alive=0s`).
    pub(crate) ollama: OllamaClient,
    /// SQLite pool. Reserved for future per-model audit logging; not used
    /// in the Phase 6 hot path. Holding it on the struct keeps the
    /// constructor signature stable when audit lands in v1.1.
    #[allow(dead_code)]
    pub(crate) db: SqlitePool,
    /// Per-tier configuration. Wrapped in `RwLock` so threshold edits via
    /// `governor_set_thresholds` propagate without a restart (Run 5).
    pub(crate) tier_config: Arc<tokio::sync::RwLock<TierConfig>>,
    /// Hardware snapshot taken once at bootstrap.
    pub(crate) hardware: HardwareInfo,
    /// Persisted user config for `auto_unload_enabled` and the per-model
    /// override map. Mutated by the Tauri commands in Run 5.
    pub(crate) config: Arc<AsyncMutex<AppConfig>>,

    // ── Shared with AppState (Arc clones) ─────────────────────────────────
    /// Last-token timestamp per model name. Updated via `try_lock` from
    /// `chat_stream`. `std::sync::Mutex` is used so the chat-stream hot path
    /// can never `.await` while holding it.
    pub(crate) model_last_used: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
    /// Cancellation tokens for in-flight chat streams, keyed by
    /// `conversation_id`. Shared with `AppState`; held on `Governor` for state parity and future direct stream aborts.
    #[allow(dead_code)]
    pub(crate) active_streams: Arc<AsyncMutex<HashMap<String, CancellationToken>>>,
    /// Maps `conversation_id -> model_name` for in-flight chat streams.
    /// `std::sync::Mutex` so the `Drop` guard installed on the streaming
    /// path is synchronous.
    pub(crate) active_stream_models: Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// Existing field: in-flight RAG ingestion jobs keyed by `job_id`.
    /// The Governor treats any non-empty map as "embedding model in use".
    pub(crate) active_ingestions:
        Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<bool>>>>>,
    /// When `Some(name)`, the next `chat_stream` call for `name` emits a
    /// `governor://embedding_swap { phase: ReloadingChat }` event before
    /// issuing `/api/chat`. Shared with `AppState` and set by ingestion worker.
    #[allow(dead_code)]
    pub(crate) chat_reload_pending: Arc<std::sync::Mutex<Option<String>>>,
    /// When `true`, the ingestion worker sleeps 1s and re-checks rather
    /// than dequeuing the next job. Set on the rising edge of
    /// `Critical` and cleared on the falling edge.
    pub(crate) ingestion_paused: Arc<AtomicBool>,

    // ── Internal — hot path mutexes (synchronous, never held across .await) ─
    /// Set on the successful Ollama force-unload response. Gates the next
    /// auto-unload by ≥ 5000 ms.
    pub(crate) last_unload_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// Per-model failure counter for the 3-strikes exclusion. Entries
    /// older than 30 s are purged at the top of each pass.
    pub(crate) consecutive_failures:
        std::sync::Mutex<HashMap<String, (u8, std::time::Instant)>>,
    /// Models excluded for the duration of the current pressure event.
    /// Cleared when `risk_state` returns to `Calm` or `Warn`.
    pub(crate) excluded_for_event: std::sync::Mutex<HashSet<String>>,
    /// Last observed `risk_state`. Used to detect rising/falling edges of
    /// `Critical`.
    pub(crate) last_risk_state: std::sync::Mutex<RiskState>,

    // ── Cached snapshots — read by Run 3's embedding-fit decision ─────────
    /// Most-recent successful `loaded_models` snapshot. Updated on every
    /// tick after `loaded_models` is computed. Read by
    /// `IngestionWorker::dequeue` via `last_loaded_snapshot()` so the
    /// embedding-fit decision does not need a fresh `/api/ps` call.
    pub(crate) last_loaded_snapshot: std::sync::Mutex<Vec<RunningModel>>,
    /// Most-recent `available_ram_mb` reading. Updated on every tick.
    /// Read by the ingestion worker through `last_available_mb()`.
    pub(crate) last_available_mb: std::sync::Mutex<u64>,

    // ── Threshold-fallback gating (Req 6.9) ───────────────────────────────
    /// One-shot flag: ensures the "rejected configured thresholds" warn
    /// log fires at most once per process, not on every tick. Set on the
    /// first invalid-threshold detection.
    pub(crate) thresholds_fallback_warned: AtomicBool,

    // ── Misc ──────────────────────────────────────────────────────────────
    /// Wall-clock when the polling task started. Used as the synthetic
    /// `idle_time` baseline for models that have never streamed in this
    /// session (Req 8.1).
    #[allow(dead_code)]
    pub(crate) started_at: std::time::Instant,
    /// Tauri handle for `emit`. Generic over the runtime `R` so tests can
    /// construct a Governor against `tauri::test::mock_app()` (which uses
    /// `MockRuntime`) while production uses the default `Wry`.
    pub(crate) app_handle: AppHandle<R>,
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

impl<R: tauri::Runtime> Governor<R> {
    /// Construct a new Governor sharing AppState's `Arc` clones.
    ///
    /// The constructor never spawns the polling task; that is the caller's
    /// responsibility (`bootstrap()` does it after `AppState` registration
    /// completes — see Task 9.2).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ollama: OllamaClient,
        db: SqlitePool,
        tier_config: Arc<tokio::sync::RwLock<TierConfig>>,
        hardware: HardwareInfo,
        config: Arc<AsyncMutex<AppConfig>>,
        model_last_used: Arc<std::sync::Mutex<HashMap<String, std::time::Instant>>>,
        active_streams: Arc<AsyncMutex<HashMap<String, CancellationToken>>>,
        active_stream_models: Arc<std::sync::Mutex<HashMap<String, String>>>,
        active_ingestions: Arc<
            AsyncMutex<HashMap<String, Arc<AsyncMutex<bool>>>>,
        >,
        chat_reload_pending: Arc<std::sync::Mutex<Option<String>>>,
        ingestion_paused: Arc<AtomicBool>,
        app_handle: AppHandle<R>,
    ) -> Self {
        Self {
            ollama,
            db,
            tier_config,
            hardware,
            config,
            model_last_used,
            active_streams,
            active_stream_models,
            active_ingestions,
            chat_reload_pending,
            ingestion_paused,
            last_unload_at: std::sync::Mutex::new(None),
            consecutive_failures: std::sync::Mutex::new(HashMap::new()),
            excluded_for_event: std::sync::Mutex::new(HashSet::new()),
            last_risk_state: std::sync::Mutex::new(RiskState::Calm),
            last_loaded_snapshot: std::sync::Mutex::new(Vec::new()),
            last_available_mb: std::sync::Mutex::new(0),
            thresholds_fallback_warned: AtomicBool::new(false),
            started_at: std::time::Instant::now(),
            app_handle,
        }
    }

    /// Cached snapshot accessor. Returns a clone of the most-recent
    /// `loaded_models` reading from the polling loop. Empty before the
    /// first successful tick.
    ///
    /// Used by `IngestionWorker::dequeue` (Run 3 / Task 14.2) so the
    /// embedding-fit decision can read a recent loaded-set without
    /// blocking on a fresh `/api/ps` HTTP call.
    #[allow(dead_code)]
    pub(crate) fn last_loaded_snapshot(&self) -> Vec<RunningModel> {
        // `lock()` here is safe — the only contention is from the polling
        // loop's own write inside `tick()`, which holds the mutex for a
        // single clone-and-store. The lock is never held across `.await`.
        match self.last_loaded_snapshot.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Cached `available_ram_mb` accessor. Returns `0` before the first
    /// successful tick.
    #[allow(dead_code)]
    pub(crate) fn last_available_mb(&self) -> u64 {
        match self.last_available_mb.lock() {
            Ok(g) => *g,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

// ---------------------------------------------------------------------------
// /proc/meminfo reader (Req 2.1, 2.4)
// ---------------------------------------------------------------------------

/// Parse `/proc/meminfo` and return `(MemTotal, MemAvailable, SwapTotal,
/// SwapFree)` as megabytes via integer division by 1024 (Req 2.1).
///
/// Returns `Err(ProcStatus::Unreadable)` on any read or parse failure —
/// missing fields, unparseable numbers, or a `read_to_string` error. The
/// caller is responsible for setting all four mem fields to `0` and
/// emitting a single warn log (Req 2.4).
///
/// Linux pseudo-files complete in microseconds; using the synchronous
/// `std::fs::read_to_string` rather than `tokio::fs` is intentional —
/// wrapping a microsecond read in `spawn_blocking` would cost more than
/// the read itself.
fn read_meminfo() -> Result<(u64, u64, u64, u64), ProcStatus> {
    let contents = std::fs::read_to_string("/proc/meminfo")
        .map_err(|_| ProcStatus::Unreadable)?;
    parse_meminfo(&contents).ok_or(ProcStatus::Unreadable)
}

/// Pure parser for the `/proc/meminfo` text body. Extracted from
/// `read_meminfo` so it can be unit-tested without touching the
/// filesystem (Task 4.2) and property-tested (P2).
///
/// Returns `(MemTotal, MemAvailable, SwapTotal, SwapFree)` as **megabytes**
/// via integer division by 1024 (Req 2.1). Returns `None` when any of the
/// four required fields is missing or its value column is non-numeric —
/// the caller maps `None` to `ProcStatus::Unreadable` and zeroes the mem
/// fields (Req 2.4).
///
/// `pub` (not `pub(crate)`) so the external property-test crate
/// `tests/property_p2_memory_invariants.rs` can reach it via
/// `heimdall_lib::governor::parse_meminfo`.
pub fn parse_meminfo(contents: &str) -> Option<(u64, u64, u64, u64)> {
    let mut total: Option<u64> = None;
    let mut available: Option<u64> = None;
    let mut swap_total: Option<u64> = None;
    let mut swap_free: Option<u64> = None;

    for line in contents.lines() {
        // /proc/meminfo lines look like: "MemTotal:       16291968 kB".
        // We split on whitespace and pick the second column as a u64 KB.
        if let Some((key, rest)) = line.split_once(':') {
            let value_kb = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok());
            match key {
                "MemTotal" => total = value_kb,
                "MemAvailable" => available = value_kb,
                "SwapTotal" => swap_total = value_kb,
                "SwapFree" => swap_free = value_kb,
                _ => {}
            }
        }
    }

    match (total, available, swap_total, swap_free) {
        (Some(t), Some(a), Some(st), Some(sf)) => {
            Some((t / 1024, a / 1024, st / 1024, sf / 1024))
        }
        _ => None,
    }
}

/// Apply the two mem invariants from Req 2.6/2.7 and compute `swap_used`.
///
/// Given the four raw MB readings `(total, available, swap_total,
/// swap_free)` returns `(total, available.min(total), swap_total,
/// swap_total.saturating_sub(swap_free))`. This guarantees
/// `available_ram_mb <= total_ram_mb` (Req 2.6) and
/// `swap_used_mb <= swap_total_mb` (Req 2.7) for any pathological input.
///
/// `pub` for the P2 property-test crate.
pub fn normalize_meminfo(
    total: u64,
    available: u64,
    swap_total: u64,
    swap_free: u64,
) -> (u64, u64, u64, u64) {
    (
        total,
        available.min(total),
        swap_total,
        swap_total.saturating_sub(swap_free),
    )
}

/// Clamp an optional RSS reading against total RAM (Req 3.8).
///
/// Returns `None` when `rss` is `Some(v)` and `v > total` (with
/// `total > 0`); otherwise returns `rss` unchanged. A `None` input maps
/// to `None`. Used for `ollama_rss_mb`, which the contract turns into
/// `None` when it implausibly exceeds physical RAM.
///
/// `pub` for the P2 property-test crate.
pub fn clamp_rss(rss: Option<u64>, total: u64) -> Option<u64> {
    match rss {
        Some(v) if total > 0 && v > total => None,
        other => other,
    }
}

/// Clamp Heimdall's own RSS to total RAM (Req 3.9).
///
/// Returns `total` when `rss > total` (with `total > 0`); otherwise
/// returns `rss` unchanged. Unlike `clamp_rss` this never produces a
/// sentinel — `heimdall_rss_mb` is a plain `u64`, so an over-total
/// reading is clamped down rather than dropped.
///
/// `pub` for the P2 property-test crate.
pub fn clamp_self_rss(rss: u64, total: u64) -> u64 {
    if total > 0 && rss > total {
        total
    } else {
        rss
    }
}

// ---------------------------------------------------------------------------
// /proc/stat reader and CPU delta computation (Req 2.2, 2.5, 2.8)
// ---------------------------------------------------------------------------

/// One sample of CPU jiffy counters from `/proc/stat`. `total` sums the
/// user/nice/system/idle/iowait/irq/softirq/steal columns (Linux's
/// canonical "active + idle" definition). `idle` keeps idle + iowait so
/// the busy-percent calculation can subtract it from total.
///
/// Crate-private — only the polling loop manipulates these.
#[derive(Debug, Clone)]
pub struct CpuJiffies {
    pub name: String,
    pub total: u64,
    pub idle: u64,
}

/// Parse `/proc/stat` into one `CpuJiffies` per `cpu*` line. The first
/// `cpu` line (the aggregate over all cores) goes at index 0; per-core
/// lines (`cpu0`, `cpu1`, …) follow in the order Linux reports them.
///
/// Returns `Err(ProcStatus::Unreadable)` on any read failure or if the
/// aggregate `cpu` line is missing. Lines that are not `cpu*` (e.g.
/// `intr`, `ctxt`, `btime`) are skipped.
fn read_stat_sample() -> Result<Vec<CpuJiffies>, ProcStatus> {
    let contents = std::fs::read_to_string("/proc/stat")
        .map_err(|_| ProcStatus::Unreadable)?;

    let mut out: Vec<CpuJiffies> = Vec::new();
    for line in contents.lines() {
        // Match `cpu` (aggregate) or `cpu0`, `cpu1`, … (per-core).
        // Anything else (intr, ctxt, btime, …) is skipped.
        let mut parts = line.split_whitespace();
        let name = match parts.next() {
            Some(n) if n == "cpu" || n.starts_with("cpu") => n.to_string(),
            _ => continue,
        };
        // Sanity check: the rest after `cpu` (if any) must be all digits.
        let suffix = &name[3..];
        if !suffix.is_empty() && !suffix.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        // Columns (Linux man page):
        //   user nice system idle iowait irq softirq steal [guest guest_nice]
        // We sum the first 8 into `total` and capture (idle + iowait) as
        // `idle` for the busy-% subtraction. Extra columns (guest /
        // guest_nice) are intentionally ignored — they are already
        // accounted for inside `user` and `nice` on modern kernels.
        let cols: Vec<u64> = parts
            .take(8)
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();
        if cols.len() < 4 {
            // We need at least user/nice/system/idle to compute anything.
            continue;
        }
        let idle_part = cols.get(3).copied().unwrap_or(0)
            + cols.get(4).copied().unwrap_or(0); // idle + iowait
        let total_part: u64 = cols.iter().sum();
        out.push(CpuJiffies {
            name,
            total: total_part,
            idle: idle_part,
        });
    }

    if out.is_empty() {
        return Err(ProcStatus::Unreadable);
    }
    Ok(out)
}

/// Compute aggregate CPU% and per-core CPU% from two `/proc/stat`
/// samples taken ~100 ms apart.
///
/// Returns `(aggregate_pct, per_core_excluding_aggregate_pct)`. Names are
/// matched between the two samples; missing names are skipped (rare
/// online/offline core race). Zero `total_delta` returns `0.0` rather
/// than `NaN` (Req 2.5 mandates the loop never emits NaN). Every value
/// is clamped to `[0.0, 100.0]` (Req 2.8).
///
/// `pub` so the P2 property-test crate and the Task 4.2 unit tests can
/// exercise it directly.
pub fn compute_cpu_percent(s1: &[CpuJiffies], s2: &[CpuJiffies]) -> (f32, Vec<f32>) {
    fn pct(prev: &CpuJiffies, curr: &CpuJiffies) -> f32 {
        let total_delta = curr.total.saturating_sub(prev.total);
        if total_delta == 0 {
            return 0.0;
        }
        let idle_delta = curr.idle.saturating_sub(prev.idle);
        let busy = 100.0 * (1.0 - (idle_delta as f32) / (total_delta as f32));
        busy.clamp(0.0, 100.0)
    }

    // Build a name → index map over s1 so we can match s2 entries by name.
    let mut by_name: HashMap<&str, &CpuJiffies> = HashMap::new();
    for j in s1 {
        by_name.insert(j.name.as_str(), j);
    }

    let mut aggregate: f32 = 0.0;
    let mut per_core: Vec<f32> = Vec::new();
    for curr in s2 {
        if let Some(prev) = by_name.get(curr.name.as_str()) {
            let p = pct(prev, curr);
            if curr.name == "cpu" {
                aggregate = p;
            } else {
                per_core.push(p);
            }
        }
    }
    (aggregate, per_core)
}

// ---------------------------------------------------------------------------
// /proc/[pid]/status readers — Ollama PID resolution and self/child RSS
// ---------------------------------------------------------------------------

/// Scan `/proc` for an Ollama process by reading each numeric directory's
/// `comm` file. Returns the numerically smallest matching PID, or `None`
/// when no entry trims to the literal string `"ollama"` (Req 3.1, 3.4).
///
/// **Never shells out** to `pgrep`/`ps`/etc. (Req 3.1).
fn find_ollama_pid() -> Option<u32> {
    find_ollama_pid_at(std::path::Path::new("/proc"))
}

/// Parameterised core of `find_ollama_pid` (Task 5.2). Scans every
/// numeric subdirectory of `proc_root`, reads its `comm` file, and
/// collects the PIDs whose trimmed `comm` equals `"ollama"`. The
/// smallest such PID wins via `pick_smallest_matching_pid`.
///
/// `pub` so the governor unit-test module can inject a fake `/proc`
/// tree built with `tempfile` rather than relying on the host's real
/// process table.
pub fn find_ollama_pid_at(proc_root: &std::path::Path) -> Option<u32> {
    let entries = std::fs::read_dir(proc_root).ok()?;
    let mut matches: Vec<u32> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        // Only consider numeric directory names (i.e. PIDs).
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let comm_path = entry.path().join("comm");
        let comm = match std::fs::read_to_string(&comm_path) {
            Ok(s) => s,
            // Process exited between readdir and read; permission denied
            // for a foreign-uid process. Either way, skip.
            Err(_) => continue,
        };
        if comm.trim() == "ollama" {
            matches.push(pid);
        }
    }
    pick_smallest_matching_pid(&matches)
}

/// Pure helper (Task 5.2): pick the numerically smallest PID from a slice
/// of matches, or `None` when the slice is empty. Extracted so the
/// "smallest match wins" rule (Req 3.1) can be unit-tested without a
/// filesystem.
pub fn pick_smallest_matching_pid(matches: &[u32]) -> Option<u32> {
    matches.iter().copied().min()
}

/// Read `VmRSS` from `/proc/<pid>/status` and return MB via integer
/// division by 1024 (Req 3.2). `None` on any failure: file missing,
/// permission denied, line absent, or the integer column unparseable
/// (Req 3.3).
fn read_status_vmrss(pid: u32) -> Option<u64> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    parse_status_vmrss(&contents)
}

/// Pure parser for a `/proc/<pid>/status` body (Task 5.2). Extracts the
/// `VmRSS` line and returns its value in **megabytes** via integer
/// division by 1024 (Req 3.2). Returns `None` when the line is absent or
/// the integer column is unparseable (Req 3.3).
///
/// Integer overflow during the KB→MB division is impossible (`u64 /
/// 1024` only ever shrinks), but a `VmRSS` column that overflows `u64`
/// on parse simply yields `None`.
///
/// `pub` so the governor unit-test module can exercise the present /
/// absent / multi-line / overflow cases directly.
pub fn parse_status_vmrss(contents: &str) -> Option<u64> {
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// Sum `VmRSS` (in MB) of every process whose parent PID equals
/// `self_pid` AND whose `comm` matches `WebKitWebProcess` or
/// `WebKitNetworkProcess` (Req 3.6).
///
/// Returns `None` when no such children exist; `Some(0)` when at least
/// one matching child exists but the sum happens to be zero (rare —
/// `VmRSS` is always non-zero in practice, but the contract makes the
/// distinction explicit).
fn read_webview_rss(self_pid: u32) -> Option<u64> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut sum_mb: u64 = 0;
    let mut found_child = false;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        // Reject non-numeric directory names (skip self, thread-N, etc.).
        // The numeric value itself is irrelevant — we only need to know
        // it parses as a PID. Run 3's selector reuses the actual PID.
        if pid_str.parse::<u32>().is_err() {
            continue;
        }
        let comm_path = entry.path().join("comm");
        let comm = match std::fs::read_to_string(&comm_path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => continue,
        };
        if comm != "WebKitWebProcess" && comm != "WebKitNetworkProcess" {
            continue;
        }

        let status_path = entry.path().join("status");
        let status = match std::fs::read_to_string(&status_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut ppid: Option<u32> = None;
        let mut rss_mb: Option<u64> = None;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("PPid:") {
                ppid = rest.split_whitespace().next().and_then(|s| s.parse().ok());
            } else if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss_mb = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|kb| kb / 1024);
            }
            if ppid.is_some() && rss_mb.is_some() {
                break;
            }
        }

        if ppid == Some(self_pid) {
            found_child = true;
            if let Some(mb) = rss_mb {
                sum_mb = sum_mb.saturating_add(mb);
            }
        }
    }

    if found_child {
        Some(sum_mb)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// /sys/class/drm VRAM reader (Req 4.1 – 4.7, 17.2, 17.3)
// ---------------------------------------------------------------------------

/// Walk `/sys/class/drm/` and sum `mem_info_vram_total` /
/// `mem_info_vram_used` across every identified discrete GPU.
///
/// **Card identification:** matches `device/vendor` against NVIDIA
/// (`0x10de`) and AMD (`0x1002`). Intel (`0x8086`) and every other
/// vendor are intentionally excluded (Req 4.7) — Ollama cannot use Intel
/// iGPUs and reporting non-zero VRAM for them would mislead the tier
/// detector.
///
/// **Status mapping:**
/// - No discrete GPU at any path → `(None, None, Absent)` (Req 4.3).
/// - At least one identified GPU but a read failed for at least one
///   card → respective sums set to `None` and status `Unavailable`
///   (Req 4.4).
/// - All identified cards returned both numbers cleanly → `Ok` with
///   `Some(sum_total_mb), Some(sum_used_mb)` (Req 4.5).
///
/// **Never shells out** to `nvidia-smi`, `nvtop`, or `lspci` (Req 4.6).
fn read_drm_vram() -> (Option<u64>, Option<u64>, VramStatus) {
    read_drm_vram_at(std::path::Path::new("/sys/class/drm"))
}

/// Parameterised core of `read_drm_vram` (Task 6.2). Walks `root` (the
/// `/sys/class/drm` directory in production) and applies the exact same
/// identification, summation, and status-mapping logic. Extracted so the
/// governor unit-test module can inject a fake sysfs tree built with
/// `tempfile` covering the seven cases in Task 6.2.
///
/// `pub` so the external-crate test harness (if ever needed) and the
/// in-file unit tests can both reach it.
pub fn read_drm_vram_at(
    drm_path: &std::path::Path,
) -> (Option<u64>, Option<u64>, VramStatus) {
    let entries = match std::fs::read_dir(drm_path) {
        Ok(e) => e,
        // No /sys/class/drm at all (containerised env, sandbox) → Absent.
        Err(_) => return (None, None, VramStatus::Absent),
    };

    let mut identified_count: u32 = 0;
    let mut total_sum_bytes: u128 = 0;
    let mut used_sum_bytes: u128 = 0;
    let mut total_read_failed = false;
    let mut used_read_failed = false;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Cards only (skip renderD<N>, skip names with '-' like
        // card0-DP-1 connector links).
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }

        // Vendor filter — discrete only.
        let vendor_path = entry.path().join("device/vendor");
        let vendor = match std::fs::read_to_string(&vendor_path) {
            Ok(s) => s.trim().to_string(),
            Err(_) => continue,
        };
        let v_lower = vendor.to_ascii_lowercase();
        if v_lower != "0x10de" && v_lower != "0x1002" {
            // Intel (0x8086) and all others — explicitly excluded.
            continue;
        }

        identified_count += 1;

        let total_path = entry.path().join("device/mem_info_vram_total");
        match std::fs::read_to_string(&total_path) {
            Ok(s) => match s.trim().parse::<u128>() {
                Ok(bytes) => total_sum_bytes = total_sum_bytes.saturating_add(bytes),
                Err(_) => total_read_failed = true,
            },
            Err(_) => total_read_failed = true,
        }

        let used_path = entry.path().join("device/mem_info_vram_used");
        match std::fs::read_to_string(&used_path) {
            Ok(s) => match s.trim().parse::<u128>() {
                Ok(bytes) => used_sum_bytes = used_sum_bytes.saturating_add(bytes),
                Err(_) => used_read_failed = true,
            },
            Err(_) => used_read_failed = true,
        }
    }

    if identified_count == 0 {
        return (None, None, VramStatus::Absent);
    }

    // 1 MB = 1,048,576 bytes (Req 4.2).
    const MB: u128 = 1024 * 1024;

    if total_read_failed || used_read_failed {
        let total_out = if total_read_failed {
            None
        } else {
            Some((total_sum_bytes / MB) as u64)
        };
        let used_out = if used_read_failed {
            None
        } else {
            Some((used_sum_bytes / MB) as u64)
        };
        return (total_out, used_out, VramStatus::Unavailable);
    }

    (
        Some((total_sum_bytes / MB) as u64),
        Some((used_sum_bytes / MB) as u64),
        VramStatus::Ok,
    )
}

// ---------------------------------------------------------------------------
// Threshold validation with documented-defaults fallback (Req 6.9)
// ---------------------------------------------------------------------------

/// Documented per-tier defaults from Req 6.6. Used as the fall-back when
/// `read_thresholds_with_fallback` rejects misconfigured values.
fn default_thresholds_for(tier: HardwareTier) -> (u64, u64, u64) {
    match tier {
        HardwareTier::Minimal => (800, 400, 200),
        HardwareTier::Standard => (1500, 800, 400),
        HardwareTier::Full => (2000, 1000, 500),
    }
}

/// Returns the thresholds the Governor will actually use for this tick.
///
/// On valid configuration (`warn > unload > critical > 0`) the configured
/// values pass through unchanged. On invalid configuration the function
/// returns the documented defaults for the active tier and emits ONE
/// warn log per process via the `thresholds_fallback_warned` AtomicBool
/// gate (Req 6.9 — "single warning indicating the configured thresholds
/// were rejected").
///
/// `fallback_warned` is the `Governor.thresholds_fallback_warned` flag
/// passed by reference so the function stays free-standing and the gate
/// stays bound to a Governor instance.
fn read_thresholds_with_fallback(
    tier: &TierConfig,
    fallback_warned: &AtomicBool,
) -> (u64, u64, u64) {
    let w = tier.governor_warn_mb;
    let u = tier.governor_unload_mb;
    let c = tier.governor_critical_mb;
    let valid = w > u && u > c && c > 0;

    if valid {
        return (w, u, c);
    }

    // Emit the warn log exactly once per process.
    if !fallback_warned.swap(true, Ordering::AcqRel) {
        tracing::warn!(
            tier = ?tier.tier,
            configured_warn_mb = w,
            configured_unload_mb = u,
            configured_critical_mb = c,
            "governor: rejected configured thresholds for tier {:?}; falling back to documented defaults",
            tier.tier
        );
    }

    default_thresholds_for(tier.tier)
}

// ---------------------------------------------------------------------------
// Polling loop and tick builder
// ---------------------------------------------------------------------------

impl<R: tauri::Runtime> Governor<R> {
    /// Drive the polling loop until cancelled.
    ///
    /// Loop body order (Req 1.1, 1.7, 1.10, 1.11):
    ///   1. Capture `tick_start` for slow-tick detection.
    ///   2. Build the metrics snapshot.
    ///   3. Emit `governor://metrics` (failure to emit is logged at warn
    ///      level but does not abort the tick).
    ///   4. Update `last_risk_state` (critical edge transitions are
    ///      stubbed for Run 3 / Task 13.1).
    ///   5. Reset 3-strikes state when leaving pressure.
    ///   6. Log slow ticks (>1000 ms) at info level.
    ///   7. `select!` between a 2000 ms sleep and the cancel token.
    ///
    /// **First tick fires immediately** — sleep AFTER, not before — so
    /// the first emit lands within 2000 ms of `run` being awaited
    /// (Req 1.11).
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        loop {
            let tick_start = std::time::Instant::now();
            let metrics = self.tick().await;

            // Step 3 — emit. A failure here usually means the window has
            // already gone away during shutdown; log it but continue so
            // the loop can observe the cancel token and exit cleanly.
            if let Err(e) = self.app_handle.emit("governor://metrics", &metrics) {
                tracing::warn!(error = %e, "governor: failed to emit governor://metrics");
            }

            // Step 4 — critical edge transitions (Run 3 / Task 13.1).
            //   On entering Critical: ingestion_paused=true, batch
            //     unload eligible, emit governor://critical.
            //   On exiting Critical:  ingestion_paused=false, emit
            //     governor://critical_cleared.
            // Read the previous state, transition once, then write.
            let prev_state = {
                let g = match self.last_risk_state.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                *g
            };
            match (prev_state, metrics.risk_state) {
                (p, RiskState::Critical) if p != RiskState::Critical => {
                    self.ingestion_paused.store(true, Ordering::Release);
                    let scheduled = self.batch_unload_eligible(&metrics).await;
                    let payload = CriticalPayload {
                        available_ram_mb: metrics.available_ram_mb,
                        critical_threshold_mb: metrics.thresholds.critical_mb,
                        scheduled_unloads: scheduled,
                    };
                    if let Err(e) =
                        self.app_handle.emit("governor://critical", &payload)
                    {
                        tracing::warn!(error = %e, "governor: failed to emit governor://critical");
                    }
                }
                (RiskState::Critical, c) if c != RiskState::Critical => {
                    self.ingestion_paused.store(false, Ordering::Release);
                    if let Err(e) =
                        self.app_handle.emit("governor://critical_cleared", &())
                    {
                        tracing::warn!(error = %e, "governor: failed to emit governor://critical_cleared");
                    }
                }
                _ => {}
            }
            // Persist the latest risk state for the next tick's edge
            // detection. Done after the transition handlers so a poison
            // recovery on the read path does not double-fire.
            {
                let mut last = match self.last_risk_state.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                *last = metrics.risk_state;
            }

            // Auto-unload pass: only fires on Unload state. Critical has
            // its own batch path above; Calm/Warn skip the pass entirely.
            // Mutually exclusive with the critical edge transitions in
            // a given tick — Critical → Unload is the only way both
            // could overlap, and a tick that just *exited* Critical is
            // by definition not in Unload.
            if matches!(metrics.risk_state, RiskState::Unload) {
                self.maybe_unload_one(&metrics).await;
            }

            // Step 5 — reset 3-strikes / cooldown when leaving pressure.
            // Run 3 wires the actual unload pass; the resets are wired
            // here because they are harmless when the unload path is
            // dormant and they keep the Run 3 diff small.
            if matches!(metrics.risk_state, RiskState::Calm | RiskState::Warn) {
                if let Ok(mut excluded) = self.excluded_for_event.lock() {
                    excluded.clear();
                }
                if let Ok(mut last_unload) = self.last_unload_at.lock() {
                    *last_unload = None;
                }
            }

            // Step 6 — slow-tick log (Req 1.6).
            let elapsed = tick_start.elapsed();
            if elapsed > Duration::from_millis(1000) {
                tracing::info!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    "governor: tick exceeded 1000 ms"
                );
            }

            // Step 7 — sleep or cancel (Req 1.4, 1.5, 1.11).
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(2000)) => {}
                _ = cancel.cancelled() => {
                    tracing::info!("governor: cancelled, exiting loop");
                    return;
                }
            }
        }
    }

    /// One polling tick. Reads every system source and returns a fully
    /// populated `GovernorMetrics`.
    ///
    /// Failure-isolation contract (Req 1.10): every source's failure is
    /// contained — the field is set to its documented sentinel (`0`,
    /// `None`, or a status enum) and the tick continues. At most one
    /// warn log per source per tick.
    pub async fn tick(&self) -> GovernorMetrics {
        // ── Memory and CPU ────────────────────────────────────────────
        let mut total_ram_mb: u64 = 0;
        let mut available_ram_mb: u64 = 0;
        let mut swap_total_mb: u64 = 0;
        let mut swap_used_mb: u64 = 0;
        let mut proc_status = ProcStatus::Readable;

        match read_meminfo() {
            Ok((t, a, st, sf)) => {
                // Apply the Req 2.6/2.7 invariants and compute swap_used via
                // the extracted pure helper so behaviour matches the P2
                // property test exactly.
                let (t2, a2, st2, su2) = normalize_meminfo(t, a, st, sf);
                total_ram_mb = t2;
                available_ram_mb = a2;
                swap_total_mb = st2;
                swap_used_mb = su2;
            }
            Err(_) => {
                tracing::warn!("governor: /proc/meminfo unreadable; mem fields zeroed for this tick");
                proc_status = ProcStatus::Unreadable;
            }
        }

        // CPU: two stat samples ~100 ms apart, then percent.
        let (cpu_aggregate_percent, cpu_per_core_percent) = {
            match read_stat_sample() {
                Ok(s1) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    match read_stat_sample() {
                        Ok(s2) => compute_cpu_percent(&s1, &s2),
                        Err(_) => {
                            tracing::warn!("governor: /proc/stat unreadable on second sample; cpu zeroed");
                            (0.0, Vec::new())
                        }
                    }
                }
                Err(_) => {
                    tracing::warn!("governor: /proc/stat unreadable on first sample; cpu zeroed");
                    (0.0, Vec::new())
                }
            }
        };

        // ── Process footprint ─────────────────────────────────────────
        let (mut ollama_rss_mb, mut ollama_online): (Option<u64>, bool) =
            match find_ollama_pid() {
                Some(pid) => match read_status_vmrss(pid) {
                    Some(mb) => (Some(mb), true),
                    None => {
                        tracing::warn!(
                            pid,
                            "governor: /proc/<pid>/status unreadable for ollama"
                        );
                        (None, false)
                    }
                },
                None => (None, false),
            };

        // Clamp ollama_rss_mb to None when value exceeds total_ram_mb (Req 3.8).
        if let Some(v) = ollama_rss_mb {
            if total_ram_mb > 0 && v > total_ram_mb {
                tracing::warn!(
                    ollama_rss_mb = v,
                    total_ram_mb = total_ram_mb,
                    "governor: ollama RSS exceeded total RAM; clamping to None"
                );
            }
        }
        ollama_rss_mb = clamp_rss(ollama_rss_mb, total_ram_mb);

        let self_pid = std::process::id();
        let mut heimdall_rss_mb: u64 = match read_status_vmrss(self_pid) {
            Some(mb) => mb,
            None => {
                tracing::warn!("governor: /proc/self/status unreadable; heimdall RSS = 0");
                0
            }
        };
        // Clamp heimdall_rss_mb to total_ram_mb (Req 3.9).
        if total_ram_mb > 0 && heimdall_rss_mb > total_ram_mb {
            tracing::warn!(
                heimdall_rss_mb,
                total_ram_mb = total_ram_mb,
                "governor: heimdall RSS exceeded total RAM; clamping to total"
            );
        }
        heimdall_rss_mb = clamp_self_rss(heimdall_rss_mb, total_ram_mb);

        let webview_rss_mb = read_webview_rss(self_pid);

        // ── VRAM ──────────────────────────────────────────────────────
        let (vram_total_mb, vram_used_mb, vram_status) = read_drm_vram();

        // ── Loaded models via /api/ps ─────────────────────────────────
        // Skip the HTTP call entirely when ollama is known down (Req 15.5)
        // — avoids 5-second timeouts on a confirmed-absent service.
        let loaded_models: Vec<RunningModel> = if !ollama_online {
            Vec::new()
        } else {
            match self.ollama.list_running().await {
                Ok(list) => list,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "governor: list_running failed; loaded_models cleared, ollama_online=false"
                    );
                    ollama_online = false;
                    Vec::new()
                }
            }
        };

        // Cache the loaded snapshot and available RAM for Run 3's
        // embedding-fit decision and any `last_*` accessor read.
        if let Ok(mut g) = self.last_loaded_snapshot.lock() {
            *g = loaded_models.clone();
        }
        if let Ok(mut g) = self.last_available_mb.lock() {
            *g = available_ram_mb;
        }

        // ── Risk state and threshold validation (Req 6.6, 6.8, 6.9) ───
        let tier = self.tier_config.read().await;
        let (mut warn_mb, mut unload_mb, mut critical_mb) =
            read_thresholds_with_fallback(&tier, &self.thresholds_fallback_warned);
        let active_tier = tier.tier;
        let detected_tier = self.hardware.detected_tier;
        let effective_tier = self.hardware.effective_tier;
        drop(tier);

        // Runtime clamp: warn_mb must not exceed total_ram_mb (Req 6.8).
        // If violated we collapse the entire triple to (total, total/2,
        // total/4) and warn — this is a degenerate machine state but
        // staying internally consistent is more useful than carrying a
        // value above physical RAM into the risk-state branches.
        if total_ram_mb > 0 && warn_mb > total_ram_mb {
            tracing::warn!(
                tier = ?active_tier,
                configured_warn_mb = warn_mb,
                total_ram_mb = total_ram_mb,
                "governor: warn_mb exceeds total RAM; clamping to (total, total/2, total/4)"
            );
            warn_mb = total_ram_mb;
            unload_mb = total_ram_mb / 2;
            critical_mb = total_ram_mb / 4;
        }

        let risk_state = if matches!(proc_status, ProcStatus::Unreadable) {
            // No memory reading → cannot derive a meaningful risk state.
            // Default to Calm so the UI does not panic-paint the user.
            RiskState::Calm
        } else {
            derive_risk_state(available_ram_mb, warn_mb, unload_mb, critical_mb)
        };

        GovernorMetrics {
            total_ram_mb,
            available_ram_mb,
            swap_total_mb,
            swap_used_mb,
            cpu_aggregate_percent,
            cpu_per_core_percent,
            ollama_online,
            ollama_rss_mb,
            heimdall_rss_mb,
            webview_rss_mb,
            vram_total_mb,
            vram_used_mb,
            vram_status,
            loaded_models,
            risk_state,
            thresholds: GovernorThresholds {
                warn_mb,
                unload_mb,
                critical_mb,
            },
            detected_tier,
            effective_tier,
            proc_status,
            cgroup_detected: false,
            timestamp_unix_ms: Utc::now().timestamp_millis(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure-function risk-state derivation (covered by P4 in Run 2)
// ---------------------------------------------------------------------------

/// Pure branch-logic mapping `available_ram_mb` to a `RiskState` against
/// the three configured thresholds. Free function (not a method) so
/// property test P4 can call it without constructing a `Governor`.
///
/// Threshold validation (`warn > unload > critical > 0`) and fall-back to
/// documented defaults on misconfiguration is the caller's responsibility
/// — see `read_thresholds_with_fallback` above. This function trusts its
/// inputs.
///
/// Branch logic (Req 6.2 – 6.5):
///   - `available_ram_mb >= warn_mb`     → `Calm`
///   - `available_ram_mb >= unload_mb`   → `Warn`
///   - `available_ram_mb >= critical_mb` → `Unload`
///   - otherwise                         → `Critical`
pub fn derive_risk_state(
    available_ram_mb: u64,
    warn_mb: u64,
    unload_mb: u64,
    critical_mb: u64,
) -> RiskState {
    if available_ram_mb >= warn_mb {
        RiskState::Calm
    } else if available_ram_mb >= unload_mb {
        RiskState::Warn
    } else if available_ram_mb >= critical_mb {
        RiskState::Unload
    } else {
        RiskState::Critical
    }
}

// ---------------------------------------------------------------------------
// Auto-unload candidate selector
//
// Pure free function, no `&self` dependency. The Governor's
// `maybe_unload_one` (below) builds the snapshot inputs and delegates the
// actual decision here. Property tests for stream-aware, ingestion-aware,
// and deterministic selection call this directly with hand-rolled fixtures.
//
// Filter order is fixed: streaming → ingestion → per-model auto-unload toggle
// → 3-strikes excluded.
// ---------------------------------------------------------------------------

/// Choose the next model to auto-unload, or `None` when no eligible
/// candidate remains.
///
/// Tie-break ordering:
///   1. Largest idle time wins (longest since last token).
///   2. On equal idle time, largest `size_total_mb` wins (free more RAM).
///   3. On equal size, lex-smallest name (UTF-8 byte-wise) wins.
///
/// Idle time defaults to `now - polling_loop_start` for models that have
/// never streamed in this session.
pub fn select_unload_candidate<'a>(
    loaded: &'a [RunningModel],
    active_stream_models_values: &HashSet<String>,
    active_ingestions_nonempty: bool,
    model_last_used: &HashMap<String, std::time::Instant>,
    embedding_model_name: &str,
    auto_unload_per_model: &HashMap<String, bool>,
    excluded_for_event: &HashSet<String>,
    polling_loop_start: std::time::Instant,
    now: std::time::Instant,
) -> Option<&'a RunningModel> {
    let candidates: Vec<&RunningModel> = loaded
        .iter()
        // 1. Streaming guard: do not unload currently streaming models.
        .filter(|m| !active_stream_models_values.contains(&m.name))
        // 2. Ingestion guard: protect the active embedding model during ingestion.
        .filter(|m| !(active_ingestions_nonempty && m.name == embedding_model_name))
        // 3. Per-model auto-unload toggle. Missing key defaults
        //    to `true` (auto-unload allowed).
        .filter(|m| {
            auto_unload_per_model
                .get(&m.name)
                .copied()
                .unwrap_or(true)
        })
        // 4. 3-strikes exclusion for the duration of this pressure event.
        .filter(|m| !excluded_for_event.contains(&m.name))
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // `max_by` chooses the largest element. Idle ascending → larger wins,
    // size_total_mb ascending → larger wins. For names we want the
    // *lex-smallest* on tie, so we invert the comparison: `b.cmp(a)`
    // yields the reversed ordering, and `max_by` then picks the smallest.
    candidates.into_iter().max_by(|a, b| {
        let idle_a = idle_time(a, model_last_used, polling_loop_start, now);
        let idle_b = idle_time(b, model_last_used, polling_loop_start, now);
        idle_a
            .cmp(&idle_b)
            .then_with(|| a.size_total_mb.cmp(&b.size_total_mb))
            .then_with(|| b.name.cmp(&a.name))
    })
}

/// Helper: idle time since the last streamed token for `m`, falling back
/// to time since the polling loop started for never-used models.
fn idle_time(
    m: &RunningModel,
    last: &HashMap<String, std::time::Instant>,
    start: std::time::Instant,
    now: std::time::Instant,
) -> std::time::Duration {
    last.get(&m.name)
        .map(|t| now.duration_since(*t))
        .unwrap_or_else(|| now.duration_since(start))
}

// ---------------------------------------------------------------------------
// Adaptive embedding orchestration (Run 3 / Task 14.1)
//
// `can_load_embedding` is a pure decision function used by the ingestion
// worker on Tier 1: given the embedding model size, the chat model size
// (or 0 when no chat model is loaded), available RAM, and the configured
// safe headroom percentage, return one of three branches:
//
//   - FitsAlongside        — proceed without unloading anything.
//   - RequiresChatUnload   — chat model must be evicted first.
//   - InsufficientEvenAlone — embedding alone exceeds the safe budget;
//                              the worker fails the job (Req 10.8).
//
// The budget is `floor(mem_available_mb * safe_headroom_pct)` — integer
// truncation matches what property test P8 asserts. `Governor::
// evaluate_embedding_fit` (below) wraps this with the cached snapshots
// the polling loop already maintains, so a fit check does not require a
// fresh `/api/ps` round-trip.
// ---------------------------------------------------------------------------

/// Pure decision function — returns whether the embedding model can be
/// loaded right now, and if so, whether the chat model needs to make
/// room first. See module-level comments above.
pub fn can_load_embedding(
    embedding_size_mb: u64,
    chat_size_mb: u64,
    mem_available_mb: u64,
    safe_headroom_pct: f32,
) -> EmbeddingFitDecision {
    // `floor` matches P8's expected budget computation exactly. Casting
    // through f32 is acceptable here because mem_available_mb on a
    // 64 GB box (~65536) and pct in (0,1] both fit cleanly in f32; the
    // worst-case rounding error is at most a few MB, well below any
    // sensible per-tier threshold.
    let budget = ((mem_available_mb as f32) * safe_headroom_pct).floor() as u64;
    if embedding_size_mb > budget {
        EmbeddingFitDecision::InsufficientEvenAlone
    } else if embedding_size_mb.saturating_add(chat_size_mb) <= budget {
        EmbeddingFitDecision::FitsAlongside
    } else {
        EmbeddingFitDecision::RequiresChatUnload
    }
}

impl<R: tauri::Runtime> Governor<R> {
    /// Evaluate whether the embedding model can be loaded right now,
    /// using the most recent cached snapshots from the polling loop
    /// rather than a fresh `/api/ps` round-trip.
    ///
    /// Called from `IngestionWorker::dequeue` (Run 3 / Task 14.2). The
    /// snapshot is at most one tick (~2 s) old, which is fine — RAM
    /// pressure does not flip in 2 s, and the next tick will react if
    /// it does.
    pub async fn evaluate_embedding_fit(
        &self,
        chat_model_name: Option<&str>,
    ) -> EmbeddingFitDecision {
        let loaded = self.last_loaded_snapshot();
        let avail = self.last_available_mb();
        let tier = self.tier_config.read().await;
        let embedding_size_mb =
            self.estimate_embedding_size_mb(&tier.embedding_model, &loaded);
        let chat_size_mb = chat_model_name
            .and_then(|n| loaded.iter().find(|m| m.name == n))
            .map(|m| m.size_total_mb)
            .unwrap_or(0);
        let pct = tier.safe_headroom_pct;
        // Drop the read guard before calling the pure function so we
        // never hold the RwLock across additional `.await` work.
        drop(tier);
        can_load_embedding(embedding_size_mb, chat_size_mb, avail, pct)
    }

    /// Predictive ingestion-pressure preview (Legendary feature, Task
    /// 28.1). Like `evaluate_embedding_fit` but returns the full
    /// `IngestionFitPreview` — the three-light `status` plus the raw MB
    /// numbers behind the decision — so the gated
    /// `governor_preview_ingestion` command can surface them to the UI.
    ///
    /// `status` mapping mirrors the `EmbeddingFitDecision` branches:
    /// `FitsAlongside → "green"`, `RequiresChatUnload → "amber"`,
    /// `InsufficientEvenAlone → "red"`.
    pub async fn preview_embedding_fit(
        &self,
        chat_model_name: Option<&str>,
    ) -> crate::models::IngestionFitPreview {
        let loaded = self.last_loaded_snapshot();
        let available_mb = self.last_available_mb();
        let tier = self.tier_config.read().await;
        let embedding_mb =
            self.estimate_embedding_size_mb(&tier.embedding_model, &loaded);
        let chat_mb = chat_model_name
            .and_then(|n| loaded.iter().find(|m| m.name == n))
            .map(|m| m.size_total_mb)
            .unwrap_or(0);
        let pct = tier.safe_headroom_pct;
        drop(tier);

        let budget_mb = ((available_mb as f32) * pct).floor() as u64;
        let decision = can_load_embedding(embedding_mb, chat_mb, available_mb, pct);
        let status = match decision {
            EmbeddingFitDecision::FitsAlongside => "green",
            EmbeddingFitDecision::RequiresChatUnload => "amber",
            EmbeddingFitDecision::InsufficientEvenAlone => "red",
        };
        crate::models::IngestionFitPreview {
            status: status.to_string(),
            embedding_mb,
            chat_mb,
            available_mb,
            budget_mb,
        }
    }

    /// Estimate the embedding model's size in MB.
    ///
    /// Look it up in the most recent loaded snapshot first — that's the
    /// authoritative number Ollama just reported. If the model is not
    /// loaded right now, fall back to a conservative default.
    /// `nomic-embed-text` is ~270 MB on disk; we round up to 350 MB to
    /// leave a small safety margin for the model header / KV cache that
    /// load adds. Tier configs that name a different embedding model
    /// still use this default — refining to a per-model lookup is a
    /// v1.1 candidate.
    fn estimate_embedding_size_mb(
        &self,
        name: &str,
        loaded: &[RunningModel],
    ) -> u64 {
        if let Some(m) = loaded.iter().find(|m| m.name == name) {
            return m.size_total_mb;
        }
        // Conservative default. Documented above; chosen to match the
        // current ScalarKind::F16 nomic-embed-text footprint plus the
        // load-time overhead Ollama adds for the KV cache.
        350
    }
}

// ---------------------------------------------------------------------------
// Auto-unload pass — `Governor::maybe_unload_one` and helpers
//
// Wired into `Governor::run` after the metrics emit when `risk_state ==
// Unload`. Critical state has its own batch path. The pass is governed by:
//
//   1. The global `auto_unload_enabled` toggle. When `false`, the loop still
//      emits metrics but issues zero unloads.
//   2. A 5-second cooldown between successful unloads. Cleared on transition
//      to Calm/Warn.
//   3. A 3-strikes exclusion for repeatedly failing names within a 30-s
//      window. Entries are purged at the top of every pass so a stale
//      failure does not poison the next pressure event.
//   4. The streaming / ingestion / per-model / 3-strikes filter chain in
//      `select_unload_candidate` above.
//
// Pessimistic-on-contention semantics: if `try_lock` on
// `active_stream_models` fails, we treat the streaming set as empty and
// rely on the pre-send re-check immediately before the HTTP send to
// avoid wrongful unloads.
// ---------------------------------------------------------------------------

/// Critical-state payload (Task 13.1). Defined here rather than at the
/// emit site so the field names live next to the struct definition the
/// frontend reads via the snake_case wire form.
#[derive(serde::Serialize)]
struct CriticalPayload {
    available_ram_mb: u64,
    critical_threshold_mb: u64,
    scheduled_unloads: Vec<String>,
}

/// Auto-unload cooldown between successive successful unloads (Req 8.3).
const UNLOAD_COOLDOWN: Duration = Duration::from_millis(5000);

/// Window over which 3 consecutive failures earn a model a placement in
/// `excluded_for_event` (Req 8.8). Entries older than this are purged at
/// the top of each pass.
const FAILURE_WINDOW: Duration = Duration::from_secs(30);

/// Number of consecutive failures within `FAILURE_WINDOW` that earn an
/// exclusion (Req 8.8).
const FAILURE_STRIKE_LIMIT: u8 = 3;

impl<R: tauri::Runtime> Governor<R> {
    /// Best-effort single-shot auto-unload pass.
    ///
    /// Called from `Governor::run` only when `risk_state == Unload`.
    /// Critical state batch-unloads via `batch_unload_eligible` (Task
    /// 13.1) without cooldown; this method respects the cooldown.
    pub(crate) async fn maybe_unload_one(&self, metrics: &GovernorMetrics) {
        // ── Step 1: global auto-unload toggle (Req 8.5) ──────────────
        // `None` (older config files) is treated as `true` for forward
        // compat — see `default_auto_unload_enabled` in adaptive_config.
        let global_enabled = {
            let cfg = self.config.lock().await;
            cfg.auto_unload_enabled.unwrap_or(true)
        };
        if !global_enabled {
            return;
        }

        // ── Step 2: 5-second cooldown gate (Req 8.3) ─────────────────
        if let Ok(g) = self.last_unload_at.lock() {
            if let Some(t) = *g {
                if std::time::Instant::now().duration_since(t) < UNLOAD_COOLDOWN {
                    return;
                }
            }
        }

        // ── Step 3: purge expired 3-strikes entries at the top of the
        //           pass (Req 8.8) ─────────────────────────────────────
        if let Ok(mut failures) = self.consecutive_failures.lock() {
            let now = std::time::Instant::now();
            failures.retain(|_, (_, ts)| now.duration_since(*ts) < FAILURE_WINDOW);
        }

        // ── Step 4: build candidate-set inputs ───────────────────────
        // Pessimistic-on-contention for `active_stream_models`: an empty
        // set on `try_lock` failure means the selector might pick a
        // streaming model — but the Req 15.1 re-check immediately
        // before the HTTP send catches that.
        let streaming_set: HashSet<String> = match self.active_stream_models.try_lock() {
            Ok(g) => g.values().cloned().collect(),
            Err(_) => HashSet::new(),
        };
        let ingestion_nonempty = match self.active_ingestions.try_lock() {
            Ok(g) => !g.is_empty(),
            // Pessimistic on contention: assume an ingestion is in
            // flight so we never unload the embedding model under
            // uncertainty (Req 7.7 / P6).
            Err(_) => true,
        };
        let model_last_used: HashMap<String, std::time::Instant> =
            match self.model_last_used.try_lock() {
                Ok(g) => g.clone(),
                Err(_) => HashMap::new(),
            };
        let excluded: HashSet<String> = match self.excluded_for_event.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };

        // tier_config is awaited; we are NOT holding any std::sync::Mutex
        // here (the temporary clones above all dropped their guards).
        let embedding_model = {
            let tier = self.tier_config.read().await;
            tier.embedding_model.clone()
        };

        // Per-model toggle map snapshot.
        let auto_unload_per_model = {
            let cfg = self.config.lock().await;
            cfg.auto_unload_per_model.clone()
        };

        // ── Step 5: select a candidate ───────────────────────────────
        let candidate_name: Option<String> = select_unload_candidate(
            &metrics.loaded_models,
            &streaming_set,
            ingestion_nonempty,
            &model_last_used,
            &embedding_model,
            &auto_unload_per_model,
            &excluded,
            self.started_at,
            std::time::Instant::now(),
        )
        .map(|m| m.name.clone());

        // ── Step 6: no candidates → emit governor://no_candidates when
        //           pressured (Req 7.8) ───────────────────────────────
        let candidate_name = match candidate_name {
            Some(n) => n,
            None => {
                if matches!(
                    metrics.risk_state,
                    RiskState::Unload | RiskState::Critical
                ) {
                    let payload = serde_json::json!({
                        "available_ram_mb": metrics.available_ram_mb,
                        "risk_state": metrics.risk_state,
                        "loaded_count": metrics.loaded_models.len(),
                    });
                    let _ = self
                        .app_handle
                        .emit("governor://no_candidates", payload);
                }
                return;
            }
        };

        // ── Step 7: re-check streaming guard immediately before send
        //           (Req 15.1) ───────────────────────────────────────
        // This closes the race where a chat stream begins between our
        // snapshot above and the HTTP send.
        let streaming_now: HashSet<String> = match self.active_stream_models.try_lock() {
            Ok(g) => g.values().cloned().collect(),
            // On contention, take the safe path and abort — the next
            // tick re-evaluates with fresh data.
            Err(_) => {
                tracing::warn!(
                    candidate = %candidate_name,
                    "governor: pre-send re-check could not acquire active_stream_models; aborting unload for this tick"
                );
                return;
            }
        };
        if streaming_now.contains(&candidate_name) {
            tracing::info!(
                candidate = %candidate_name,
                "governor: candidate began streaming between snapshot and send; aborting unload"
            );
            return;
        }

        // ── Step 8: HTTP send ────────────────────────────────────────
        match self.ollama.force_unload(&candidate_name).await {
            Ok(()) => {
                if let Ok(mut g) = self.last_unload_at.lock() {
                    *g = Some(std::time::Instant::now());
                }
                if let Ok(mut failures) = self.consecutive_failures.lock() {
                    failures.remove(&candidate_name);
                }
                tracing::info!(
                    model = %candidate_name,
                    "governor: auto-unload succeeded"
                );
            }
            Err(e) => {
                tracing::warn!(
                    model = %candidate_name,
                    error = %e,
                    "governor: force_unload failed"
                );
                if let Ok(mut failures) = self.consecutive_failures.lock() {
                    let entry = failures
                        .entry(candidate_name.clone())
                        .or_insert((0, std::time::Instant::now()));
                    entry.0 = entry.0.saturating_add(1);
                    entry.1 = std::time::Instant::now();
                    if entry.0 >= FAILURE_STRIKE_LIMIT {
                        if let Ok(mut excluded) = self.excluded_for_event.lock() {
                            excluded.insert(candidate_name.clone());
                        }
                        tracing::warn!(
                            model = %candidate_name,
                            failures = entry.0,
                            "governor: 3-strikes — excluding model for this pressure event"
                        );
                    }
                }
            }
        }
    }

    /// Critical-state batch unload (Task 13.1).
    ///
    /// Collects every model NOT in `active_stream_models` AND (when
    /// ingestion is non-empty) NOT the embedding model, then sends
    /// `force_unload` for each one in sequence with no cooldown between
    /// requests (Req 9.7). Returns the list of names actually scheduled
    /// — used for the `governor://critical` event payload.
    pub(crate) async fn batch_unload_eligible(
        &self,
        metrics: &GovernorMetrics,
    ) -> Vec<String> {
        let streaming: HashSet<String> = match self.active_stream_models.try_lock() {
            Ok(g) => g.values().cloned().collect(),
            // Pessimistic on contention here too: an empty set means we
            // schedule everything, but a wrongful unload during a stream
            // is non-recoverable while a missed unload is. The 5-second
            // worst case is that one streaming model survives this pass.
            Err(_) => HashSet::new(),
        };
        let ingest_nonempty = match self.active_ingestions.try_lock() {
            Ok(g) => !g.is_empty(),
            // Pessimistic: protect the embedding model under uncertainty.
            Err(_) => true,
        };
        let embedding_model = {
            let tier = self.tier_config.read().await;
            tier.embedding_model.clone()
        };

        let mut scheduled: Vec<String> = Vec::new();
        for m in &metrics.loaded_models {
            if streaming.contains(&m.name) {
                continue;
            }
            if ingest_nonempty && m.name == embedding_model {
                continue;
            }
            scheduled.push(m.name.clone());
        }

        for name in &scheduled {
            if let Err(e) = self.ollama.force_unload(name).await {
                tracing::warn!(
                    model = %name,
                    error = %e,
                    "governor: critical batch unload failed"
                );
            }
        }
        scheduled
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    /// `derive_risk_state` is the function P4 (risk-state monotonicity)
    /// validates — but a couple of example assertions guard against
    /// future refactors flipping a comparison operator.
    #[test]
    fn derive_risk_state_branches() {
        // Tier 1 defaults: warn=800, unload=400, critical=200.
        assert_eq!(derive_risk_state(2000, 800, 400, 200), RiskState::Calm);
        assert_eq!(derive_risk_state(800, 800, 400, 200), RiskState::Calm);
        assert_eq!(derive_risk_state(799, 800, 400, 200), RiskState::Warn);
        assert_eq!(derive_risk_state(400, 800, 400, 200), RiskState::Warn);
        assert_eq!(derive_risk_state(399, 800, 400, 200), RiskState::Unload);
        assert_eq!(derive_risk_state(200, 800, 400, 200), RiskState::Unload);
        assert_eq!(derive_risk_state(199, 800, 400, 200), RiskState::Critical);
        assert_eq!(derive_risk_state(0, 800, 400, 200), RiskState::Critical);
    }

    /// Sanity-check the threshold fallback gate: invalid input returns
    /// the per-tier defaults.
    #[test]
    fn read_thresholds_with_fallback_uses_defaults_on_invalid() {
        use crate::models::{HardwareTier, ScalarKind};

        let bad = TierConfig {
            tier: HardwareTier::Minimal,
            rag_enabled: false,
            embedding_model: "nomic-embed-text".to_string(),
            chunk_size_tokens: 256,
            chunk_overlap_tokens: 32,
            max_vectors: None,
            auto_unload_minutes: None,
            rag_top_k: 5,
            quantization: ScalarKind::F16,
            index_mmap: true,
            // Inverted: warn < unload (invalid).
            governor_warn_mb: 100,
            governor_unload_mb: 400,
            governor_critical_mb: 200,
            safe_headroom_pct: 0.80,
        };
        let warned = AtomicBool::new(false);
        let (w, u, c) = read_thresholds_with_fallback(&bad, &warned);
        assert_eq!((w, u, c), (800, 400, 200));
        assert!(warned.load(Ordering::Acquire));
    }

    /// Valid configuration passes through unchanged.
    #[test]
    fn read_thresholds_with_fallback_preserves_valid() {
        use crate::models::{HardwareTier, ScalarKind};

        let good = TierConfig {
            tier: HardwareTier::Standard,
            rag_enabled: true,
            embedding_model: "nomic-embed-text".to_string(),
            chunk_size_tokens: 512,
            chunk_overlap_tokens: 64,
            max_vectors: None,
            auto_unload_minutes: None,
            rag_top_k: 10,
            quantization: ScalarKind::F32,
            index_mmap: true,
            governor_warn_mb: 1500,
            governor_unload_mb: 800,
            governor_critical_mb: 400,
            safe_headroom_pct: 0.80,
        };
        let warned = AtomicBool::new(false);
        let (w, u, c) = read_thresholds_with_fallback(&good, &warned);
        assert_eq!((w, u, c), (1500, 800, 400));
        assert!(!warned.load(Ordering::Acquire));
    }

    // ── Task 4.2 — parse_meminfo (5 cases) ─────────────────────────────

    #[test]
    fn parse_meminfo_well_formed() {
        let text = "\
MemTotal:       16291968 kB
MemFree:         1000000 kB
MemAvailable:    8388608 kB
SwapTotal:       2097152 kB
SwapFree:        1048576 kB
";
        let (t, a, st, sf) = parse_meminfo(text).expect("well-formed parses");
        // KB / 1024 = MB.
        assert_eq!(t, 16291968 / 1024);
        assert_eq!(a, 8388608 / 1024);
        assert_eq!(st, 2097152 / 1024);
        assert_eq!(sf, 1048576 / 1024);
    }

    #[test]
    fn parse_meminfo_missing_field_returns_none() {
        // MemAvailable absent → None.
        let text = "\
MemTotal:       16291968 kB
SwapTotal:       2097152 kB
SwapFree:        1048576 kB
";
        assert!(parse_meminfo(text).is_none());
    }

    #[test]
    fn parse_meminfo_non_numeric_returns_none() {
        let text = "\
MemTotal:       sixteen kB
MemAvailable:    8388608 kB
SwapTotal:       2097152 kB
SwapFree:        1048576 kB
";
        assert!(parse_meminfo(text).is_none());
    }

    #[test]
    fn parse_meminfo_trailing_whitespace_ok() {
        // Extra spaces and a trailing blank line must not break parsing.
        let text = "MemTotal:   16291968 kB   \nMemAvailable:  8388608 kB\nSwapTotal: 2097152 kB\nSwapFree:  1048576 kB\n\n";
        let (t, a, st, sf) = parse_meminfo(text).expect("trailing whitespace parses");
        assert_eq!(t, 16291968 / 1024);
        assert_eq!(a, 8388608 / 1024);
        assert_eq!(st, 2097152 / 1024);
        assert_eq!(sf, 1048576 / 1024);
    }

    #[test]
    fn parse_meminfo_empty_returns_none() {
        assert!(parse_meminfo("").is_none());
    }

    #[test]
    fn normalize_meminfo_clamps_available_and_computes_swap_used() {
        // available > total → clamped to total; swap_used = total - free.
        let (t, a, st, su) = normalize_meminfo(1000, 9999, 500, 200);
        assert_eq!(t, 1000);
        assert_eq!(a, 1000, "available clamped to total");
        assert_eq!(st, 500);
        assert_eq!(su, 300, "swap_used = swap_total - swap_free");
    }

    #[test]
    fn normalize_meminfo_swap_free_exceeds_total_saturates() {
        let (_, _, st, su) = normalize_meminfo(1000, 500, 100, 999);
        assert_eq!(st, 100);
        assert_eq!(su, 0, "saturating_sub prevents underflow");
    }

    #[test]
    fn clamp_rss_drops_over_total() {
        assert_eq!(clamp_rss(Some(5000), 1000), None);
        assert_eq!(clamp_rss(Some(500), 1000), Some(500));
        assert_eq!(clamp_rss(None, 1000), None);
        // total == 0 (unreadable meminfo) → never clamps.
        assert_eq!(clamp_rss(Some(5000), 0), Some(5000));
    }

    #[test]
    fn clamp_self_rss_clamps_down() {
        assert_eq!(clamp_self_rss(5000, 1000), 1000);
        assert_eq!(clamp_self_rss(500, 1000), 500);
        assert_eq!(clamp_self_rss(5000, 0), 5000);
    }

    // ── Task 4.2 — compute_cpu_percent (3 cases) ───────────────────────

    fn jiffies(name: &str, total: u64, idle: u64) -> CpuJiffies {
        CpuJiffies {
            name: name.to_string(),
            total,
            idle,
        }
    }

    #[test]
    fn compute_cpu_percent_idle_dominant() {
        // total delta 1000, idle delta 900 → 10% busy.
        let s1 = vec![jiffies("cpu", 0, 0), jiffies("cpu0", 0, 0)];
        let s2 = vec![jiffies("cpu", 1000, 900), jiffies("cpu0", 1000, 900)];
        let (agg, per_core) = compute_cpu_percent(&s1, &s2);
        assert!((agg - 10.0).abs() < 0.01, "aggregate ~10%, got {agg}");
        assert_eq!(per_core.len(), 1);
        assert!((per_core[0] - 10.0).abs() < 0.01);
    }

    #[test]
    fn compute_cpu_percent_busy_dominant() {
        // total delta 1000, idle delta 100 → 90% busy.
        let s1 = vec![jiffies("cpu", 0, 0)];
        let s2 = vec![jiffies("cpu", 1000, 100)];
        let (agg, _) = compute_cpu_percent(&s1, &s2);
        assert!((agg - 90.0).abs() < 0.01, "aggregate ~90%, got {agg}");
        assert!((0.0..=100.0).contains(&agg));
    }

    #[test]
    fn compute_cpu_percent_zero_delta_is_zero_not_nan() {
        // Identical samples → total_delta 0 → 0.0, never NaN (Req 2.5).
        let s1 = vec![jiffies("cpu", 500, 250), jiffies("cpu0", 500, 250)];
        let s2 = s1.clone();
        let (agg, per_core) = compute_cpu_percent(&s1, &s2);
        assert_eq!(agg, 0.0);
        assert!(!agg.is_nan());
        for c in per_core {
            assert_eq!(c, 0.0);
            assert!(!c.is_nan());
        }
    }

    // ── Task 5.2 — pick_smallest_matching_pid + find_ollama_pid_at +
    //    parse_status_vmrss ──────────────────────────────────────────────

    #[test]
    fn pick_smallest_matching_pid_cases() {
        assert_eq!(pick_smallest_matching_pid(&[]), None);
        assert_eq!(pick_smallest_matching_pid(&[42]), Some(42));
        assert_eq!(pick_smallest_matching_pid(&[900, 17, 333]), Some(17));
    }

    /// Build a fake `/proc`-style tree under a tempdir: one directory per
    /// `(pid, comm)` pair, each containing a `comm` file.
    fn fake_proc(entries: &[(u32, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (pid, comm) in entries {
            let pid_dir = dir.path().join(pid.to_string());
            std::fs::create_dir_all(&pid_dir).expect("mkdir pid");
            std::fs::write(pid_dir.join("comm"), format!("{comm}\n"))
                .expect("write comm");
        }
        dir
    }

    #[test]
    fn find_ollama_pid_at_single_match() {
        let dir = fake_proc(&[(101, "ollama"), (202, "bash")]);
        assert_eq!(find_ollama_pid_at(dir.path()), Some(101));
    }

    #[test]
    fn find_ollama_pid_at_multi_match_picks_smallest() {
        let dir = fake_proc(&[(500, "ollama"), (123, "ollama"), (900, "ollama")]);
        assert_eq!(find_ollama_pid_at(dir.path()), Some(123));
    }

    #[test]
    fn find_ollama_pid_at_no_match() {
        let dir = fake_proc(&[(1, "systemd"), (2, "kthreadd")]);
        assert_eq!(find_ollama_pid_at(dir.path()), None);
    }

    #[test]
    fn find_ollama_pid_at_missing_root_is_none() {
        // A non-existent /proc root yields None rather than panicking
        // (models the permission-denied / sandboxed case).
        let missing = std::path::Path::new("/nonexistent-proc-root-xyz");
        assert_eq!(find_ollama_pid_at(missing), None);
    }

    #[test]
    fn parse_status_vmrss_present() {
        let text = "\
Name:\tollama
State:\tS (sleeping)
VmRSS:\t  524288 kB
Threads:\t12
";
        assert_eq!(parse_status_vmrss(text), Some(524288 / 1024));
    }

    #[test]
    fn parse_status_vmrss_absent() {
        let text = "Name:\tollama\nState:\tS\n";
        assert_eq!(parse_status_vmrss(text), None);
    }

    #[test]
    fn parse_status_vmrss_multi_line_tail() {
        // VmRSS appears amid many lines; the parser must find it anywhere.
        let text = "\
Name:\tx
VmPeak:\t999999 kB
VmSize:\t888888 kB
VmRSS:\t  1048576 kB
VmData:\t111 kB
";
        assert_eq!(parse_status_vmrss(text), Some(1048576 / 1024));
    }

    #[test]
    fn parse_status_vmrss_overflow_returns_none() {
        // A value beyond u64::MAX is unparseable → None, not a panic.
        let text = "VmRSS:\t99999999999999999999999999 kB\n";
        assert_eq!(parse_status_vmrss(text), None);
    }

    // ── Task 6.2 — read_drm_vram_at (7 cases) ──────────────────────────

    /// Build a fake `/sys/class/drm`-style tree. Each card is
    /// `(card_name, vendor, total_bytes, used_bytes)`; `None` for a byte
    /// value means the corresponding file is omitted (simulating an
    /// unreadable / missing sysfs attribute).
    fn fake_drm(
        cards: &[(&str, &str, Option<&str>, Option<&str>)],
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (card, vendor, total, used) in cards {
            let device = dir.path().join(card).join("device");
            std::fs::create_dir_all(&device).expect("mkdir device");
            std::fs::write(device.join("vendor"), format!("{vendor}\n"))
                .expect("write vendor");
            if let Some(t) = total {
                std::fs::write(device.join("mem_info_vram_total"), t)
                    .expect("write total");
            }
            if let Some(u) = used {
                std::fs::write(device.join("mem_info_vram_used"), u)
                    .expect("write used");
            }
        }
        dir
    }

    const ONE_GIB: &str = "1073741824"; // bytes
    const HALF_GIB: &str = "536870912"; // bytes

    #[test]
    fn read_drm_vram_at_no_gpu_absent() {
        // Empty drm dir → Absent.
        let dir = tempfile::tempdir().expect("tempdir");
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, None);
        assert_eq!(u, None);
        assert_eq!(status, VramStatus::Absent);
    }

    #[test]
    fn read_drm_vram_at_nvidia_only_ok() {
        let dir = fake_drm(&[("card0", "0x10de", Some(ONE_GIB), Some(HALF_GIB))]);
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, Some(1024));
        assert_eq!(u, Some(512));
        assert_eq!(status, VramStatus::Ok);
    }

    #[test]
    fn read_drm_vram_at_amd_only_ok() {
        let dir = fake_drm(&[("card0", "0x1002", Some(ONE_GIB), Some(HALF_GIB))]);
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, Some(1024));
        assert_eq!(u, Some(512));
        assert_eq!(status, VramStatus::Ok);
    }

    #[test]
    fn read_drm_vram_at_both_sums() {
        let dir = fake_drm(&[
            ("card0", "0x10de", Some(ONE_GIB), Some(HALF_GIB)),
            ("card1", "0x1002", Some(ONE_GIB), Some(HALF_GIB)),
        ]);
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, Some(2048), "totals sum across cards");
        assert_eq!(u, Some(1024), "used sums across cards");
        assert_eq!(status, VramStatus::Ok);
    }

    #[test]
    fn read_drm_vram_at_intel_only_absent() {
        // Intel iGPU (0x8086) is excluded → no identified cards → Absent.
        let dir = fake_drm(&[("card0", "0x8086", Some(ONE_GIB), Some(HALF_GIB))]);
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, None);
        assert_eq!(u, None);
        assert_eq!(status, VramStatus::Absent);
    }

    #[test]
    fn read_drm_vram_at_partial_read_unavailable() {
        // NVIDIA card identified but `used` file missing → Unavailable,
        // total still readable.
        let dir = fake_drm(&[("card0", "0x10de", Some(ONE_GIB), None)]);
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, Some(1024));
        assert_eq!(u, None);
        assert_eq!(status, VramStatus::Unavailable);
    }

    #[test]
    fn read_drm_vram_at_unparseable_unavailable() {
        // Identified card but the total bytes file is garbage → Unavailable.
        let dir = fake_drm(&[("card0", "0x1002", Some("not-a-number"), Some(HALF_GIB))]);
        let (t, u, status) = read_drm_vram_at(dir.path());
        assert_eq!(t, None, "unparseable total → None");
        assert_eq!(u, Some(512));
        assert_eq!(status, VramStatus::Unavailable);
    }
}
