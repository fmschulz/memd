# memd

[![Version](https://img.shields.io/badge/version-1.0.0-blue)](CHANGELOG.md)
[![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-fmschulz.github.io%2Fmemd-blue)](https://fmschulz.github.io/memd/)

`memd` is a local memory CLI for coding agents and AI scientists. Each trusted
machine gets one shared, persistent store: raw searchable content, structured
task history, and canonical collaboration artifacts. A hybrid dense + sparse
stack indexes it; an explicit trust boundary decides what counts as verified.

Agents retrieve bounded context with `memd agent-context` or `memd search`,
read the generated context file, and record durable progress with `memd add`.
For low-latency local use, `memd` keeps the store and indexes hot through a
private CLI-managed warm worker driven by ordinary CLI commands.

**Full documentation: [fmschulz.github.io/memd](https://fmschulz.github.io/memd/)**

## What it does

| Surface | Purpose | Primary CLI commands |
| --- | --- | --- |
| Raw memory | Store and search chunks: code, docs, notes, traces, decisions | `memd add`, `memd search`, `memd get`, `memd stats` |
| Agent context | Bounded pre-work context + JSON audit logs | `memd agent-context --output .memd/context.md` |
| Startup memory | Refresh project `memory.md` with ranked takeaways and a concrete `Agent action:` line per takeaway | `memd memory-md`, `memd eval-memory-md` |
| Usefulness report | Usage-ledger and store self-diagnosis for growth, learning, retrieval, and warnings | `memd report --strict` |
| Warm CLI | Keep store/index state hot for repeated local calls | `memd warm start`, `memd warm status` |
| Batch CLI | Many structured operations in one loaded process | `memd batch --jsonl requests.jsonl` |
| Export/import | Manual cross-machine moves through portable OMF | `memd export-omf`, `memd import-omf` |
| Operations | Structured memory/task/artifact/context/code/debug ops | `memd call task.start --json '{...}'` |
| Guardrails | Pin tenant/project scope and verify CLI-first agent wiring | `memd init`, `memd doctor` |

Use `memd search --mode brief_project|resume_task|find_failures|find_decisions|find_evidence|find_highlights`
when retrieval should bias toward persisted digests and canonical summaries.
Use `--compact` and `--token-budget` to keep agent context small.
High-priority durable writes (`priority:8+` or `importance:8+`) must include
a concrete `Agent action:` line. The gate accepts a sentence of at least 24
characters containing an imperative verb (verify, run, use, check, avoid,
prefer, record, treat, ...). Tell the next agent what to verify, run, reuse, or
avoid.

## 30-second quickstart

```bash
git clone https://github.com/fmschulz/memd
cd memd
make install   # binary + agent skill + enforcement
memd doctor
```

Prebuilt binary only (no clone):

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh | sh
```

From source, manual:

```bash
cargo build --release
```

First memory:

```bash
memd add \
  --tenant-id quickstart --project-id auth \
  --chunk-type summary --tags kind:note \
  --text "parseConfig reads TOML and validates required auth fields"

memd search \
  --tenant-id quickstart --project-id auth \
  --query "auth config validation" --compact --token-budget 2000

memd agent-context \
  --tenant-id quickstart --project-id auth \
  --query "auth config validation prior work" \
  --k 2 --token-budget 700 --output .memd/context.md
```

Full walkthrough: [Quick start](https://fmschulz.github.io/memd/quickstart/).

## Architecture

```mermaid
flowchart TB
  subgraph clients["Clients"]
    direction LR
    ca["Coding agent"]
    sci["AI scientist"]
    human["Human or controller"]
  end

  subgraph cli_flow["Skill and CLI workflow"]
    direction LR
    agent_context["memd agent-context"]
    search_add["memd search / memd add"]
  end

  subgraph core["memd core"]
    direction TB
    cli_call["memd call"]
    handlers["Memory, task, artifact, code, context, debug operations"]
    metrics["Metrics and cache statistics"]
  end

  subgraph retrieval["Hybrid retrieval"]
    direction LR
    hybrid["Hybrid searcher"]
    dense["Dense HNSW search"]
    sparse["BM25 sparse search"]
    tiered["Hot tier and semantic cache"]
    rerank["Optional rerankers"]
  end

  subgraph storage["Persistent store"]
    direction LR
    sqlite["SQLite metadata"]
    segments["Segment files"]
    wal["Write-ahead log"]
    structural["Structural code index"]
  end

  ca --> agent_context
  sci --> agent_context
  human --> search_add
  human --> cli_call
  agent_context --> handlers
  search_add --> handlers
  cli_call --> handlers
  cli_call --> metrics
  handlers --> hybrid
  handlers --> sqlite
  handlers --> structural
  hybrid --> dense
  hybrid --> sparse
  hybrid --> tiered
  hybrid --> rerank
```

More: [Architecture](https://fmschulz.github.io/memd/architecture/),
[Trust boundary](https://fmschulz.github.io/memd/trust-boundary/),
[Data layout](https://fmschulz.github.io/memd/data-layout/).

## Concurrency model

Writes route through the private warm worker by default (`--warm auto` starts
or reuses it for routable commands), and the worker holds the data-dir
exclusive writer flock for its lifetime; any direct-write fallback or
`--warm off` write takes the same flock with a bounded retry. Reads open the
store in ReadOnly mode without taking the lock or mutating disk, and the worker
probes SQLite `data_version` before each request so direct fallback mutations
are visible before serving. More: [Shared topology](https://fmschulz.github.io/memd/shared-topology/)
and [Operational contract](https://fmschulz.github.io/memd/operational-contract/).
Measured on the dev machine (2026-06, hardening validation run): an 8-writer × 3-round
write storm leaves 24/24 concurrent writes readable (7 of 16 were lost in the
2026-06-09 audit before the writer lock); warm-routed `memd add` p50 is 31 ms (vs ~1.6 s cold); a write is
searchable within p95 <70 ms.

## Headline benchmark

Cross-system retrieval on upstream
[`locomo10.json`](https://github.com/snap-stanford/locomo) (10 conversations,
5,882 turns, 1,536 queries, MRR@10 over categories 1–4):

| System | MRR@10 | Hit@10 | Avg search | Seed |
|---|---:|---:|---:|---:|
| **`memd` (hybrid)** | **0.412** | **0.613** | **23.2 ms** | 197 s |
| `superlocalmemory` v3.4.46 (lexical) | 0.369 | 0.599 | 804.5 ms | 1.8 s |
| `mem0` v2.0.2 (LLM-extracted) | 0.354 | 0.591 | 40.9 ms | 13,424 s |

Benchmark caveat: mem0 used a self-hosted vLLM `gemma4-31b` endpoint rather
than the GPT-4-class model used in the upstream Mem0 paper, and
superlocalmemory ran in lexical-only fallback because its published semantic
configuration was unreachable; full protocol:
https://fmschulz.github.io/memd/benchmarking/.

`memd` leads on MRR@10 (+12% vs SuperLocalMemory, +16% vs Mem0), Hit@10, and
search latency. Internal task-memory benchmark (`cli_warm`: Hit@3 1.00,
MRR 0.87, 9.7 ms), Bright-Pro biology adapter, multi-turn token benchmark, and
reproducibility notes are documented at
[Benchmarking](https://fmschulz.github.io/memd/benchmarking/).

## Agent skill

The agent skill is the default way to make agents use `memd` through the CLI.

Install the binary, agent skill, and enforcement in one command:

```bash
git clone https://github.com/fmschulz/memd
cd memd
make install   # binary + agent skill + enforcement
memd doctor
```

Prebuilt binary only (no clone):

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh | sh
```

The prebuilt installer installs only the binary. To install the skill and
enforcement, use `make install` from a clone. `make install-binary` installs
only the binary, `make menu` opens an interactive TUI to pick components, and
`make uninstall` removes what `make install` installed.

For component-target development, `make install-skill` installs the skill as
symlinks. Use `make install-skill-bundle` to copy the current skill plus the
repo-built binary into each unique existing standard skill directory among
`~/.agents/skills`, `~/.claude/skills`, and `~/.codex/skills`.

Run `memd doctor` after installation to verify the binary, data directory,
global rules, SessionStart hook, and current project scope.

More: [Agent skill](https://fmschulz.github.io/memd/agent-skill/),
[Self-improvement loop](https://fmschulz.github.io/memd/self-improvement/).

## Compiled wiki

[`tools/wiki/`](tools/wiki) ships `memd-wiki`, a Python console script that
compiles a Karpathy-style markdown wiki from live `memd` project state
(`index.md`, `log.md`, `projects/<project_id>.md`,
`tasks/<task_id>.md`, `libraries/{failures,decisions,evidence,highlights}.md`).
Pages are trust-aware: they display `trust_tier`, `requires_verification`,
and `grounded_by` links.

```bash
pip install -e tools/wiki/
memd-wiki build
memd-wiki serve
```

More: [Compiled wiki](https://fmschulz.github.io/memd/compiled-wiki/).

## More

- [Quick start](https://fmschulz.github.io/memd/quickstart/)
- [CLI reference](https://fmschulz.github.io/memd/cli-reference/)
- [Configuration](https://fmschulz.github.io/memd/configuration/)
- [OMF — Open Memory Format](https://fmschulz.github.io/memd/omf/)
- [Task-memory schema](https://fmschulz.github.io/memd/scientific-task-memory/schema/)
- [Changelog](CHANGELOG.md)
