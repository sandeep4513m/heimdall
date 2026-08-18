/// adaptive_config.rs — Hardware detection and capability tier assignment
///
/// Runs at startup before any UI renders. Reads RAM, VRAM, and CPU metrics
/// via the `sysinfo` crate, assigns one of three hardware tiers, and returns
/// a TierConfig that drives all adaptive behaviour in the application.
///
/// Tier assignment:
///   Minimal  — < 6 GB total RAM, no GPU
///   Standard — 6–16 GB total RAM, optional GPU
///   Full     — 16+ GB total RAM, GPU available
///
/// The user can override the tier in config.toml. The override is respected
/// but the detected hardware info is always reported honestly in the UI.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use sysinfo::System;
use tracing::{info, instrument, warn};

use crate::models::{HardwareInfo, HardwareTier, ScalarKind, TierConfig};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MB: u64 = 1024 * 1024;

/// Threshold below which we assign Tier 1 (Minimal), in MB.
const TIER1_MAX_RAM_MB: u64 = 6 * 1024; // 6 GB

/// Threshold below which we assign Tier 2 (Standard), in MB.
const TIER2_MAX_RAM_MB: u64 = 16 * 1024; // 16 GB

// ---------------------------------------------------------------------------
// Config file structure
// ---------------------------------------------------------------------------

/// The full application configuration loaded from ~/.heimdall/config.toml.
///
/// All fields have sensible defaults so a missing or partial config.toml
/// is always valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// User-specified tier override. None = auto-detect.
    pub tier_override: Option<HardwareTier>,

    /// Ollama base URL. Defaults to http://localhost:11434.
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,

    /// Default model for text chat.
    pub default_chat_model: Option<String>,

    /// Default model for vision input.
    pub default_vision_model: Option<String>,

    /// Default embedding model for RAG.
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,

    /// Whether to enable RAG globally (can be overridden per-tier).
    #[serde(default = "default_true")]
    pub rag_enabled: bool,

    /// Auto-unload idle models. None = use tier default.
    pub auto_unload_minutes: Option<u32>,

    // ── Phase 6 additions ──
    /// Master switch for the Governor's auto-unload pass. When `Some(false)`
    /// the polling loop still emits metrics but issues zero unload requests
    /// (Req 8.5). `None` is treated as `Some(true)` for forward-compat with
    /// pre-Phase-6 `config.toml` files. Default: `Some(true)`.
    #[serde(default = "default_auto_unload_enabled")]
    pub auto_unload_enabled: Option<bool>,
    /// Per-model auto-unload toggle. Missing keys default to `true`
    /// (Req 8.6). Persisted as a TOML inline table on disk.
    #[serde(default)]
    pub auto_unload_per_model: HashMap<String, bool>,

    /// **Legendary feature flag (Task 28.1) — default OFF.** Gates the
    /// predictive ingestion-pressure preview command
    /// `governor_preview_ingestion`. When `Some(false)` or `None` the
    /// command returns a `status: "disabled"` payload without touching
    /// the Governor. Opt-in by writing `Some(true)` to `config.toml`.
    #[serde(default = "default_legendary_predictive_preview")]
    pub legendary_predictive_preview: Option<bool>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            tier_override: None,
            ollama_url: default_ollama_url(),
            default_chat_model: None,
            default_vision_model: None,
            embedding_model: default_embedding_model(),
            rag_enabled: true,
            auto_unload_minutes: None,
            auto_unload_enabled: default_auto_unload_enabled(),
            auto_unload_per_model: HashMap::new(),
            legendary_predictive_preview: default_legendary_predictive_preview(),
        }
    }
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_embedding_model() -> String {
    "nomic-embed-text".to_string()
}

fn default_true() -> bool {
    true
}

/// Default for `AppConfig.auto_unload_enabled`. Phase 6 ships with the
/// auto-unload pass enabled — users who want manual control disable it
/// via the Governor panel toggle, which writes `Some(false)` to disk.
fn default_auto_unload_enabled() -> Option<bool> {
    Some(true)
}

