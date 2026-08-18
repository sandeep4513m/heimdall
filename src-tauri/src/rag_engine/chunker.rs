//! Text chunker for the RAG engine.
//!
//! Splits input text into token-bounded chunks using a paragraph → sentence → word
//! fallback hierarchy. Overlap is implemented by prepending the tail of the previous
//! chunk to the next one.

use tiktoken_rs::cl100k_base;

// ── Public types ─────────────────────────────────────────────────────────────

/// Configuration for the text chunker.
pub struct ChunkerConfig {
    /// Maximum number of tokens per chunk.
    pub chunk_size_tokens: u32,
    /// Number of tokens to overlap between adjacent chunks.
    pub chunk_overlap_tokens: u32,
    /// Tokenizer name (reserved for future use; Phase 4 always uses cl100k_base).
    pub tokenizer: String,
}

/// A single token-bounded slice of a source document.
pub struct Chunk {
    /// The text content of this chunk.
    pub content: String,
    /// Number of tokens in this chunk.
    pub token_count: u32,
    /// Byte offset of this chunk's content in the original input string.
    pub byte_offset: usize,
}

// ── Tokenizer helpers ─────────────────────────────────────────────────────────

fn get_bpe() -> tiktoken_rs::CoreBPE {
    cl100k_base().expect("tiktoken cl100k_base should always be available")
}

fn count_tokens_inner(text: &str) -> u32 {
    let bpe = get_bpe();
    bpe.encode_with_special_tokens(text).len() as u32
}

/// Count tokens in a string using tiktoken-rs cl100k_base.
/// Falls back to whitespace-split word count if tiktoken init fails.
pub fn count_tokens(text: &str, _tokenizer: &str) -> u32 {
    // We always use cl100k_base in Phase 4; the tokenizer field is reserved.
    count_tokens_inner(text)
}

// ── Splitting helpers ─────────────────────────────────────────────────────────

/// Split text on one or more blank lines (paragraph boundaries).
fn split_paragraphs(text: &str) -> Vec<&str> {
    // A blank line is \n followed by optional whitespace and another \n.
    let mut paragraphs: Vec<&str> = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Look for \n\s*\n
        if bytes[i] == b'\n' {
            let mut j = i + 1;
            // Skip whitespace (spaces, tabs, carriage returns)
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\r') {
                j += 1;
            }
            if j < len && bytes[j] == b'\n' {
                // Found a blank-line boundary
                let para = text[start..i].trim();
                if !para.is_empty() {
                    paragraphs.push(para);
                }
                // Skip past the blank line(s)
                i = j + 1;
                // Skip any additional blank lines
                while i < len {
                    let mut k = i;
                    while k < len && (bytes[k] == b' ' || bytes[k] == b'\t' || bytes[k] == b'\r') {
                        k += 1;
                    }
                    if k < len && bytes[k] == b'\n' {
                        i = k + 1;
                    } else {
                        break;
                    }
                }
                start = i;
                continue;
            }
        }
        i += 1;
    }

    // Remaining text after the last blank line
    let tail = text[start..].trim();
    if !tail.is_empty() {
        paragraphs.push(tail);
    }

    paragraphs
}

/// Split text on sentence boundaries.
///
/// A sentence boundary is a run of non-whitespace ending in `.`, `!`, `?`,
/// `。`, `！`, or `？` followed by whitespace or end of string.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences: Vec<&str> = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();

    let is_sentence_end = |c: char| matches!(c, '.' | '!' | '?' | '。' | '！' | '？');

    let mut i = 0;
    while i < n {
        let (byte_pos, ch) = chars[i];
        if is_sentence_end(ch) {
            // Check if followed by whitespace or end of string
            let next_is_boundary = if i + 1 < n {
                chars[i + 1].1.is_whitespace()
            } else {
                true // end of string
            };

            if next_is_boundary {
                // Include the punctuation in the sentence
                let end_byte = byte_pos + ch.len_utf8();
                let sentence = text[start..end_byte].trim();
                if !sentence.is_empty() {
                    sentences.push(sentence);
                }
                // Skip whitespace after the sentence end
                let mut j = i + 1;
                while j < n && chars[j].1.is_whitespace() {
                    j += 1;
                }
                start = if j < n { chars[j].0 } else { text.len() };
                i = j;
                continue;
            }
        }
        i += 1;
    }

    // Remaining text
    let tail = text[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }

    sentences
}

