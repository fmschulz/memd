# Phase 4: Sparse Lexical + Fusion - Context

**Gathered:** 2026-01-30
**Status:** Ready for planning

<domain>
## Phase Boundary

Hybrid retrieval combining dense vector search with lexical (keyword-based) search. This phase adds BM25 indexing for keyword matching, fuses dense + sparse candidates using reciprocal rank fusion (RRF), and applies feature-based reranking with recency/project bonuses. Context packing handles deduplication and diversity via MMR.

This phase does NOT include:
- Query understanding/expansion (out of scope)
- User feedback integration (future phase)
- Structural indexes (Phase 6)

</domain>

<decisions>
## Implementation Decisions

### Index granularity & tokenization

**Granularity:**
- Sentence-level indexing, not full chunks
- Split chunks into sentences, index each sentence separately
- Better precision for keyword queries targeting specific terms

**Code-aware tokenization:**
- Different handling for code vs prose
- Split identifiers: camelCase → [camel, Case], snake_case → [snake, case]
- Preserve operators and keywords for exact matching
- Better retrieval for function names and code symbols

**Normalization:**
- Use lemmatization (dictionary-based normalization)
- running → run, better → good
- More accurate for prose than stemming
- Applied to prose; code identifiers may need different handling

**Case sensitivity:**
- Hybrid approach: preserve case for acronyms
- Lowercase prose terms for matching flexibility
- Preserve all-caps terms (HTTP, API, SQL, etc.)
- Maintains signal from capitalization in code while keeping prose flexible

### Claude's Discretion

- Stopword handling (whether to remove common words)
- Special character handling (punctuation, symbols)
- Exact fusion weights for dense vs sparse signals
- RRF parameter tuning (k value)
- MMR diversity parameter (lambda)
- Context packing window size
- Snippet extraction length and strategy
- Feature normalization in reranker

</decisions>

<specifics>
## Specific Ideas

No specific requirements — open to standard approaches for areas under Claude's discretion.

Researcher should investigate:
- Sentence splitting strategies that work for both code and prose
- Lemmatization libraries compatible with Rust ecosystem
- Acronym detection heuristics (all-caps, 2+ chars, etc.)
- Code tokenization libraries or patterns (tree-sitter, regex-based)

</specifics>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope.

</deferred>

---

*Phase: 04-sparse-lexical-+-fusion*
*Context gathered: 2026-01-30*
