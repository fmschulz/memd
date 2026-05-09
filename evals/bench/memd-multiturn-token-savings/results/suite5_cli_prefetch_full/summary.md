# memd multi-turn token-savings benchmark: suite5_cli_prefetch_full

## Aggregate By Interface

| agent | condition | cells | tests | retrieval | provider tokens | MCP payload added | CLI output est. | total incl. retrieval | elapsed total | median elapsed | Claude cache create | Claude cache read | median visible tools | median memd tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| claude | cli_prefetch | 5 | 5 | 5 | 675413 | 0 | 6679 | 675413 | 141 | 24 | 60220 | 603748 | 24 | 0 |
| claude | cli_search | 5 | 5 | 5 | 882319 | 0 | 14861 | 882319 | 151 | 26 | 81961 | 790420 | 24 | 0 |
| claude | without | 5 | 5 | 0 | 630858 | 0 | 0 | 630858 | 119 | 20 | 62754 | 558713 | 24 | 0 |
| codex | cli_prefetch | 5 | 5 | 5 | 165580 | 0 | 6648 | 165580 | 178 | 31 |  |  |  |  |
| codex | cli_search | 5 | 5 | 5 | 138806 | 0 | 14843 | 138806 | 565 | 113 |  |  |  |  |
| codex | without | 5 | 5 | 0 | 175871 | 0 | 0 | 175871 | 171 | 34 |  |  |  |  |

## Per-cell

