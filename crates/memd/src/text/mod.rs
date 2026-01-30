//! Text processing module for code-aware tokenization and sentence splitting.
//!
//! This module provides the foundation for BM25 lexical search with:
//! - Code-aware tokenization (camelCase, snake_case splitting)
//! - Sentence splitting for code and prose
//! - Text processing pipeline combining both

mod sentence;
mod tokenizer;

pub use sentence::{Sentence, SentenceSplitter};
pub use tokenizer::{CodeTokenStream, CodeTokenizer, TokenType, TypedToken};

/// A processed sentence with tokenization.
#[derive(Debug, Clone)]
pub struct ProcessedSentence {
    /// Original sentence text.
    pub text: String,
    /// Tokenized form.
    pub tokens: Vec<String>,
    /// Whether this is code content.
    pub is_code: bool,
    /// Byte offset in original document.
    pub offset: usize,
}

/// Text processor combining sentence splitting and tokenization.
///
/// This is the main entry point for text processing, providing a unified
/// interface for preparing text for BM25 indexing.
#[derive(Clone)]
pub struct TextProcessor {
    sentence_splitter: SentenceSplitter,
    tokenizer: CodeTokenizer,
}

impl Default for TextProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl TextProcessor {
    /// Create a new text processor.
    pub fn new() -> Self {
        Self {
            sentence_splitter: SentenceSplitter::new(),
            tokenizer: CodeTokenizer::new(),
        }
    }

    /// Process a chunk of text into sentences with tokens.
    ///
    /// This method:
    /// 1. Splits text into sentences (code blocks preserved)
    /// 2. Tokenizes each sentence with code-awareness
    /// 3. Returns processed sentences ready for indexing
    pub fn process_chunk(&self, text: &str) -> Vec<ProcessedSentence> {
        let sentences = self.sentence_splitter.split(text);

        sentences
            .into_iter()
            .map(|sentence| {
                let tokens = self.tokenizer.tokenize(&sentence.text);
                ProcessedSentence {
                    text: sentence.text,
                    tokens,
                    is_code: sentence.is_code,
                    offset: sentence.offset,
                }
            })
            .collect()
    }

    /// Get direct access to the tokenizer.
    pub fn tokenizer(&self) -> &CodeTokenizer {
        &self.tokenizer
    }

    /// Get direct access to the sentence splitter.
    pub fn sentence_splitter(&self) -> &SentenceSplitter {
        &self.sentence_splitter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_processor_basic() {
        let processor = TextProcessor::new();

        let text = "This function parses JSON data.";
        let processed = processor.process_chunk(text);

        assert_eq!(processed.len(), 1);
        assert!(!processed[0].is_code);
        assert!(processed[0].tokens.contains(&"pars".to_string())); // stemmed "parses"
        assert!(processed[0].tokens.contains(&"JSON".to_string())); // acronym preserved
    }

    #[test]
    fn test_text_processor_code() {
        let processor = TextProcessor::new();

        let code = "fn parseJSONData(input: &str) -> Result<Data, Error> {\n    todo!()\n}";
        let processed = processor.process_chunk(code);

        assert!(!processed.is_empty());
        assert!(processed[0].is_code);
    }

    #[test]
    fn test_text_processor_mixed() {
        let processor = TextProcessor::new();

        let mixed = "This is a description.\nfn example() { }\nAnother line.";
        let processed = processor.process_chunk(mixed);

        // Should have prose, code, and prose
        let has_code = processed.iter().any(|p| p.is_code);
        let has_prose = processed.iter().any(|p| !p.is_code);

        assert!(has_code);
        assert!(has_prose);
    }

    #[test]
    fn test_text_processor_empty() {
        let processor = TextProcessor::new();
        let processed = processor.process_chunk("");
        assert!(processed.is_empty());
    }

    #[test]
    fn test_tokens_contain_split_identifiers() {
        let processor = TextProcessor::new();

        let text = "The getUserById function returns a User.";
        let processed = processor.process_chunk(text);

        assert_eq!(processed.len(), 1);
        let tokens = &processed[0].tokens;

        // Should contain split parts of getUserById
        assert!(tokens.contains(&"get".to_string()));
        assert!(tokens.contains(&"user".to_string()));
        assert!(tokens.contains(&"by".to_string()));
        assert!(tokens.contains(&"id".to_string()));
    }
}
