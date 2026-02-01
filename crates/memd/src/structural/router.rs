//! Query router for intent classification and routing.
//!
//! Classifies natural language queries into structured intents and routes
//! them to appropriate search backends (structural, semantic, trace).

use regex::Regex;

/// Query intent classification.
///
/// Determines which search backend should handle the query.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum QueryIntent {
    #[default]
    /// Semantic search (default fallback).
    SemanticSearch,

    /// Find where a symbol is defined.
    SymbolDefinition(String),
    /// Find all usages/references of a symbol.
    SymbolReferences(String),
    /// Find who calls a function.
    SymbolCallers(String),
    /// Find who imports a module.
    ModuleImports(String),
    /// Find all symbols in a file.
    FileSymbols(String),

    /// Tool call history, optionally filtered by tool name.
    ToolCalls(Option<String>),
    /// Error/stack trace search, optionally by signature.
    ErrorSearch(Option<String>),

    /// Documentation question-answering.
    DocQa,
    /// Why was a decision made.
    DecisionWhy(String),
    /// What's the next step in a plan.
    PlanNext,
}

impl QueryIntent {
    /// Returns whether this intent should blend semantic context with structural results.
    ///
    /// Per CONTEXT.md: "structural first, semantic expands"
    /// Code-intent queries get both precise structural results and related semantic context.
    pub fn should_blend_semantic(&self) -> bool {
        matches!(
            self,
            QueryIntent::SymbolDefinition(_)
                | QueryIntent::SymbolReferences(_)
                | QueryIntent::SymbolCallers(_)
                | QueryIntent::ModuleImports(_)
                | QueryIntent::FileSymbols(_)
        )
    }

    /// Returns whether this intent should fall back to semantic on empty results.
    pub fn should_fallback_semantic(&self) -> bool {
        matches!(
            self,
            QueryIntent::SymbolDefinition(_)
                | QueryIntent::SymbolReferences(_)
                | QueryIntent::SymbolCallers(_)
                | QueryIntent::ModuleImports(_)
                | QueryIntent::FileSymbols(_)
        )
    }

    /// Returns true if this is a trace/debug query (no semantic blending).
    pub fn is_trace_query(&self) -> bool {
        matches!(self, QueryIntent::ToolCalls(_) | QueryIntent::ErrorSearch(_))
    }
}

/// A compiled regex pattern with capture group index.
struct CompiledPattern {
    regex: Regex,
    /// Which capture group contains the target (1-indexed).
    extract_group: usize,
}

impl CompiledPattern {
    fn new(pattern: &str, extract_group: usize) -> Self {
        Self {
            regex: Regex::new(pattern).expect("Invalid pattern"),
            extract_group,
        }
    }

    fn extract(&self, text: &str) -> Option<String> {
        self.regex.captures(text).and_then(|caps| {
            caps.get(self.extract_group)
                .map(|m| m.as_str().trim().to_string())
        })
    }

    fn matches(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }
}

/// Result of query routing with confidence and behavior flags.
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// Classified intent.
    pub intent: QueryIntent,
    /// Confidence in classification (0.0-1.0). Prefix = 1.0, pattern = 0.9.
    pub confidence: f32,
    /// If structural returns empty, fall back to semantic.
    pub fallback_to_semantic: bool,
    /// For code queries, blend semantic results as context.
    pub blend_semantic_context: bool,
}

impl RouteResult {
    fn semantic() -> Self {
        Self {
            intent: QueryIntent::SemanticSearch,
            confidence: 1.0,
            fallback_to_semantic: false,
            blend_semantic_context: false,
        }
    }

    fn from_intent(intent: QueryIntent, confidence: f32) -> Self {
        let blend_semantic_context = intent.should_blend_semantic();
        let fallback_to_semantic = intent.should_fallback_semantic();
        Self {
            intent,
            confidence,
            fallback_to_semantic,
            blend_semantic_context,
        }
    }
}

