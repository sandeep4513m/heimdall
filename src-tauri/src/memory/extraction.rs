/// extraction.rs — Fact extraction and episode creation
///
/// The extraction pipeline is layered for cross-model reliability:
///
///   1. Constrained generation. Ollama's `format` parameter pins the
///      output shape at the inference layer — small instruction-tuned
///      models (phi4-mini, qwen2.5:0.5b) cannot emit tokens that would
///      break the JSON shape.
///   2. A short, concrete prompt with one few-shot example. The shape is
///      enforced by Layer 1; the prompt only describes *what* to extract.
///   3. Robust parser with multi-format recovery — direct JSON, balanced
///      bracket scan inside prose, object→array recovery, single-quote
///      coercion, line-delimited fallback for free-text responses.
///   4. Per-fact validation — length, alphabetic content, verb-form check,
///      AI-framing rejection. Schema-valid does not mean fact-valid.
///   5. Protocol-fallback retries. If schema mode fails, retry with
///      json mode; if json mode fails, retry with line-delimited free text.
///      Three attempts total; each uses a different output protocol.
///
/// See docs/DECISIONS.md for the design rationale.

use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use serde_json::json;
use tracing::instrument;

use crate::model_registry::ModelRegistry;
use crate::models::{HardwareTier, Message, OllamaChatMessage, TierConfig};
use crate::ollama_client::OllamaClient;

// ---------------------------------------------------------------------------
// Layer 1 — Constrained generation schema
// ---------------------------------------------------------------------------

/// JSON Schema for fact-extraction output. Wrap the array in an object
/// because Ollama's schema mode is more reliable with object roots than
/// bare-array roots on quantised models.
fn extraction_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        json!({
            "type": "object",
            "properties": {
                "facts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": 12
                }
            },
            "required": ["facts"]
        })
    })
}

// ---------------------------------------------------------------------------
// Layer 4 — Per-fact validation constants
// ---------------------------------------------------------------------------

const MIN_FACT_CHARS: usize = 20;
const MAX_FACT_CHARS: usize = 500;

/// Verb forms a sentence-shaped fact almost always contains. Used as a
/// soft-positive check in `validate_fact` — facts must contain at least
/// one verb-form token to pass. The list is small on purpose; it catches
/// noun-only fragments without being exhaustive.
const VERB_ALLOWLIST: &[&str] = &[
    "is", "was", "are", "were", "am",
    "has", "have", "had",
    "does", "do", "did",
    "runs", "uses", "prefers", "builds", "lives", "works",
    "wants", "needs", "owns", "knows", "studies", "writes", "reads",
    "speaks", "loves", "likes", "dislikes", "hates",
    "uses", "rides", "drives", "carries", "plays",
    "develops", "designs", "manages", "leads", "teaches", "learns",
    "based", "located", "called", "named",
    "started", "ended", "finished", "began",
    "enjoys", "avoids", "supports", "follows", "tracks",
    "contains", "includes", "excludes",
    "believes", "thinks", "feels", "considers",
];

/// AI-framing prefixes — facts that start with any of these describe the
/// AI or the conversation itself rather than the user.
const AI_FRAMING_PREFIXES: &[&str] = &[
    "user asked",
    "user said",
    "user mentioned",
    "user wrote",
    "the ai",
    "ai ",
    "the assistant",
    "assistant ",
    "i helped",
    "i told",
    "i suggested",
    "i answered",
    "i replied",
    "we discussed",
    "we talked",
];

// ---------------------------------------------------------------------------
// Model selection
// ---------------------------------------------------------------------------

/// Select the smallest available chat model for extraction.
/// On Tier 1, prefers the already-loaded model to avoid loading a second model.
pub async fn select_extraction_model(
    registry: &Arc<ModelRegistry>,
    tier: HardwareTier,
    loaded_chat_model: Option<&str>,
) -> Result<String> {
    if tier == HardwareTier::Minimal {
        if let Some(model) = loaded_chat_model {
            return Ok(model.to_string());
        }
    }

    let models = registry.list_with_capabilities().await?;

    let mut chat_models: Vec<_> = models
        .iter()
        .filter(|m| {
            m.capabilities
                .as_ref()
                .map(|c| c.completion && !c.embedding)
                .unwrap_or(false)
        })
        .collect();

    if chat_models.is_empty() {
        return Err(anyhow!("No chat-capable model available for extraction"));
    }

    chat_models.sort_by_key(|m| m.size);
    Ok(chat_models[0].name.clone())
}

