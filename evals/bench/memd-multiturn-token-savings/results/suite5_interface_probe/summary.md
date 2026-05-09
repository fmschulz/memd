# memd multi-turn token-savings benchmark: suite5_interface_probe

## Aggregate By Interface

| agent | condition | cells | tests | retrieval | provider tokens | MCP payload added | CLI output est. | total incl. retrieval | elapsed total | median elapsed | Claude cache create | Claude cache read | median visible tools | median memd tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| claude | cli_search | 1 | 1 | 1 | 184679 | 0 | 2969 | 184679 | 28 | 28 | 16182 | 166812 | 24 | 0 |
| claude | full_mcp | 1 | 1 | 1 | 211162 | 2541 | 0 | 213703 | 25 | 25 | 16424 | 193008 | 79 | 55 |
| claude | thin_mcp | 1 | 1 | 1 | 205132 | 2583 | 0 | 207715 | 28 | 28 | 15953 | 187345 | 25 | 1 |
| claude | without | 1 | 1 | 0 | 161950 | 0 | 0 | 161950 | 27 | 27 | 3270 | 156698 | 24 | 0 |

## Per-cell

| agent | episode | condition | tests | tokens | total incl. retrieval | turns | shell | memd calls | retrieval | MCP payload | CLI output est. | elapsed | visible tools | memd tools |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| claude | schema_defaults_transfer | without | 1 | 161950 | 161950 | 10 | 2 | 0 | 0 | 0 | 0 | 27 | 24 | 0 |
| claude | schema_defaults_transfer | full_mcp | 1 | 211162 | 213703 | 9 | 2 | 1 | 1 | 2541 | 0 | 25 | 79 | 55 |
| claude | schema_defaults_transfer | thin_mcp | 1 | 205132 | 207715 | 10 | 2 | 1 | 1 | 2583 | 0 | 28 | 25 | 1 |
| claude | schema_defaults_transfer | cli_search | 1 | 184679 | 184679 | 7 | 5 | 0 | 1 | 2553 | 2969 | 28 | 24 | 0 |

## Paired Net Token Savings

| agent | episode | condition | without tokens | condition tokens | MCP payload added | CLI output est. | condition total | solver savings | net savings | net savings % | without sec | condition sec | sec saved | sec saved % | tests | retrieval |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| claude | schema_defaults_transfer | full_mcp | 161950 | 211162 | 2541 | 0 | 213703 | -49212 | -51753 | -32.0% | 27 | 25 | 2 | 7.4% | 1/1 | 1 |
| claude | schema_defaults_transfer | thin_mcp | 161950 | 205132 | 2583 | 0 | 207715 | -43182 | -45765 | -28.3% | 27 | 28 | -1 | -3.7% | 1/1 | 1 |
| claude | schema_defaults_transfer | cli_search | 161950 | 184679 | 0 | 2969 | 184679 | -22729 | -22729 | -14.0% | 27 | 28 | -1 | -3.7% | 1/1 | 1 |

Median net savings: -45765 tokens across 3 pairs.

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. MCP payload is added to full_mcp/thin_mcp totals because it is measured outside provider tokens. CLI retrieval output is reported separately because command output is visible in the agent transcript.
