# memd multi-turn token-savings pilot: pilot1

## Per-cell

| agent | episode | cond | tests | tokens | turns | shell | memd calls | retrieval | memd req | memd resp | memd total | elapsed |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | timezone_boundary_transfer | without | 1 | 29457 | 5 | 6 | 0 | 0 | 0 | 0 | 0 | 32 |
| codex | timezone_boundary_transfer | with | 1 | 43912 | 6 | 6 | 1 | 1 | 207 | 1552 | 1759 | 32 |
| claude | timezone_boundary_transfer | without | 1 | 140312 | 8 | 2 | 0 | 0 | 0 | 0 | 0 | 26 |
| claude | timezone_boundary_transfer | with | 1 | 160671 | 8 | 3 | 1 | 1 | 67 | 2464 | 2531 | 31 |

## Paired Net Token Savings

| agent | episode | without tokens | with provider tokens | memd payload | with total | solver savings | net savings | net savings % | tests with/without | retrieval |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---:|
| claude | timezone_boundary_transfer | 140312 | 160671 | 2531 | 163202 | -20359 | -22890 | -16.3% | 1/1 | 1 |
| codex | timezone_boundary_transfer | 29457 | 43912 | 1759 | 45671 | -14455 | -16214 | -55.0% | 1/1 | 1 |

Median net savings: -19552 tokens across 2 pairs.

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. These are useful within each agent but not billing-equivalent across agents.
