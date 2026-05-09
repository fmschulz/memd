# memd multi-turn token-savings benchmark: suite5_interface

## Aggregate By Interface

| agent | condition | cells | tests | retrieval | provider tokens | MCP payload added | CLI output est. | total incl. retrieval | elapsed total | median elapsed | Claude cache create | Claude cache read | median visible tools | median memd tools |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| claude | cli_search | 5 | 5 | 5 | 789458 | 0 | 14919 | 789458 | 122 | 24 | 79748 | 700736 | 24 | 0 |
| claude | full_mcp | 5 | 5 | 5 | 929068 | 13873 | 0 | 942941 | 130 | 25 | 95538 | 824174 | 79 | 55 |
| claude | thin_mcp | 5 | 5 | 5 | 874685 | 10960 | 12762 | 885645 | 118 | 24 | 74918 | 790984 | 25 | 1 |
| claude | without | 5 | 5 | 0 | 681312 | 0 | 0 | 681312 | 134 | 23 | 63998 | 607040 | 24 | 0 |
| codex | cli_search | 5 | 5 | 5 | 134977 | 0 | 14901 | 134977 | 621 | 151 |  |  |  |  |
| codex | full_mcp | 5 | 5 | 5 | 178669 | 12807 | 0 | 191476 | 197 | 43 |  |  |  |  |
| codex | thin_mcp | 5 | 5 | 4 | 218422 | 8950 | 10410 | 227372 | 185 | 32 |  |  |  |  |
| codex | without | 5 | 5 | 0 | 180095 | 0 | 0 | 180095 | 188 | 35 |  |  |  |  |

## Per-cell

