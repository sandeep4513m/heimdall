//! proptest_strategies.rs — Shared `proptest` generators for the
//! Model Intelligence Registry property tests (P1–P7, tasks 5.2–5.8).
//!
//! Cargo treats every file at the top level of `tests/` as its own
//! integration test crate, which means files compiled here run as a
//! standalone binary AND get re-compiled into any sibling integration
//! test that declares `mod proptest_strategies;` at its top. The
//! `#![allow(dead_code)]` blanket below silences the unused-symbol
//! warnings that the latter mode would otherwise emit when an
//! individual test only uses a subset of the strategies.
//!
//! Every public item is exported as `pub` so sibling tests (`property_p1_*.rs`,
//! `property_p2_*.rs`, …) can reach them via `mod proptest_strategies;`
//! followed by `use proptest_strategies::*;`.

#![allow(dead_code)]

use heimdall_lib::models::{CapabilitySource, ModelCapabilities};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Public alphabet
//
// The five strings below are the *recognised* set per Requirement 3.1 and
// the design's `parse_api_show_capabilities` allowlist. Anything outside
// this set is "noise" and must be ignored when computing flags but
// preserved verbatim in `raw_capabilities`.
// ---------------------------------------------------------------------------

/// The five capability strings recognised by
/// `ModelRegistry::parse_api_show_capabilities`. Tests that need the
/// allowlist itself (rather than a generator) can `use` this constant.
pub const RECOGNISED_CAPABILITIES: &[&str] = &[
    "completion",
    "vision",
    "thinking",
    "tools",
    "embedding",
];

/// A small fixed pool of *unrecognised* capability strings used to
/// exercise the "ignore unknown, preserve verbatim" requirement
/// (Requirements 3.1.a and 3.1.b). Kept tiny on purpose: the goal is
/// to verify the registry's behaviour on known noise, not to fuzz
/// arbitrary UTF-8.
pub const UNRECOGNISED_CAPABILITIES: &[&str] = &[
    "audio",
    "foo",
    "multimodal",
    "code-interpreter",
];

// ---------------------------------------------------------------------------
// String-level strategies
// ---------------------------------------------------------------------------

/// Strategy producing one of the five recognised capability strings,
/// uniformly at random.
///
/// Used directly by tasks that need a single capability string (rare),
/// and as a building block by `prop_capability_array()`.
pub fn prop_capability_string() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("completion"),
        Just("vision"),
        Just("thinking"),
        Just("tools"),
        Just("embedding"),
    ]
}

/// Strategy producing one of the four canned unrecognised strings.
/// Internal helper for `prop_capability_array()`.
fn prop_unrecognised_capability_string() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("audio"),
        Just("foo"),
        Just("multimodal"),
        Just("code-interpreter"),
    ]
}

/// Strategy producing a single capability-array element — heavily biased
/// toward the recognised set (4 in 5) with occasional noise (1 in 5) so
/// that the unknown-string handling path is exercised on roughly 20% of
/// generated arrays without dominating the recognised-string coverage.
fn prop_capability_array_element() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop_capability_string().prop_map(String::from),
        1 => prop_unrecognised_capability_string().prop_map(String::from),
    ]
}

/// Strategy producing a `Vec<String>` modelling an Ollama
/// `/api/show.capabilities` array. Length is 0..=8 (covering both the
/// "empty array → fall through to template/heuristic" path of
/// Requirement 6.2 and arrays that exercise multiple recognised flags
/// simultaneously per Requirement 3.1). Elements may include
/// unrecognised strings such as `"audio"` or `"foo"` so the
/// "ignore unknown / preserve verbatim" branch (Requirements 3.1.a and
/// 3.1.b) is tested.
///
/// Duplicate recognised strings are allowed; per Requirement 3.1 an
/// exact match "appears one or more times in the input array" is the
/// gating condition, so duplicates must not change any flag.
pub fn prop_capability_array() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(prop_capability_array_element(), 0..=8)
}

/// Strategy producing a model digest in Ollama's canonical form:
/// the literal prefix `"sha256:"` followed by exactly 64 lowercase
/// hexadecimal characters.
///
/// Exact-shape strings (no shrinker noise) are what the registry's
/// `(model_name, digest)` lookup actually compares against, so we
/// generate them at the same shape that `/api/tags` would emit.
pub fn prop_digest() -> impl Strategy<Value = String> {
    // 64 lowercase hex chars; `unwrap` is fine because the regex is a
    // compile-time literal we control.
    proptest::string::string_regex("[0-9a-f]{64}")
        .expect("hex regex compiles")
        .prop_map(|hex| format!("sha256:{}", hex))
}

/// Strategy producing a model name in the alphabet Ollama actually
/// uses on the wire: lowercase ASCII alphanumerics plus `-`, `:`, `.`,
/// length 1..=64.
///
/// Examples that fall in-range: `gemma3`, `llama3.1:8b`,
/// `mxbai-embed-large`, `deepseek-r1:7b-instruct-q4_k_m`.
pub fn prop_model_name() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z0-9][a-z0-9\\-:.]{0,63}")
        .expect("model-name regex compiles")
}

