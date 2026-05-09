# memd multi-turn token-savings benchmark: suite5_interface_probe_codex

## Aggregate By Interface

| agent | condition | cells | tests | retrieval | provider tokens | MCP payload added | CLI output est. | total incl. retrieval | elapsed total | median elapsed | Claude cache create | Claude cache read | median visible tools | median memd tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | cli_search | 1 | 1 | 1 | 35582 | 0 | 2969 | 35582 | 110 | 110 |  |  |  |  |
| codex | thin_mcp | 1 | 1 | 1 | 23846 | 1650 | 1911 | 25496 | 38 | 38 |  |  |  |  |

## Per-cell

| agent | episode | condition | tests | tokens | total incl. retrieval | turns | shell | memd calls | retrieval | MCP payload | CLI output est. | elapsed | visible tools | memd tools |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | schema_defaults_transfer | thin_mcp | 1 | 23846 | 25496 | 5 | 11 | 1 | 1 | 1650 | 1911 | 38 |  |  |
| codex | schema_defaults_transfer | cli_search | 1 | 35582 | 35582 | 6 | 9 | 0 | 1 | 2553 | 2969 | 110 |  |  |

## Paired Net Token Savings

| agent | episode | condition | without tokens | condition tokens | MCP payload added | CLI output est. | condition total | solver savings | net savings | net savings % | without sec | condition sec | sec saved | sec saved % | tests | retrieval |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. MCP payload is added to full_mcp/thin_mcp totals because it is measured outside provider tokens. CLI retrieval output is reported separately because command output is visible in the agent transcript.
