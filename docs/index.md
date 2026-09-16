# memd

[![Version](https://img.shields.io/badge/version-1.7.1-blue)](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md){ .md-button }
[![Rust](https://img.shields.io/badge/Rust-2021-orange?logo=rust&logoColor=white)](https://github.com/fmschulz/memd/blob/main/Cargo.toml){ .md-button }
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/fmschulz/memd/blob/main/LICENSE){ .md-button }

`memd` is a local memory CLI for coding agents and AI scientists. Each trusted
machine gets one shared, persistent store: raw searchable content, structured
task history, and canonical collaboration artifacts. A hybrid dense + sparse
stack indexes it. Experience cases connect problems, attempts, check receipts,
and conditional lessons, with the calling session's provenance.

Agents retrieve bounded context with `memd agent-context` or `memd search`,
read the generated context file, and record operational facts with `memd add`.
Keep project plans, test results, and handoffs in repository files. Use memd
for facts those files cannot answer, such as another machine's mounts or
deployment state.
Concise experience cases can also preserve reusable repairs, linked to the
repository evidence. These commands require a source build from `main`;
published 1.7.1 binaries do not include them.
For low-latency local use, `memd` keeps the store and indexes hot through a
private CLI-managed warm worker driven by ordinary CLI commands.

---

## What memd does

| Surface | Purpose | Primary CLI commands |
| --- | --- | --- |
| **Raw memory** | Store operational facts and index selected source material for retrieval | `memd add`, `memd search`, `memd get`, `memd stats` |
| **Experience cases** | Preserve attempts and checks; return applicable current lessons or an explicit abstention | `memd experience record`, `check`, `get`, `find`, `export`, `import` |
| **Agent context** | Bounded pre-work context + JSON audit logs | `memd agent-context --output .memd/context.md` |
| **Startup memory** | Refresh project `memory.md` with scope, health warnings, and ranked facts filtered against repository documents | `memd memory-md`, `memd eval-memory-md --agent-usefulness` |
| **Usefulness report** | Usage-ledger and store self-diagnosis for growth, learning, retrieval, and warnings | `memd report --strict` |
| **Warm CLI** | Keep store/index state hot for repeated local calls | `memd warm start`, `memd warm status` |
| **Batch CLI** | Many structured operations in one loaded process | `memd batch --jsonl requests.jsonl` |
| **Export/import** | Manual cross-machine moves through portable OMF | `memd export-omf`, `memd import-omf` |
| **Operations** | Structured memory / task / artifact / context / code / debug ops | `memd call task.start --json '{...}'` |
| **Guardrails** | Pin tenant/project scope and verify CLI-first agent wiring | `memd init`, `memd doctor` |

---

## What hybrid retrieval buys

LoCoMo retrieval over 1,531 questions, one build, one corpus, one machine. Only
the retrieval path varies, so the comparison isolates fusion from everything
else:

| Retrieval | MRR@10 | p50 latency |
| --- | ---: | ---: |
| **hybrid (dense + BM25, RRF)** | **0.4766** | 27.0 ms |
| dense only | 0.3230 | 21.3 ms |
| BM25 only | 0.3376 | 8.3 ms |

Fusion adds 0.139 MRR@10 over the better single channel at roughly 3x the
sparse-only latency.

Cross-system accuracy numbers are not published here. Comparing memory systems
requires every system to share a pinned dataset, answer model, judge, retrieval
depth, and token budget, with per-item rows bound to an immutable manifest; that
evidence lives in the benchmark repository. See [Benchmarking](benchmarking.md)
for the contract.

---

## Start here

- [Record a checked repair](experience-how-to.md): a runnable case with failure, repair, recall, and transfer.
- [Experience memory](experience-memory.md): provenance, check coverage, applicability, and trust limits.
- [**Quick start**](quickstart.md) — install, store first memory, retrieve, build agent context.
- [**Operational contract**](operational-contract.md) — what agents should write, avoid, verify, and clean up.
- [**Architecture**](architecture.md) — hybrid retrieval, storage, trust boundary diagram.
- [**CLI reference**](cli-reference.md) — every command and operation.
- [**Agent skill**](agent-skill.md) — install `memd` into Claude Code / Codex with one command.

## More

- [GitHub repo](https://github.com/fmschulz/memd)
- [Changelog](https://github.com/fmschulz/memd/blob/main/CHANGELOG.md)