/// Default for `AppConfig.legendary_predictive_preview` (Task 28.1). The
/// predictive ingestion-pressure preview is a gated, opt-in feature; it
/// ships **off** so the default install behaves exactly as before. Users
/// enable it by setting `legendary_predictive_preview = true` in
/// `config.toml`.
fn default_legendary_predictive_preview() -> Option<bool> {
    Some(false)
}

// ---------------------------------------------------------------------------
// Config file I/O
// ---------------------------------------------------------------------------

/// Load the application config from ~/.heimdall/config.toml.
///
/// If the file does not exist, writes a default config and returns it.
/// If the file is malformed, logs a warning and returns the default.
#[instrument]
pub async fn load_config(config_path: &PathBuf) -> AppConfig {
    match tokio::fs::read_to_string(config_path).await {
        Ok(contents) => match toml::from_str::<AppConfig>(&contents) {
            Ok(cfg) => {
                info!("Config loaded from {}", config_path.display());
                cfg
            }
            Err(e) => {
                warn!(
                    "Config file at {} is malformed ({}), using defaults",
                    config_path.display(),
                    e
                );
                AppConfig::default()
            }
        },
        Err(_) => {
            // File doesn't exist — write defaults and return them
            let default = AppConfig::default();
            if let Err(e) = write_config(config_path, &default).await {
                warn!("Could not write default config: {}", e);
            }
            default
        }
    }
}

/// Persist the current config to disk.
pub async fn write_config(config_path: &PathBuf, config: &AppConfig) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    let toml_str = toml::to_string_pretty(config).context("Failed to serialise config")?;

    tokio::fs::write(config_path, toml_str)
        .await
        .with_context(|| format!("Failed to write config to {}", config_path.display()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 6 — auto-unload settings helpers (Task 12.2)
//
// Thin convenience wrappers around `write_config` so the Tauri commands
// in `lib.rs` (`governor_set_auto_unload_global`,
// `governor_set_auto_unload_for_model`) do not have to reach into
// `AppConfig` themselves. Reqs 8.5, 8.6, 12.4.
// ---------------------------------------------------------------------------

/// Set the global auto-unload toggle and persist `config.toml`.
///
/// `config` is mutated in place AND written to disk so callers that hold
/// the `Arc<Mutex<AppConfig>>` see the change immediately. Callers who
/// only have a read snapshot should pass a clone they own and then
/// re-acquire the lock to swap the live config — this helper does not
/// reach into `AppState` itself.
pub async fn set_auto_unload_global(
    config_path: &PathBuf,
    config: &mut AppConfig,
    enabled: bool,
) -> Result<()> {
    config.auto_unload_enabled = Some(enabled);
    write_config(config_path, config).await
}

/// Set the per-model auto-unload toggle for `name` and persist
/// `config.toml`.
///
/// Inserting `false` disables auto-unload for that single model;
/// inserting `true` re-enables it. Removing a key (not exposed here) is
/// equivalent to the default `true`.
pub async fn set_auto_unload_for_model(
    config_path: &PathBuf,
    config: &mut AppConfig,
    name: &str,
    enabled: bool,
) -> Result<()> {
    config
        .auto_unload_per_model
        .insert(name.to_string(), enabled);
    write_config(config_path, config).await
}

// ---------------------------------------------------------------------------
// Hardware detection
// ---------------------------------------------------------------------------

/// Detect hardware metrics and assign a capability tier.
///
/// This is called once at startup. The result is stored in AppState and
/// exposed to the frontend via the `get_hardware_info` Tauri command.
///
/// The returned `HardwareInfo` carries both `detected_tier` (what the box
/// actually is) and `effective_tier` (what the app behaves as, after any
/// `tier_override` from config). Phase 6 Governor surfaces both so users
/// can see when an override is active.
#[instrument]
pub fn detect_hardware(config: &AppConfig) -> HardwareInfo {
    let mut sys = System::new_all();
    sys.refresh_all();

    let total_ram_mb = sys.total_memory() / MB;
    let available_ram_mb = sys.available_memory() / MB;
    let cpu_cores = sys.cpus().len() as u32;

    let vram_mb = detect_vram_mb();

    // Assign tier based on total RAM (VRAM presence upgrades Tier 1 → Tier 2)
    let detected_tier = if total_ram_mb < TIER1_MAX_RAM_MB && vram_mb.is_none() {
        HardwareTier::Minimal
    } else if total_ram_mb < TIER2_MAX_RAM_MB {
        HardwareTier::Standard
    } else {
        HardwareTier::Full
    };

    // Respect user override; keep both numbers
    let effective_tier = config.tier_override.unwrap_or(detected_tier);

    if config.tier_override.is_some() && config.tier_override != Some(detected_tier) {
        info!(
            "Tier override active: detected={:?}, effective={:?}",
            detected_tier, effective_tier
        );
    }

    info!(
        "Hardware: {} MB RAM total, {} MB available, {} CPU cores, VRAM: {:?} MB → Tier: {:?}",
        total_ram_mb, available_ram_mb, cpu_cores, vram_mb, effective_tier
    );

    HardwareInfo {
        total_ram_mb,
        available_ram_mb,
        vram_mb,
        cpu_cores,
        detected_tier,
        effective_tier,
    }
}

/// Attempt to read VRAM from sysfs.
///
/// Returns None if no GPU is detected or if the read fails.
/// This is best-effort — failure is not an error.
///
/// **Intel iGPUs are intentionally not surfaced**: Ollama cannot use them
/// for inference (no Vulkan/CUDA path exists in stable Ollama). Reporting
/// non-zero VRAM for an iGPU would cause Heimdall to promote a 4 GB laptop
/// to Standard tier and quietly disappoint. Re-evaluate when Ollama gains
/// Vulkan support.
fn detect_vram_mb() -> Option<u64> {
    // Try NVIDIA first via sysfs
    if let Some(vram) = read_nvidia_sysfs_vram() {
        return Some(vram);
    }

    // Try AMD via sysfs
    if let Some(vram) = read_amd_sysfs_vram() {
        return Some(vram);
    }

    None
}

/// Read NVIDIA VRAM from /sys/class/drm/card*/device/mem_info_vram_total
fn read_nvidia_sysfs_vram() -> Option<u64> {
    // Walk /sys/class/drm/ looking for NVIDIA cards
    let drm_path = std::path::Path::new("/sys/class/drm");
    if !drm_path.exists() {
        return None;
    }

    let entries = std::fs::read_dir(drm_path).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // card0, card1, etc. (not renderD128 etc.)
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }

        // Check for NVIDIA vendor
        let vendor_path = entry.path().join("device/vendor");
        if let Ok(vendor) = std::fs::read_to_string(&vendor_path) {
            // NVIDIA PCI vendor ID is 0x10de
            if !vendor.trim().eq_ignore_ascii_case("0x10de") {
                continue;
            }
        } else {
            continue;
        }

        // Read total VRAM
        let vram_path = entry.path().join("device/mem_info_vram_total");
        if let Ok(contents) = std::fs::read_to_string(&vram_path) {
            if let Ok(bytes) = contents.trim().parse::<u64>() {
                return Some(bytes / MB);
            }
        }
    }

    None
}

