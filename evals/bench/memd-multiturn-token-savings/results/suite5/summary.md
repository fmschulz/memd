# memd multi-turn token-savings benchmark: suite5

## Aggregate By Interface

| agent | condition | cells | tests | retrieval | provider tokens | MCP payload added | CLI output est. | total incl. retrieval | elapsed total | median elapsed | Claude cache create | Claude cache read | median visible tools | median memd tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| claude | full_mcp | 5 | 5 | 5 | 947290 | 12684 | 0 | 959974 | 184 | 30 | 82555 | 855408 | 79 | 55 |
| claude | without | 5 | 5 | 0 | 745192 | 0 | 0 | 745192 | 153 | 25 | 63587 | 671264 | 24 | 0 |
| codex | full_mcp | 5 | 5 | 0 | 210291 | 12743 | 0 | 223034 | 183 | 37 |  |  |  |  |
| codex | without | 5 | 5 | 0 | 252383 | 0 | 0 | 252383 | 260 | 34 |  |  |  |  |

## Per-cell

| agent | episode | condition | tests | tokens | total incl. retrieval | turns | shell | memd calls | retrieval | MCP payload | CLI output est. | elapsed | visible tools | memd tools |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | timezone_boundary_transfer_v2 | without | 1 | 35122 | 35122 | 5 | 13 | 0 | 0 | 0 | 0 | 128 |  |  |
| codex | timezone_boundary_transfer_v2 | full_mcp | 1 | 30159 | 32958 | 7 | 11 | 1 | 0 | 2799 | 0 | 43 |  |  |
| codex | pagination_cursor_transfer | without | 1 | 54645 | 54645 | 5 | 9 | 0 | 0 | 0 | 0 | 34 |  |  |
| codex | pagination_cursor_transfer | full_mcp | 1 | 57058 | 59764 | 5 | 7 | 1 | 0 | 2706 | 0 | 34 |  |  |
| codex | cache_key_scope_transfer | without | 1 | 40526 | 40526 | 5 | 8 | 0 | 0 | 0 | 0 | 34 |  |  |
| codex | cache_key_scope_transfer | full_mcp | 1 | 41170 | 43904 | 5 | 8 | 1 | 0 | 2734 | 0 | 37 |  |  |
| codex | schema_defaults_transfer | without | 1 | 79048 | 79048 | 5 | 9 | 0 | 0 | 0 | 0 | 31 |  |  |
| codex | schema_defaults_transfer | full_mcp | 1 | 38308 | 41051 | 6 | 8 | 1 | 0 | 2743 | 0 | 37 |  |  |
| codex | stream_backpressure_transfer | without | 1 | 43042 | 43042 | 5 | 7 | 0 | 0 | 0 | 0 | 33 |  |  |
| codex | stream_backpressure_transfer | full_mcp | 1 | 43596 | 45357 | 6 | 8 | 1 | 0 | 1761 | 0 | 32 |  |  |
| claude | timezone_boundary_transfer_v2 | without | 1 | 123419 | 123419 | 7 | 2 | 0 | 0 | 0 | 0 | 52 | 24 | 0 |
| claude | timezone_boundary_transfer_v2 | full_mcp | 1 | 162425 | 165182 | 8 | 2 | 1 | 1 | 2757 | 0 | 45 | 79 | 55 |
| claude | pagination_cursor_transfer | without | 1 | 136026 | 136026 | 7 | 2 | 0 | 0 | 0 | 0 | 25 | 24 | 0 |
| claude | pagination_cursor_transfer | full_mcp | 1 | 180706 | 183077 | 8 | 2 | 1 | 1 | 2371 | 0 | 30 | 79 | 55 |
| claude | cache_key_scope_transfer | without | 1 | 185623 | 185623 | 9 | 4 | 0 | 0 | 0 | 0 | 24 | 24 | 0 |
| claude | cache_key_scope_transfer | full_mcp | 1 | 182088 | 184647 | 8 | 2 | 1 | 1 | 2559 | 0 | 50 | 79 | 55 |
| claude | schema_defaults_transfer | without | 1 | 160892 | 160892 | 9 | 2 | 0 | 0 | 0 | 0 | 24 | 24 | 0 |
| claude | schema_defaults_transfer | full_mcp | 1 | 212284 | 214808 | 9 | 5 | 1 | 1 | 2524 | 0 | 30 | 79 | 55 |
| claude | stream_backpressure_transfer | without | 1 | 139232 | 139232 | 8 | 2 | 0 | 0 | 0 | 0 | 28 | 24 | 0 |
| claude | stream_backpressure_transfer | full_mcp | 1 | 209787 | 212260 | 9 | 2 | 1 | 1 | 2473 | 0 | 29 | 79 | 55 |

## Paired Net Token Savings

| agent | episode | condition | without tokens | condition tokens | MCP payload added | CLI output est. | condition total | solver savings | net savings | net savings % | without sec | condition sec | sec saved | sec saved % | tests | retrieval |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| claude | cache_key_scope_transfer | full_mcp | 185623 | 182088 | 2559 | 0 | 184647 | 3535 | 976 | 0.5% | 24 | 50 | -26 | -108.3% | 1/1 | 1 |
| claude | pagination_cursor_transfer | full_mcp | 136026 | 180706 | 2371 | 0 | 183077 | -44680 | -47051 | -34.6% | 25 | 30 | -5 | -20.0% | 1/1 | 1 |
| claude | schema_defaults_transfer | full_mcp | 160892 | 212284 | 2524 | 0 | 214808 | -51392 | -53916 | -33.5% | 24 | 30 | -6 | -25.0% | 1/1 | 1 |
| claude | stream_backpressure_transfer | full_mcp | 139232 | 209787 | 2473 | 0 | 212260 | -70555 | -73028 | -52.5% | 28 | 29 | -1 | -3.6% | 1/1 | 1 |
| claude | timezone_boundary_transfer_v2 | full_mcp | 123419 | 162425 | 2757 | 0 | 165182 | -39006 | -41763 | -33.8% | 52 | 45 | 7 | 13.5% | 1/1 | 1 |
| codex | cache_key_scope_transfer | full_mcp | 40526 | 41170 | 2734 | 0 | 43904 | -644 | -3378 | -8.3% | 34 | 37 | -3 | -8.8% | 1/1 | 0 |
| codex | pagination_cursor_transfer | full_mcp | 54645 | 57058 | 2706 | 0 | 59764 | -2413 | -5119 | -9.4% | 34 | 34 | 0 | 0.0% | 1/1 | 0 |
| codex | schema_defaults_transfer | full_mcp | 79048 | 38308 | 2743 | 0 | 41051 | 40740 | 37997 | 48.1% | 31 | 37 | -6 | -19.4% | 1/1 | 0 |
| codex | stream_backpressure_transfer | full_mcp | 43042 | 43596 | 1761 | 0 | 45357 | -554 | -2315 | -5.4% | 33 | 32 | 1 | 3.0% | 1/1 | 0 |
| codex | timezone_boundary_transfer_v2 | full_mcp | 35122 | 30159 | 2799 | 0 | 32958 | 4963 | 2164 | 6.2% | 128 | 43 | 85 | 66.4% | 1/1 | 0 |

Median net savings: -4248 tokens across 10 pairs.

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. MCP payload is added to full_mcp/thin_mcp totals because it is measured outside provider tokens. CLI retrieval output is reported separately because command output is visible in the agent transcript.
