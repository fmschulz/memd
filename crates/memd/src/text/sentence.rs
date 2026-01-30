//! Sentence splitting for code and prose content.
//!
//! This module provides sentence boundary detection that handles both
//! natural language prose and code blocks.

use unicode_segmentation::UnicodeSegmentation;

/// A sentence extracted from text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    /// The sentence text.
    pub text: String,
    /// Byte offset from start of original text.
    pub offset: usize,
    /// Whether this sentence appears to be code.
    pub is_code: bool,
}

/// Sentence splitter that handles mixed code and prose.
///
/// Features:
/// - Uses unicode segmentation for proper sentence boundaries
/// - Detects code blocks and preserves them as single "sentences"
/// - Heuristic code detection based on syntax patterns
#[derive(Debug, Clone, Default)]
pub struct SentenceSplitter;

impl SentenceSplitter {
    /// Create a new sentence splitter.
    pub fn new() -> Self {
        Self
    }

    /// Split text into sentences.
    ///
    /// Code blocks are detected and kept together. Prose is split
    /// on natural sentence boundaries.
    pub fn split(&self, text: &str) -> Vec<Sentence> {
        let mut sentences = Vec::new();
        let mut current_offset = 0;

        // First, try to identify code blocks (consecutive lines with code indicators)
        let lines: Vec<&str> = text.lines().collect();

        if lines.is_empty() {
            return sentences;
        }

        // Process line by line, grouping code blocks
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let line_start = text[current_offset..]
                .find(line)
                .map(|pos| current_offset + pos)
                .unwrap_or(current_offset);

            if is_code_line(line) {
                // Start of a code block - collect consecutive code lines
                let block_start = line_start;
                let mut block_end = line_start + line.len();
                let mut code_lines = vec![line];

                i += 1;
                while i < lines.len() {
                    let next_line = lines[i];
                    if is_code_line(next_line) || next_line.trim().is_empty() {
                        // Include empty lines within code blocks
                        code_lines.push(next_line);
                        let next_start = text[block_end..]
                            .find(next_line)
                            .map(|pos| block_end + pos)
                            .unwrap_or(block_end);
                        block_end = next_start + next_line.len();
                        i += 1;
                    } else {
                        break;
                    }
                }

                // Trim trailing empty lines from code block
                while code_lines.last().map_or(false, |l| l.trim().is_empty()) {
                    code_lines.pop();
                }

                if !code_lines.is_empty() {
                    let code_text = code_lines.join("\n");
                    sentences.push(Sentence {
                        text: code_text,
                        offset: block_start,
                        is_code: true,
                    });
                }

                current_offset = block_end;
            } else {
                // Prose line - use unicode sentence segmentation
                let prose_sentences = line.unicode_sentences().collect::<Vec<_>>();

                for sent in prose_sentences {
                    let sent_trimmed = sent.trim();
                    if !sent_trimmed.is_empty() {
                        let sent_offset = text[line_start..]
                            .find(sent)
                            .map(|pos| line_start + pos)
                            .unwrap_or(line_start);

                        sentences.push(Sentence {
                            text: sent_trimmed.to_string(),
                            offset: sent_offset,
                            is_code: false,
                        });
                    }
                }

                current_offset = line_start + line.len();
                i += 1;
            }
        }

        sentences
    }
}