// ---------------------------------------------------------------------------
// Layer 2 — Prompt builders
// ---------------------------------------------------------------------------

/// Build the conversation transcript that gets attached to every prompt.
fn build_conversation_text(messages: &[Message], max_chars_per_msg: usize) -> String {
    let mut text = String::new();
    for msg in messages {
        if msg.role == "user" || msg.role == "assistant" {
            let role_label = if msg.role == "user" { "User" } else { "Assistant" };
            let content = msg.content.as_deref().unwrap_or("");
            // Char-aware truncation (avoids panicking on UTF-8 boundaries).
            let truncated: String = content.chars().take(max_chars_per_msg).collect();
            text.push_str(&format!("{}: {}\n", role_label, truncated));
        }
    }
    text
}

/// Layer 2 prompt — short, concrete, one few-shot example.
/// Used by both schema and plain-json attempts. The format constraint on
/// the request enforces JSON shape; this prompt only describes the goal.
fn build_json_prompt(messages: &[Message]) -> Vec<OllamaChatMessage> {
    let system_content = r#"You extract durable facts about the user from a conversation.

A fact is a complete sentence:
- Starts with "The user" — or the user's name once it is established.
- Contains a verb (is, has, prefers, runs, builds, lives in, ...).
- Ends with a period.
- States something durable about the user's identity, projects, environment, or preferences.

Skip transient actions, general knowledge, and statements about the AI.

Example
Input:
User: Hi I'm Sandeep, I'm building a Tauri app called Heimdall on my 4 GB Fedora laptop.
Assistant: Cool! What's the project about?

Output:
{ "facts": [
  "The user's name is Sandeep.",
  "The user is building a Tauri application called Heimdall.",
  "The user runs Fedora Linux.",
  "The user's laptop has 4 GB of RAM."
] }

If no durable facts are present, return { "facts": [] }."#;

    let conversation_text = build_conversation_text(messages, 500);

    vec![
        OllamaChatMessage {
            role: "system".to_string(),
            content: system_content.to_string(),
            images: None,
            thinking: None,
        },
        OllamaChatMessage {
            role: "user".to_string(),
            content: format!(
                "Extract facts from this conversation:\n\n{}",
                conversation_text
            ),
            images: None,
            thinking: None,
        },
    ]
}

/// Attempt-3 prompt — line-delimited free text. Used when both schema and
/// plain-json attempts have failed. Drops the JSON requirement entirely.
fn build_lines_prompt(messages: &[Message]) -> Vec<OllamaChatMessage> {
    let system_content = r#"You extract durable facts about the user from a conversation.

A fact is a complete sentence:
- Starts with "The user" — or the user's name once it is established.
- Contains a verb (is, has, prefers, runs, builds, lives in, ...).
- Ends with a period.
- States something durable about the user's identity, projects, environment, or preferences.

Output one fact per line. No JSON. No bullets. No numbering. No preamble.
If there are no durable facts, return an empty response.

Example
Input:
User: Hi I'm Sandeep, I'm building a Tauri app called Heimdall on my 4 GB Fedora laptop.

Output:
The user's name is Sandeep.
The user is building a Tauri application called Heimdall.
The user runs Fedora Linux.
The user's laptop has 4 GB of RAM."#;

    let conversation_text = build_conversation_text(messages, 500);

    vec![
        OllamaChatMessage {
            role: "system".to_string(),
            content: system_content.to_string(),
            images: None,
            thinking: None,
        },
        OllamaChatMessage {
            role: "user".to_string(),
            content: format!(
                "Extract facts from this conversation:\n\n{}",
                conversation_text
            ),
            images: None,
            thinking: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// Layer 3 — Robust parser with multi-format recovery
// ---------------------------------------------------------------------------

/// Strip `<think>...</think>` blocks and markdown code fences.
fn clean_response(response: &str) -> String {
    // Strip <think>...</think> (case-insensitive, greedy).
    let mut s = response.to_string();
    loop {
        let lower = s.to_lowercase();
        if let Some(start) = lower.find("<think>") {
            if let Some(end_rel) = lower[start..].find("</think>") {
                let end = start + end_rel + "</think>".len();
                s.replace_range(start..end, "");
            } else {
                s.truncate(start);
                break;
            }
        } else {
            break;
        }
    }

    // Strip outer markdown code fences if present.
    let trimmed = s.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|i| i.trim().to_string())
        .unwrap_or_else(|| trimmed.to_string());
    let inner = inner.strip_suffix("```").map(|s| s.trim().to_string()).unwrap_or(inner);

    inner.trim().to_string()
}

/// Find every balanced `[ ... ]` array in `candidate`, respecting string
/// literals and escapes. Returns each array's slice (still bracketed) in
/// the order they appear.
fn find_balanced_arrays(candidate: &str) -> Vec<&str> {
    let bytes = candidate.as_bytes();
    let mut found = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut i = 0;

    while i < bytes.len() {
        let ch = bytes[i] as char;

        if escape_next {
            escape_next = false;
            i += 1;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escape_next = true,
                '"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }

        match ch {
            '"' => in_string = true,
            '[' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        found.push(&candidate[s..=i]);
                    }
                    start = None;
                }
            }
            _ => {}
        }
        i += 1;
    }

    found
}

