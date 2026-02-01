//! BM25 sparse index using Tantivy.
//!
//! Implements the SparseIndex trait with Tantivy's inverted index for
//! keyword-based retrieval. Uses CodeTokenizer for code-aware tokenization.

use std::path::PathBuf;
use std::sync::Mutex;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Term};

use crate::error::{MemdError, Result};
use crate::index::sparse::{SparseIndex, SparseSearchResult};
use crate::text::CodeTokenizer;
use crate::types::{ChunkId, TenantId};

/// Default memory budget for index writer (50MB).
const DEFAULT_WRITER_MEMORY_BYTES: usize = 50_000_000;

/// BM25 sparse index using Tantivy.
///
/// Provides keyword-based search with BM25 scoring and code-aware tokenization.
/// Thread-safe: reader is cloneable, writer is mutex-protected.
pub struct Bm25Index {
    /// The Tantivy index
    index: Index,
    /// Index reader for search operations
    reader: IndexReader,
    /// Index writer for insert/delete (mutex for thread safety)
    writer: Mutex<IndexWriter>,
    /// Schema for document structure (kept for potential future use)
    #[allow(dead_code)]
    schema: Schema,
    /// Field for tenant ID
    tenant_field: Field,
    /// Field for chunk ID
    chunk_field: Field,
    /// Field for sentence index within chunk
    sentence_field: Field,
    /// Field for indexed text
    text_field: Field,
}

impl Bm25Index {
    /// Create a new in-memory BM25 index.
    pub fn new() -> Result<Self> {
        Self::with_path(None)
    }

    /// Create a new BM25 index with optional persistence path.
    ///
    /// If path is None, creates an in-memory index.
    /// If path is Some, creates a persistent index at the given directory.
    pub fn with_path(path: Option<PathBuf>) -> Result<Self> {
        // Build schema
        let mut schema_builder = Schema::builder();

        // Tenant ID: stored and indexed as exact string
        let tenant_field = schema_builder.add_text_field("tenant_id", STRING | STORED);

        // Chunk ID: stored and indexed as exact string
        let chunk_field = schema_builder.add_text_field("chunk_id", STRING | STORED);

        // Sentence index: stored for result metadata
        let sentence_field = schema_builder.add_u64_field("sentence_idx", STORED);

        // Text field: indexed with code tokenizer for BM25 search
        let text_indexing = TextFieldIndexing::default()
            .set_tokenizer("code")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let text_options = TextOptions::default()
            .set_indexing_options(text_indexing)
            .set_stored();
        let text_field = schema_builder.add_text_field("text", text_options);

        let schema = schema_builder.build();

        // Create index
        let index = match path {
            Some(p) => {
                std::fs::create_dir_all(&p)?;
                Index::create_in_dir(&p, schema.clone())
                    .map_err(|e| MemdError::StorageError(format!("create index: {}", e)))?
            }
            None => Index::create_in_ram(schema.clone()),
        };

        // Register code tokenizer
        let code_tokenizer = TextAnalyzer::builder(CodeTokenizer::new()).build();
        index.tokenizers().register("code", code_tokenizer);

        // Create writer
        let writer = index
            .writer(DEFAULT_WRITER_MEMORY_BYTES)
            .map_err(|e| MemdError::StorageError(format!("create writer: {}", e)))?;

        // Create reader with automatic reload
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| MemdError::StorageError(format!("create reader: {}", e)))?;

        Ok(Self {
            index,
            reader,
            writer: Mutex::new(writer),
            schema,
            tenant_field,
            chunk_field,
            sentence_field,
            text_field,
        })
    }

    /// Commit pending changes to make them searchable.
    ///
    /// Called automatically after batch operations, but can be called
    /// manually to force visibility of recent writes.
    pub fn commit(&self) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| MemdError::StorageError(format!("lock writer: {}", e)))?;
        writer
            .commit()
            .map_err(|e| MemdError::StorageError(format!("commit: {}", e)))?;
        Ok(())
    }

    /// Get total number of documents in the index.
    pub fn total_docs(&self) -> Result<u64> {
        let searcher = self.reader.searcher();
        Ok(searcher.num_docs())
    }
}

impl Default for Bm25Index {
    fn default() -> Self {
        Self::new().expect("failed to create default Bm25Index")
    }
}

