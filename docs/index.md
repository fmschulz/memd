# memd

[![Version](https://img.shields.io/badge/version-0.60.0-blue)](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md){ .md-button }
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
| **Guardrails** | Pin tenant/project scope and verify CLI-first agent wiring | `memd init`, `memd doctor` |

---

## Headline number

Cross-system LoCoMo retrieval (10 conversations, 5,882 turns, 1,536 queries):

| System | MRR@10 | Hit@10 | Avg search |
| --- | ---: | ---: | ---: |
| **`memd` v0.50.0** | **0.420** | **0.621** | **26.7 ms** |
| `superlocalmemory` v3.4.46 (lexical) | 0.369 | 0.599 | 804.5 ms |
| `mem0` v2.0.2 (LLM-extracted) | 0.354 | 0.591 | 40.9 ms |

`memd` wins on every quality metric and search latency. See
[Benchmarking](benchmarking.md) for per-category breakdown, reproducibility
notes, and the internal-corpus + Bright-Pro adapter results.

---

## Start here

- [**Quick start**](quickstart.md) — install, store first memory, retrieve, build agent context.
- [**Operational contract**](operational-contract.md) — what agents should write, avoid, verify, and clean up.
- [**Architecture**](architecture.md) — hybrid retrieval, storage, trust boundary diagram.
- [**CLI reference**](cli-reference.md) — every command and operation.
- [**Agent skill**](agent-skill.md) — install `memd` into Claude Code / Codex with one command.

## More

- [GitHub repo](https://github.com/fmschulz/memd)
- [Changelog](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md)
