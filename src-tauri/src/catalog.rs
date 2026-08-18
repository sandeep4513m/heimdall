/// catalog.rs — Phase 6 model catalog and hardware-aware recommendations
///
/// The catalog is a small curated list of well-known Ollama models bundled
/// at compile time as `resources/model_catalog.json`. Frontend uses it to
/// drive the Models tab "suggested models" picker (Run 4 / Run 5). Backend
/// loads it once during `bootstrap()` into `AppState.model_catalog` so
/// every Tauri command sees the same parsed value.
///
/// The compute_recommendation function computes a tier-aware
/// `ModelRecommendation` from a model size — used by the Models tab to
/// label each row as "fits comfortably / requires management / exceeds
/// tier" without an LLM call.
///
/// Catalog entries are deliberately conservative — only models we have
/// validated run on the named tier. Adding entries is a content edit, not
/// a code change; the JSON is parsed at startup and surfaced as-is.

use serde::{Deserialize, Serialize};

use crate::models::{HardwareInfo, HardwareTier, ModelRecommendation, TierConfig};

/// One entry in the bundled `model_catalog.json`. Wire form is the same
/// as the on-disk JSON (snake_case via `HardwareTier`'s serde rename).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Ollama model name as you would type it to `ollama pull`.
    pub name: String,
    /// On-disk size in MiB. Used both for the recommendation pass and
    /// for the rough-budget banner in the Models tab.
    pub size_mb: u64,
    /// Capability tags. Stored as plain strings rather than the
    /// `ModelCapabilities` struct because the catalog is a hint about
    /// what a fresh pull *should* be able to do — authoritative
    /// capability data lives in `ModelRegistry` once the user actually
    /// runs `/api/show` against the model.
    pub capabilities: Vec<String>,
    /// Lowest hardware tier on which this model is recommended. Models
    /// with `min_tier = "full"` will not appear in the Tier 1 picker.
    pub min_tier: HardwareTier,
}

/// The full bundled catalog. `version` is bumped whenever the schema
/// changes so the frontend can detect a stale build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub version: u32,
    pub entries: Vec<CatalogEntry>,
}

/// Tier-overhead constants for `compute_recommendation`. These approximate
/// the non-model RAM Heimdall + WebKit + Ollama need to keep running
/// alongside a loaded model — the recommendation reserves this much RAM
/// before sizing the model.
///
/// The numbers are conservative: a fresh Heimdall + WebKit on Tier 1
/// holds around 150 MB, but adding 50 MB of slack for the kernel page
/// cache and short-lived spikes during model load gives users a real
/// margin of safety. Tier 2/3 grow proportionally.
fn tier_overhead_mb(tier: HardwareTier) -> u64 {
    match tier {
        HardwareTier::Minimal => 200,
        HardwareTier::Standard => 400,
        HardwareTier::Full => 600,
    }
}

/// Hardware-aware classification — pure function (no LLM call).
///
/// Branches per design.md "Hardware-aware recommendation":
///   - `size_mb + tier_overhead < total_ram_mb / 2` → `FitsComfortably`
///   - `size_mb + tier_overhead < total_ram_mb`     → `RequiresManagement`
///   - otherwise                                    → `ExceedsTier`
///
/// The "comfortably" threshold at half-total-RAM is intentional: it
/// leaves room for a second model (e.g. an embedding model alongside a
/// chat model) to be loaded simultaneously without risk of swap
/// thrashing.
pub fn compute_recommendation(
    size_mb: u64,
    tier: &TierConfig,
    hw: &HardwareInfo,
) -> ModelRecommendation {
    let combined = size_mb.saturating_add(tier_overhead_mb(tier.tier));
    if combined < hw.total_ram_mb / 2 {
        ModelRecommendation::FitsComfortably
    } else if combined < hw.total_ram_mb {
        ModelRecommendation::RequiresManagement
    } else {
        ModelRecommendation::ExceedsTier
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ScalarKind;

    fn hw(total_ram_mb: u64) -> HardwareInfo {
        HardwareInfo {
            total_ram_mb,
            available_ram_mb: total_ram_mb / 2,
            vram_mb: None,
            cpu_cores: 4,
            detected_tier: HardwareTier::Minimal,
            effective_tier: HardwareTier::Minimal,
        }
    }

    fn tier(t: HardwareTier) -> TierConfig {
        TierConfig {
            tier: t,
            rag_enabled: false,
            embedding_model: "nomic-embed-text".into(),
            chunk_size_tokens: 256,
            chunk_overlap_tokens: 32,
            max_vectors: None,
            auto_unload_minutes: None,
            rag_top_k: 5,
            quantization: ScalarKind::F16,
            index_mmap: true,
            governor_warn_mb: 800,
            governor_unload_mb: 400,
            governor_critical_mb: 200,
            safe_headroom_pct: 0.80,
        }
    }

    #[test]
    fn fits_comfortably_under_half_ram() {
        // 4 GB box, Minimal tier overhead = 200 MB.
        // 1500 + 200 = 1700 < 2048 → FitsComfortably.
        let r = compute_recommendation(1500, &tier(HardwareTier::Minimal), &hw(4096));
        assert_eq!(r, ModelRecommendation::FitsComfortably);
    }

    #[test]
    fn requires_management_between_half_and_full() {
        // 4 GB box, Minimal tier overhead = 200 MB.
        // 2200 + 200 = 2400 → 2048 ≤ 2400 < 4096 → RequiresManagement.
        let r = compute_recommendation(2200, &tier(HardwareTier::Minimal), &hw(4096));
        assert_eq!(r, ModelRecommendation::RequiresManagement);
    }

    #[test]
    fn exceeds_tier_at_or_above_full_ram() {
        // 4 GB box, Minimal tier overhead = 200 MB.
        // 4000 + 200 = 4200 ≥ 4096 → ExceedsTier.
        let r = compute_recommendation(4000, &tier(HardwareTier::Minimal), &hw(4096));
        assert_eq!(r, ModelRecommendation::ExceedsTier);
    }

    #[test]
    fn parse_bundled_catalog() {
        // Sanity check: the bundled JSON parses and has the documented
        // 8 entries. If a future edit drops or adds entries this test
        // fails loudly so we update the design alongside the data.
        const BUNDLED: &str = include_str!("../resources/model_catalog.json");
        let cat: ModelCatalog =
            serde_json::from_str(BUNDLED).expect("bundled catalog must parse");
        assert_eq!(cat.version, 1);
        assert_eq!(cat.entries.len(), 8);
    }
}
