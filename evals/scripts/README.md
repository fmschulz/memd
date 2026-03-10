# Embedding Model Comparison Tool

## Overview

This tool provides automated benchmarking and comparison of different embedding models for memd. It runs the same test suite with different models and generates a comparative analysis report.

## Offline Benchmark Protocol

For reproducible retrieval-quality benchmarking against labeled corpora, use:

```bash
./evals/scripts/run_offline_retrieval_benchmark.sh \
  --model all-minilm \
  --system-variant hybrid-feature \
  --bootstrap-iterations 1000 \
  --seed 42
```

This runs the benchmark protocol suite against:

- `evals/datasets/retrieval/beir_fiqa.json`
- `evals/datasets/retrieval/beir_scidocs.json`
- `evals/datasets/retrieval/beir_trec-covid.json`

Outputs are a machine-readable cross-corpus JSON report:

- `cross_corpus_<model>_<system_variant>.json` (normalized cross-corpus report with per-dataset summaries)

To guard against quality regressions across releases, compare reports with:

```bash
cargo run -p memd-evals -- --suite benchmark-regression --skip-build \
  --baseline-report evals/results/offline/baseline.json \
  --candidate-report evals/results/offline/candidate.json \
  --significance-alpha 0.05 \
  --min-effect-size 0.1 \
  --regression-report-json evals/results/offline/regression_gate.json
```

### LongMemEval (public corpus) benchmark

```bash
# Optional throughput tuning for indexing-heavy runs:
# MEMD_EVAL_INGEST_BATCH_SIZE controls harness add_batch size (default 32)
# MEMD_EMBED_BATCH_SIZE controls memd dense embed batch size (default 32)
MEMD_EVAL_INGEST_BATCH_SIZE=128 MEMD_EMBED_BATCH_SIZE=64 \
./evals/scripts/run_longmemeval_benchmark.sh \
  --split s \
  --model all-minilm \
  --system-variant hybrid-feature \
  --max-queries 200 \
  --max-sessions-per-query 40
```

This downloads LongMemEval (if needed), runs the benchmark harness, and writes:

- `longmemeval_<split>_<model>_<system_variant>.json`

### Variant matrix benchmark (recommended for baseline comparisons)

```bash
./evals/scripts/run_variant_matrix_benchmark.sh \
  --model all-minilm \
  --with-longmemeval-s \
  --max-queries 200 \
  --max-sessions-per-query 40 \
  --seed 42
```

Default variants:

- `hybrid-feature`
- `hybrid-cross-encoder`
- `dense-only`
- `bm25-only`

You can keep runs fast for CI/smoke checks by setting:

```bash
--max-queries 200 --max-documents 10000
```

## Usage

### Quick Start

```bash
# From project root
./evals/scripts/compare_models.sh
```

This will:
1. Build memd
2. Download both embedding models (if needed)
3. Run hybrid retrieval benchmarks with all-MiniLM-L6-v2
4. Run hybrid retrieval benchmarks with Qwen3-Embedding-0.6B
5. Generate a comparison report

### Output Files

All results are saved to `evals/results/` with timestamps:

- `all-minilm_YYYY-MM-DD_HH-MM-SS.log` - Full benchmark log for all-MiniLM
- `qwen3_YYYY-MM-DD_HH-MM-SS.log` - Full benchmark log for Qwen3
- `model_comparison_YYYY-MM-DD_HH-MM-SS.md` - Comparative analysis report

### Individual Model Benchmarks

```bash
# Test all-MiniLM-L6-v2
./docker-dev.sh eval --suite hybrid --embedding-model all-minilm

# Test Qwen3-Embedding-0.6B
./docker-dev.sh eval --suite hybrid --embedding-model qwen3

# Test with retrieval suite instead
./docker-dev.sh eval --suite retrieval --embedding-model all-minilm
```

## Supported Models

