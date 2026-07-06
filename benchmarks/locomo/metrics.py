"""LoCoMo metric implementations.

Answer metrics mirror the upstream evaluation code
(https://raw.githubusercontent.com/snap-research/locomo/main/task_eval/evaluation.py):
- normalize_answer: strip commas, lowercase, remove punctuation, remove
  articles (a|an|the|and), collapse whitespace.
- F1: Porter-stemmed token multiset overlap (precision/recall harmonic mean).
- EM: set equality of normalized (unstemmed) tokens.
- SubEM is NOT defined upstream; we use the common memory-paper definition:
  normalized gold answer appears as a substring of the normalized prediction.

Retrieval metrics: MRR@k (reciprocal rank of the first gold evidence id),
Hit@k (any gold in top-k), evidence recall@k and precision@k.
"""

import re
import string
from collections import Counter

import porter

_ARTICLES = re.compile(r"\b(a|an|the|and)\b")
_PUNC = set(string.punctuation)


def normalize_answer(s: str) -> str:
    s = str(s).replace(",", "")
    s = s.lower()
    s = "".join(ch for ch in s if ch not in _PUNC)
    s = _ARTICLES.sub(" ", s)
    return " ".join(s.split())


def f1_score(prediction: str, ground_truth: str) -> float:
    pred_tokens = [porter.stem(w) for w in normalize_answer(prediction).split()]
    gold_tokens = [porter.stem(w) for w in normalize_answer(ground_truth).split()]
    common = Counter(pred_tokens) & Counter(gold_tokens)
    num_same = sum(common.values())
    if num_same == 0 or not pred_tokens or not gold_tokens:
        return 0.0
    precision = num_same / len(pred_tokens)
    recall = num_same / len(gold_tokens)
    return 2 * precision * recall / (precision + recall)


def exact_match_score(prediction: str, ground_truth: str) -> bool:
    return set(normalize_answer(prediction).split()) == set(
        normalize_answer(ground_truth).split()
    )


def sub_em_score(prediction: str, ground_truth: str) -> bool:
    gold = normalize_answer(ground_truth)
    return bool(gold) and gold in normalize_answer(prediction)


def reciprocal_rank(ranked_ids, gold_ids, k):
    gold = set(gold_ids)
    for idx, rid in enumerate(ranked_ids[:k], start=1):
        if rid in gold:
            return 1.0 / idx
    return 0.0


def hit_at(ranked_ids, gold_ids, k):
    gold = set(gold_ids)
    return any(rid in gold for rid in ranked_ids[:k])


def evidence_recall_at(ranked_ids, gold_ids, k):
    gold = set(gold_ids)
    if not gold:
        return None
    got = sum(1 for g in gold if g in set(ranked_ids[:k]))
    return got / len(gold)


def evidence_precision_at(ranked_ids, gold_ids, k):
    top = ranked_ids[:k]
    if not top:
        return 0.0
    gold = set(gold_ids)
    return sum(1 for r in top if r in gold) / len(top)


def percentile(sorted_values, p):
    if not sorted_values:
        return None
    idx = min(len(sorted_values) - 1, max(0, int(round(p / 100 * (len(sorted_values) - 1)))))
    return sorted_values[idx]
