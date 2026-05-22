# memd

[![Version](https://img.shields.io/badge/version-0.50.0-blue)](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md){ .md-button }
[![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white)](https://github.com/fmschulz/memd/blob/main/Cargo.toml){ .md-button }
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/fmschulz/memd/blob/main/LICENSE){ .md-button }

**`memd` is a local memory CLI** that gives coding agents and AI scientists a
single shared, persistent memory — raw searchable content, structured task
history, and canonical collaboration artifacts — indexed by a hybrid dense +
sparse retrieval stack and gated by an explicit trust boundary.

Agents retrieve bounded context with `memd agent-context` or `memd search`,
read the generated context file, and record durable progress with `memd add`.
For low-latency local use, `memd` keeps the store and indexes hot through a
private CLI-managed warm worker driven by ordinary CLI commands.

---

## What memd does

| Surface | Purpose | Primary CLI commands |
| --- | --- | --- |
| **Raw memory** | Store and search chunks: code, docs, notes, traces, decisions | `memd add`, `memd search`, `memd get`, `memd stats` |
| **Agent context** | Bounded pre-work context + JSON audit logs | `memd agent-context --output .memd/context.md` |
| **Warm CLI** | Keep store/index state hot for repeated local calls | `memd warm start`, `memd warm status` |
| **Batch CLI** | Many structured operations in one loaded process | `memd batch --jsonl requests.jsonl` |
| **Export/import** | Move local memory to portable formats | `memd export-omf`, `memd import-omf` |
| **Operations** | Structured memory / task / artifact / context / code / debug ops | `memd call task.start --json '{…}'` |
| **Guardrails** | Pin tenant/project scope and CLI-first agent rules | `memd init` |

---

## Headline number

| Lane | Retrieval setup | Hit@3 | MRR | Avg search latency |
| --- | --- | ---: | ---: | ---: |
| `cli_warm` | private warm worker | 1.00 | 0.87 | **9.7 ms** |
| `cli_batch` | streaming JSONL in one loaded process | 1.00 | 0.87 | **0.6 ms** |

Internal task-memory benchmark, see [Benchmarking](benchmarking.md) for the full table and the cross-system LoCoMo + Bright-Pro adapter results.

---

## Start here

- [**Quick start**](quickstart.md) — install, store first memory, retrieve, build agent context.
- [**Architecture**](architecture.md) — hybrid retrieval, storage, trust boundary diagram.
- [**CLI reference**](cli-reference.md) — every command and operation.
- [**Agent skill**](agent-skill.md) — install `memd` into Claude Code / Codex with one command.

## More

- [GitHub repo](https://github.com/fmschulz/memd)
- [Changelog](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md)