/// Read AMD VRAM from /sys/class/drm/card*/device/mem_info_vram_total
fn read_amd_sysfs_vram() -> Option<u64> {
    let drm_path = std::path::Path::new("/sys/class/drm");
    if !drm_path.exists() {
        return None;
    }

    let entries = std::fs::read_dir(drm_path).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }

        // Check for AMD vendor (0x1002)
        let vendor_path = entry.path().join("device/vendor");
        if let Ok(vendor) = std::fs::read_to_string(&vendor_path) {
            if !vendor.trim().eq_ignore_ascii_case("0x1002") {
                continue;
            }
        } else {
            continue;
        }

        let vram_path = entry.path().join("device/mem_info_vram_total");
        if let Ok(contents) = std::fs::read_to_string(&vram_path) {
            if let Ok(bytes) = contents.trim().parse::<u64>() {
                return Some(bytes / MB);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tier config derivation
// ---------------------------------------------------------------------------

/// Build the per-tier configuration from detected hardware and user config.
///
/// User overrides in AppConfig take precedence over tier defaults.
/// Uses `effective_tier` so a user who set `tier_override = "standard"` on
/// a 4 GB box gets Standard tier values (chunk size, embedding, etc).
pub fn build_tier_config(hardware: &HardwareInfo, config: &AppConfig) -> TierConfig {
    let base = base_tier_config(hardware.effective_tier);

    // Apply user overrides
    TierConfig {
        tier: hardware.effective_tier,
        rag_enabled: config.rag_enabled && base.rag_enabled,
        embedding_model: config.embedding_model.clone(),
        auto_unload_minutes: config.auto_unload_minutes.or(base.auto_unload_minutes),
        ..base
    }
}

/// Return the default TierConfig for a given tier.
fn base_tier_config(tier: HardwareTier) -> TierConfig {
    match tier {
        HardwareTier::Minimal => TierConfig {
            tier,
            rag_enabled: false,
            embedding_model: "nomic-embed-text".to_string(),
            chunk_size_tokens: 256,
            chunk_overlap_tokens: 32,
            max_vectors: Some(10_000),
            auto_unload_minutes: Some(2),
            rag_top_k: 5,
            quantization: ScalarKind::F16,
            index_mmap: true,
            // ── Phase 6 — Tier 1 governor thresholds (Req 6.6) ──
            governor_warn_mb: 800,
            governor_unload_mb: 400,
            governor_critical_mb: 200,
            safe_headroom_pct: 0.80,
        },
        HardwareTier::Standard => TierConfig {
            tier,
            rag_enabled: true,
            embedding_model: "nomic-embed-text".to_string(),
            chunk_size_tokens: 512,
            chunk_overlap_tokens: 64,
            max_vectors: Some(100_000),
            auto_unload_minutes: Some(10),
            rag_top_k: 10,
            quantization: ScalarKind::F32,
            index_mmap: true,
            // ── Phase 6 — Tier 2 governor thresholds (Req 6.6) ──
            governor_warn_mb: 1500,
            governor_unload_mb: 800,
            governor_critical_mb: 400,
            safe_headroom_pct: 0.80,
        },
        HardwareTier::Full => TierConfig {
            tier,
            rag_enabled: true,
            embedding_model: "nomic-embed-text".to_string(),
            chunk_size_tokens: 1024,
            chunk_overlap_tokens: 128,
            max_vectors: None, // unlimited
            auto_unload_minutes: None, // user preference
            rag_top_k: 10,
            quantization: ScalarKind::F32,
            index_mmap: false,
            // ── Phase 6 — Tier 3 governor thresholds (Req 6.6) ──
            governor_warn_mb: 2000,
            governor_unload_mb: 1000,
            governor_critical_mb: 500,
            safe_headroom_pct: 0.80,
        },
    }
}

// ---------------------------------------------------------------------------
// Heimdall data directory helpers
// ---------------------------------------------------------------------------

/// Return the path to the Heimdall data directory: ~/.heimdall/
pub fn heimdall_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".heimdall"))
}

