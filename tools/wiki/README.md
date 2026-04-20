# memd-wiki

Deterministic compiler that builds a Karpathy-style compiled markdown wiki from
live `memd` project state, read through the MCP HTTP API.

> **Status:** relocated under `tools/wiki/` from
> `prototypes/compiled_wiki/` in the 0.9.0 cycle. Config loader,
> containment guard, determinism pin, force-emit, and lint land in
> follow-up steps of the Item 7 plan
> (`docs/plans/active/2026-04-20-item7-compiled-wiki-promotion.md`,
> gitignored per project convention).

## Scope

Version 1 is deterministic and compiler-style.

Pages built:

- `index.md`
- `log.md`
- `projects/<project_id>.md`
- `tasks/<task_id>.md`
- `libraries/{failures,decisions,evidence,highlights}.md`
- `manifest.json`

`memd` tools called:

- `context.brief_project`
- `task.resume`
- `artifact.list_thread`
- `artifact.find_failures`
- `artifact.find_decisions`
- `artifact.find_evidence`
- `artifact.find_highlights`

The compiler is trust-aware:

- digest-backed pages display the MCP `trust_tier`
- pages show whether the source payload still requires verification
- digest-backed pages render `grounding_refs` as links back to canonical
  task pages
- thread event rows surface artifact verification and promotion state when
  present

LLM-authored concept/entity pages (v2) and a browsable runtime (v3) are
explicitly deferred.

## Install

`memd-wiki` is stdlib-only Python ≥ 3.11. No third-party runtime deps.

Development install from a `memd` checkout:

```bash
pip install -e tools/wiki/
```

Release install directly from the repo (pick a tag matching your `memd`
binary — see "Version compatibility" below; replace `<TAG>` with the
tag you want, e.g. `v0.9.0` once that release ships):

```bash
pipx install "memd-wiki @ git+https://github.com/fmschulz/memd@<TAG>#subdirectory=tools/wiki"
```

Either path exposes the `memd-wiki` console script.

## Version compatibility

`memd-wiki` is version-aligned with the `memd` server it talks to. On
startup it parses its own `__version__` and the server's
`serverInfo.version` (from the MCP `initialize` response) as
`MAJOR.MINOR.PATCH` and compares them:

| Situation | Behavior |
|---|---|
| exact match | OK — silent |
| patch-only skew (e.g. `0.9.0` vs `0.9.3`) | WARN on stderr, proceed |
| MAJOR or MINOR mismatch (e.g. `0.9.x` vs `0.8.x` or `0.10.x`) | hard fail with `ServerIncompatibleError` |
| server did not report version / unparseable | WARN on stderr, proceed |

Releases of `memd-wiki` and `memd` are tagged in lockstep. To override
the gate (not recommended), construct `McpHttpClient(..., check_compat=False)`
from Python — the CLI keeps the gate on.

## Usage

After install:

```bash
memd-wiki \
  --tenant-id memd \
  --project-id memd \
  --memd-url http://127.0.0.1:8787/mcp
```

Or from a source checkout without install:

```bash
python -m compiled_wiki.cli \
  --tenant-id memd \
  --project-id memd \
  --memd-url http://127.0.0.1:8787/mcp
```

Generated wiki is written to `output/`.

## Tests

```bash
python -m unittest discover -s tests
```

## Layout

- `compiled_wiki/`: Python package
- `schema/AGENTS.md`: compiler-owned wiki conventions
- `tests/`: unit + smoke tests
- `output/`: generated markdown, ignored by git
