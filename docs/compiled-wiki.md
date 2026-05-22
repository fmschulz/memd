# Compiled wiki

[`tools/wiki/`](https://github.com/fmschulz/memd/tree/main/tools/wiki) ships
`memd-wiki`, a Python console script that compiles a Karpathy-style markdown
wiki from live `memd` project state. Pages include `index.md`, `log.md`,
`projects/<project_id>.md`, `tasks/<task_id>.md`, and
`libraries/{failures,decisions,evidence,highlights}.md`, each trust-aware
(displays `trust_tier`, `requires_verification`, and `grounded_by` links).

## Install

Stdlib-only, Python ≥ 3.11:

```bash
pip install -e tools/wiki/
memd-wiki build
memd-wiki lint
```

## Serve

```bash
memd-wiki serve [--output-dir PATH] [--host 127.0.0.1] [--port 8099]
```

Read-only stdlib HTTP server over a compiled tree. Binds localhost by
default; pass `--host 0` or `--port 0` for ephemeral addresses (useful in
tests and CI). SIGINT / SIGTERM shut the server down cleanly.
`memd-wiki serve` never rebuilds the tree — run `memd-wiki build` first
whenever memd state changes.

## Three ownership lanes

The wiki has three lanes:

| Lane | Prefixes | Writer | Lint enforcement |
| --- | --- | --- | --- |
| compiler | `index.md`, `log.md`, `projects/`, `tasks/`, `libraries/`, `manifest.json` | the `memd-wiki build` compiler | strict — manifest-drift removes orphans |
| llm | `concepts/`, `entities/` | the compiler renders one page per `wiki_page` artifact | strict — one file per WikiPage; `concept-*` checks |
| human | `notes/` (opt-in) | humans, out-of-band | permissive — compiler never writes here |

`memd-wiki` is version-aligned with the `memd` binary it talks to
(MAJOR.MINOR must match; patch skew warns). See
[`tools/wiki/README.md`](https://github.com/fmschulz/memd/tree/main/tools/wiki)
for the full install paths, `.memd/config.json` `wiki` subsection,
containment guard, determinism contract, and the 5-check lint table.