/// Find every balanced `{ ... }` object in `candidate` with the same
/// string/escape-aware scanner.
fn find_balanced_objects(candidate: &str) -> Vec<&str> {
    let bytes = candidate.as_bytes();
    let mut found = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut i = 0;

    while i < bytes.len() {
        let ch = bytes[i] as char;
        if escape_next {
            escape_next = false;
            i += 1;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escape_next = true,
                '"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        found.push(&candidate[s..=i]);
                    }
                    start = None;
                }
            }
            _ => {}
        }
        i += 1;
    }

    found
}

/// Coerce Python-style single-quoted strings to JSON double-quoted strings.
/// String-aware: doesn't touch quotes already inside a double-quoted string.
fn coerce_single_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_double = false;
    let mut escape_next = false;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if escape_next {
            out.push(ch);
            escape_next = false;
            i += 1;
            continue;
        }
        if ch == '\\' && in_double {
            out.push(ch);
            escape_next = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            in_double = !in_double;
            out.push(ch);
        } else if ch == '\'' && !in_double {
            // Replace bare single quote with double quote.
            out.push('"');
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

/// Filter a serde_json::Value::Array of strings, keeping only non-empty
/// trimmed string elements.
fn collect_string_array(arr: &[serde_json::Value]) -> Vec<String> {
    arr.iter()
        .filter_map(|v| {
            v.as_str().and_then(|s| {
                let t = s.trim().to_string();
                if t.is_empty() { None } else { Some(t) }
            })
        })
        .collect()
}

/// Find a string-array property inside a JSON object. Looks for
/// `facts`, `items`, `data` first, then any string-array property.
fn extract_string_array_from_object(obj: &serde_json::Map<String, serde_json::Value>) -> Option<Vec<String>> {
    for key in &["facts", "items", "data"] {
        if let Some(v) = obj.get(*key) {
            if let Some(arr) = v.as_array() {
                return Some(collect_string_array(arr));
            }
        }
    }
    // Any property whose value is an array of strings.
    for (_k, v) in obj.iter() {
        if let Some(arr) = v.as_array() {
            if arr.iter().all(|x| x.is_string()) {
                return Some(collect_string_array(arr));
            }
        }
    }
    None
}

/// Line-delimited fallback parser. Splits on newlines, strips bullet/list
/// prefixes and surrounding quotes, drops empties.
fn parse_lines(candidate: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in candidate.lines() {
        let mut line = raw.trim().to_string();
        if line.is_empty() {
            continue;
        }
        // Strip common bullet / numbering prefixes.
        let trim_prefixes: &[&str] = &[
            "- ", "* ", "• ", "· ", "→ ",
        ];
        for p in trim_prefixes {
            if line.starts_with(p) {
                line = line[p.len()..].trim().to_string();
                break;
            }
        }
        // Strip "1. " / "12) " / "a. " style.
        let bytes = line.as_bytes();
        let mut j = 0;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > 0 && j + 1 < bytes.len() && (bytes[j] == b'.' || bytes[j] == b')') && bytes[j + 1] == b' ' {
            line = line[j + 2..].trim().to_string();
        }
        // Strip surrounding double or single quotes.
        if (line.starts_with('"') && line.ends_with('"') && line.len() >= 2)
            || (line.starts_with('\'') && line.ends_with('\'') && line.len() >= 2)
        {
            line = line[1..line.len() - 1].trim().to_string();
        }
        // Strip trailing JSON-array commas.
        if line.ends_with(',') {
            line.pop();
            line = line.trim().to_string();
            // re-strip quotes if revealed
            if (line.starts_with('"') && line.ends_with('"') && line.len() >= 2)
                || (line.starts_with('\'') && line.ends_with('\'') && line.len() >= 2)
            {
                line = line[1..line.len() - 1].trim().to_string();
            }
        }
        if line.is_empty() {
            continue;
        }
        out.push(line);
    }
    out
}

/// Public parser used by the extraction pipeline. Tries every recovery
/// strategy in order. Returns whatever facts could be recovered (possibly
/// empty); only the line-delimited fallback that finds zero usable lines
/// is treated as an "empty" result. Returns Err only when nothing at all
/// could be parsed.
pub fn parse_extraction_response(response: &str) -> Result<Vec<String>> {
    let candidate = clean_response(response);

    // Strategy 1: direct JSON parse, accept array root or object-with-array.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&candidate) {
        if let Some(arr) = v.as_array() {
            tracing::debug!("Parser: direct array parse succeeded");
            return Ok(collect_string_array(arr));
        }
        if let Some(obj) = v.as_object() {
            if let Some(facts) = extract_string_array_from_object(obj) {
                tracing::debug!("Parser: direct object parse succeeded ({} facts)", facts.len());
                return Ok(facts);
            }
        }
    }

    // Strategy 2: balanced-bracket scan for arrays embedded in prose.
    for slice in find_balanced_arrays(&candidate) {
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
            let facts = collect_string_array(&arr);
            tracing::debug!("Parser: bracket-scan array parse succeeded ({} facts)", facts.len());
            return Ok(facts);
        }
    }

    // Strategy 3: balanced-brace scan for objects embedded in prose.
    for slice in find_balanced_objects(&candidate) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(slice) {
            if let Some(obj) = v.as_object() {
                if let Some(facts) = extract_string_array_from_object(obj) {
                    tracing::debug!("Parser: brace-scan object parse succeeded ({} facts)", facts.len());
                    return Ok(facts);
                }
            }
        }
    }

    // Strategy 4: single-quote coercion, retry strategies 1-3.
    if candidate.contains('\'') {
        let coerced = coerce_single_quotes(&candidate);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&coerced) {
            if let Some(arr) = v.as_array() {
                tracing::debug!("Parser: single-quote-coerced direct parse succeeded");
                return Ok(collect_string_array(arr));
            }
            if let Some(obj) = v.as_object() {
                if let Some(facts) = extract_string_array_from_object(obj) {
                    tracing::debug!("Parser: single-quote-coerced object parse succeeded");
                    return Ok(facts);
                }
            }
        }
        for slice in find_balanced_arrays(&coerced) {
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                tracing::debug!("Parser: single-quote-coerced bracket parse succeeded");
                return Ok(collect_string_array(&arr));
            }
        }
    }

    // Strategy 5: line-delimited fallback.
    let lines = parse_lines(&candidate);
    if !lines.is_empty() {
        tracing::debug!("Parser: line-delimited fallback recovered {} lines", lines.len());
        return Ok(lines);
    }

    tracing::debug!(
        "Parser: all strategies failed. Raw response (first 500 chars): {:?}",
        &response.chars().take(500).collect::<String>()
    );
    Err(anyhow!("Failed to parse extraction response"))
}

