# memd-wiki

Deterministic compiler that builds a Karpathy-style compiled markdown wiki from
live `memd` project state, read through the MCP HTTP API.

> **Status:** relocated under `tools/wiki/` from
> `prototypes/compiled_wiki/` in the 0.9.0 cycle. Packaging, config,
> containment, and lint land in follow-up steps of the Item 7 plan
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

## Usage

From this directory:

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
