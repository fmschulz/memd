# Benchmark Datasets for Evaluation

This document lists standard IR/code retrieval benchmark datasets that can be integrated into the memd eval suite.

## 1. BEIR Benchmark

**Overview:** Heterogeneous IR benchmark with 18 diverse datasets for zero-shot evaluation

**Source:** [github.com/beir-cellar/beir](https://github.com/beir-cellar/beir)

**Datasets Available:**
- MS MARCO (passage ranking)
- Natural Questions
- HotpotQA
- FiQA-2018 (financial)
- SciFact (scientific fact verification)
- ArguAna (argument retrieval)
- And 12 more diverse domains

**Download:**
```python
from beir import util

dataset = "scifact"  # or any of: msmarco, nq, hotpotqa, fiqa, etc.
url = f"https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/{dataset}.zip"
data_path = util.download_and_unzip(url, out_dir)
```

**Alternative Download:**
- HuggingFace: https://huggingface.co/BeIR
- ir_datasets: `ir_datasets.load("beir/scifact")`

**Format:**
- queries.jsonl: Query ID, query text
- corpus.jsonl: Document ID, title, text
- qrels.tsv: Query-document relevance judgments

**Integration Steps:**
1. Download SciFact (smallest, ~65K docs) or MS MARCO (8.8M docs)
2. Convert to hybrid_test.json format
3. Create evals/datasets/retrieval/beir_scifact.json
4. Add to eval suite with BEIR-specific metrics

**Citation:**
Thakur et al. "BEIR: A Heterogeneous Benchmark for Zero-shot Evaluation of Information Retrieval Models". NeurIPS 2021.

---

## 2. CodeSearchNet

**Overview:** Code retrieval benchmark with 6M functions across 6 programming languages

**Source:** [github.com/github/CodeSearchNet](https://github.com/github/CodeSearchNet)

**Languages:** Go, Java, JavaScript, PHP, Python, Ruby

**Components:**
- **Corpus:** 6 million documented functions from open-source repositories
- **Test Set:** 99 natural language queries with ~4K expert relevance annotations
- **Challenge Set:** Held-out test queries for leaderboard evaluation

**Download:**

**Method 1: Official Script**
```bash
git clone https://github.com/github/CodeSearchNet
cd CodeSearchNet
./script/setup
```

**Method 2: Direct S3 Download**
```bash
# Download specific language
wget https://s3.amazonaws.com/code-search-net/CodeSearchNet/v2/python.zip
unzip python.zip
```

**Method 3: ir_datasets**
```python
import ir_datasets
dataset = ir_datasets.load("codesearchnet")
for doc in dataset.docs_iter():
    print(doc)  # doc_id, code, docstring, etc.
```

**Format:**
- Each language has training/validation/test splits
- JSONL files with fields: repo, path, func_name, original_string, language, code, code_tokens, docstring, docstring_tokens, sha, url

**Integration Steps:**
1. Download Python subset (~500MB)
2. Extract 100-200 function-docstring pairs for test set
3. Create natural language queries from docstrings
4. Convert to hybrid_test.json format with code as documents
5. Create evals/datasets/retrieval/codesearchnet_python.json
6. Measure code-specific retrieval quality

**Use Cases for memd:**
- Test code function search by natural language description
- Evaluate identifier tokenization (camelCase, snake_case)
- Benchmark keyword matching for function names
- Test semantic similarity on code documentation

**Citation:**
Husain et al. "CodeSearchNet Challenge: Evaluating the State of Semantic Code Search". arXiv:1909.09436, 2019.

---

## Integration Priority

1. **Start with SciFact (BEIR)** - smallest, well-curated, good for semantic search baseline
2. **Add CodeSearchNet Python** - code-specific, tests identifier tokenization

## Recommended Eval Suite Structure

```
evals/datasets/retrieval/
├── hybrid_test.json          # Current custom dataset (v1.1)
├── beir_scifact.json         # Scientific claim verification
└── codesearchnet_py.json     # Python function search
```

## Sources

- BEIR: https://github.com/beir-cellar/beir
- BEIR Datasets: https://github.com/beir-cellar/beir/wiki/Datasets-available
- BEIR on HuggingFace: https://huggingface.co/BeIR
- CodeSearchNet: https://github.com/github/CodeSearchNet
- CodeSearchNet Paper: https://arxiv.org/abs/1909.09436
- CodeSearchNet on Kaggle: https://www.kaggle.com/datasets/omduggineni/codesearchnet
- ir_datasets Documentation: https://ir-datasets.com/
