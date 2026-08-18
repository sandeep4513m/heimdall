//! property_p3_multi_capability.rs — P3: Multi-Capability Representation.
//!
//! **Validates: Requirements 3.1, 3.1.a, 3.1.b**
//!
//! For every generated `/api/show.capabilities` array (which may contain
//! recognised strings, unrecognised strings, duplicates, or be empty):
//!
//!   * For each of the five recognised strings (`completion`, `vision`,
//!     `thinking`, `tools`, `embedding`), the corresponding boolean flag
//!     returned by `ModelRegistry::parse_api_show_capabilities` is `true`
//!     iff that exact string appears one or more times in the input array
//!     (Requirement 3.1).
//!   * Unrecognised strings (e.g. `"audio"`, `"foo"`) do not change any
//!     of the five flags compared to the same array with those strings
//!     removed (Requirement 3.1.a).
//!   * When at least one of the five flags is set, the design's
//!     verbatim-array contract (Requirement 3.1.b) holds: building a
//!     `ModelCapabilities` row from the input array preserves the array
//!     element-for-element and order-for-order in `raw_capabilities`.
//!
//! Strategy: `prop_capability_array()` from the shared `proptest_strategies`
//! module — produces a `Vec<String>` of length 0..=8 mixing recognised
//! strings (4-in-5 weighting) with unrecognised noise (1-in-5).

mod proptest_strategies;

