# Contributing to memd

Thank you for your interest in contributing to memd! This document provides guidelines for contributing to the project.

## Getting Started

1. Fork the repository on GitHub
2. Clone your fork locally
3. Create a new branch for your feature or bugfix
4. Make your changes
5. Run tests and ensure they pass
6. Submit a pull request

## Development Setup

### Prerequisites

- Rust 1.75+ ([install](https://rustup.rs/))
- Git

### Build and Test

```bash
# Clone your fork
git clone https://github.com/YOUR_USERNAME/memd.git
cd memd

# Build the project
cargo build

# Run tests
cargo test

# Run evaluation suite
cargo run --bin memd-evals -- --suite all

# Format code
cargo fmt

# Check lints
cargo clippy -- -D warnings
```

## Code Guidelines

### Code Style

- Follow Rust standard style (enforced by `cargo fmt`)
- Run `cargo clippy` and address all warnings
- Keep functions focused and under 100 lines when possible
- Use meaningful variable and function names

### Testing

All new features must include tests:

- **Unit tests**: Test individual functions and modules
- **Integration tests**: Test component interactions
- **Evaluation tests**: Add to relevant test suite (A-F) if applicable

```bash
# Run specific test
cargo test test_name

# Run with output
cargo test -- --show-output

# Run specific suite
cargo run --bin memd-evals -- --suite retrieval
```

### Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/) format:

```
type(scope): subject

body (optional)
```

Types:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `test`: Adding or updating tests
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `chore`: Maintenance tasks

Examples:
```
feat(embeddings): add support for custom pooling strategies

fix(search): correct HNSW distance calculation for edge cases

docs: update QUICKSTART with MCP integration examples
```

## Pull Request Process

1. **Update documentation**: If your change affects user-facing behavior, update README.md or QUICKSTART.md

2. **Add tests**: Ensure your changes are covered by tests

3. **Run full test suite**:
   ```bash
   cargo test
   cargo run --bin memd-evals -- --suite all
   ```

4. **Format and lint**:
   ```bash
   cargo fmt
   cargo clippy -- -D warnings
   ```

5. **Write clear PR description**:
   - What problem does this solve?
   - How does it solve it?
   - Any breaking changes?
   - Related issues?

6. **Keep PRs focused**: One feature or fix per PR when possible

## Areas for Contribution

### High Priority

- **Performance optimizations**: Improve search latency, reduce memory usage
- **Additional language support**: Extend structural parsing to more languages
- **Documentation**: Examples, tutorials, use case guides
- **Bug fixes**: See [Issues](https://github.com/fmschulz/memd/issues)

### Medium Priority

- **Embedding backends**: Support for additional embedding models
- **Query improvements**: Better intent classification, query understanding
- **Tooling**: CLI utilities, debugging helpers
- **Testing**: Additional test cases, edge case coverage

### Future Features

- **Architecture B**: Graph memory module (see docs/implementation_v0.md)
- **Learned router**: ML-based query intent classification
- **Distributed storage**: Multi-node deployment support

## Architecture Overview

memd is organized into modules:

```
crates/memd/src/
├── chunking/       # Text chunking
├── compaction/     # Index maintenance
├── embeddings/     # Embedding generation
├── index/          # HNSW, BM25, caching
├── mcp/            # MCP protocol server
├── metrics/        # Performance tracking
├── retrieval/      # Search fusion and reranking
├── store/          # Storage layer
├── structural/     # Code parsing and indexing
├── text/           # Text processing
├── tiered/         # Hot/warm/cold tiering
└── types/          # Core types
```

### Key Traits

- `Embedder`: Generate embeddings from text
- `Store`: Memory storage interface
- `WarmTierSearch`: Warm index search interface

When adding features, prefer implementing existing traits over creating new abstractions.

## Evaluation Suites

memd includes 6 test suites in `evals/harness/src/suites/`:

- **Suite A**: MCP protocol conformance
- **Suite B**: Retrieval quality
- **Suite C**: Hybrid search
- **Suite D**: Tiered search
- **Suite E**: Structural queries
- **Suite F**: Compaction

When adding features, add tests to the relevant suite.

## Performance Expectations

Target metrics (measured by evaluation suite):

- **Retrieval Quality**: Recall@10 > 0.7 (semantic), > 0.9 (keyword)
- **Latency**: p50 < 100ms (warm tier), p99 < 500ms
- **Cache Hit Rate**: > 80% for hot tier
- **Structural Accuracy**: > 80% for definitions/imports

## Questions or Issues?

- **Questions**: Open a [Discussion](https://github.com/fmschulz/memd/discussions)
- **Bugs**: File an [Issue](https://github.com/fmschulz/memd/issues)
- **Security**: Email fmschulz@gmail.com (do not file public issues)

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

Thank you for contributing to memd! 🚀