### all-MiniLM-L6-v2 (default)
- **Dimensions**: 384
- **Model Size**: ~23MB (quantized)
- **MTEB Score**: 56.3
- **Pooling**: Mean pooling
- **Speed**: Fast
- **Use Case**: General purpose, resource-constrained environments

### Qwen3-Embedding-0.6B
- **Dimensions**: 1024
- **Model Size**: ~614MB (quantized)
- **MTEB Score**: 64.33 (+15% vs all-MiniLM)
- **Pooling**: Last-token pooling
- **Speed**: Slower
- **Use Case**: Maximum quality, ample resources

## Test Suites

### Hybrid Retrieval Suite
- Tests keyword (sparse/BM25) and semantic (dense/embedding) retrieval
- Measures Recall@10, MRR, Precision@10 per query type
- Includes performance baseline (p50, p90, p99 latency)
- **Recommended** for model comparison

### Retrieval Suite
- Tests dense embedding retrieval only
- Code similarity evaluation
- Measures Recall@10, MRR, Precision@10

## Metrics Collected

### Quality Metrics
- **Recall@10**: What fraction of relevant documents appear in top 10 results
- **MRR (Mean Reciprocal Rank)**: How high the first relevant result ranks
- **Precision@10**: What fraction of top 10 results are relevant

### Performance Metrics
- **p50 latency**: Median query time
- **p90 latency**: 90th percentile query time
- **p99 latency**: 99th percentile query time
- **Mean latency**: Average query time

### Per Query Type (Hybrid Suite)
- **Keyword queries**: Exact match tests (target: 0.9 recall)
- **Semantic queries**: Conceptual similarity (target: 0.7 recall)
- **Mixed queries**: Benefits from both (target: 0.75 recall)

## Performance Targets

- p50 latency: < 100ms
- p99 latency: < 500ms
- Overall Recall@10: > 0.75
- Overall MRR: > 0.6
- Keyword Recall@10: > 0.85

## Model Selection Guide

### Choose all-MiniLM-L6-v2 if:
- Working with limited resources (memory, disk, compute)
- Need fast query response times
- Quality is "good enough" (56.3 MTEB)
- Running on edge devices or embedded systems

### Choose Qwen3-Embedding-0.6B if:
- Maximum quality is priority
- Have sufficient resources (1GB+ memory for model)
- Willing to accept slower query times
- Need best-in-class retrieval accuracy

## Troubleshooting

### Model Download Fails
- Check internet connection
- Verify Hugging Face URLs are accessible
- Check available disk space (~1GB needed for both models)

### Benchmark Hangs
- Check memd logs for errors
- Verify Docker has enough resources
- Try running with smaller dataset first

### Different Results on Re-run
- Embedding models are deterministic
- Variability comes from BM25 scoring or test order
- Re-run 3 times and average for stable results

## Implementation Details

### Model Warmup
The script includes a warmup phase that:
- Downloads models to Docker cache (in `/root/.cache/memd/models/`)
- Initializes ONNX runtime
- Prevents download timeouts during actual benchmarks

### Docker Environment
All benchmarks run in Docker to ensure:
- Correct glibc version
- Consistent environment
- Isolated model cache
- Reproducible results

### Benchmark Flow
1. Build memd binary
2. Warmup: Download and initialize both models
3. Run all-MiniLM benchmark (hybrid suite)
4. Run Qwen3 benchmark (hybrid suite)
5. Extract metrics from logs
6. Generate comparison report

## Next Steps

After running benchmarks:
1. Review the generated comparison report
2. Analyze quality vs. performance trade-offs
3. Choose appropriate model for your use case
4. Update production config accordingly

## Files

- `compare_models.sh` - Main comparison script
- `../results/` - Benchmark outputs and reports
- `../datasets/retrieval/hybrid_test.json` - Test dataset
- `../../crates/memd/src/embeddings/download.rs` - Model download logic

## See Also

- [Benchmark Datasets](../evals/BENCHMARK_DATASETS.md)
- [Eval Harness](../evals/harness/)
- [Test Suites](../evals/harness/src/suites/)
