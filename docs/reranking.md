# Optional rerankers

`memd` has two opt-in rerank paths that sit on top of the default hybrid
search. Neither is compiled into the binary by default and neither is on the
quickstart path. The default search command always returns the built-in
hybrid ranking; rerankers are explicit precision lifts.

## ONNX cross-encoder (in-process)

ONNX in this repo is **only** for the optional cross-encoder reranker. The
default embedding path is Candle; a normal `cargo build` does not enable ONNX.

```bash
cargo build --release --features cross-encoder-reranker

./target/release/memd --search-variant hybrid-cross-encoder search \
  --tenant-id quickstart \
  --query "auth config validation"
```

Runtime behaviour:

- `--search-variant hybrid-cross-encoder` selects the ONNX reranker for
  hybrid search.
- The scorer is initialized when the persistent store opens, not lazily on
  first query.
- If the feature is not compiled in, or ONNX initialization fails, `memd`
  logs a warning and falls back to the feature reranker.

Model and runtime assets:

- Cross-encoder model: `Xenova/ms-marco-MiniLM-L-6-v2` ONNX
- Tokenizer: matching `tokenizer.json`
- ONNX Runtime shared library: downloaded from GitHub releases on supported
  targets (`linux/x86_64`, `linux/aarch64`)
- Default cache dir: `~/.cache/memd/cross-encoder`

Cross-encoder environment variables:

| Variable | Purpose |
| --- | --- |
| `MEMD_CROSS_ENCODER_DISABLE` | Set to `1` to disable the cross-encoder. |
| `MEMD_CROSS_ENCODER_MODEL_PATH` | Use a local ONNX model file instead of downloading one. |
| `MEMD_CROSS_ENCODER_TOKENIZER_PATH` | Use a local tokenizer file instead of downloading one. |
| `MEMD_CROSS_ENCODER_CACHE_DIR` | Override the cache directory; default is `~/.cache/memd/cross-encoder`. |
| `MEMD_CROSS_ENCODER_ORT_DYLIB_PATH` | Use a local ONNX Runtime shared library. |
| `MEMD_CROSS_ENCODER_ORT_URL` | Download ONNX Runtime from a custom archive URL. |
| `MEMD_CROSS_ENCODER_ORT_VERSION` | Select the ONNX Runtime release version used for the default download URL. |

Real ONNX smoke test (requires network on first run):

```bash
cargo test -p memd --features cross-encoder-reranker \
  smoke_real_onnx_scores_relevant_pair_higher -- --ignored --nocapture
```

## MemReranker-4B (out-of-process, Python runtime)

MemReranker-4B is available only as an explicit post-retrieval search
option. It is not compiled into the Rust binary, not enabled by default, and
not part of the rapid setup path.

```bash
./target/release/memd search \
  --tenant-id quickstart \
  --project-id auth \
  --query "auth config validation" \
  --k 50 \
  --reranker auto \
  --format markdown
```

Runtime behaviour:

- `--reranker none` is the default.
- `--reranker auto` uses MemReranker-4B only when CUDA, Python, PyTorch,
  `sentence-transformers`, and the model runtime are available; otherwise
  the output falls back to the built-in search order and records the
  fallback reason in JSON output.
- `--reranker memreranker-4b` requires the model path and fails if the
  optional runtime is unavailable.
- `--reranker-device cpu` is allowed for experiments but not recommended
  for interactive agent use.

The optional path loads `IAAR-Shanghai/MemReranker-4B` through
`sentence_transformers.CrossEncoder` with `trust_remote_code=True`. Pin the
model revision in controlled benchmark environments if exact reproducibility
is required.

## Which reranker should I use?

| Need | Use |
| --- | --- |
| Faster + smaller, in-process | ONNX cross-encoder (`hybrid-cross-encoder`) |
| Highest precision, GPU-friendly | MemReranker-4B (`--reranker auto`) |
| Default agent use | Neither — hybrid feature reranker already does well |

The [Bright-Pro adapter](benchmarking.md#bright-pro-scoped-adapter-biology-q5d141)
table shows the recall/nDCG lift each path delivers on a biology workload.
