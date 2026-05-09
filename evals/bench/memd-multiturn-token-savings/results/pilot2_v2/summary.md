# memd multi-turn token-savings pilot: pilot2_v2

## Per-cell

| agent | episode | cond | tests | tokens | turns | shell | memd calls | retrieval | memd req | memd resp | memd total | elapsed |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | timezone_boundary_transfer_v2 | without | 1 | 33570 | 5 | 11 | 0 | 0 | 0 | 0 | 0 | 103 |
| codex | timezone_boundary_transfer_v2 | with | 1 | 27847 | 6 | 10 | 1 | 1 | 214 | 2585 | 2799 | 39 |
| claude | timezone_boundary_transfer_v2 | without | 1 | 121844 | 7 | 2 | 0 | 0 | 0 | 0 | 0 | 25 |
| claude | timezone_boundary_transfer_v2 | with | 1 | 168592 | 8 | 2 | 1 | 1 | 67 | 2690 | 2757 | 36 |

## Paired Net Token Savings

| agent | episode | without tokens | with provider tokens | memd payload | with total | solver savings | net savings | net savings % | tests with/without | retrieval |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---:|
| claude | timezone_boundary_transfer_v2 | 121844 | 168592 | 2757 | 171349 | -46748 | -49505 | -40.6% | 1/1 | 1 |
| codex | timezone_boundary_transfer_v2 | 33570 | 27847 | 2799 | 30646 | 5723 | 2924 | 8.7% | 1/1 | 1 |

Median net savings: -23290 tokens across 2 pairs.

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. These are useful within each agent but not billing-equivalent across agents.