/// Query router for intent classification.
///
/// Classifies queries based on explicit prefixes and natural language patterns.
/// Patterns are case-insensitive and designed for common developer queries.
pub struct QueryRouter {
    definition_patterns: Vec<CompiledPattern>,
    caller_patterns: Vec<CompiledPattern>,
    reference_patterns: Vec<CompiledPattern>,
    import_patterns: Vec<CompiledPattern>,
    error_patterns: Vec<CompiledPattern>,
    tool_patterns: Vec<CompiledPattern>,
    file_symbols_patterns: Vec<CompiledPattern>,
}

impl Default for QueryRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryRouter {
    /// Create a new query router with default patterns.
    pub fn new() -> Self {
        Self {
            definition_patterns: vec![
                CompiledPattern::new(r"(?i)where is (\w+) defined", 1),
                CompiledPattern::new(r"(?i)definition of (\w+)", 1),
                CompiledPattern::new(r"(?i)find (?:class|function|type|struct|enum) (\w+)", 1),
                CompiledPattern::new(r"(?i)what is (\w+)", 1),
                CompiledPattern::new(r"(?i)show (?:me )?(\w+) (?:definition|source)", 1),
                CompiledPattern::new(r"(?i)where is (\w+)\?", 1),
                CompiledPattern::new(r"(?i)go to (\w+)", 1),
            ],
            caller_patterns: vec![
                CompiledPattern::new(r"(?i)who calls (\w+)", 1),
                CompiledPattern::new(r"(?i)callers of (\w+)", 1),
                CompiledPattern::new(r"(?i)what calls (\w+)", 1),
                CompiledPattern::new(r"(?i)(\w+) is called by", 1),
                CompiledPattern::new(r"(?i)find callers (?:of )?(\w+)", 1),
                CompiledPattern::new(r"(?i)who uses (\w+)", 1),
            ],
            reference_patterns: vec![
                CompiledPattern::new(r"(?i)references to (\w+)", 1),
                CompiledPattern::new(r"(?i)usages of (\w+)", 1),
                CompiledPattern::new(r"(?i)where is (\w+) used", 1),
                CompiledPattern::new(
                    r"(?i)find all (?:uses|usages|references) (?:of )?(\w+)",
                    1,
                ),
                CompiledPattern::new(r"(?i)find (\w+) references", 1),
            ],
            import_patterns: vec![
                CompiledPattern::new(r"(?i)who imports (\w+)", 1),
                CompiledPattern::new(r"(?i)files (?:that )?import (\w+)", 1),
                CompiledPattern::new(r"(?i)(\w+) importers", 1),
                CompiledPattern::new(r"(?i)where is (\w+) imported", 1),
            ],
            error_patterns: vec![
                CompiledPattern::new(r"(?i)errors? (?:in|from|about) (\w+)", 1),
                CompiledPattern::new(r"(?i)stack traces? (?:for|with) (\w+)", 1),
                CompiledPattern::new(r"(?i)exceptions? (?:in|from) (\w+)", 1),
                CompiledPattern::new(r"(?i)what went wrong", 0), // General error search
                CompiledPattern::new(r"(?i)recent errors", 0),
                CompiledPattern::new(r"(?i)show errors", 0),
            ],
            tool_patterns: vec![
                CompiledPattern::new(r"(?i)(?:tool )?calls to (\w+)", 1),
                CompiledPattern::new(r"(?i)when did (?:we|I) (?:call|use) (\w+)", 1),
                CompiledPattern::new(r"(?i)history of (\w+) calls", 1),
                CompiledPattern::new(r"(?i)recent tool calls", 0), // General tool search
                CompiledPattern::new(r"(?i)tool history", 0),
            ],
            file_symbols_patterns: vec![
                CompiledPattern::new(r"(?i)symbols in (.+\.(?:rs|py|ts|js|go|java|cpp|c|h))", 1),
                CompiledPattern::new(r"(?i)what's in (.+\.(?:rs|py|ts|js|go|java|cpp|c|h))", 1),
                CompiledPattern::new(r"(?i)outline of (.+\.(?:rs|py|ts|js|go|java|cpp|c|h))", 1),
            ],
        }
    }

