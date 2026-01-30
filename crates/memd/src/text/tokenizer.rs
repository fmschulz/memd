//! Code-aware tokenization for hybrid search.
//!
//! This module provides tokenization that handles both code identifiers
//! (camelCase, snake_case splitting) and prose (stemming/normalization).

use rust_stemmers::{Algorithm, Stemmer};
use std::sync::Arc;
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Type of content being tokenized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    /// Code identifiers and keywords
    Code,
    /// Natural language prose
    Prose,
    /// Mixed code and prose
    Mixed,
}

/// A token with type information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedToken {
    pub text: String,
    pub token_type: TokenType,
    pub offset_from: usize,
    pub offset_to: usize,
}

/// Code-aware tokenizer that splits identifiers and normalizes prose.
///
/// Features:
/// - Splits camelCase: "parseUserInput" -> ["parse", "User", "Input"]
/// - Splits snake_case: "parse_user_input" -> ["parse", "user", "input"]
/// - Preserves acronyms: "HTTPResponse" -> ["HTTP", "Response"]
/// - Stems prose tokens using Porter algorithm
#[derive(Clone)]
pub struct CodeTokenizer {
    stemmer: Arc<Stemmer>,
}

impl Default for CodeTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeTokenizer {
    /// Create a new code tokenizer with English stemmer.
    pub fn new() -> Self {
        Self {
            stemmer: Arc::new(Stemmer::create(Algorithm::English)),
        }
    }

    /// Tokenize text into typed tokens.
    pub fn tokenize_typed(&self, text: &str) -> Vec<TypedToken> {
        let mut tokens = Vec::new();
        let mut current_pos = 0;

        for word in text.split(|c: char| c.is_whitespace() || c == '_' || is_separator(c)) {
            if word.is_empty() {
                current_pos += 1; // separator
                continue;
            }

            let word_start = text[current_pos..].find(word).unwrap_or(0) + current_pos;
            let word_end = word_start + word.len();

            // Split on camelCase boundaries
            let subtokens = split_camel_case(word);

            let mut subtoken_pos = word_start;
            for subtoken in subtokens {
                if subtoken.is_empty() {
                    continue;
                }

                let subtoken_end = subtoken_pos + subtoken.len();
                let token_type = detect_token_type(&subtoken);

                // Normalize based on token type
                let normalized = match token_type {
                    TokenType::Code => {
                        // Preserve acronyms (2+ uppercase), lowercase others
                        if is_acronym(&subtoken) {
                            subtoken.clone()
                        } else {
                            subtoken.to_lowercase()
                        }
                    }
                    TokenType::Prose => {
                        // Stem prose tokens
                        let lower = subtoken.to_lowercase();
                        self.stemmer.stem(&lower).to_string()
                    }
                    TokenType::Mixed => subtoken.to_lowercase(),
                };

                tokens.push(TypedToken {
                    text: normalized,
                    token_type,
                    offset_from: subtoken_pos,
                    offset_to: subtoken_end,
                });

                subtoken_pos = subtoken_end;
            }

            current_pos = word_end;
        }

        tokens
    }

    /// Tokenize text into simple string tokens (lowercased, stemmed).
    pub fn tokenize(&self, text: &str) -> Vec<String> {
        self.tokenize_typed(text)
            .into_iter()
            .map(|t| t.text)
            .collect()
    }
}

/// Split a word on camelCase boundaries.
///
/// Examples:
/// - "getUserById" -> ["get", "User", "By", "Id"]
/// - "HTTPResponse" -> ["HTTP", "Response"]
/// - "parseJSONData" -> ["parse", "JSON", "Data"]
fn split_camel_case(word: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = word.chars().collect();

    for i in 0..chars.len() {
        let c = chars[i];

        if current.is_empty() {
            current.push(c);
            continue;
        }

        let prev_upper = chars.get(i.saturating_sub(1)).map_or(false, |c| c.is_uppercase());
        let curr_upper = c.is_uppercase();
        let next_lower = chars.get(i + 1).map_or(false, |c| c.is_lowercase());

        // Split cases:
        // 1. lowercase -> uppercase: "getUser" at 'U'
        // 2. uppercase -> uppercase -> lowercase: "HTTPResponse" at 'R' (keep HTTP together)
        let should_split = if !prev_upper && curr_upper {
            // Case 1: lowercase to uppercase
            true
        } else if prev_upper && curr_upper && next_lower {
            // Case 2: end of acronym (e.g., "HTTPResponse")
            true
        } else {
            false
        };

        if should_split {
            if !current.is_empty() {
                result.push(current);
            }
            current = String::new();
        }

        current.push(c);
    }

    if !current.is_empty() {
        result.push(current);
    }

    result
}

/// Check if a string is an acronym (2+ consecutive uppercase letters).
fn is_acronym(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    chars.len() >= 2 && chars.iter().all(|c| c.is_uppercase())
}