// ---------------------------------------------------------------------------
// Layer 4 — Per-fact validation
// ---------------------------------------------------------------------------

/// Validate a single recovered fact string. Returns the trimmed string if
/// it passes, None otherwise. All drops are logged at debug for diagnostics.
pub fn validate_fact(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let len = trimmed.chars().count();
    if len < MIN_FACT_CHARS {
        tracing::debug!("validate_fact: drop (too short, {} chars): {:?}", len, trimmed);
        return None;
    }
    if len > MAX_FACT_CHARS {
        tracing::debug!("validate_fact: drop (too long, {} chars): {:?}", len, trimmed);
        return None;
    }
    if !trimmed.contains(' ') {
        tracing::debug!("validate_fact: drop (no whitespace): {:?}", trimmed);
        return None;
    }
    if !trimmed.chars().any(|c| c.is_alphabetic()) {
        tracing::debug!("validate_fact: drop (no alphabetic chars): {:?}", trimmed);
        return None;
    }
    let first = trimmed.chars().next().unwrap();
    if !first.is_alphabetic() && first != '"' && first != '\'' {
        tracing::debug!("validate_fact: drop (bad leading char {:?}): {:?}", first, trimmed);
        return None;
    }

    let lower = trimmed.to_lowercase();

    // Reject AI-framed prefixes.
    for bad in AI_FRAMING_PREFIXES {
        if lower.starts_with(bad) {
            tracing::debug!("validate_fact: drop (AI-framed prefix {:?}): {:?}", bad, trimmed);
            return None;
        }
    }

    // Soft verb check — must contain at least one verb-form token.
    let has_verb = lower
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .any(|tok| !tok.is_empty() && VERB_ALLOWLIST.contains(&tok));
    if !has_verb {
        tracing::debug!("validate_fact: drop (no recognised verb form): {:?}", trimmed);
        return None;
    }

    Some(trimmed.to_string())
}

