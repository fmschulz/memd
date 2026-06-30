# memd

[![Version](https://img.shields.io/badge/version-1.2.1-blue)](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md){ .md-button }
[![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white)](https://github.com/fmschulz/memd/blob/main/Cargo.toml){ .md-button }
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/fmschulz/memd/blob/main/LICENSE){ .md-button }

`memd` is a local memory CLI for coding agents and AI scientists. Each trusted
machine gets one shared, persistent store: raw searchable content, structured
task history, and canonical collaboration artifacts. A hybrid dense + sparse
stack indexes it; an explicit trust boundary decides what counts as verified.

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
| **Startup memory** | Refresh project `memory.md` with latest project state, ranked fact libraries, and concrete action guidance | `memd memory-md`, `memd eval-memory-md --agent-usefulness` |
| **Usefulness report** | Usage-ledger and store self-diagnosis for growth, learning, retrieval, and warnings | `memd report --strict` |
| **Warm CLI** | Keep store/index state hot for repeated local calls | `memd warm start`, `memd warm status` |
| **Batch CLI** | Many structured operations in one loaded process | `memd batch --jsonl requests.jsonl` |
| **Export/import** | Manual cross-machine moves through portable OMF | `memd export-omf`, `memd import-omf` |
| **Operations** | Structured memory / task / artifact / context / code / debug ops | `memd call task.start --json '{...}'` |
| **Guardrails** | Pin tenant/project scope and verify CLI-first agent wiring | `memd init`, `memd doctor` |

---

## Headline number

Cross-system LoCoMo retrieval (10 conversations, 5,882 turns, 1,536 queries):

| System | MRR@10 | Hit@10 | Avg search |
| --- | ---: | ---: | ---: |
| **`memd` (hybrid)** | **0.412** | **0.613** | **23.2 ms** |
| `superlocalmemory` v3.4.46 (lexical) | 0.369 | 0.599 | 804.5 ms |
| `mem0` v2.0.2 (LLM-extracted) | 0.354 | 0.591 | 40.9 ms |

`memd` leads on MRR@10 (+12% vs SuperLocalMemory, +16% vs Mem0), Hit@10, and
search latency. Caveats: mem0 used a self-hosted gemma4-31b, and
superlocalmemory ran lexical-only — full protocol in [Benchmarking](benchmarking.md).

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