    /// Classify a query into an intent.
    ///
    /// First checks for explicit prefixes (deterministic), then tries pattern matching.
    pub fn classify(&self, query: &str) -> RouteResult {
        let query = query.trim();

        // 1. Check explicit prefixes first (override pattern detection)
        if let Some(target) = query.strip_prefix("def:") {
            let target = target.trim().to_string();
            return RouteResult::from_intent(QueryIntent::SymbolDefinition(target), 1.0);
        }
        if let Some(target) = query.strip_prefix("callers:") {
            let target = target.trim().to_string();
            return RouteResult::from_intent(QueryIntent::SymbolCallers(target), 1.0);
        }
        if let Some(target) = query.strip_prefix("refs:") {
            let target = target.trim().to_string();
            return RouteResult::from_intent(QueryIntent::SymbolReferences(target), 1.0);
        }
        if let Some(target) = query.strip_prefix("imports:") {
            let target = target.trim().to_string();
            return RouteResult::from_intent(QueryIntent::ModuleImports(target), 1.0);
        }
        if let Some(target) = query.strip_prefix("errors:") {
            let target = target.trim();
            let sig = if target.is_empty() {
                None
            } else {
                Some(target.to_string())
            };
            return RouteResult::from_intent(QueryIntent::ErrorSearch(sig), 1.0);
        }
        if let Some(target) = query.strip_prefix("tools:") {
            let target = target.trim();
            let name = if target.is_empty() {
                None
            } else {
                Some(target.to_string())
            };
            return RouteResult::from_intent(QueryIntent::ToolCalls(name), 1.0);
        }
        if let Some(target) = query.strip_prefix("file:") {
            let target = target.trim().to_string();
            return RouteResult::from_intent(QueryIntent::FileSymbols(target), 1.0);
        }

        // 2. Try pattern matching in priority order
        // Definition patterns
        for pattern in &self.definition_patterns {
            if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::SymbolDefinition(target), 0.9);
            }
        }

        // Caller patterns
        for pattern in &self.caller_patterns {
            if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::SymbolCallers(target), 0.9);
            }
        }

        // Reference patterns
        for pattern in &self.reference_patterns {
            if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::SymbolReferences(target), 0.9);
            }
        }

        // Import patterns
        for pattern in &self.import_patterns {
            if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::ModuleImports(target), 0.9);
            }
        }

        // File symbols patterns
        for pattern in &self.file_symbols_patterns {
            if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::FileSymbols(target), 0.9);
            }
        }

        // Error patterns
        for pattern in &self.error_patterns {
            if pattern.extract_group == 0 {
                // General pattern (no capture)
                if pattern.matches(query) {
                    return RouteResult::from_intent(QueryIntent::ErrorSearch(None), 0.8);
                }
            } else if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::ErrorSearch(Some(target)), 0.9);
            }
        }

        // Tool patterns
        for pattern in &self.tool_patterns {
            if pattern.extract_group == 0 {
                // General pattern (no capture)
                if pattern.matches(query) {
                    return RouteResult::from_intent(QueryIntent::ToolCalls(None), 0.8);
                }
            } else if let Some(target) = pattern.extract(query) {
                return RouteResult::from_intent(QueryIntent::ToolCalls(Some(target)), 0.9);
            }
        }

        // 3. Default to semantic search
        RouteResult::semantic()
    }

    /// Classify with detailed route result.
    pub fn route(&self, query: &str) -> RouteResult {
        self.classify(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_definition_queries() {
        let router = QueryRouter::new();

        // Pattern-based
        let result = router.classify("where is UserService defined");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "UserService"
        ));
        assert_eq!(result.confidence, 0.9);

        let result = router.classify("definition of process_data");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "process_data"
        ));

        let result = router.classify("find function handle_request");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "handle_request"
        ));

        let result = router.classify("what is Config");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "Config"
        ));

        let result = router.classify("show me Parser definition");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "Parser"
        ));
    }

    #[test]
    fn test_classify_caller_queries() {
        let router = QueryRouter::new();

        let result = router.classify("who calls process_data");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolCallers(ref s) if s == "process_data"
        ));

        let result = router.classify("callers of handle_request");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolCallers(ref s) if s == "handle_request"
        ));

        let result = router.classify("what calls authenticate");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolCallers(ref s) if s == "authenticate"
        ));

        let result = router.classify("save is called by");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolCallers(ref s) if s == "save"
        ));
    }

    #[test]
    fn test_classify_reference_queries() {
        let router = QueryRouter::new();

        let result = router.classify("references to Config");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolReferences(ref s) if s == "Config"
        ));

        let result = router.classify("usages of UserStore");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolReferences(ref s) if s == "UserStore"
        ));

        let result = router.classify("where is Logger used");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolReferences(ref s) if s == "Logger"
        ));

        let result = router.classify("find all uses of Handler");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolReferences(ref s) if s == "Handler"
        ));
    }

    #[test]
    fn test_classify_import_queries() {
        let router = QueryRouter::new();

        let result = router.classify("who imports json");
        assert!(matches!(
            result.intent,
            QueryIntent::ModuleImports(ref s) if s == "json"
        ));

        let result = router.classify("files that import serde");
        assert!(matches!(
            result.intent,
            QueryIntent::ModuleImports(ref s) if s == "serde"
        ));
    }

    #[test]
    fn test_classify_error_queries() {
        let router = QueryRouter::new();

        let result = router.classify("errors in main");
        assert!(matches!(
            result.intent,
            QueryIntent::ErrorSearch(Some(ref s)) if s == "main"
        ));

        let result = router.classify("what went wrong");
        assert!(matches!(result.intent, QueryIntent::ErrorSearch(None)));

        let result = router.classify("recent errors");
        assert!(matches!(result.intent, QueryIntent::ErrorSearch(None)));
    }

    #[test]
    fn test_classify_tool_queries() {
        let router = QueryRouter::new();

        let result = router.classify("calls to read_file");
        assert!(matches!(
            result.intent,
            QueryIntent::ToolCalls(Some(ref s)) if s == "read_file"
        ));

        let result = router.classify("recent tool calls");
        assert!(matches!(result.intent, QueryIntent::ToolCalls(None)));

        let result = router.classify("when did we call write");
        assert!(matches!(
            result.intent,
            QueryIntent::ToolCalls(Some(ref s)) if s == "write"
        ));
    }

    #[test]
    fn test_explicit_prefix_override() {
        let router = QueryRouter::new();

        // Explicit prefixes should have confidence 1.0
        let result = router.classify("def: UserService");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "UserService"
        ));
        assert_eq!(result.confidence, 1.0);

        let result = router.classify("callers: process_data");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolCallers(ref s) if s == "process_data"
        ));
        assert_eq!(result.confidence, 1.0);

        let result = router.classify("refs: Config");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolReferences(ref s) if s == "Config"
        ));
        assert_eq!(result.confidence, 1.0);

        let result = router.classify("imports: json");
        assert!(matches!(
            result.intent,
            QueryIntent::ModuleImports(ref s) if s == "json"
        ));
        assert_eq!(result.confidence, 1.0);

        let result = router.classify("errors:");
        assert!(matches!(result.intent, QueryIntent::ErrorSearch(None)));
        assert_eq!(result.confidence, 1.0);

        let result = router.classify("errors: TypeError");
        assert!(matches!(
            result.intent,
            QueryIntent::ErrorSearch(Some(ref s)) if s == "TypeError"
        ));

        let result = router.classify("tools:");
        assert!(matches!(result.intent, QueryIntent::ToolCalls(None)));
        assert_eq!(result.confidence, 1.0);

        let result = router.classify("tools: read_file");
        assert!(matches!(
            result.intent,
            QueryIntent::ToolCalls(Some(ref s)) if s == "read_file"
        ));
    }

    #[test]
    fn test_fallback_to_semantic() {
        let router = QueryRouter::new();

        // Unrecognized query should fall back to semantic
        let result = router.classify("how does authentication work");
        assert!(matches!(result.intent, QueryIntent::SemanticSearch));
        assert_eq!(result.confidence, 1.0);
        assert!(!result.fallback_to_semantic);
        assert!(!result.blend_semantic_context);
    }

    #[test]
    fn test_case_insensitive_matching() {
        let router = QueryRouter::new();

        let result = router.classify("WHERE IS UserService DEFINED");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolDefinition(ref s) if s == "UserService"
        ));

        let result = router.classify("WHO CALLS process_data");
        assert!(matches!(
            result.intent,
            QueryIntent::SymbolCallers(ref s) if s == "process_data"
        ));
    }

    #[test]
    fn test_should_blend_semantic() {
        // Code-intent queries should blend
        assert!(QueryIntent::SymbolDefinition("foo".into()).should_blend_semantic());
        assert!(QueryIntent::SymbolReferences("foo".into()).should_blend_semantic());
        assert!(QueryIntent::SymbolCallers("foo".into()).should_blend_semantic());
        assert!(QueryIntent::ModuleImports("foo".into()).should_blend_semantic());
        assert!(QueryIntent::FileSymbols("foo.rs".into()).should_blend_semantic());

        // Non-code queries should not blend
        assert!(!QueryIntent::SemanticSearch.should_blend_semantic());
        assert!(!QueryIntent::ToolCalls(None).should_blend_semantic());
        assert!(!QueryIntent::ErrorSearch(None).should_blend_semantic());
        assert!(!QueryIntent::DocQa.should_blend_semantic());
    }

    #[test]
    fn test_is_trace_query() {
        assert!(QueryIntent::ToolCalls(None).is_trace_query());
        assert!(QueryIntent::ToolCalls(Some("read".into())).is_trace_query());
        assert!(QueryIntent::ErrorSearch(None).is_trace_query());
        assert!(QueryIntent::ErrorSearch(Some("TypeError".into())).is_trace_query());

        assert!(!QueryIntent::SemanticSearch.is_trace_query());
        assert!(!QueryIntent::SymbolDefinition("foo".into()).is_trace_query());
    }

    #[test]
    fn test_route_result_flags() {
        let router = QueryRouter::new();

        // Definition query should have blend and fallback flags
        let result = router.classify("where is Config defined");
        assert!(result.blend_semantic_context);
        assert!(result.fallback_to_semantic);

        // Trace query should not have blend flag
        let result = router.classify("recent tool calls");
        assert!(!result.blend_semantic_context);
        assert!(!result.fallback_to_semantic);

        // Semantic search should not have flags
        let result = router.classify("how does caching work");
        assert!(!result.blend_semantic_context);
        assert!(!result.fallback_to_semantic);
    }

    #[test]
    fn test_file_symbols_queries() {
        let router = QueryRouter::new();

        let result = router.classify("symbols in src/main.rs");
        assert!(matches!(
            result.intent,
            QueryIntent::FileSymbols(ref s) if s == "src/main.rs"
        ));

        let result = router.classify("what's in utils.py");
        assert!(matches!(
            result.intent,
            QueryIntent::FileSymbols(ref s) if s == "utils.py"
        ));

        // Explicit prefix
        let result = router.classify("file: src/lib.rs");
        assert!(matches!(
            result.intent,
            QueryIntent::FileSymbols(ref s) if s == "src/lib.rs"
        ));
    }
}