| agent | episode | condition | tests | tokens | total incl. retrieval | turns | shell | memd calls | retrieval | MCP payload | CLI output est. | elapsed | visible tools | memd tools |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| codex | timezone_boundary_transfer_v2 | without | 1 | 33933 | 33933 | 5 | 10 | 0 | 0 | 0 | 0 | 41 |  |  |
| codex | timezone_boundary_transfer_v2 | full_mcp | 1 | 49436 | 52235 | 5 | 10 | 1 | 1 | 2799 | 0 | 44 |  |  |
| codex | timezone_boundary_transfer_v2 | thin_mcp | 1 | 58337 | 59802 | 6 | 6 | 1 | 0 | 1465 | 1721 | 32 |  |  |
| codex | timezone_boundary_transfer_v2 | cli_search | 1 | 25956 | 25956 | 6 | 10 | 0 | 1 | 2656 | 3073 | 156 |  |  |
| codex | pagination_cursor_transfer | without | 1 | 50278 | 50278 | 5 | 10 | 0 | 0 | 0 | 0 | 35 |  |  |
| codex | pagination_cursor_transfer | full_mcp | 1 | 22982 | 25688 | 6 | 9 | 1 | 1 | 2706 | 0 | 43 |  |  |
| codex | pagination_cursor_transfer | thin_mcp | 1 | 59791 | 61455 | 5 | 8 | 1 | 1 | 1664 | 1926 | 42 |  |  |
| codex | pagination_cursor_transfer | cli_search | 1 | 15604 | 15604 | 6 | 9 | 0 | 1 | 2508 | 2923 | 32 |  |  |
| codex | cache_key_scope_transfer | without | 1 | 19168 | 19168 | 6 | 9 | 0 | 0 | 0 | 0 | 34 |  |  |
| codex | cache_key_scope_transfer | full_mcp | 1 | 20267 | 22056 | 5 | 6 | 1 | 1 | 1789 | 0 | 28 |  |  |
| codex | cache_key_scope_transfer | thin_mcp | 1 | 56183 | 58774 | 5 | 6 | 1 | 1 | 2591 | 3013 | 30 |  |  |
| codex | cache_key_scope_transfer | cli_search | 1 | 32061 | 32061 | 5 | 5 | 0 | 1 | 2548 | 2964 | 151 |  |  |
| codex | schema_defaults_transfer | without | 1 | 40870 | 40870 | 5 | 10 | 0 | 0 | 0 | 0 | 32 |  |  |
| codex | schema_defaults_transfer | full_mcp | 1 | 59950 | 62698 | 5 | 12 | 1 | 1 | 2748 | 0 | 45 |  |  |
| codex | schema_defaults_transfer | thin_mcp | 1 | 22462 | 24074 | 5 | 9 | 1 | 1 | 1612 | 1872 | 49 |  |  |
| codex | schema_defaults_transfer | cli_search | 1 | 34308 | 34308 | 6 | 9 | 0 | 1 | 2553 | 2969 | 124 |  |  |
| codex | stream_backpressure_transfer | without | 1 | 35846 | 35846 | 6 | 13 | 0 | 0 | 0 | 0 | 46 |  |  |
| codex | stream_backpressure_transfer | full_mcp | 1 | 26034 | 28799 | 5 | 7 | 1 | 1 | 2765 | 0 | 37 |  |  |
| codex | stream_backpressure_transfer | thin_mcp | 1 | 21649 | 23267 | 6 | 8 | 1 | 1 | 1618 | 1878 | 32 |  |  |
| codex | stream_backpressure_transfer | cli_search | 1 | 27048 | 27048 | 6 | 9 | 0 | 1 | 2557 | 2972 | 158 |  |  |
| claude | timezone_boundary_transfer_v2 | without | 1 | 122367 | 122367 | 7 | 2 | 0 | 0 | 0 | 0 | 28 | 24 | 0 |
| claude | timezone_boundary_transfer_v2 | full_mcp | 1 | 164564 | 167202 | 8 | 2 | 1 | 1 | 2638 | 0 | 32 | 79 | 55 |
| claude | timezone_boundary_transfer_v2 | thin_mcp | 1 | 150529 | 152156 | 8 | 2 | 1 | 1 | 1627 | 1887 | 25 | 25 | 1 |
| claude | timezone_boundary_transfer_v2 | cli_search | 1 | 133829 | 133829 | 6 | 3 | 0 | 1 | 2671 | 3091 | 26 | 24 | 0 |
| claude | pagination_cursor_transfer | without | 1 | 114238 | 114238 | 7 | 2 | 0 | 0 | 0 | 0 | 23 | 24 | 0 |
| claude | pagination_cursor_transfer | full_mcp | 1 | 181250 | 183618 | 8 | 2 | 1 | 1 | 2368 | 0 | 23 | 79 | 55 |
| claude | pagination_cursor_transfer | thin_mcp | 1 | 174202 | 176789 | 8 | 2 | 1 | 1 | 2587 | 3014 | 22 | 25 | 1 |
| claude | pagination_cursor_transfer | cli_search | 1 | 155259 | 155259 | 7 | 3 | 0 | 1 | 2508 | 2923 | 22 | 24 | 0 |
| claude | cache_key_scope_transfer | without | 1 | 114001 | 114001 | 5 | 3 | 0 | 0 | 0 | 0 | 20 | 24 | 0 |
| claude | cache_key_scope_transfer | full_mcp | 1 | 183010 | 185529 | 8 | 2 | 1 | 1 | 2519 | 0 | 25 | 79 | 55 |
| claude | cache_key_scope_transfer | thin_mcp | 1 | 169179 | 170797 | 8 | 2 | 1 | 1 | 1618 | 1884 | 20 | 25 | 1 |
| claude | cache_key_scope_transfer | cli_search | 1 | 156818 | 156818 | 7 | 5 | 0 | 1 | 2548 | 2963 | 24 | 24 | 0 |
| claude | schema_defaults_transfer | without | 1 | 138943 | 138943 | 9 | 2 | 0 | 0 | 0 | 0 | 22 | 24 | 0 |
| claude | schema_defaults_transfer | full_mcp | 1 | 218630 | 222529 | 9 | 2 | 1 | 1 | 3899 | 0 | 25 | 79 | 55 |
| claude | schema_defaults_transfer | thin_mcp | 1 | 204827 | 207412 | 10 | 2 | 1 | 1 | 2585 | 3013 | 27 | 25 | 1 |
| claude | schema_defaults_transfer | cli_search | 1 | 185688 | 185688 | 7 | 5 | 0 | 1 | 2553 | 2969 | 26 | 24 | 0 |
| claude | stream_backpressure_transfer | without | 1 | 191763 | 191763 | 9 | 3 | 0 | 0 | 0 | 0 | 41 | 24 | 0 |
| claude | stream_backpressure_transfer | full_mcp | 1 | 181614 | 184063 | 8 | 2 | 1 | 1 | 2449 | 0 | 25 | 79 | 55 |
| claude | stream_backpressure_transfer | thin_mcp | 1 | 175948 | 178491 | 8 | 2 | 1 | 1 | 2543 | 2964 | 24 | 25 | 1 |
| claude | stream_backpressure_transfer | cli_search | 1 | 157864 | 157864 | 7 | 5 | 0 | 1 | 2557 | 2973 | 24 | 24 | 0 |