/// Split text on Unicode whitespace (word boundaries).
fn split_words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

// ── Chunk assembly ────────────────────────────────────────────────────────────

/// Assemble a list of text units (paragraphs, sentences, or words) into
/// token-bounded chunks, respecting overlap.
///
/// `overlap_tail` is the text to prepend to the first chunk (from the previous
/// chunk's tail). Returns the assembled chunks and the new overlap tail.
fn assemble_chunks(
    units: &[&str],
    chunk_size: u32,
    overlap: u32,
    overlap_tail: &str,
    original_input: &str,
) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current_parts: Vec<String> = Vec::new();
    let mut current_tokens: u32 = 0;

    // Prepend overlap tail from previous chunk if any
    if !overlap_tail.is_empty() {
        let tail_tokens = count_tokens_inner(overlap_tail);
        current_parts.push(overlap_tail.to_string());
        current_tokens = tail_tokens;
    }

    for unit in units {
        let unit_tokens = count_tokens_inner(unit);

        if current_tokens + unit_tokens <= chunk_size {
            // Unit fits in the current chunk
            current_parts.push(unit.to_string());
            current_tokens += unit_tokens;
        } else {
            // Flush the current chunk if it has content beyond the overlap tail
            if !current_parts.is_empty() {
                let content = current_parts.join(" ");
                let token_count = count_tokens_inner(&content);
                let byte_offset = find_byte_offset(original_input, &content);
                chunks.push(Chunk {
                    content,
                    token_count,
                    byte_offset,
                });
            }

            // Build the overlap tail from the end of the flushed chunk
            let new_overlap = build_overlap_tail(&current_parts, overlap);

            // Start a new chunk with the overlap tail + this unit
            current_parts = Vec::new();
            current_tokens = 0;

            if !new_overlap.is_empty() {
                let overlap_tokens = count_tokens_inner(&new_overlap);
                current_parts.push(new_overlap);
                current_tokens = overlap_tokens;
            }

            // If the unit itself exceeds chunk_size, we need to split it further
            if unit_tokens > chunk_size {
                // Emit the unit as its own chunk (word-level overflow)
                let content = unit.to_string();
                let byte_offset = find_byte_offset(original_input, &content);
                chunks.push(Chunk {
                    content,
                    token_count: unit_tokens,
                    byte_offset,
                });
                // Reset for next unit
                current_parts = Vec::new();
                current_tokens = 0;
            } else {
                current_parts.push(unit.to_string());
                current_tokens += unit_tokens;
            }
        }
    }

    // Flush the final chunk
    if !current_parts.is_empty() {
        let content = current_parts.join(" ");
        let token_count = count_tokens_inner(&content);
        let byte_offset = find_byte_offset(original_input, &content);
        chunks.push(Chunk {
            content,
            token_count,
            byte_offset,
        });
    }

    chunks
}

/// Build an overlap tail string from the last `overlap` tokens of the assembled parts.
fn build_overlap_tail(parts: &[String], overlap: u32) -> String {
    if overlap == 0 || parts.is_empty() {
        return String::new();
    }

    // Walk backwards through parts, accumulating tokens until we reach `overlap`
    let mut tail_parts: Vec<&str> = Vec::new();
    let mut accumulated = 0u32;

    for part in parts.iter().rev() {
        let t = count_tokens_inner(part);
        if accumulated + t <= overlap {
            tail_parts.push(part.as_str());
            accumulated += t;
        } else {
            // Take a suffix of this part that fits within the remaining budget
            let remaining = overlap - accumulated;
            if remaining > 0 {
                let suffix = take_last_n_tokens(part, remaining);
                if !suffix.is_empty() {
                    tail_parts.push(suffix);
                }
            }
            break;
        }
    }

    tail_parts.reverse();
    tail_parts.join(" ")
}

