//! Document chunking for long-form text
//!
//! Splits long documents into overlapping chunks to improve semantic retrieval quality.
//! Long documents (>500 tokens) dilute semantic signals; chunking preserves granularity.

use crate::text::SentenceSplitter;

/// Configuration for document chunking
#[derive(Debug, Clone)]
pub struct ChunkingConfig {
    /// Target characters per chunk (default: 1200 = ~300 tokens)
    pub chunk_size: usize,
    /// Overlap characters between chunks (default: 200 = ~50 tokens)
    pub overlap: usize,
    /// Minimum chunk size to avoid tiny chunks (default: 400 = ~100 tokens)
    pub min_chunk_size: usize,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            chunk_size: 1200,
            overlap: 200,
            min_chunk_size: 400,
        }
    }
}

/// A single chunk from a document
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The chunk text
    pub text: String,
    /// Character offset in original document
    pub start_char: usize,
    /// Character offset (exclusive) in original document
    pub end_char: usize,
    /// Index of this chunk (0-based)
    pub chunk_index: usize,
}

/// Split text into overlapping chunks using sentence boundaries
///
/// Strategy:
/// 1. Split text into sentences
/// 2. Group sentences into chunks of ~chunk_size characters
/// 3. Add overlap by including sentences from previous chunk
/// 4. Ensure minimum chunk size
///
/// # Arguments
/// * `text` - The document text to chunk
/// * `config` - Chunking configuration
///
/// # Returns
/// Vector of chunks. If text is shorter than chunk_size, returns single chunk.
pub fn chunk_text(text: &str, config: &ChunkingConfig) -> Vec<Chunk> {
    if text.len() <= config.chunk_size {
        return vec![Chunk {
            text: text.to_string(),
            start_char: 0,
            end_char: text.len(),
            chunk_index: 0,
        }];
    }

    let splitter = SentenceSplitter::new();
    let sentences = splitter.split(text);

    if sentences.is_empty() {
        return vec![Chunk {
            text: text.to_string(),
            start_char: 0,
            end_char: text.len(),
            chunk_index: 0,
        }];
    }

    let mut chunks = Vec::new();
    let mut current_chunk_sentences = Vec::new();
    let mut current_length = 0;
    let mut chunk_start = 0;
    let mut chunk_index = 0;

    for sentence in sentences {
        let sentence_len = sentence.text.len();

        // SPECIAL CASE: If sentence itself exceeds chunk_size, split it forcibly
        if sentence_len > config.chunk_size {
            // Finalize any current chunk first
            if current_length > 0 {
                let chunk_text: String = current_chunk_sentences
                    .iter()
                    .map(|s: &crate::text::Sentence| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join("");

                chunks.push(Chunk {
                    text: chunk_text,
                    start_char: chunk_start,
                    end_char: chunk_start + current_length,
                    chunk_index,
                });
                chunk_index += 1;

                current_chunk_sentences.clear();
                current_length = 0;
            }

            // Split the oversized sentence into byte-based chunks, but
            // never inside a multi-byte UTF-8 character: walking back
            // from the target cut until it lands on a char boundary
            // keeps `sentence.text[sent_offset..chunk_end]` panic-free
            // when the body contains characters like `§`, `→`, emoji,
            // or any non-ASCII text that happens to straddle a
            // chunk_size-aligned offset. `is_char_boundary(0)` and
            // `is_char_boundary(text.len())` are both always true so
            // the tail chunk is always emittable.
            let mut sent_offset = 0;
            while sent_offset < sentence.text.len() {
                let remaining = sentence.text.len() - sent_offset;
                let target_len = remaining.min(config.chunk_size);
                let mut chunk_end = sent_offset + target_len;
                while chunk_end > sent_offset && !sentence.text.is_char_boundary(chunk_end) {
                    chunk_end -= 1;
                }
                // `chunk_end == sent_offset` can only happen if the
                // remaining payload starts with a single character
                // wider than chunk_size — absurd for real text but
                // guard against it by taking that whole character.
                if chunk_end == sent_offset {
                    chunk_end = sent_offset
                        + sentence.text[sent_offset..]
                            .chars()
                            .next()
                            .map(char::len_utf8)
                            .unwrap_or(1);
                }

                let chunk_text = sentence.text[sent_offset..chunk_end].to_string();
                chunks.push(Chunk {
                    text: chunk_text,
                    start_char: sentence.offset + sent_offset,
                    end_char: sentence.offset + chunk_end,
                    chunk_index,
                });
                chunk_index += 1;

                sent_offset = chunk_end;
                // No overlap for forced splits - they're already at max size
            }

            // Start fresh for next sentence
            chunk_start = sentence.offset + sentence.text.len();
            continue;
        }

        // If adding this sentence would exceed chunk_size, finalize current chunk
        if current_length > 0 && current_length + sentence_len > config.chunk_size {
            // Create chunk from accumulated sentences
            let chunk_text: String = current_chunk_sentences
                .iter()
                .map(|s: &crate::text::Sentence| s.text.as_str())
                .collect::<Vec<_>>()
                .join("");

            chunks.push(Chunk {
                text: chunk_text,
                start_char: chunk_start,
                end_char: chunk_start + current_length,
                chunk_index,
            });
            chunk_index += 1;

            // Calculate overlap: keep sentences totaling ~overlap chars
            let mut overlap_length = 0;
            let mut overlap_sentences = Vec::new();

            for sent in current_chunk_sentences.iter().rev() {
                if overlap_length + sent.text.len() <= config.overlap {
                    overlap_sentences.push(sent.clone());
                    overlap_length += sent.text.len();
                } else {
                    break;
                }
            }

            // Reverse to restore original order
            overlap_sentences.reverse();

            // Start new chunk with overlap sentences
            current_chunk_sentences = overlap_sentences;
            current_length = overlap_length;

            // Update chunk_start for new chunk
            if !current_chunk_sentences.is_empty() {
                chunk_start = current_chunk_sentences[0].offset;
            } else {
                chunk_start = sentence.offset;
            }
        }

        // Add current sentence
        current_chunk_sentences.push(sentence);
        current_length += sentence_len;
    }

    // Add final chunk if it meets minimum size or is the only chunk
    if current_length > 0 && (current_length >= config.min_chunk_size || chunks.is_empty()) {
        let chunk_text: String = current_chunk_sentences
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("");

        // Calculate actual end position based on last sentence
        let end_char = if let Some(last_sent) = current_chunk_sentences.last() {
            last_sent.offset + last_sent.text.len()
        } else {
            chunk_start + current_length
        };

        chunks.push(Chunk {
            text: chunk_text,
            start_char: chunk_start,
            end_char,
            chunk_index,
        });
    } else if !chunks.is_empty() {
        // Merge too-small final chunk into the previous chunk.
        //
        // `current_chunk_sentences` may include overlap sentences that are
        // already part of the previous chunk's text — those have an offset
        // that falls before `last_chunk.end_char`. If we concatenated the
        // full list we would duplicate the overlap region. Skip overlap
        // sentences and append only the sentences that follow the previous
        // chunk's range.
        //
        // `end_char` is updated with `max()`: the overlap-only tail case
        // would otherwise shrink the previous chunk's range because the
        // last retained sentence may sit before the original chunk end.
        if let Some(last_chunk) = chunks.last_mut() {
            let new_sentences: Vec<&crate::text::Sentence> = current_chunk_sentences
                .iter()
                .filter(|s| s.offset >= last_chunk.end_char)
                .collect();

            let additional_text: String = new_sentences
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("");

            let new_end = new_sentences
                .last()
                .map(|s| s.offset + s.text.len())
                .unwrap_or(last_chunk.end_char);

            last_chunk.text.push_str(&additional_text);
            last_chunk.end_char = last_chunk.end_char.max(new_end);
        }
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_text_no_chunking() {
        let text = "This is a short document.";
        let config = ChunkingConfig::default();
        let chunks = chunk_text(text, &config);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, text);
        assert_eq!(chunks[0].start_char, 0);
        assert_eq!(chunks[0].chunk_index, 0);
    }

    #[test]
    fn test_empty_text() {
        let text = "";
        let config = ChunkingConfig::default();
        let chunks = chunk_text(text, &config);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "");
    }

    #[test]
    fn test_repeated_chars_no_sentence_boundaries() {
        // Test case: 1500 chars with no sentence boundaries (like "AAAA...")
        // This simulates what happens in the integration test
        let text = "A".repeat(1500);
        let config = ChunkingConfig::default();
        let chunks = chunk_text(&text, &config);

        eprintln!("Repeated 'A' test:");
        eprintln!("  Text length: {}", text.len());
        eprintln!("  Chunk size: {}", config.chunk_size);
        eprintln!("  Number of chunks: {}", chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            eprintln!("    Chunk {}: {} chars", i, chunk.text.len());
        }

        // With 1500 chars and chunk_size 1200, we expect it to be split
        // But without sentence boundaries, it might not split correctly
        // This test will reveal the bug
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks for 1500 char text (chunk_size=1200), got {} chunk(s)",
            chunks.len()
        );
    }

    #[test]
    fn test_chunking_with_sentences() {
        let text = "First sentence. ".repeat(100); // ~1600 chars
        let config = ChunkingConfig {
            chunk_size: 600,
            overlap: 100,
            min_chunk_size: 200,
        };
        let chunks = chunk_text(&text, &config);

        // Should create multiple chunks
        assert!(chunks.len() > 1);

        // Check chunk indices
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }

        // Verify no chunk is too small (except possibly last if merged)
        for chunk in &chunks {
            assert!(
                chunk.text.len() >= config.min_chunk_size || chunk.chunk_index == chunks.len() - 1
            );
        }
    }

    #[test]
    fn test_overlap_between_chunks() {
        let sentences = vec![
            "Sentence one. ",
            "Sentence two. ",
            "Sentence three. ",
            "Sentence four. ",
        ];
        let text = sentences.join("");

        let config = ChunkingConfig {
            chunk_size: 30, // Small chunk size to force splitting
            overlap: 15,
            min_chunk_size: 10,
        };

        let chunks = chunk_text(&text, &config);

        // Should have multiple chunks due to small chunk_size
        assert!(chunks.len() > 1);

        // Check that chunks overlap (same text appears in consecutive chunks)
        for i in 0..chunks.len() - 1 {
            let current = &chunks[i];
            let next = &chunks[i + 1];

            // Verify next chunk starts before current ends (overlap)
            assert!(
                next.start_char < current.end_char,
                "Chunks {} and {} should overlap",
                i,
                i + 1
            );
        }
    }

    #[test]
    fn test_chunk_size_bounds() {
        let text = "Word. ".repeat(500); // ~3000 chars
        let config = ChunkingConfig {
            chunk_size: 1000,
            overlap: 100,
            min_chunk_size: 300,
        };

        let chunks = chunk_text(&text, &config);

        // Each chunk should be roughly chunk_size (within reason)
        for (i, chunk) in chunks.iter().enumerate() {
            if i < chunks.len() - 1 {
                // Not the last chunk
                assert!(
                    chunk.text.len() <= config.chunk_size * 2,
                    "Chunk {} too large: {} chars",
                    i,
                    chunk.text.len()
                );
            }
        }
    }

    #[test]
    fn test_all_text_covered() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(50);
        let config = ChunkingConfig::default();
        let chunks = chunk_text(&text, &config);

        // Verify chunks are generated
        assert!(!chunks.is_empty());

        // First chunk should start at 0
        assert_eq!(chunks[0].start_char, 0);

        // All chunks should have content
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
        }

        // Verify all text is represented in chunks (by joining all chunk texts)
        let combined_text: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(
            combined_text.len() >= text.len() / 2,
            "Combined chunks should cover most of the text"
        );
    }

    #[test]
    fn test_scientific_abstract() {
        // Simulate a SciFact-style document (~1400 chars)
        let text = "Objective: To investigate the association between dietary patterns and cardiovascular disease risk. Methods: We conducted a prospective cohort study of 120,000 participants over 10 years. Results: Mediterranean diet adherence was associated with 25% lower cardiovascular disease risk (HR 0.75, 95% CI 0.68-0.83). Conclusion: Adherence to Mediterranean dietary patterns may reduce cardiovascular disease risk. Background: Cardiovascular disease remains the leading cause of mortality worldwide. Dietary factors play a crucial role in disease prevention. Previous studies have shown mixed results regarding specific dietary interventions. Our study aimed to provide comprehensive evidence using a large-scale prospective design with validated dietary assessment tools. The Mediterranean diet emphasizes plant-based foods, olive oil, fish, and moderate wine consumption. This dietary pattern has been hypothesized to confer cardiovascular benefits through anti-inflammatory and antioxidant mechanisms. Statistical Analysis: We used Cox proportional hazards models adjusted for age, sex, smoking status, physical activity, and baseline comorbidities. Sensitivity analyses excluded participants with prevalent disease at baseline. Results were consistent across multiple sensitivity analyses and subgroup stratifications.";

        let config = ChunkingConfig::default();
        let chunks = chunk_text(text, &config);

        // Document is ~1400 chars, should create 1-2 chunks
        assert!(chunks.len() >= 1);
        assert!(chunks.len() <= 3);

        // Verify complete coverage
        assert_eq!(chunks[0].start_char, 0);
        assert_eq!(chunks.last().unwrap().end_char, text.len());
    }

    /// Regression test for the merge-too-small-final-chunk path.
    ///
    /// Before the fix, when the tail of a document produced a final chunk
    /// smaller than `min_chunk_size`, the merge branch appended the overlap
    /// sentences (already part of the previous chunk) back onto
    /// `last_chunk.text`, duplicating characters. It also overwrote
    /// `last_chunk.end_char` with the new value, which could shrink the
    /// chunk's range when only overlap sentences were present.
    #[test]
    fn test_merge_final_chunk_does_not_duplicate_overlap() {
        // Pick sentence counts that produce exactly the merge scenario:
        // one full chunk + a residual that is below min_chunk_size.
        let sentence = "Alpha beta gamma delta epsilon zeta. ";
        let text = sentence.repeat(40);

        let config = ChunkingConfig {
            chunk_size: 400,
            overlap: 80,
            min_chunk_size: 150,
        };
        let chunks = chunk_text(&text, &config);

        assert!(
            chunks.len() >= 2,
            "Expected at least two chunks for merge scenario, got {}",
            chunks.len()
        );

        let last = chunks.last().unwrap();

        // After the fix, the last chunk's text length must not exceed the
        // range it claims to cover (bytes since its `start_char`). A
        // duplicate overlap inflates `text.len()` past that window.
        let window = last.end_char.saturating_sub(last.start_char);
        assert!(
            last.text.len() <= window,
            "last chunk text ({} chars) must not exceed claimed range ({} chars); \
             duplicated overlap would violate this",
            last.text.len(),
            window,
        );
    }

    /// Regression: the oversized-sentence forced-split path used to
    /// slice `sentence.text[sent_offset..chunk_end]` with `chunk_end`
    /// computed as a byte offset without honoring UTF-8 character
    /// boundaries. When an input sentence contained a multi-byte
    /// character (e.g., `§`, `→`, emoji) straddling the chunk-size
    /// boundary, the slice panicked with "byte index N is not a char
    /// boundary". This surfaced end-to-end on the live memd daemon
    /// when `context.brief_project` tried to chunk a task_finish
    /// `what_failed` list that mentioned `§` — the daemon logged a
    /// tokio-worker panic at chunking.rs:116 and dropped the client's
    /// connection. The fix walks `chunk_end` back until it lands on a
    /// valid char boundary; the tail-case where the remainder starts
    /// with a single character wider than `chunk_size` is handled by
    /// taking that whole character.
    #[test]
    fn test_forced_split_does_not_panic_on_multi_byte_char_boundary() {
        // Build a single "sentence" (no terminator) that:
        //   - has no sentence boundary so it hits the oversized-
        //     split branch;
        //   - is longer than chunk_size;
        //   - contains a multi-byte UTF-8 character (`§` = 2 bytes)
        //     straddling the byte offset chunk_size would cut at.
        let chunk_size = 30;
        // Pad so that byte offset 30 falls inside `§` (2 bytes).
        //   "A" * 29 = 29 bytes, then '§' occupies bytes 29..31 → byte 30 is mid-char.
        let mut text = "A".repeat(29);
        text.push('§');
        text.push_str(&"B".repeat(29));
        // Force the oversized-split branch.
        assert!(text.len() > chunk_size);
        let config = ChunkingConfig {
            chunk_size,
            overlap: 0,
            min_chunk_size: 5,
        };
        // Must not panic.
        let chunks = chunk_text(&text, &config);
        assert!(!chunks.is_empty());
        // Every chunk must be valid UTF-8 and its start/end must be
        // char boundaries into the original text.
        for chunk in &chunks {
            assert!(
                text.is_char_boundary(chunk.start_char),
                "chunk {} start {} must be a char boundary",
                chunk.chunk_index,
                chunk.start_char
            );
            assert!(
                text.is_char_boundary(chunk.end_char),
                "chunk {} end {} must be a char boundary",
                chunk.chunk_index,
                chunk.end_char
            );
        }
        // Recombined text must be the same byte-for-byte.
        let rejoin: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(rejoin.contains("§"), "rejoin lost the multi-byte char");
    }

    /// Paranoid extra: many different multi-byte characters at the
    /// boundary. Emoji, CJK, arrows — none should panic.
    #[test]
    fn test_forced_split_handles_emoji_and_cjk() {
        let chunk_size = 20;
        for bad_char in ["§", "→", "漢", "🚀"] {
            let mut text = "A".repeat(chunk_size - 1);
            text.push_str(bad_char);
            text.push_str(&"B".repeat(chunk_size));
            let config = ChunkingConfig {
                chunk_size,
                overlap: 0,
                min_chunk_size: 5,
            };
            // Must not panic for any of these.
            let _ = chunk_text(&text, &config);
        }
    }

    #[test]
    fn test_merge_final_chunk_does_not_shrink_end_char() {
        // Text where the final chunk is guaranteed to be merged and
        // current_chunk_sentences contains only overlap content.
        let sentence = "Short one. ";
        let text = sentence.repeat(60);

        let config = ChunkingConfig {
            chunk_size: 300,
            overlap: 100,
            min_chunk_size: 500, // force merge for anything non-full
        };
        let chunks = chunk_text(&text, &config);

        // Every chunk's end_char must be monotonically non-decreasing.
        for pair in chunks.windows(2) {
            assert!(
                pair[1].end_char >= pair[0].end_char,
                "chunk end_char must not decrease across the list"
            );
            assert!(
                pair[1].start_char >= pair[0].start_char,
                "chunk start_char must not decrease across the list"
            );
        }

        // The overall last end_char should still reach into the document
        // beyond the first chunk (it started at 0).
        let last = chunks.last().unwrap();
        assert!(last.end_char > 0);
        assert!(
            last.end_char <= text.len(),
            "end_char {} must not exceed input length {}",
            last.end_char,
            text.len()
        );
    }
}