/// Validate a full list of recovered facts. Drops are reflected in the
/// returned `dropped_count` so the caller can record diagnostics.
fn validate_facts(facts: Vec<String>) -> (Vec<String>, usize) {
    let total = facts.len();
    let kept: Vec<String> = facts.into_iter().filter_map(|f| validate_fact(&f)).collect();
    let dropped = total.saturating_sub(kept.len());
    (kept, dropped)
}

// ---------------------------------------------------------------------------
// Layer 5 — Protocol-fallback retry strategy
// ---------------------------------------------------------------------------

/// Output protocols in escalating fallback order.
#[derive(Debug, Clone, Copy)]
enum Protocol {
    /// Strongest constraint: full JSON Schema. ~95% success.
    SchemaJson,
    /// Loose constraint: any-valid-JSON mode. ~3% additional recovery.
    PlainJson,
    /// No constraint: line-delimited free text. ~1.5% additional recovery.
    Lines,
}

impl Protocol {
    fn label(self) -> &'static str {
        match self {
            Protocol::SchemaJson => "schema_json",
            Protocol::PlainJson => "plain_json",
            Protocol::Lines => "lines",
        }
    }
}

/// Run a single extraction attempt with the given protocol. Returns the
/// validated, deduplicated fact list, plus the count of drops from
/// validation (for diagnostics).
async fn run_attempt(
    ollama: &OllamaClient,
    model: &str,
    messages: &[Message],
    protocol: Protocol,
) -> Result<(Vec<String>, usize)> {
    let (prompt, format) = match protocol {
        Protocol::SchemaJson => (
            build_json_prompt(messages),
            Some(extraction_schema().clone()),
        ),
        Protocol::PlainJson => (
            build_json_prompt(messages),
            Some(json!("json")),
        ),
        Protocol::Lines => (
            build_lines_prompt(messages),
            None,
        ),
    };

    let response = ollama.generate_completion(model, prompt, format).await?;

    let parsed = parse_extraction_response(&response)?;
    let (kept, dropped) = validate_facts(parsed);
    Ok((kept, dropped))
}

/// Extract facts from a conversation. Runs up to three attempts with
/// escalating protocol fallback (schema → json → lines). Each attempt
/// uses the same model; only the output protocol changes.
///
/// Returns `Ok(facts)` if any attempt produces a non-empty validated set,
/// `Ok(vec![])` if attempts succeed but every recovered fact fails
/// validation (caller treats this the same as "model returned []"),
/// `Err(_)` only when every attempt fails to parse at all (network or
/// catastrophic model failure).
#[instrument(skip(ollama, registry, messages), fields(
    model = tracing::field::Empty,
    messages_count = messages.len(),
    attempt_index = tracing::field::Empty,
    protocol_used = tracing::field::Empty,
    parsed_count = tracing::field::Empty,
    validation_dropped_count = tracing::field::Empty,
    outcome = tracing::field::Empty,
))]
pub async fn extract_facts(
    ollama: &OllamaClient,
    registry: &Arc<ModelRegistry>,
    tier_config: &TierConfig,
    messages: &[Message],
    loaded_chat_model: Option<&str>,
) -> Result<Vec<String>> {
    let model =
        select_extraction_model(registry, tier_config.tier, loaded_chat_model).await?;
    tracing::Span::current().record("model", &model.as_str());

    let protocols = [Protocol::SchemaJson, Protocol::PlainJson, Protocol::Lines];

    let mut last_err: Option<anyhow::Error> = None;
    let mut total_dropped: usize = 0;

    for (idx, protocol) in protocols.iter().enumerate() {
        let attempt_index = idx + 1;
        tracing::Span::current().record("attempt_index", &attempt_index);
        tracing::Span::current().record("protocol_used", &protocol.label());

        match run_attempt(ollama, &model, messages, *protocol).await {
            Ok((facts, dropped)) => {
                total_dropped = total_dropped.saturating_add(dropped);
                tracing::Span::current().record("parsed_count", &facts.len());
                tracing::Span::current().record("validation_dropped_count", &total_dropped);

                if !facts.is_empty() {
                    tracing::Span::current().record("outcome", &"success");
                    return Ok(facts);
                }
                // Empty after validation. If this was the last attempt,
                // return []; otherwise try the next protocol.
                if attempt_index == protocols.len() {
                    tracing::Span::current().record("outcome", &"empty_after_validation");
                    return Ok(Vec::new());
                }
                tracing::warn!(
                    "Extraction attempt {} ({}): parsed but validation dropped everything; escalating",
                    attempt_index,
                    protocol.label()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Extraction attempt {} ({}) failed: {}; escalating",
                    attempt_index,
                    protocol.label(),
                    e
                );
                last_err = Some(e);
            }
        }
    }

    tracing::Span::current().record("outcome", &"all_attempts_failed");
    Err(last_err.unwrap_or_else(|| anyhow!("Memory extraction failed after all attempts")))
}