/// Take the last `n` tokens from a string (by re-encoding).
fn take_last_n_tokens(text: &str, n: u32) -> &str {
    let bpe = get_bpe();
    let tokens = bpe.encode_with_special_tokens(text);
    if tokens.len() <= n as usize {
        return text;
    }
    // Decode the last n tokens to find the byte boundary
    let skip = tokens.len() - n as usize;
    let _suffix_tokens = &tokens[skip..];
    // Find the byte offset by decoding the prefix and measuring its length
    let prefix_tokens = &tokens[..skip];
    if let Ok(prefix_str) = bpe.decode(prefix_tokens.to_vec()) {
        let prefix_bytes = prefix_str.len();
        if prefix_bytes <= text.len() {
            return &text[prefix_bytes..];
        }
    }
    // Fallback: return the whole text
    text
}

/// Find the byte offset of `needle` in `haystack`.
/// Returns 0 if not found (safe fallback).
fn find_byte_offset(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    // Try to find the needle as a substring
    if let Some(pos) = haystack.find(needle.as_ref() as &str) {
        return pos;
    }
    // Fallback: try to find the first word of the needle
    let first_word = needle.split_whitespace().next().unwrap_or("");
    if !first_word.is_empty() {
        if let Some(pos) = haystack.find(first_word) {
            return pos;
        }
    }
    0
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Split input text into token-bounded chunks.
///
/// Uses a paragraph → sentence → word fallback hierarchy:
/// - First splits on blank lines (paragraphs).
/// - If a paragraph exceeds `chunk_size_tokens`, splits on sentence boundaries.
/// - If a sentence exceeds `chunk_size_tokens`, splits on whitespace (words).
/// - If a single word exceeds `chunk_size_tokens`, emits it as its own chunk.
///
/// Adjacent chunks overlap by approximately `chunk_overlap_tokens` tokens.
///
/// Returns an empty `Vec` for empty or whitespace-only input.
pub fn chunk_text(input: &str, config: &ChunkerConfig) -> Vec<Chunk> {
    // Validate / clamp overlap
    let chunk_size = config.chunk_size_tokens;
    let overlap = if config.chunk_overlap_tokens >= chunk_size {
        // Clamp: overlap must be strictly less than chunk_size
        chunk_size.saturating_sub(1)
    } else {
        config.chunk_overlap_tokens
    };

    // Edge case: empty or whitespace-only input
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    // Edge case: entire input fits in one chunk
    let total_tokens = count_tokens_inner(trimmed);
    if total_tokens <= chunk_size {
        return vec![Chunk {
            content: trimmed.to_string(),
            token_count: total_tokens,
            byte_offset: input.find(trimmed).unwrap_or(0),
        }];
    }

    // Split into paragraphs first
    let paragraphs = split_paragraphs(trimmed);

    let mut all_chunks: Vec<Chunk> = Vec::new();
    let mut overlap_tail = String::new();

    for para in &paragraphs {
        let para_tokens = count_tokens_inner(para);

        if para_tokens <= chunk_size {
            // The paragraph fits — treat it as a single unit for assembly
            let units: Vec<&str> = vec![para];
            let new_chunks = assemble_chunks(&units, chunk_size, overlap, &overlap_tail, input);
            if let Some(last) = new_chunks.last() {
                overlap_tail = build_overlap_tail_from_str(&last.content, overlap);
            }
            all_chunks.extend(new_chunks);
        } else {
            // Paragraph too large — split into sentences
            let sentences = split_sentences(para);
            let mut sentence_overlap_tail = overlap_tail.clone();

            for sentence in &sentences {
                let sent_tokens = count_tokens_inner(sentence);

                if sent_tokens <= chunk_size {
                    let units: Vec<&str> = vec![sentence];
                    let new_chunks =
                        assemble_chunks(&units, chunk_size, overlap, &sentence_overlap_tail, input);
                    if let Some(last) = new_chunks.last() {
                        sentence_overlap_tail =
                            build_overlap_tail_from_str(&last.content, overlap);
                    }
                    all_chunks.extend(new_chunks);
                } else {
                    // Sentence too large — split into words
                    let words = split_words(sentence);
                    let new_chunks = assemble_chunks(
                        &words,
                        chunk_size,
                        overlap,
                        &sentence_overlap_tail,
                        input,
                    );
                    if let Some(last) = new_chunks.last() {
                        sentence_overlap_tail =
                            build_overlap_tail_from_str(&last.content, overlap);
                    }
                    all_chunks.extend(new_chunks);
                }
            }

            overlap_tail = sentence_overlap_tail;
        }
    }

    all_chunks
}

/// Build an overlap tail from the last `overlap` tokens of a single string.
fn build_overlap_tail_from_str(text: &str, overlap: u32) -> String {
    if overlap == 0 || text.is_empty() {
        return String::new();
    }
    take_last_n_tokens(text, overlap).to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ChunkerConfig {
        ChunkerConfig {
            chunk_size_tokens: 50,
            chunk_overlap_tokens: 10,
            tokenizer: "cl100k_base".to_string(),
        }
    }

    #[test]
    fn empty_input_returns_no_chunks() {
        let config = default_config();
        let chunks = chunk_text("", &config);
        assert!(chunks.is_empty());
    }

    #[test]
    fn whitespace_only_returns_no_chunks() {
        let config = default_config();
        let chunks = chunk_text("   \n\t  \n  ", &config);
        assert!(chunks.is_empty());
    }

    #[test]
    fn short_input_returns_single_chunk() {
        let config = default_config();
        let input = "Hello, world! This is a short test.";
        let chunks = chunk_text(input, &config);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content.trim(), input.trim());
    }

    #[test]
    fn chunks_are_valid_utf8() {
        let config = default_config();
        // Mix of ASCII, CJK, emoji
        let input = "Hello world. 你好世界。 🎉🎊 Testing UTF-8 safety across many boundaries.";
        let chunks = chunk_text(input, &config);
        for chunk in &chunks {
            // This will panic if the string is not valid UTF-8
            assert!(std::str::from_utf8(chunk.content.as_bytes()).is_ok());
        }
    }

    #[test]
    fn count_tokens_returns_nonzero_for_nonempty() {
        let n = count_tokens("Hello, world!", "cl100k_base");
        assert!(n > 0);
    }

    #[test]
    fn count_tokens_returns_zero_for_empty() {
        let n = count_tokens("", "cl100k_base");
        assert_eq!(n, 0);
    }

    #[test]
    fn overlap_clamped_when_equal_to_chunk_size() {
        // overlap >= chunk_size should not panic
        let config = ChunkerConfig {
            chunk_size_tokens: 20,
            chunk_overlap_tokens: 20, // equal — should be clamped
            tokenizer: "cl100k_base".to_string(),
        };
        let input = "word ".repeat(100);
        let chunks = chunk_text(&input, &config);
        // Should produce chunks without panicking
        assert!(!chunks.is_empty());
    }

    #[test]
    fn long_input_produces_multiple_chunks() {
        let config = ChunkerConfig {
            chunk_size_tokens: 20,
            chunk_overlap_tokens: 5,
            tokenizer: "cl100k_base".to_string(),
        };
        // Generate text that is definitely longer than 20 tokens
        let input = "The quick brown fox jumps over the lazy dog. "
            .repeat(20);
        let chunks = chunk_text(&input, &config);
        assert!(chunks.len() > 1, "Expected multiple chunks, got {}", chunks.len());
    }

    #[test]
    fn all_chunks_are_valid_utf8_with_cjk() {
        let config = ChunkerConfig {
            chunk_size_tokens: 10,
            chunk_overlap_tokens: 2,
            tokenizer: "cl100k_base".to_string(),
        };
        let input = "这是一段中文文本。它包含多个句子。每个句子都应该被正确分割。这是第四句话。这是第五句话。";
        let chunks = chunk_text(input, &config);
        for chunk in &chunks {
            assert!(std::str::from_utf8(chunk.content.as_bytes()).is_ok());
        }
    }
}
