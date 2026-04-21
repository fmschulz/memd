# memd-wiki

Deterministic compiler that builds a Karpathy-style compiled markdown wiki from
live `memd` project state, read through the MCP HTTP API.

> **Status:** v0.10.0 ships LLM-authored concept / entity pages
> alongside the v1 compiler-owned surface. The opt-in v2 lanes are
> empty by default — a fresh install with no `wiki_page` artifacts
> behaves identically to v0.9.0.

## Scope

The wiki has three ownership lanes (manifest `schema_version = 2`):

| Lane | Prefixes | Writer | Lint enforcement |
|---|---|---|---|
| compiler | `index.md`, `log.md`, `projects/`, `tasks/`, `libraries/`, `manifest.json` | the `memd-wiki build` compiler | strict — manifest-drift removes orphans |
| llm | `concepts/`, `entities/` | the compiler renders one page per `wiki_page` artifact | strict — one file per WikiPage; `concept-*` checks |
| human | `notes/` (declared, opt-in) | humans, out-of-band | permissive — compiler never writes here |

Pages built (compiler lane):

- `index.md`
- `log.md`
- `projects/<project_id>.md`
- `tasks/<task_id>.md`
- `libraries/{failures,decisions,evidence,highlights}.md`
- `manifest.json`

Pages built (LLM-authoring lane, only when WikiPage artifacts exist):

- `concepts/<artifact_id>.md` — `artifact_role = concept`
- `entities/<artifact_id>.md` — `artifact_role = entity`

`memd` tools called:

- `context.brief_project`, `task.resume`, `artifact.list_thread`
- `artifact.find_failures`, `artifact.find_decisions`,
  `artifact.find_evidence`, `artifact.find_highlights`
- `artifact.search` (filter `artifact_kind = wiki_page` for
  authoring lane; second call filtered on `reply_to_artifact_id`
  to fetch verification children)
- `artifact.get` (resolves each WikiPage's grounding refs into full
  citation records)

The compiler is trust-aware:

- digest-backed pages display the MCP `trust_tier`
- pages show whether the source payload still requires verification
- digest-backed pages render `grounding_refs` as links back to canonical
  task pages
- thread event rows surface artifact verification and promotion state when
  present
- WikiPage trust never changes — a concept / entity page sits at
  `CanonicalRecord` forever; "Verified by:" footers come exclusively
  from distinct-writer Verification *children* of the page

A browsable runtime (`memd-wiki serve`) is explicitly deferred to v3.

## Authoring concept / entity pages

A WikiPage is a regular memd artifact created via the existing
`artifact.create` MCP tool. The four required boundary rules
(enforced server-side):

```jsonc
{
  "artifact_kind": "wiki_page",
  "tenant_id": "your_tenant",
  "project_id": "your_project",
  "task_id": "task-where-this-page-lives",
  "agent_id": "your-author-id",
  "artifact_role": "concept",          // or "entity"
  "summary": "≤500-byte page subtitle",
  "content": "# Markdown body…\n\n…",   // ≤256KB
  "related_artifact_ids": [             // non-empty grounding
    "0199ae...task_finish_id",
    "0199af...evidence_id"
  ]
}
```

The next `memd-wiki build` will:

1. Fetch the page via `artifact.search` (filter
   `artifact_kind=wiki_page`).
2. Resolve each `related_artifact_ids` entry via `artifact.get` so
   the rendered footer can cite the kind, role, and trust tier of
   every cited record.
3. Write `concepts/<artifact_id>.md` (or `entities/<artifact_id>.md`)
   with YAML frontmatter, the summary as title, the markdown body,
   a `## Grounded By` footer, and (if any distinct-writer
   `Verification` children exist) a `## Verified By` footer.
4. Add an entry to `manifest.concept_pages` so the lint can validate
   each page's grounding shape.

To get a `Verified by:` line in the footer, a *distinct* agent must
file a `Verification` artifact via `artifact.create` with
`reply_to_artifact_id = <wiki_page_id>` and
`supports_claim = true`. The server-side
`promote_if_countersigned` path promotes the verification child to
`VerifiedRecord` (the page itself stays at `CanonicalRecord` —
this is the codex-caught §4.2 trust rule).

## Lint exit codes

`memd-wiki lint` returns:

- `0` clean
- `1` warnings only (`task-snapshot-stale`, `trust-tier-ungrounded`,
  `concept-stale`)
- `2` errors (`library-missing-grounding`, `dead-backlink`,
  `manifest-drift`, `manifest-missing`, `manifest-invalid`,
  `concept-missing-grounding`,
  `concept-contradicts-canonical`, `concept-trust-tier-ungrounded`)

Or — when the on-disk manifest declares a future
`schema_version` — exit 2 with a one-line "upgrade memd-wiki"
diagnostic.

## Migrating from v0.9.0

```bash
memd-wiki migrate --output-dir <wiki_dir>
```

Upgrades a v1 manifest to v2 in place: bumps `schema_version` to 2,
adds the new lane prefix lists, and an empty `concept_pages`. Use
`--dry-run` to print the upgraded manifest to stdout without writing.
The next `memd-wiki build` overwrites the manifest with the
canonical v2 shape regardless, so the explicit migrate is only
needed if you want to read the wiki with v0.10.0 lint before
recompiling.

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

## Determinism contract (v1)

`memd-wiki build` commits to the following narrow-but-honest
determinism contract on unchanged memd state:

1. Running the compiler a second time with no memd state change
   yields `written=0` (every page was byte-identical to the prior
   run).
2. `manifest.json` is byte-identical between consecutive runs on
   unchanged state.

The prototype already behaves this way; step 5 of the Item 7 plan
pins it with a unit test (`tests/test_determinism.py`) using a mocked
MCP transport so the invariant survives future refactors.

**What the contract does NOT claim in v1:** that a single new memd
artifact changes only one page. The compiler derives a global
`source_snapshot_at_ms` from the max over project/library/task
artifact timestamps and renders it into every page's footer, so any
new artifact legitimately churns aggregate surfaces (`log.md`,
`index.md`, `projects/*.md`). The v2 may strengthen this once the
snapshot-footer model is reworked; v1 accepts the churn as honest.

## Manifest ownership (plan §6.1)

`manifest.json` carries `schema_version` (currently `1`) and
`compiler_owned_prefixes`, the set of output paths the compiler
manages. v1 emits:

```
["index.md", "log.md", "manifest.json", "projects/", "tasks/", "libraries/"]
```

Paths outside these prefixes are ignored by the compiler entirely.
v2 may add LLM-authored / human-edited prefixes (e.g. `concepts/`)
without changing the manifest format.

## Containment guard

`memd-wiki` refuses to write under the memd data directory. The
guard matches the Rust `memd export-markdown` containment rules
verbatim:

- Refuses when `--output-dir` resolves inside `$HOME/.memd/data/`
  (the default data dir).
- Refuses when `--output-dir` resolves inside a `data_dir` declared
  in the nearest-ancestor `.memd/tenant_scope.json`. Discovery stops
  at the first scope file found even if malformed or missing
  `data_dir` — an outer project's config cannot silently take over.
- Refuses when any pre-existing symlink sits BELOW the outdir root.
  The outdir itself may be a symlink (operators may point the CLI at
  symlinked directories they own); only intermediate and leaf
  components inside outdir are inspected. Non-existing components
  are fine (they will be created on write).

`--data-dir PATH` overrides the default+discovery composition and
makes the guard check only that path (matches Rust's explicit-
override semantics).

## Configuration

`memd-wiki` reads the nearest-ancestor `.memd/config.json`, using the
`wiki` subsection plus the top-level `tenant_id` / `project_id`. Missing
fields fall through to hardcoded defaults. CLI flags win over everything.

Example `.memd/config.json`:

```json
{
  "tenant_id": "memd",
  "project_id": "memd",
  "wiki": {
    "outdir": "docs/compiled_wiki",
    "max_tasks": 25,
    "library_k": 20,
    "memd_url": "http://127.0.0.1:8787/mcp"
  }
}
```

Precedence (highest first):

1. CLI flags (`--tenant-id`, `--project-id`, `--output-dir`,
   `--max-tasks`, `--library-k`, `--memd-url`)
2. `wiki.<field>` in the nearest `.memd/config.json`
3. Top-level `tenant_id` / `project_id` in the same file
4. Built-in defaults (`http://127.0.0.1:8787/mcp`, 25, 20,
   `./compiled_wiki/` relative to CWD)

Relative `wiki.outdir` resolves against the project root that owns
the config file (the directory that contains `.memd/`), not the CWD.
Absolute paths are used as-is. If a candidate `.memd/config.json`
exists but cannot be parsed, `memd-wiki` exits with a precise error
rather than silently falling through.

`--config-start PATH` overrides the starting directory for the
ancestor walk.

## Usage

`memd-wiki` ships two subcommands: `build` (the default when no
subcommand is given) and `lint`.

### Build

After install, with a `.memd/config.json` in the project:

```bash
memd-wiki build
```

Or override individual fields from the CLI:

```bash
memd-wiki build \
  --tenant-id memd \
  --project-id memd \
  --memd-url http://127.0.0.1:8787/mcp
```

From a source checkout without install:

```bash
python -m compiled_wiki.cli build \
  --tenant-id memd \
  --project-id memd
```

Generated wiki is written to the resolved output directory (CLI flag,
config, or `./compiled_wiki/` under CWD).

### Lint

```bash
memd-wiki lint
```

Runs 5 health checks over the compiled tree. Exit codes:

- `0` — clean
- `1` — warnings only
- `2` — errors

Checks (plan §5):

| Check | Severity | What it flags |
|---|---|---|
| `library-missing-grounding` | ERROR | digest-backed library page has no grounded_by refs |
| `dead-backlink` | ERROR | a library / project / index / log page links to a `tasks/<id>.md` that was not emitted |
| `trust-tier-ungrounded` | WARN | task page renders from `compiled_digest_hint` with `requires_verification=True` and no grounded sibling |
| `manifest-drift` | ERROR | extra file under compiler-owned prefixes, or a manifest-implied page missing from disk (scoped to `manifest.compiler_owned_prefixes`; force-emit task pages are accepted) |
| `manifest-missing` / `manifest-invalid` | ERROR | `manifest.json` missing, non-JSON, or shape-wrong |

`task-snapshot-stale` is reserved for a future memd-backed lookup; the
v1 offline lint leaves that slot as a skippable callback rather than
ship a wrong heuristic.

## Tests

```bash
python -m unittest discover -s tests
```

## Layout

- `compiled_wiki/`: Python package
- `schema/AGENTS.md`: compiler-owned wiki conventions
- `tests/`: unit + smoke tests
- `output/`: generated markdown, ignored by git