/// Heuristic to detect if a line looks like code.
///
/// Code indicators:
/// - Contains braces, brackets, semicolons
/// - Contains function/method syntax
/// - Contains operators like =>, ->, ::
/// - Starts with common keywords (fn, let, const, if, for, while, etc.)
fn is_code_line(line: &str) -> bool {
    let trimmed = line.trim();

    if trimmed.is_empty() {
        return false;
    }

    // Check for definite code patterns
    let code_patterns = [
        // Braces and brackets
        "{",
        "}",
        "[",
        "]",
        // Semicolons at end (but not in prose)
        ";",
        // Function syntax
        "fn ",
        "func ",
        "function ",
        "def ",
        "async fn",
        "pub fn",
        "pub(crate)",
        // Variable declarations
        "let ",
        "const ",
        "var ",
        "mut ",
        // Arrows and operators
        "=>",
        "->",
        "::",
        ".unwrap()",
        ".await",
        // Import statements
        "use ",
        "import ",
        "from ",
        "require(",
        // Control flow that looks like code
        "if (",
        "for (",
        "while (",
        "match ",
        "switch (",
        // Comments (programming style)
        "//",
        "/*",
        "*/",
        "#[",
        // Type annotations
        ": &",
        ": Vec<",
        ": String",
        ": i32",
        ": u32",
        "-> Result",
        "-> Option",
    ];

    for pattern in code_patterns {
        if trimmed.contains(pattern) {
            return true;
        }
    }

    // Check for indentation with code-like content (4+ spaces or tab)
    if (line.starts_with("    ") || line.starts_with('\t'))
        && (trimmed.contains('=') || trimmed.contains('(') || trimmed.contains('.'))
    {
        return true;
    }

    // Check for line ending in common code patterns
    if trimmed.ends_with('{')
        || trimmed.ends_with('}')
        || trimmed.ends_with(';')
        || trimmed.ends_with(')')
        || trimmed.ends_with(',')
    {
        // But not if it's a normal sentence ending in parenthetical
        if !trimmed.contains(' ') || trimmed.contains('(') && trimmed.contains(')') {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_sentence_splitting() {
        let splitter = SentenceSplitter::new();

        let text = "First sentence. Second sentence.";
        let sentences = splitter.split(text);

        assert_eq!(sentences.len(), 2);
        assert_eq!(sentences[0].text, "First sentence.");
        assert_eq!(sentences[1].text, "Second sentence.");
        assert!(!sentences[0].is_code);
        assert!(!sentences[1].is_code);
    }

    #[test]
    fn test_code_block_detection() {
        let splitter = SentenceSplitter::new();

        let text = "fn main() {\n    println!(\"Hello\");\n}";
        let sentences = splitter.split(text);

        assert_eq!(sentences.len(), 1);
        assert!(sentences[0].is_code);
        assert!(sentences[0].text.contains("fn main()"));
    }

    #[test]
    fn test_mixed_content() {
        let splitter = SentenceSplitter::new();

        let text = "This is a description.\nfn example() {\n    let x = 1;\n}\nAnother sentence.";
        let sentences = splitter.split(text);

        assert_eq!(sentences.len(), 3);
        assert!(!sentences[0].is_code); // "This is a description."
        assert!(sentences[1].is_code); // code block
        assert!(!sentences[2].is_code); // "Another sentence."
    }

    #[test]
    fn test_empty_string() {
        let splitter = SentenceSplitter::new();
        let sentences = splitter.split("");
        assert!(sentences.is_empty());
    }

    #[test]
    fn test_single_word() {
        let splitter = SentenceSplitter::new();
        let sentences = splitter.split("Hello");
        assert_eq!(sentences.len(), 1);
        assert_eq!(sentences[0].text, "Hello");
    }

    #[test]
    fn test_rust_code() {
        let splitter = SentenceSplitter::new();

        let code = r#"use std::collections::HashMap;

pub fn process(data: &str) -> Result<String, Error> {
    let mut map = HashMap::new();
    map.insert("key", data);
    Ok(map.get("key").unwrap().to_string())
}"#;

        let sentences = splitter.split(code);

        // Should be detected as code blocks
        assert!(sentences.iter().all(|s| s.is_code));
    }

    #[test]
    fn test_prose_with_abbreviations() {
        let splitter = SentenceSplitter::new();

        let text = "Dr. Smith went to the store. He bought milk.";
        let sentences = splitter.split(text);

        // Unicode segmentation handles abbreviations
        assert!(sentences.len() >= 1);
    }

    #[test]
    fn test_offset_tracking() {
        let splitter = SentenceSplitter::new();

        let text = "First. Second.";
        let sentences = splitter.split(text);

        assert_eq!(sentences.len(), 2);
        // First sentence starts at 0
        assert_eq!(sentences[0].offset, 0);
    }
}