/// Return the path to the SQLite database: ~/.heimdall/db/heimdall.db
pub fn db_path() -> Result<PathBuf> {
    Ok(heimdall_dir()?.join("db").join("heimdall.db"))
}

/// Return the path to the config file: ~/.heimdall/config.toml
pub fn config_path() -> Result<PathBuf> {
    Ok(heimdall_dir()?.join("config.toml"))
}

/// Return the path to the vector store directory: ~/.heimdall/vectors/
pub fn vectors_dir() -> Result<PathBuf> {
    Ok(heimdall_dir()?.join("vectors"))
}

/// Return the path to the knowledge directory: ~/.heimdall/knowledge/
pub fn knowledge_dir() -> Result<PathBuf> {
    Ok(heimdall_dir()?.join("knowledge"))
}

/// Return the path to the log file: ~/.heimdall/logs/heimdall.log
pub fn log_path() -> Result<PathBuf> {
    Ok(heimdall_dir()?.join("logs").join("heimdall.log"))
}

/// Ensure all required Heimdall directories exist.
///
/// Called once at startup before any other initialisation.
pub async fn ensure_dirs() -> Result<()> {
    let base = heimdall_dir()?;
    let dirs = [
        base.join("db"),
        base.join("vectors"),
        base.join("knowledge"),
        base.join("logs"),
    ];

    for dir in &dirs {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    }

    info!("Heimdall directories verified at {}", base.display());
    Ok(())
}