impl SparseIndex for Bm25Index {
    fn insert(
        &self,
        tenant_id: &TenantId,
        chunk_id: &ChunkId,
        sentences: &[String],
    ) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| MemdError::StorageError(format!("lock writer: {}", e)))?;

        for (idx, sentence) in sentences.iter().enumerate() {
            let doc = doc!(
                self.tenant_field => tenant_id.as_str(),
                self.chunk_field => chunk_id.to_string(),
                self.sentence_field => idx as u64,
                self.text_field => sentence.clone(),
            );
            writer
                .add_document(doc)
                .map_err(|e| MemdError::StorageError(format!("add document: {}", e)))?;
        }

        // Commit after batch
        writer
            .commit()
            .map_err(|e| MemdError::StorageError(format!("commit: {}", e)))?;

        Ok(())
    }

    fn search(
        &self,
        tenant_id: &TenantId,
        query: &str,
        k: usize,
    ) -> Result<Vec<SparseSearchResult>> {
        // Reload to see recent commits (BEFORE getting searcher)
        self.reader
            .reload()
            .map_err(|e| MemdError::StorageError(format!("reload reader: {}", e)))?;

        let searcher = self.reader.searcher();

        // Build tenant filter
        let tenant_term = Term::from_field_text(self.tenant_field, tenant_id.as_str());
        let tenant_query: Box<dyn Query> =
            Box::new(TermQuery::new(tenant_term, IndexRecordOption::Basic));

        // Build text query using tokenizer
        let query_parser =
            tantivy::query::QueryParser::for_index(&self.index, vec![self.text_field]);
        let text_query = query_parser
            .parse_query(query)
            .map_err(|e| MemdError::ValidationError(format!("parse query: {}", e)))?;

        // Combine with boolean query: must match tenant AND text
        let combined_query = BooleanQuery::new(vec![
            (Occur::Must, tenant_query),
            (Occur::Must, text_query),
        ]);

        // Execute search
        let top_docs = searcher
            .search(&combined_query, &TopDocs::with_limit(k))
            .map_err(|e| MemdError::StorageError(format!("search: {}", e)))?;

        // Convert results
        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc = searcher.doc::<tantivy::TantivyDocument>(doc_address).map_err(|e| {
                MemdError::StorageError(format!("retrieve doc: {}", e))
            })?;

            // Extract fields
            let chunk_id_str = doc
                .get_first(self.chunk_field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| MemdError::StorageError("missing chunk_id".into()))?;

            let sentence_idx = doc
                .get_first(self.sentence_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;

            let chunk_id = ChunkId::parse(chunk_id_str)?;

            results.push(SparseSearchResult {
                chunk_id,
                score,
                sentence_idx,
            });
        }

        Ok(results)
    }

    fn delete(&self, tenant_id: &TenantId, chunk_id: &ChunkId) -> Result<bool> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| MemdError::StorageError(format!("lock writer: {}", e)))?;

        // Build query to match both tenant and chunk
        let tenant_term = Term::from_field_text(self.tenant_field, tenant_id.as_str());
        let chunk_term = Term::from_field_text(self.chunk_field, &chunk_id.to_string());

        // Delete by term queries combined
        // Tantivy delete_term deletes all docs matching that term
        // We need to be more precise, so we use delete_query
        let tenant_query: Box<dyn Query> =
            Box::new(TermQuery::new(tenant_term, IndexRecordOption::Basic));
        let chunk_query: Box<dyn Query> =
            Box::new(TermQuery::new(chunk_term, IndexRecordOption::Basic));

        let delete_query = BooleanQuery::new(vec![
            (Occur::Must, tenant_query),
            (Occur::Must, chunk_query),
        ]);

        // Get count before delete to determine if anything was deleted
        let searcher = self.reader.searcher();
        let count_before = searcher
            .search(&delete_query, &tantivy::collector::Count)
            .unwrap_or(0);

        writer
            .delete_query(Box::new(delete_query))
            .map_err(|e| MemdError::StorageError(format!("delete query: {}", e)))?;
        writer
            .commit()
            .map_err(|e| MemdError::StorageError(format!("commit: {}", e)))?;

        Ok(count_before > 0)
    }

    fn doc_count(&self, tenant_id: &TenantId) -> Result<u64> {
        // Reload to see recent commits
        self.reader
            .reload()
            .map_err(|e| MemdError::StorageError(format!("reload reader: {}", e)))?;

        let searcher = self.reader.searcher();

        let tenant_term = Term::from_field_text(self.tenant_field, tenant_id.as_str());
        let tenant_query = TermQuery::new(tenant_term, IndexRecordOption::Basic);

        let count = searcher
            .search(&tenant_query, &tantivy::collector::Count)
            .map_err(|e| MemdError::StorageError(format!("count: {}", e)))?;

        Ok(count as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_tenant() -> TenantId {
        TenantId::new("test_tenant").unwrap()
    }

    #[test]
    fn test_insert_and_search() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let chunk_id = ChunkId::new();

        let sentences = vec![
            "function parseJSON() { return JSON.parse(data); }".to_string(),
            "Returns parsed data from JSON string".to_string(),
        ];

        index.insert(&tenant, &chunk_id, &sentences).unwrap();

        // Search for parseJSON
        let results = index.search(&tenant, "parseJSON", 10).unwrap();
        assert!(!results.is_empty(), "should find parseJSON");
        assert_eq!(results[0].chunk_id, chunk_id);
        assert!(results[0].score > 0.0);

        // Search for parsed data
        let results = index.search(&tenant, "parsed data", 10).unwrap();
        assert!(!results.is_empty(), "should find 'parsed data'");
    }

    #[test]
    fn test_keyword_exact_match() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let chunk_id = ChunkId::new();

        let sentences = vec![
            "The getUserById function returns a User object".to_string(),
        ];

        index.insert(&tenant, &chunk_id, &sentences).unwrap();

        // Identifier should be split: getUserById -> get, user, by, id
        let results = index.search(&tenant, "getUserById", 10).unwrap();
        assert!(!results.is_empty(), "should find getUserById");

        // Individual tokens should also match
        let results = index.search(&tenant, "user", 10).unwrap();
        assert!(!results.is_empty(), "should find 'user' token");
    }

    #[test]
    fn test_tenant_isolation() {
        let index = Bm25Index::new().unwrap();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();
        let chunk_id = ChunkId::new();

        let sentences = vec!["secret data for tenant A".to_string()];

        index.insert(&tenant_a, &chunk_id, &sentences).unwrap();

        // Tenant B should not see tenant A's data
        let results = index.search(&tenant_b, "secret", 10).unwrap();
        assert!(results.is_empty(), "tenant B should not see tenant A data");

        // Tenant A should see their own data
        let results = index.search(&tenant_a, "secret", 10).unwrap();
        assert!(!results.is_empty(), "tenant A should see their data");
    }

    #[test]
    fn test_delete() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let chunk_id = ChunkId::new();

        let sentences = vec!["deletable content here".to_string()];

        index.insert(&tenant, &chunk_id, &sentences).unwrap();

        // Verify it exists
        let results = index.search(&tenant, "deletable", 10).unwrap();
        assert!(!results.is_empty(), "should find before delete");

        // Delete it
        let deleted = index.delete(&tenant, &chunk_id).unwrap();
        assert!(deleted, "should return true for successful delete");

        // Verify it's gone (need to reload)
        index.reader.reload().unwrap();
        let results = index.search(&tenant, "deletable", 10).unwrap();
        assert!(results.is_empty(), "should not find after delete");
    }

    #[test]
    fn test_multiple_sentences() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let chunk_id = ChunkId::new();

        let sentences = vec![
            "First sentence about apples".to_string(),
            "Second sentence about oranges".to_string(),
            "Third sentence about bananas".to_string(),
        ];

        index.insert(&tenant, &chunk_id, &sentences).unwrap();

        // Search for oranges (sentence index 1)
        let results = index.search(&tenant, "oranges", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].sentence_idx, 1, "should match sentence at index 1");
    }

    #[test]
    fn test_code_identifiers() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let chunk_id = ChunkId::new();

        let sentences = vec![
            "snake_case_function and camelCaseMethod examples".to_string(),
        ];

        index.insert(&tenant, &chunk_id, &sentences).unwrap();

        // Snake case components should be searchable
        let results = index.search(&tenant, "snake", 10).unwrap();
        assert!(!results.is_empty(), "should find 'snake' from snake_case");

        let results = index.search(&tenant, "case", 10).unwrap();
        assert!(!results.is_empty(), "should find 'case' from snake_case");

        // Camel case components should be searchable
        let results = index.search(&tenant, "camel", 10).unwrap();
        assert!(!results.is_empty(), "should find 'camel' from camelCase");

        let results = index.search(&tenant, "method", 10).unwrap();
        assert!(!results.is_empty(), "should find 'method' from camelCaseMethod");
    }

    #[test]
    fn test_doc_count() {
        let index = Bm25Index::new().unwrap();
        let tenant_a = TenantId::new("tenant_a").unwrap();
        let tenant_b = TenantId::new("tenant_b").unwrap();

        // Insert 3 sentences for tenant_a
        index
            .insert(
                &tenant_a,
                &ChunkId::new(),
                &["sentence 1".to_string(), "sentence 2".to_string()],
            )
            .unwrap();

        // Insert 1 sentence for tenant_b
        index
            .insert(&tenant_b, &ChunkId::new(), &["other".to_string()])
            .unwrap();

        // Check counts
        assert_eq!(index.doc_count(&tenant_a).unwrap(), 2);
        assert_eq!(index.doc_count(&tenant_b).unwrap(), 1);
    }

    #[test]
    fn test_empty_query() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();

        // Empty query should not crash
        let results = index.search(&tenant, "", 10);
        // May return error or empty results, but should not panic
        assert!(results.is_ok() || results.is_err());
    }

    #[test]
    fn test_special_characters() {
        let index = Bm25Index::new().unwrap();
        let tenant = create_test_tenant();
        let chunk_id = ChunkId::new();

        let sentences = vec![
            "Error: SQLITE_BUSY at line 42".to_string(),
            "fn main() -> Result<(), Error>".to_string(),
        ];

        index.insert(&tenant, &chunk_id, &sentences).unwrap();

        // Should find content with special chars
        let results = index.search(&tenant, "SQLITE", 10).unwrap();
        assert!(!results.is_empty(), "should find SQLITE");

        let results = index.search(&tenant, "Error", 10).unwrap();
        assert!(!results.is_empty(), "should find Error");
    }
}