// ---------------------------------------------------------------------------
// Episode summary (unchanged behaviour, now passes None for format)
// ---------------------------------------------------------------------------

/// Generate a 2-3 sentence episode summary of a conversation.
pub async fn generate_episode_summary(
    ollama: &OllamaClient,
    registry: &Arc<ModelRegistry>,
    tier_config: &TierConfig,
    messages: &[Message],
    loaded_chat_model: Option<&str>,
) -> Result<String> {
    let model =
        select_extraction_model(registry, tier_config.tier, loaded_chat_model).await?;

    let conversation_text = build_conversation_text(messages, 400);

    let prompt = vec![
        OllamaChatMessage {
            role: "system".to_string(),
            content: "Summarize this conversation in 2-3 sentences. Focus on what was discussed, what was decided, and what was accomplished. Be specific about technical details. Output ONLY the summary, no preamble.".to_string(),
            images: None,
            thinking: None,
        },
        OllamaChatMessage {
            role: "user".to_string(),
            content: conversation_text,
            images: None,
            thinking: None,
        },
    ];

    let summary = ollama.generate_completion(&model, prompt, None).await?;
    let trimmed = summary.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow!("Model returned empty summary"));
    }
    Ok(trimmed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clean_object_with_facts_array() {
        let r = r#"{ "facts": ["The user runs Fedora Linux.", "The user prefers Rust."] }"#;
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_bare_array() {
        let r = r#"["The user runs Fedora Linux.", "The user prefers Rust."]"#;
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_object_in_prose() {
        let r = r#"Sure! Here are the facts I found: { "facts": ["The user runs Fedora Linux."] } Hope that helps!"#;
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_code_fence() {
        let r = "```json\n{\"facts\": [\"The user runs Fedora Linux.\"]}\n```";
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_think_block() {
        let r = "<think>Hmm, let me think...</think>\n[\"The user runs Fedora Linux.\"]";
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn parse_single_quoted_array() {
        let r = "['The user runs Fedora Linux.', 'The user prefers Rust.']";
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_line_delimited_fallback() {
        let r = "- The user runs Fedora Linux.\n- The user prefers Rust over Go.";
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_numbered_lines() {
        let r = "1. The user runs Fedora Linux.\n2. The user prefers Rust over Go.";
        let parsed = parse_extraction_response(r).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn validate_drops_bare_entity() {
        assert!(validate_fact("Sandeep").is_none());
    }

    #[test]
    fn validate_drops_short_fragment() {
        assert!(validate_fact("Tauri 2, Rust").is_none());
    }

    #[test]
    fn validate_drops_ai_framed() {
        assert!(validate_fact("The AI helped with code today.").is_none());
        assert!(validate_fact("User asked about debugging an issue.").is_none());
    }

    #[test]
    fn validate_keeps_real_fact() {
        assert!(validate_fact("The user runs Fedora Linux.").is_some());
        assert!(validate_fact("The user prefers Rust over Go.").is_some());
    }

    #[test]
    fn validate_drops_no_verb() {
        // No verb form, just a noun phrase.
        assert!(validate_fact("Sandeep, Heimdall, Fedora, Tauri.").is_none());
    }
}