// ---------------------------------------------------------------------------
// Composite strategies
// ---------------------------------------------------------------------------

/// Strategy producing a `CapabilitySource` value, uniform over the four
/// variants. Used by `prop_model_capabilities()` and any test that
/// needs to vary provenance independently of detection.
pub fn prop_capability_source() -> impl Strategy<Value = CapabilitySource> {
    prop_oneof![
        Just(CapabilitySource::ApiShow),
        Just(CapabilitySource::Template),
        Just(CapabilitySource::Heuristic),
        Just(CapabilitySource::UserOverride),
    ]
}

/// Strategy producing a short, plausible value for the `family`,
/// `parameter_size`, or `quantization_level` metadata fields. Used
/// inside `Option`-wrappers below so `None` is also exercised.
fn prop_short_metadata_string() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9_]{1,16}")
        .expect("metadata regex compiles")
}

/// Strategy producing a Unix-second timestamp in a sensible range —
/// roughly 1970-01-01 through 2055 — so timestamps round-trip through
/// SQLite `INTEGER` storage without sign or truncation surprises.
fn prop_timestamp() -> impl Strategy<Value = i64> {
    0_i64..2_700_000_000_i64
}

/// Strategy producing a fully populated `ModelCapabilities` value.
///
/// All five capability flags are independent booleans (so the tests
/// exercise multi-capability rows per Requirement 3.1), `digest` and
/// `model_name` come from their dedicated strategies above,
/// `raw_capabilities` is drawn from `prop_capability_array()`, and the
/// optional metadata fields each take `Some(short_string) | None`. The
/// flag values are intentionally NOT derived from `raw_capabilities`:
/// the persistence and cache properties (P1, P2, P5, P7) only require
/// arbitrary-but-stable rows, and decoupling the two surfaces lets P3
/// test the parser directly without colliding with this generator.
///
/// Returned via `BoxedStrategy` so the strategy type is nameable in
/// downstream test signatures and free of the `proptest`-internal
/// closure types that `impl Strategy` would otherwise leak.
///
/// Implementation note: `proptest`'s `Strategy` trait is only
/// implemented for tuples up to 10 elements (one of `proptest`'s known
/// quirks — see the `tuple.rs` source). The struct has 14 fields, so
/// we split the inputs into two nested tuples (identity + flags;
/// metadata) and recombine in `prop_map`. The grouping is purely
/// syntactic and does not affect the distribution of generated values.
pub fn prop_model_capabilities() -> BoxedStrategy<ModelCapabilities> {
    // Group 1: identity, capability flags, and provenance.
    let identity_and_flags = (
        prop_model_name(),
        prop_digest(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        prop_capability_source(),
    );

    // Group 2: optional metadata, raw capabilities array, and timestamps.
    let metadata_and_raw = (
        prop_capability_array(),
        proptest::option::of(prop_short_metadata_string()),
        proptest::option::of(prop_short_metadata_string()),
        proptest::option::of(prop_short_metadata_string()),
        prop_timestamp(),
        prop_timestamp(),
    );

    (identity_and_flags, metadata_and_raw)
        .prop_map(
            |(
                (
                    model_name,
                    digest,
                    completion,
                    vision,
                    thinking,
                    tools,
                    embedding,
                    capability_source,
                ),
                (
                    raw_capabilities,
                    family,
                    parameter_size,
                    quantization_level,
                    detected_at,
                    updated_at,
                ),
            )| ModelCapabilities {
                model_name,
                digest,
                completion,
                vision,
                thinking,
                tools,
                embedding,
                capability_source,
                raw_capabilities,
                family,
                parameter_size,
                quantization_level,
                detected_at,
                updated_at,
            },
        )
        .boxed()
}

// ---------------------------------------------------------------------------
// Self-tests
//
// These are not properties under test in the design — they are
// sanity checks that the strategies themselves produce values in the
// promised shape. They keep this file from rotting silently when one
// of the strategies is changed and run cheaply (16 cases each).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod self_tests {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn capability_string_in_recognised_set(s in prop_capability_string()) {
            prop_assert!(RECOGNISED_CAPABILITIES.contains(&s));
        }

        #[test]
        fn capability_array_length_in_range(arr in prop_capability_array()) {
            prop_assert!(arr.len() <= 8);
        }

        #[test]
        fn digest_has_correct_shape(d in prop_digest()) {
            prop_assert!(d.starts_with("sha256:"));
            prop_assert_eq!(d.len(), "sha256:".len() + 64);
            prop_assert!(d["sha256:".len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }

        #[test]
        fn model_name_uses_allowed_alphabet(name in prop_model_name()) {
            prop_assert!(!name.is_empty() && name.len() <= 64);
            prop_assert!(name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()
                    || c == '-' || c == ':' || c == '.'));
        }

        #[test]
        fn model_capabilities_round_trip_matches_strategy(caps in prop_model_capabilities()) {
            // Sanity: every field is populated according to the
            // surface contract of the strategies above.
            prop_assert!(!caps.model_name.is_empty());
            prop_assert!(caps.digest.starts_with("sha256:"));
            prop_assert!(caps.raw_capabilities.len() <= 8);
        }
    }
}