/// Check if a character is a separator (not whitespace or underscore).
fn is_separator(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'')
}

/// Detect the token type based on content.
fn detect_token_type(token: &str) -> TokenType {
    let has_upper = token.chars().any(|c| c.is_uppercase());
    let has_lower = token.chars().any(|c| c.is_lowercase());
    let all_alpha = token.chars().all(|c| c.is_alphabetic());

    if !all_alpha {
        return TokenType::Code;
    }

    if is_acronym(token) {
        return TokenType::Code;
    }

    if has_upper && has_lower {
        // Mixed case like "User" or "Input"
        TokenType::Code
    } else if has_lower && !has_upper {
        // All lowercase - likely prose
        TokenType::Prose
    } else {
        TokenType::Mixed
    }
}

// Tantivy Tokenizer trait implementation

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let tokens = self.tokenize_typed(text);
        CodeTokenStream::new(tokens)
    }
}

/// Token stream for tantivy integration.
pub struct CodeTokenStream {
    tokens: Vec<TypedToken>,
    index: usize,
    token: Token,
}

impl CodeTokenStream {
    fn new(tokens: Vec<TypedToken>) -> Self {
        Self {
            tokens,
            index: 0,
            token: Token::default(),
        }
    }
}

impl TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        if self.index >= self.tokens.len() {
            return false;
        }

        let typed = &self.tokens[self.index];
        self.token.text.clear();
        self.token.text.push_str(&typed.text);
        self.token.offset_from = typed.offset_from;
        self.token.offset_to = typed.offset_to;
        self.token.position = self.index;
        self.token.position_length = 1;

        self.index += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camel_case_splitting() {
        let tokenizer = CodeTokenizer::new();

        // Basic camelCase
        let tokens = tokenizer.tokenize("getUserById");
        assert_eq!(tokens, vec!["get", "user", "by", "id"]);

        // Multiple words
        let tokens = tokenizer.tokenize("parseUserInput");
        assert_eq!(tokens, vec!["pars", "user", "input"]); // "parse" stems to "pars"
    }

    #[test]
    fn test_snake_case_splitting() {
        let tokenizer = CodeTokenizer::new();

        let tokens = tokenizer.tokenize("get_user_by_id");
        assert_eq!(tokens, vec!["get", "user", "by", "id"]);

        let tokens = tokenizer.tokenize("parse_user_input");
        assert_eq!(tokens, vec!["pars", "user", "input"]);
    }

    #[test]
    fn test_acronym_preservation() {
        let tokenizer = CodeTokenizer::new();

        // Acronyms should be preserved uppercase
        let tokens = tokenizer.tokenize("HTTPResponse");
        assert_eq!(tokens, vec!["HTTP", "respons"]); // "Response" stems to "respons"

        let tokens = tokenizer.tokenize("parseJSONData");
        assert_eq!(tokens, vec!["pars", "JSON", "data"]);

        // API, SQL should stay uppercase
        let tokens = tokenizer.tokenize("getAPIKey");
        assert_eq!(tokens, vec!["get", "API", "key"]);
    }

    #[test]
    fn test_prose_normalization() {
        let tokenizer = CodeTokenizer::new();

        // Stemming should apply
        let tokens = tokenizer.tokenize("running quickly");
        assert_eq!(tokens, vec!["run", "quick"]);

        let tokens = tokenizer.tokenize("the users are connecting");
        assert_eq!(tokens, vec!["the", "user", "ar", "connect"]);
    }

    #[test]
    fn test_operator_preservation() {
        let tokenizer = CodeTokenizer::new();

        // Operators should be split out (as separators)
        let tokens = tokenizer.tokenize("x && y");
        assert_eq!(tokens, vec!["x", "y"]); // && is filtered as separator
    }

    #[test]
    fn test_mixed_identifiers() {
        let tokenizer = CodeTokenizer::new();

        let tokens = tokenizer.tokenize("parseJSONData");
        assert_eq!(tokens, vec!["pars", "JSON", "data"]);

        let tokens = tokenizer.tokenize("XMLHttpRequest");
        assert_eq!(tokens, vec!["XML", "http", "request"]);
    }

    #[test]
    fn test_typed_tokens() {
        let tokenizer = CodeTokenizer::new();

        let typed = tokenizer.tokenize_typed("HTTPResponse");
        assert_eq!(typed.len(), 2);
        assert_eq!(typed[0].text, "HTTP");
        assert_eq!(typed[0].token_type, TokenType::Code);
        assert_eq!(typed[1].text, "respons");
        assert_eq!(typed[1].token_type, TokenType::Prose);
    }

    #[test]
    fn test_tantivy_tokenizer_trait() {
        use tantivy::tokenizer::Tokenizer;

        let mut tokenizer = CodeTokenizer::new();
        let mut stream = tokenizer.token_stream("getUserById");

        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }

        assert_eq!(tokens, vec!["get", "user", "by", "id"]);
    }
}