| agent | episode | condition | tests | tokens | total incl. retrieval | turns | shell | memd calls | retrieval | MCP payload | CLI output est. | elapsed | visible tools | memd tools |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | timezone_boundary_transfer_v2 | without | 1 | 57689 | 57689 | 6 | 8 | 0 | 0 | 0 | 0 | 34 |  |  |
| codex | timezone_boundary_transfer_v2 | cli_search | 1 | 37179 | 37179 | 6 | 10 | 0 | 1 | 2656 | 3073 | 142 |  |  |
| codex | timezone_boundary_transfer_v2 | cli_prefetch | 1 | 17870 | 17870 | 5 | 11 | 0 | 1 | 0 | 1442 | 50 |  |  |
| codex | pagination_cursor_transfer | without | 1 | 12724 | 12724 | 5 | 7 | 0 | 0 | 0 | 0 | 31 |  |  |
| codex | pagination_cursor_transfer | cli_search | 1 | 55047 | 55047 | 5 | 8 | 0 | 1 | 2493 | 2906 | 139 |  |  |
| codex | pagination_cursor_transfer | cli_prefetch | 1 | 12818 | 12818 | 5 | 8 | 0 | 1 | 0 | 1286 | 31 |  |  |
| codex | cache_key_scope_transfer | without | 1 | 12827 | 12827 | 4 | 7 | 0 | 0 | 0 | 0 | 26 |  |  |
| codex | cache_key_scope_transfer | cli_search | 1 | 14900 | 14900 | 5 | 6 | 0 | 1 | 2506 | 2921 | 113 |  |  |
| codex | cache_key_scope_transfer | cli_prefetch | 1 | 12408 | 12408 | 4 | 5 | 0 | 1 | 0 | 1314 | 30 |  |  |
| codex | schema_defaults_transfer | without | 1 | 31340 | 31340 | 6 | 9 | 0 | 0 | 0 | 0 | 35 |  |  |
| codex | schema_defaults_transfer | cli_search | 1 | 16352 | 16352 | 6 | 10 | 0 | 1 | 2554 | 2969 | 62 |  |  |
| codex | schema_defaults_transfer | cli_prefetch | 1 | 83368 | 83368 | 5 | 7 | 0 | 1 | 0 | 1286 | 31 |  |  |
| codex | stream_backpressure_transfer | without | 1 | 61291 | 61291 | 6 | 13 | 0 | 0 | 0 | 0 | 45 |  |  |
| codex | stream_backpressure_transfer | cli_search | 1 | 15328 | 15328 | 6 | 8 | 0 | 1 | 2558 | 2974 | 109 |  |  |
| codex | stream_backpressure_transfer | cli_prefetch | 1 | 39116 | 39116 | 5 | 9 | 0 | 1 | 0 | 1320 | 36 |  |  |
| claude | timezone_boundary_transfer_v2 | without | 1 | 122892 | 122892 | 7 | 2 | 0 | 0 | 0 | 0 | 37 | 24 | 0 |
| claude | timezone_boundary_transfer_v2 | cli_search | 1 | 225949 | 225949 | 10 | 4 | 0 | 1 | 2671 | 3091 | 33 | 24 | 0 |
| claude | timezone_boundary_transfer_v2 | cli_prefetch | 1 | 167227 | 167227 | 8 | 2 | 0 | 1 | 0 | 1442 | 41 | 24 | 0 |
| claude | pagination_cursor_transfer | without | 1 | 114373 | 114373 | 7 | 2 | 0 | 0 | 0 | 0 | 17 | 24 | 0 |
| claude | pagination_cursor_transfer | cli_search | 1 | 155721 | 155721 | 7 | 3 | 0 | 1 | 2494 | 2907 | 26 | 24 | 0 |
| claude | pagination_cursor_transfer | cli_prefetch | 1 | 114940 | 114940 | 6 | 2 | 0 | 1 | 0 | 1286 | 21 | 24 | 0 |
| claude | cache_key_scope_transfer | without | 1 | 115368 | 115368 | 7 | 3 | 0 | 0 | 0 | 0 | 19 | 24 | 0 |
| claude | cache_key_scope_transfer | cli_search | 1 | 183917 | 183917 | 8 | 5 | 0 | 1 | 2506 | 2921 | 25 | 24 | 0 |
| claude | cache_key_scope_transfer | cli_prefetch | 1 | 115270 | 115270 | 6 | 1 | 0 | 1 | 0 | 1314 | 32 | 24 | 0 |
| claude | schema_defaults_transfer | without | 1 | 139440 | 139440 | 8 | 3 | 0 | 0 | 0 | 0 | 20 | 24 | 0 |
| claude | schema_defaults_transfer | cli_search | 1 | 159663 | 159663 | 8 | 4 | 0 | 1 | 2553 | 2969 | 26 | 24 | 0 |
| claude | schema_defaults_transfer | cli_prefetch | 1 | 138325 | 138325 | 7 | 1 | 0 | 1 | 0 | 1317 | 24 | 24 | 0 |
| claude | stream_backpressure_transfer | without | 1 | 138785 | 138785 | 8 | 2 | 0 | 0 | 0 | 0 | 26 | 24 | 0 |
| claude | stream_backpressure_transfer | cli_search | 1 | 157069 | 157069 | 7 | 4 | 0 | 1 | 2558 | 2973 | 41 | 24 | 0 |
| claude | stream_backpressure_transfer | cli_prefetch | 1 | 139651 | 139651 | 7 | 1 | 0 | 1 | 0 | 1320 | 23 | 24 | 0 |

## Paired Net Token Savings