## Paired Net Token Savings

| agent | episode | condition | without tokens | condition tokens | MCP payload added | CLI output est. | condition total | solver savings | net savings | net savings % | without sec | condition sec | sec saved | sec saved % | tests | retrieval |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| claude | cache_key_scope_transfer | full_mcp | 114001 | 183010 | 2519 | 0 | 185529 | -69009 | -71528 | -62.7% | 20 | 25 | -5 | -25.0% | 1/1 | 1 |
| claude | cache_key_scope_transfer | thin_mcp | 114001 | 169179 | 1618 | 1884 | 170797 | -55178 | -56796 | -49.8% | 20 | 20 | 0 | 0.0% | 1/1 | 1 |
| claude | cache_key_scope_transfer | cli_search | 114001 | 156818 | 0 | 2963 | 156818 | -42817 | -42817 | -37.6% | 20 | 24 | -4 | -20.0% | 1/1 | 1 |
| claude | pagination_cursor_transfer | full_mcp | 114238 | 181250 | 2368 | 0 | 183618 | -67012 | -69380 | -60.7% | 23 | 23 | 0 | 0.0% | 1/1 | 1 |
| claude | pagination_cursor_transfer | thin_mcp | 114238 | 174202 | 2587 | 3014 | 176789 | -59964 | -62551 | -54.8% | 23 | 22 | 1 | 4.3% | 1/1 | 1 |
| claude | pagination_cursor_transfer | cli_search | 114238 | 155259 | 0 | 2923 | 155259 | -41021 | -41021 | -35.9% | 23 | 22 | 1 | 4.3% | 1/1 | 1 |
| claude | schema_defaults_transfer | full_mcp | 138943 | 218630 | 3899 | 0 | 222529 | -79687 | -83586 | -60.2% | 22 | 25 | -3 | -13.6% | 1/1 | 1 |
| claude | schema_defaults_transfer | thin_mcp | 138943 | 204827 | 2585 | 3013 | 207412 | -65884 | -68469 | -49.3% | 22 | 27 | -5 | -22.7% | 1/1 | 1 |
| claude | schema_defaults_transfer | cli_search | 138943 | 185688 | 0 | 2969 | 185688 | -46745 | -46745 | -33.6% | 22 | 26 | -4 | -18.2% | 1/1 | 1 |
| claude | stream_backpressure_transfer | full_mcp | 191763 | 181614 | 2449 | 0 | 184063 | 10149 | 7700 | 4.0% | 41 | 25 | 16 | 39.0% | 1/1 | 1 |
| claude | stream_backpressure_transfer | thin_mcp | 191763 | 175948 | 2543 | 2964 | 178491 | 15815 | 13272 | 6.9% | 41 | 24 | 17 | 41.5% | 1/1 | 1 |
| claude | stream_backpressure_transfer | cli_search | 191763 | 157864 | 0 | 2973 | 157864 | 33899 | 33899 | 17.7% | 41 | 24 | 17 | 41.5% | 1/1 | 1 |
| claude | timezone_boundary_transfer_v2 | full_mcp | 122367 | 164564 | 2638 | 0 | 167202 | -42197 | -44835 | -36.6% | 28 | 32 | -4 | -14.3% | 1/1 | 1 |
| claude | timezone_boundary_transfer_v2 | thin_mcp | 122367 | 150529 | 1627 | 1887 | 152156 | -28162 | -29789 | -24.3% | 28 | 25 | 3 | 10.7% | 1/1 | 1 |
| claude | timezone_boundary_transfer_v2 | cli_search | 122367 | 133829 | 0 | 3091 | 133829 | -11462 | -11462 | -9.4% | 28 | 26 | 2 | 7.1% | 1/1 | 1 |
| codex | cache_key_scope_transfer | full_mcp | 19168 | 20267 | 1789 | 0 | 22056 | -1099 | -2888 | -15.1% | 34 | 28 | 6 | 17.6% | 1/1 | 1 |
| codex | cache_key_scope_transfer | thin_mcp | 19168 | 56183 | 2591 | 3013 | 58774 | -37015 | -39606 | -206.6% | 34 | 30 | 4 | 11.8% | 1/1 | 1 |
| codex | cache_key_scope_transfer | cli_search | 19168 | 32061 | 0 | 2964 | 32061 | -12893 | -12893 | -67.3% | 34 | 151 | -117 | -344.1% | 1/1 | 1 |
| codex | pagination_cursor_transfer | full_mcp | 50278 | 22982 | 2706 | 0 | 25688 | 27296 | 24590 | 48.9% | 35 | 43 | -8 | -22.9% | 1/1 | 1 |
| codex | pagination_cursor_transfer | thin_mcp | 50278 | 59791 | 1664 | 1926 | 61455 | -9513 | -11177 | -22.2% | 35 | 42 | -7 | -20.0% | 1/1 | 1 |
| codex | pagination_cursor_transfer | cli_search | 50278 | 15604 | 0 | 2923 | 15604 | 34674 | 34674 | 69.0% | 35 | 32 | 3 | 8.6% | 1/1 | 1 |
| codex | schema_defaults_transfer | full_mcp | 40870 | 59950 | 2748 | 0 | 62698 | -19080 | -21828 | -53.4% | 32 | 45 | -13 | -40.6% | 1/1 | 1 |
| codex | schema_defaults_transfer | thin_mcp | 40870 | 22462 | 1612 | 1872 | 24074 | 18408 | 16796 | 41.1% | 32 | 49 | -17 | -53.1% | 1/1 | 1 |
| codex | schema_defaults_transfer | cli_search | 40870 | 34308 | 0 | 2969 | 34308 | 6562 | 6562 | 16.1% | 32 | 124 | -92 | -287.5% | 1/1 | 1 |
| codex | stream_backpressure_transfer | full_mcp | 35846 | 26034 | 2765 | 0 | 28799 | 9812 | 7047 | 19.7% | 46 | 37 | 9 | 19.6% | 1/1 | 1 |
| codex | stream_backpressure_transfer | thin_mcp | 35846 | 21649 | 1618 | 1878 | 23267 | 14197 | 12579 | 35.1% | 46 | 32 | 14 | 30.4% | 1/1 | 1 |
| codex | stream_backpressure_transfer | cli_search | 35846 | 27048 | 0 | 2972 | 27048 | 8798 | 8798 | 24.5% | 46 | 158 | -112 | -243.5% | 1/1 | 1 |
| codex | timezone_boundary_transfer_v2 | full_mcp | 33933 | 49436 | 2799 | 0 | 52235 | -15503 | -18302 | -53.9% | 41 | 44 | -3 | -7.3% | 1/1 | 1 |
| codex | timezone_boundary_transfer_v2 | thin_mcp | 33933 | 58337 | 1465 | 1721 | 59802 | -24404 | -25869 | -76.2% | 41 | 32 | 9 | 22.0% | 1/1 | 0 |
| codex | timezone_boundary_transfer_v2 | cli_search | 33933 | 25956 | 0 | 3073 | 25956 | 7977 | 7977 | 23.5% | 41 | 156 | -115 | -280.5% | 1/1 | 1 |

Median net savings: -15598 tokens across 30 pairs.

Token caveat: Codex uses its CLI footer. Claude uses modelUsage totals. MCP payload is added to full_mcp/thin_mcp totals because it is measured outside provider tokens. CLI retrieval output is reported separately because command output is visible in the agent transcript.
