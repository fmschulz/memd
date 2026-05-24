# memd-wiki Schema

This directory defines the rules for the `memd-wiki` compiler.

## Layers

There are three layers:

1. Raw source of truth: live `memd` project state accessed through the local CLI.
2. Compiled wiki: markdown files generated into `output/`.
3. Schema: this file, which defines how the compiler organizes and maintains pages.

## Ownership

- `output/` is compiler-owned.
- Humans and agents should not hand-edit generated pages.
- Manual edits belong in source systems or in future synthesis layers, not in generated files.

## Page Types

- `index.md`: top-level catalog and entry point
- `log.md`: chronological change/event log
- `projects/<project_id>.md`: project overview page
- `tasks/<task_id>.md`: one page per task thread
- `libraries/*.md`: digest-backed failure/decision/evidence/highlight pages

## Provenance

Every generated page should expose enough provenance for a reader to trace claims back
to `memd` task IDs, artifact IDs, and timestamps.

## Linking

- `index.md` links to the project page, library pages, and task pages.
- project pages link to tasks and libraries.
- library pages link back to task pages.
- task pages link back to the project page.

## Version 1 Constraints

- deterministic only
- no LLM-authored concept/entity pages
- no direct writes back into `memd`
- no manual curation layer inside `output/`