| agent | episode | condition | without tokens | condition tokens | MCP payload added | CLI output est. | condition total | solver savings | net savings | net savings % | without sec | condition sec | sec saved | sec saved % | tests | retrieval |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| claude | cache_key_scope_transfer | cli_search | 115368 | 183917 | 0 | 2921 | 183917 | -68549 | -68549 | -59.4% | 19 | 25 | -6 | -31.6% | 1/1 | 1 |
| claude | cache_key_scope_transfer | cli_prefetch | 115368 | 115270 | 0 | 1314 | 115270 | 98 | 98 | 0.1% | 19 | 32 | -13 | -68.4% | 1/1 | 1 |
| claude | pagination_cursor_transfer | cli_search | 114373 | 155721 | 0 | 2907 | 155721 | -41348 | -41348 | -36.2% | 17 | 26 | -9 | -52.9% | 1/1 | 1 |
| claude | pagination_cursor_transfer | cli_prefetch | 114373 | 114940 | 0 | 1286 | 114940 | -567 | -567 | -0.5% | 17 | 21 | -4 | -23.5% | 1/1 | 1 |
| claude | schema_defaults_transfer | cli_search | 139440 | 159663 | 0 | 2969 | 159663 | -20223 | -20223 | -14.5% | 20 | 26 | -6 | -30.0% | 1/1 | 1 |
| claude | schema_defaults_transfer | cli_prefetch | 139440 | 138325 | 0 | 1317 | 138325 | 1115 | 1115 | 0.8% | 20 | 24 | -4 | -20.0% | 1/1 | 1 |
| claude | stream_backpressure_transfer | cli_search | 138785 | 157069 | 0 | 2973 | 157069 | -18284 | -18284 | -13.2% | 26 | 41 | -15 | -57.7% | 1/1 | 1 |
| claude | stream_backpressure_transfer | cli_prefetch | 138785 | 139651 | 0 | 1320 | 139651 | -866 | -866 | -0.6% | 26 | 23 | 3 | 11.5% | 1/1 | 1 |
| claude | timezone_boundary_transfer_v2 | cli_search | 122892 | 225949 | 0 | 3091 | 225949 | -103057 | -103057 | -83.9% | 37 | 33 | 4 | 10.8% | 1/1 | 1 |
| claude | timezone_boundary_transfer_v2 | cli_prefetch | 122892 | 167227 | 0 | 1442 | 167227 | -44335 | -44335 | -36.1% | 37 | 41 | -4 | -10.8% | 1/1 | 1 |
| codex | cache_key_scope_transfer | cli_search | 12827 | 14900 | 0 | 2921 | 14900 | -2073 | -2073 | -16.2% | 26 | 113 | -87 | -334.6% | 1/1 | 1 |
| codex | cache_key_scope_transfer | cli_prefetch | 12827 | 12408 | 0 | 1314 | 12408 | 419 | 419 | 3.3% | 26 | 30 | -4 | -15.4% | 1/1 | 1 |
| codex | pagination_cursor_transfer | cli_search | 12724 | 55047 | 0 | 2906 | 55047 | -42323 | -42323 | -332.6% | 31 | 139 | -108 | -348.4% | 1/1 | 1 |
| codex | pagination_cursor_transfer | cli_prefetch | 12724 | 12818 | 0 | 1286 | 12818 | -94 | -94 | -0.7% | 31 | 31 | 0 | 0.0% | 1/1 | 1 |
| codex | schema_defaults_transfer | cli_search | 31340 | 16352 | 0 | 2969 | 16352 | 14988 | 14988 | 47.8% | 35 | 62 | -27 | -77.1% | 1/1 | 1 |
| codex | schema_defaults_transfer | cli_prefetch | 31340 | 83368 | 0 | 1286 | 83368 | -52028 | -52028 | -166.0% | 35 | 31 | 4 | 11.4% | 1/1 | 1 |
| codex | stream_backpressure_transfer | cli_search | 61291 | 15328 | 0 | 2974 | 15328 | 45963 | 45963 | 75.0% | 45 | 109 | -64 | -142.2% | 1/1 | 1 |
| codex | stream_backpressure_transfer | cli_prefetch | 61291 | 39116 | 0 | 1320 | 39116 | 22175 | 22175 | 36.2% | 45 | 36 | 9 | 20.0% | 1/1 | 1 |
| codex | timezone_boundary_transfer_v2 | cli_search | 57689 | 37179 | 0 | 3073 | 37179 | 20510 | 20510 | 35.6% | 34 | 142 | -108 | -317.6% | 1/1 | 1 |
| codex | timezone_boundary_transfer_v2 | cli_prefetch | 57689 | 17870 | 0 | 1442 | 17870 | 39819 | 39819 | 69.0% | 34 | 50 | -16 | -47.1% | 1/1 | 1 |

Median net savings: -716 tokens across 20 pairs.

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. MCP payload is added to full_mcp/thin_mcp totals because it is measured outside provider tokens. CLI retrieval output is reported separately because command output is visible in the agent transcript.