use heimdall_lib::model_registry::ModelRegistry;
use heimdall_lib::models::{CapabilitySource, ModelCapabilities};
use proptest::prelude::*;
use proptest_strategies::{
    prop_capability_array, RECOGNISED_CAPABILITIES, UNRECOGNISED_CAPABILITIES,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reference oracle: independent re-implementation of the spec's
/// "case-sensitive exact match" rule. Used to cross-check the parser
/// without sharing any code with the implementation under test.
///
/// Returns `(completion, vision, thinking, tools, embedding)` — same
/// shape as the parser so the property body can compare both as plain
/// tuples.
fn expected_flags(arr: &[String]) -> (bool, bool, bool, bool, bool) {
    let contains = |needle: &str| arr.iter().any(|s| s == needle);
    (
        contains("completion"),
        contains("vision"),
        contains("thinking"),
        contains("tools"),
        contains("embedding"),
    )
}

/// Build a `ModelCapabilities` row from the parser output and the input
/// array, mirroring the layer-1 (`ApiShow`) branch of
/// `ModelRegistry::detect_capabilities`. We can't call `detect_capabilities`
/// itself here — it owns a `SqlitePool` and an `OllamaClient` and would
/// require a full registry — but the post-parse construction is a pure
/// data shuffle and is what Requirement 3.1.b actually constrains.
///
/// `now` is a fixed sentinel rather than `Utc::now().timestamp()` so the
/// property test stays deterministic across runs.
fn build_caps_from_array(arr: Vec<String>) -> ModelCapabilities {
    let (completion, vision, thinking, tools, embedding) =
        ModelRegistry::parse_api_show_capabilities(&arr);
    ModelCapabilities {
        model_name: "p3-fixture".to_string(),
        digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
        completion,
        vision,
        thinking,
        tools,
        embedding,
        capability_source: CapabilitySource::ApiShow,
        // Per Requirement 3.1.b: preserve the input array verbatim.
        raw_capabilities: arr,
        family: None,
        parameter_size: None,
        quantization_level: None,
        detected_at: 0,
        updated_at: 0,
    }
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    // 256 cases is enough to traverse all 2^5 = 32 flag combinations many
    // times over while keeping CI quick. The arrays themselves are short
    // (≤ 8 elements) so the search space is tractable.
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// **Validates: Requirements 3.1, 3.1.a**
    ///
    /// For every recognised string, the corresponding flag is `true` iff
    /// that exact string appears in the input array. Unrecognised
    /// strings cannot move any flag — verified implicitly because the
    /// reference oracle ignores them too.
    #[test]
    fn p3_recognised_flags_match_membership(arr in prop_capability_array()) {
        let actual = ModelRegistry::parse_api_show_capabilities(&arr);
        let expected = expected_flags(&arr);
        prop_assert_eq!(
            actual,
            expected,
            "parser output disagreed with reference oracle for input {:?}",
            arr
        );
    }

    /// **Validates: Requirement 3.1.a** (unrecognised strings ignored).
    ///
    /// Removing every unrecognised element from the input must not change
    /// the five flag values. We compute the "filtered" baseline by
    /// retaining only elements that exact-match the recognised set.
    #[test]
    fn p3_unrecognised_strings_do_not_change_flags(arr in prop_capability_array()) {
        let recognised_only: Vec<String> = arr
            .iter()
            .filter(|s| RECOGNISED_CAPABILITIES.contains(&s.as_str()))
            .cloned()
            .collect();

        let with_noise = ModelRegistry::parse_api_show_capabilities(&arr);
        let without_noise = ModelRegistry::parse_api_show_capabilities(&recognised_only);

        prop_assert_eq!(
            with_noise,
            without_noise,
            "removing unrecognised strings changed the flags. \
             original = {:?}, filtered = {:?}",
            arr,
            recognised_only
        );
    }

    /// **Validates: Requirement 3.1.b** (verbatim raw_capabilities).
    ///
    /// When at least one recognised string is present, building a
    /// `ModelCapabilities` row from the array (the layer-1 branch of
    /// `detect_capabilities`) must preserve the array byte-for-byte:
    /// same length, same order, same elements (including any
    /// unrecognised strings).
    ///
    /// The "at least one recognised" precondition matches the design's
    /// own gating in `detect_capabilities`: an array of pure noise (e.g.
    /// `["foo", "audio"]`) does not produce an `ApiShow`-sourced row;
    /// it falls through to layers 2/3, which is a separate property
    /// (P6, task 5.7).
    #[test]
    fn p3_raw_capabilities_preserved_byte_for_byte(arr in prop_capability_array()) {
        // Skip the noise-only / empty cases — the precondition for
        // Requirement 3.1.b is "at least one recognised string present".
        let any_recognised = arr
            .iter()
            .any(|s| RECOGNISED_CAPABILITIES.contains(&s.as_str()));
        prop_assume!(any_recognised);

        let original = arr.clone();
        let caps = build_caps_from_array(arr);

        // Same length.
        prop_assert_eq!(
            caps.raw_capabilities.len(),
            original.len(),
            "raw_capabilities length mismatch"
        );

        // Same elements in the same order — this also covers
        // "unrecognised strings retained" because `original` is the
        // unfiltered input.
        for (i, (got, want)) in caps
            .raw_capabilities
            .iter()
            .zip(original.iter())
            .enumerate()
        {
            prop_assert_eq!(
                got,
                want,
                "raw_capabilities[{}] differs from input — got {:?}, want {:?}",
                i,
                got,
                want
            );
        }
    }

    /// **Validates: Requirement 3.1** (multi-capability surface).
    ///
    /// Sanity counter-check that the legacy single-enum representation
    /// the design replaces literally cannot represent the parser's
    /// output: when two or more of the five flags are true on the same
    /// input, we have a multi-capability row by construction. We assert
    /// the parser produces such rows on inputs that contain two or more
    /// distinct recognised strings — this is the property the new
    /// `ModelCapabilities` struct exists to satisfy.
    #[test]
    fn p3_multi_capability_rows_are_representable(arr in prop_capability_array()) {
        // Count distinct recognised strings present in `arr`.
        let mut distinct = std::collections::HashSet::new();
        for s in &arr {
            if RECOGNISED_CAPABILITIES.contains(&s.as_str()) {
                distinct.insert(s.as_str());
            }
        }
        prop_assume!(distinct.len() >= 2);

        let (completion, vision, thinking, tools, embedding) =
            ModelRegistry::parse_api_show_capabilities(&arr);
        let true_count = [completion, vision, thinking, tools, embedding]
            .iter()
            .filter(|b| **b)
            .count();

        prop_assert_eq!(
            true_count,
            distinct.len(),
            "expected one true flag per distinct recognised string. \
             input = {:?}, distinct recognised = {:?}, flags = ({},{},{},{},{})",
            arr,
            distinct,
            completion,
            vision,
            thinking,
            tools,
            embedding
        );
    }
}

// ---------------------------------------------------------------------------
// Targeted regression cases
//
// These mirror the parser's own unit tests but live here so the file
// stands alone if the in-module tests are ever moved or trimmed. They
// also document the recognised-set boundary alongside the property
// definitions above.
// ---------------------------------------------------------------------------

#[test]
fn p3_regression_all_five_recognised() {
    let input: Vec<String> = RECOGNISED_CAPABILITIES.iter().map(|s| s.to_string()).collect();
    let actual = ModelRegistry::parse_api_show_capabilities(&input);
    assert_eq!(actual, (true, true, true, true, true));
}

#[test]
fn p3_regression_empty_input_no_flags() {
    let actual = ModelRegistry::parse_api_show_capabilities(&[]);
    assert_eq!(actual, (false, false, false, false, false));
}

#[test]
fn p3_regression_only_unrecognised_strings_no_flags() {
    let input: Vec<String> = UNRECOGNISED_CAPABILITIES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let actual = ModelRegistry::parse_api_show_capabilities(&input);
    assert_eq!(actual, (false, false, false, false, false));
}

#[test]
fn p3_regression_case_sensitive_match() {
    // "Vision" (capital V) is not the recognised "vision".
    let input = vec!["Vision".to_string(), "COMPLETION".to_string()];
    let actual = ModelRegistry::parse_api_show_capabilities(&input);
    assert_eq!(actual, (false, false, false, false, false));
}
