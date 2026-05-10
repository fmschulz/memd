# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

## [0.13.0] - 2026-05-10

### Added

- CLI-first agent workflow: `memd agent-context`, expanded `memd search`,
  `memd add`, and `memd call <operation> --json ...` now cover the ordinary
  agent workflow without external client-tool registration.
- Private warm-worker commands (`memd warm start/status/stop`) and
  `--warm auto|off|required` for `search`, `agent-context`, and `call`, giving
  repeated local retrieval a low-latency path while keeping the public
  interface as shell commands.
- `memd batch --jsonl` for benchmark and script workloads that need many
  structured operations in one loaded process.
- Optional MemReranker-4B post-retrieval reranking for `memd search` through
  explicit `--reranker auto|memreranker-4b` flags. The default search path
  stays slim and does not load Python, PyTorch, Hugging Face models, or GPU
  runtimes.
- Bright-Pro adapter code for scoped static-retrieval checks against the
  Bright-Pro framework, including `memd`, SuperLocalMemory, and optional
  MemReranker comparison lanes.

### Changed

- Root documentation, quickstart, skill docs, installer scripts, and verifier
  scripts now document skill + CLI as the main workflow.
- The compiled JSON-RPC/HTTP transport code was removed from the Rust crate;
  reusable operation handlers remain available through `memd call` and
  `memd batch`.
- The bundled `memd` skill binary was refreshed to the current release build.
- README benchmarking now highlights the recommended warm-worker and batch
  execution modes. Startup-overhead diagnostics remain in raw benchmark
  artifacts for reproducibility, but are not presented as the recommended agent
  workflow.
- Task-memory benchmark scripts now run against the release binary by default
  and can report `cli_cold`, `cli_warm`, and `cli_batch` lanes.
- `memd-wiki` compiler code now handles path-shaped project identifiers with a
  safe page stem, prunes missing referenced tasks instead of failing a build,
  and backfills digest-library grounding refs from concrete result rows when
  available.
- `memd-wiki` now reads project state through the local `memd` executable
  instead of a network endpoint.

### Fixed

- README Mermaid diagrams were simplified so GitHub renders them reliably.
- CLI workflow docs no longer point to stale client-registration files or
  wrapper guard paths that are not part of the shipped workflow.
- Release-facing ignore rules now keep local caches, draft paper files, raw
  benchmark payloads, and local agent configuration out of normal commits.
- `memory.health` now computes duplicate aggregate counts and ratios over the
  full requested tenant/project scope even when `include_examples=true`.
  `duplicate_limit` limits only the returned example groups, so
  `memory.dream` reports no longer understate duplicate pressure.
- `context.find_relevant_context(include_hot=true)` now bounds the legacy
  hot-context pre-scan before falling through to normal retrieval, avoiding
  long lookups on large or duplicate-heavy tenants.
- Retrieval/list scans now skip unreadable stale segment rows with a warning
  instead of aborting the whole scan; strict `memory.get` behavior is
  unchanged.

## [0.12.0] - 2026-04-21

### Added

- **BEIR retrieval-gate infrastructure.** The `memd-evals` harness now
  computes BEIR-standard nDCG@{1, 5, 10, 100}, supports graded qrels,
  reads `CrossCorpusReport` / `BenchmarkReport` interchangeably in the
  regression gate, accepts a TOML dataset manifest, and ships with a
  committed baseline + GitHub Actions workflow so accidental retrieval
  regressions fail PR checks instead of silently landing.
  - **P1 nDCG math** (`59df881`). New
    `calculate_ndcg(retrieved, grades, k)` in
    `evals/harness/src/suites/benchmark_protocol/math.rs` using the
    standard `2^rel - 1` gain with `log2(rank + 1)` discount.
    Ten unit tests against hand-computed textbook vectors with 1e-12
    tolerance: binary / graded perfect-ranking = 1.0, reverse-ranking
    against the [3,2,1] ideal = 0.6806060567602009, retrieved outside
    qrels = 0.0, empty grades = 0.0, k=0 = 0.0, iDCG caps at known
    relevant docs (1 retrieved / 2 relevant → 0.6131…), cutoff
    excludes positions beyond k, mixed ranking with a zero slot =
    0.9594535145926796, k > retrieved.len() with complete qrels = 1.0.
  - **P2 graded qrels + per-query nDCG in reports** (`a062d8f`).
    Additive `queries[].relevance_grades: HashMap<String, u8>` on
    dataset JSON. `build_query_grades()` synthesizes grade=1 for
    every entry in `relevant[]`, and explicit grades override that
    (including grade=0 to demote a binary-relevant entry).
    `QueryMetrics` / `BenchmarkSummary` gain
    `ndcg_at_k: BTreeMap<usize, f64>` fields — BTreeMap for
    byte-stable JSON, `#[serde(default)]` so pre-0.12 baselines
    read clean. The benchmark runner asks the retriever for top-100
    (the max cutoff in `NDCG_K_VALUES = &[1, 5, 10, 100]`);
    recall/MRR/P@10 are explicitly sliced to top-10 so old baselines
    remain numerically comparable. Stdout summary lines gain an
    `nDCG@10 {x.xxx | n/a}` suffix.
  - **P3 CrossCorpusReport-aware regression gate + `--metric`**
    (`410d7f4`, folded as `117d117`).
    `load_report_either` tries `CrossCorpusReport` first and falls
    back to `BenchmarkReport`. Cross-corpus alignment pairs queries
    per-dataset (match by `dataset_path`, then by `query_id` within
    that dataset) so `q1` collisions across datasets can't produce
    false positives in the paired test. New `--metric` flag (default
    `ndcg_at_10`; accepts `all`, `recall_at_10`, `mrr`,
    `precision_at_10`, or `ndcg_at_<k>` for any positive k). Metrics
    absent from both sides skip with an actionable "regenerate
    baseline" reason instead of failing on missing data.
    `DatasetBenchmarkResult` gains `#[serde(default)] query_metrics`
    so cross-corpus reports carry the per-query detail the paired
    gate needs.
  - **P4 `--dataset-manifest` + trec-covid soft fetch + README
    reconcile** (`303c834`). New `evals/bench/beir_manifest.toml`
    lists fiqa / scidocs / trec-covid with path + name +
    qrels_format hint + approx_bytes + license. Relative paths
    resolve against the manifest's own directory. `memd-evals`
    gains a `--dataset-manifest` flag that concatenates manifest
    entries after explicit `--dataset-path` flags. The offline
    dataset fetcher gains `try_fetch_one` for `beir_trec-covid.json`
    — warns on 404 instead of failing the whole script (the base
    commit mirror doesn't carry trec-covid today; manual drop-in is
    still supported). `evals/bench/datasets/retrieval/README.md`
    reconciled: new tier table names all five BEIR files the
    harness ever touches, including the two manual-only entries
    (`beir_scifact_fixed.json`, `beir_nfcorpus.json`) that
    `scifact.rs` and `nfcorpus.rs` silently expect.
  - **P5 committed baseline + regeneration ritual** (`1fc3a92`).
    `evals/bench/baselines/beir_v1.json` — a `CrossCorpusReport`
    generated at this commit against fiqa (17 queries after
    relevance filtering) + scidocs (30 queries) with
    `--embedding-model all-minilm --system-variant hybrid-feature
    --max-queries 30 --max-documents 500 --seed 42
    --bootstrap-iterations 1000`. Headline: normalized nDCG@10 =
    0.4347 (fiqa 0.4969, scidocs 0.3725); normalized nDCG@100 =
    0.5221. `evals/BENCHMARK_PROTOCOL.md` gains the full nDCG
    section, a pinned-parameter table that lockstep-mirrors the CI
    env vars, and a two-PR regeneration ritual (PR 1 lands the
    substantive change; the gate fails; PR 2 lands the refreshed
    baseline with a written justification).
  - **P6 CI workflow** (`87a37be`, folded as `c1ac5b8` after Codex
    review). `.github/workflows/retrieval-gate.yml` runs on
    pull_request + push-to-main against `crates/` / `evals/` /
    `Cargo.{toml,lock}`. Single job: cache cargo + BEIR datasets +
    embedding model, build release memd + memd-evals, run
    `--suite benchmark` with the pinned-parameter set,
    run `--suite benchmark-regression --metric ndcg_at_10` against
    the committed baseline, upload candidate + regression reports
    as an always-retain artifact. Concurrency group cancels
    superseded runs for the same ref. `timeout-minutes: 60`
    accommodates the cold-cache path. Least-privilege
    `permissions: { contents: read, actions: read }`. All pinned
    parameters live as top-level env vars
    (`BEIR_EMBEDDING_MODEL`, `BEIR_SYSTEM_VARIANT`,
    `BEIR_MAX_QUERIES`, `BEIR_MAX_DOCUMENTS`, `BEIR_SEED`,
    `BEIR_BOOTSTRAP_ITERATIONS`, `BEIR_METRIC`,
    `BEIR_SIGNIFICANCE_ALPHA`, `BEIR_MIN_EFFECT_SIZE`) so a bump
    is a single edit.

### Changed

- `memd-evals`: retrieval depth for the benchmark protocol is now 100
  (max of `NDCG_K_VALUES`), up from 10. Historical recall/MRR/P@10
  are explicitly sliced to the top-10 window inside `evaluate_queries`
  so pre-0.12 baselines remain numerically comparable.

### Deferred to v0.12.x+

- Heavy-BEIR lane (`nq`, `hotpotqa`, `arguana`, `webis-touche2020`)
  gated on a `--include-heavy` branch in the fetcher with a
  `--accept-licenses` gate.
- Commit-SHA pinning for the GitHub Actions in
  `.github/workflows/retrieval-gate.yml` (a repo-wide supply-chain
  audit pass, not a retrieval-gate concern specifically).
- `beir_trec-covid.json` mirror hosting (deferred until a future
  commit bump that includes it in the pinned base-URL commit).

## [0.11.0] - 2026-04-21

### Added

- **memd-wiki serve v3.0: read-only HTTP runtime over the compiled
  tree.** The four-phase sub-plan deferred from v0.10.0 shipped as
  four discrete commits (`fdb67fc`, `6711f87`, `4ec36d7`, `8087f3a`)
  with a mid-P1 security fold (`ffc825e`). `memd-wiki serve` binds
  localhost by default and exposes the compiler's full page set plus
  the human-owned `notes/` lane at stable artifact-id URLs. Zero new
  third-party dependencies — the server is stdlib
  `http.server.ThreadingHTTPServer` + a hand-rolled Markdown → HTML
  renderer.
  - **P0 `memd-wiki serve` skeleton** (`fdb67fc`). Registered the
    `serve` subparser next to `build` / `lint` / `migrate`, plus a
    pure `resolve_route(outdir, url_path)` + `make_handler(outdir)`
    factory. Signal-safe shutdown on SIGINT + SIGTERM.
  - **P1 hand-rolled Markdown → HTML renderer** (`6711f87`, folded
    in `ffc825e`). `tools/wiki/compiled_wiki/html_render.py` covers
    the exact dialect the deterministic compiler emits (ATX
    headings, `-` lists with 2-space continuation lines, inline
    code, link, italic/bold, YAML frontmatter) plus the minimal
    superset LLM-authored concept bodies need (fenced code, hr).
    Pure string-in / string-out so golden-byte tests don't bind a
    socket. Every link `href` runs through a URL-scheme allowlist
    — `http`, `https`, `mailto`, `ftp`, `ftps`, and scheme-less
    relative URLs are permitted; `javascript:` / `data:` / etc.
    render as HTML-escaped literal markdown so the filtered URL
    stays visible instead of silently dropped. The scheme guard
    runs AFTER the link rewriter so a malicious rewriter cannot
    smuggle an unsafe URL through. The served document is wrapped
    in a minimal self-contained `<html>` shell with an inline
    `<style>` block (zero external asset requests).
  - **P2 expanded route table + containment guard** (`4ec36d7`).
    Formal routes for `/`, `/log`, `/manifest.json` (raw
    `application/json`), `/concepts/<id>/`, `/entities/<id>/`,
    `/tasks/<id>/`, `/projects/<id>/`, `/libraries/<name>/`, and
    `/notes/a/b/c/`. Layered defenses before hitting disk: per-
    segment char allowlist `[A-Za-z0-9._-]+`, top-level prefix
    whitelist, reuse of
    `containment.reject_if_any_symlink_inside_outdir` (fail-closed
    parity with the Rust export-markdown CLI), and `Path.is_file()`
    — all rejections funnel to a 404 `text/plain` so the server
    does not leak why a request was refused.
  - **P3 per-page link rewriter** (`8087f3a`). Compiler-emitted
    relative `.md` links (`projects/memd.md`,
    `../tasks/019dadab.md`, `../libraries/failures.md`) are
    resolved against the current page's outdir-relative path and
    rewritten to the P2 route shape. Query + fragment suffixes are
    preserved. Paths that normalize to an outdir escape
    (`../outside.md` from `index.md`) pass through unchanged so
    the browser 404s cleanly. Round-trip integration test crawls
    BFS from `/`, scrapes every `<a href>` via
    `html.parser.HTMLParser`, and asserts zero dead internal links
    on a seeded linked tree.

### Changed

- Both `Cargo.toml` and `tools/wiki/pyproject.toml` move to 0.11.0
  in lockstep. The MCP tool contract and manifest schema are
  unchanged — a v0.11.0 `memd-wiki` works against a v0.11.0 memd
  server identically to how v0.10.0 against v0.10.0 worked, plus
  the new `serve` subcommand.

### Notes

- Test counts: Rust suite unchanged at 910 / 4 ignored; Python
  `tools/wiki` grew from 174 to 249 (+75 = 23 html_render unit +
  9 URL-scheme / edge-case folds + 23 P2 route-resolver + 15 P3
  rewriter + 5 integration/round-trip) with zero regressions.
- v3.1+ deferrals documented in `tools/wiki/README.md`: slugs +
  `concept_slugs` manifest field, `--build`/`--open`/`--quiet`
  flags, watch + live rebuild, in-memory search.

## [0.10.0] - 2026-04-20

### Added

- **memd-wiki v2: LLM-authored concept / entity pages (plan
  `docs/plans/active/2026-04-20-memd-wiki-v2-llm-authored-pages.md`).**
  Adds a first-class lane for LLM-authored knowledge pages alongside
  the deterministic compiler-owned surface from v1. Six phases
  shipped as discrete commits, each with its own test gate; the
  rollout is opt-in (default `concept_pages = []`) so a fresh
  install runs identically to v0.9.0.
  - **`ArtifactKind::WikiPage` + `TaskArtifact::content`** Phase 0:
    new variant on `ArtifactKind` with `as_str = "wiki_page"`,
    `FromStr` mapping (error-message updated to enumerate the new
    kind), and a nullable `content: Option<String>` field on
    `TaskArtifact` carrying the markdown body. The trust model is
    explicit: a fresh `WikiPage` sits at `TrustTier::CanonicalRecord`
    and stays there forever — the existing
    `promote_if_countersigned` path early-returns on non-Review/
    Revision/Verification/Decision kinds, so the WikiPage's own
    `promotion_state` never reaches `Verified`. Verification of the
    page's claims is signaled by *children*: distinct-writer
    `Verification` artifacts whose `reply_to_artifact_id` targets
    the page promote themselves (via the existing path) and the
    renderer surfaces them as a `Verified by:` footer. MCP-boundary
    validator rejects non-empty `content` on every other kind.
  - **`artifact.create` accepts `wiki_page`** Phase 1: enriched
    boundary validator requires non-empty `related_artifact_ids`
    (grounding refs), `summary ≤ 500 bytes`, `artifact_role ∈
    {"concept", "entity"}`, and `content ≤ 256KB`. Constants live
    on `pub(crate)` so follow-on work references the canonical
    limits.
  - **Python compiler reads `wiki_page` artifacts** Phase 2:
    `tools/wiki/compiled_wiki/compiler.py` now calls `artifact.search`
    with `artifact_kind=wiki_page` filter, resolves each grounding
    ref via `artifact.get`, fetches Verification children via a
    second `artifact.search` call (filter on `reply_to_artifact_id`
    + `promotion_state=verified`), and renders one markdown page
    per WikiPage under `concepts/<artifact_id>.md` (role=concept) or
    `entities/<artifact_id>.md` (role=entity). Stable sort key
    `(artifact_role, entity_name or summary[:50], artifact_id)`
    keeps output byte-identical under arbitrary backend
    permutation. The render emits YAML frontmatter, a summary
    heading + metadata bullets, the raw markdown body from
    `content`, a `## Grounded By` footer with backlinks, and (when
    Verification children exist) a `## Verified By` footer. v1
    surfaces (index, log, projects, tasks, libraries) are
    unchanged.
  - **Manifest schema v2** Phase 2: bumps
    `MANIFEST_SCHEMA_VERSION` from 1 to 2, adds
    `llm_authored_prefixes = ["concepts/", "entities/"]`,
    `human_owned_prefixes = ["notes/"]` (declared for v3 but the
    compiler never writes there), and a `concept_pages` list with
    one entry per WikiPage (artifact_id, path, trust_tier, role,
    grounding_refs, source_updated_at_ms). Empty-by-default so a
    fresh install with no WikiPage artifacts produces a clean v2
    manifest.
  - **Four new lint checks** Phase 3, scoped to
    `manifest.concept_pages`:
    - `concept-missing-grounding` (ERROR, paranoid): WikiPage with
      empty `grounding_refs` (validator already rejects this; the
      check defends against rows from a prior-version server).
    - `concept-stale` (WARN, oracle-gated): page snapshot lags the
      newest grounded artifact by more than `concept_staleness_ms`
      (default 30 days). Skipped silently when no oracle is
      provided.
    - `concept-contradicts-canonical` (ERROR, syntactic scaffold):
      page cites ONLY `task_finish` artifacts with
      `status=rejected`; v3 layers an LLM-backed semantic diff
      onto the same hook.
    - `concept-trust-tier-ungrounded` (ERROR): page self-labels
      `verified: true` (frontmatter or body) without a matching
      `Verified by:` footer line. Closes the trust-laundering
      vector codex caught during plan review.
  - **Manifest forward-compat + `memd-wiki migrate`** Phase 4: new
    `WikiManifestTooNewError` raised by `check_manifest_version`
    when `manifest.schema_version > MAX_KNOWN_MANIFEST_SCHEMA_VERSION`.
    `lint_output_dir` re-raises it so a future-manifest reader gets
    a clear "upgrade memd-wiki" diagnostic instead of a silent
    partial lint. New `memd-wiki migrate` subcommand upgrades v1
    manifests to v2 in place (preserves existing fields, adds
    empty new lanes via `setdefault`); `--dry-run` prints the
    upgraded manifest without writing.
  - **MCP contract pin** Phase 2: `mcp_contract.py` adds
    `artifact.search` (with filter args) and `artifact.get` to
    `REQUIRED_MCP_TOOLS` so a memd-side rename or schema change
    breaks the contract test rather than the wiki silently.
- **Advisory single-writer lockfile for HF model downloads (Candle
  follow-up from v0.8.0 handoff).** `download_file` in
  `crates/memd/src/embeddings/download.rs` now acquires a sibling
  `<target>.lock` via atomic `create_new` (O_EXCL on Unix, CREATE_NEW
  on Windows) before streaming. Late-arriving processes see the
  contended lock, poll (50ms → 2s backoff, 15-minute bound) for the
  target file to publish, then return `Ok` without re-downloading.
  Stale locks older than 60 minutes (generous for a ~614MB Qwen3
  download on a 2 Mbps link) are reclaimed. Correctness is still
  anchored by the existing `hard_link`-based first-writer-wins
  publish — the lock is purely a bandwidth optimization that degrades
  to the pre-existing race-safe behavior on lock-owner crash,
  permission errors, or wait-timeout. Adds 10 new unit tests
  (`test_advisory_lock_*`, `test_wait_*`,
  `test_download_file_waiter_reuses_publication`) covering lock
  acquisition, contention, stale reclaim, TOCTOU re-check on
  lock-disappearance, and an end-to-end two-caller cooperation path
  against a single-connection HTTP server.

### Changed

- **MCP `artifact_kind` enum extended.** The `artifact.create`,
  `task.search`, and `artifact.search` JSON schemas now accept
  `"wiki_page"` in addition to the v1 set. JSON consumers with
  closed-enum validators must update.

### Migration

- Existing v0.9.0 wikis: run `memd-wiki migrate --output-dir
  <wiki_dir>` once to upgrade the manifest from `schema_version=1`
  to `schema_version=2`. The next `memd-wiki build` will overwrite
  the manifest with the canonical v2 shape regardless, so the
  explicit migrate is only needed if you want to read the manifest
  with v0.10.0 lint before recompiling.
- Existing v0.9.0 servers: refresh
  `memd-skill/bin/linux-x64/memd` and `~/.local/bin/memd` from the
  v0.10.0 release build, then restart the daemon. The MCP wire
  format is additive; pre-v0.10.0 clients keep working but cannot
  author `wiki_page` artifacts.

## [0.9.0] - 2026-04-20

### Added

- **memd-wiki: first-class sibling surface (Item 7 / plan
  `docs/plans/active/2026-04-20-item7-compiled-wiki-promotion.md`).**
  Relocated `prototypes/compiled_wiki/` to `tools/wiki/` as a tracked
  Python package with its own `pyproject.toml` (`requires-python>=3.11`,
  `console_scripts memd-wiki = compiled_wiki.cli:main`), version-
  aligned with the `memd` workspace. Ships:
  - **Server-version compat gate** via parsed MAJOR.MINOR
    (`compiled_wiki.compat`), stderr WARN on patch skew, hard fail
    `ServerIncompatibleError` on MAJOR.MINOR mismatch. Vendored
    stdlib semver parser (`_semver.py`).
  - **Config loader** (`compiled_wiki.config_loader`): nearest-
    ancestor `.memd/config.json` with `wiki` subsection (`outdir`,
    `max_tasks`, `library_k`, `memd_url`); CLI > config > built-in
    defaults precedence; typed `ConfigLoadError` with file path on
    malformed input; stops at the first scope-file hit even if
    incomplete.
  - **Containment guard** (`compiled_wiki.containment`), a verbatim
    port of `memd export-markdown`'s three refusal rules: reject
    `outdir` inside `$HOME/.memd/data`, reject `outdir` inside the
    scope-resolved `data_dir` from the nearest-ancestor
    `.memd/tenant_scope.json`, and reject any pre-existing symlink
    component below `outdir`. Uses `os.lstat` + `stat.S_ISLNK` so
    `ELOOP` / `ENOTDIR` / permission errors surface as refusals.
  - **Deterministic rebuild contract.** Second run on unchanged
    memd state yields `written=0` and byte-identical `manifest.json`.
    Stable secondary sorts (`task_id`, `artifact_id`) on tasks /
    thread artifacts / library results / log entries so the
    invariant holds under arbitrary backend reordering of a
    logically identical payload.
  - **Manifest v1**: `manifest.json` now embeds `schema_version: 1`
    and `compiler_owned_prefixes`, scaffolding the v2 LLM-authored /
    human-edited ownership split without changing the format.
  - **Force-emit referenced task pages.** Libraries and the project
    page can link to any task returned by a digest query; the
    compiler now emits `tasks/<id>.md` for every referenced task
    (primary window union with `grounding_refs` + library
    `results.task_id`) so internal links never dangle.
  - **Lint subcommand** (`memd-wiki lint`): 5 health checks with
    exit codes `0` / `1` / `2` — library-missing-grounding
    (ERROR), dead-backlink (ERROR, scoped to compiler-owned
    prefixes), trust-tier-ungrounded (WARN), manifest-drift (ERROR,
    force-emit task pages accepted), manifest-missing /
    -invalid (ERROR).
  - **MCP tool contract pin** (`compiled_wiki.mcp_contract`): the
    7 tools memd-wiki depends on (`context.brief_project`,
    `task.resume`, `artifact.list_thread`, and the 4
    `artifact.find_*` surfaces) are pinned as typed expectations.
    Offline test asserts the contract matches compiler.py call
    sites; live integration test (skippable when no daemon reachable)
    asserts the running memd honors it.
  - Full operator README at `tools/wiki/README.md`.

### Fixed

- **Candle embedder `RelativeUrlWithoutBase` boot failure.** Replaced
  `hf-hub` 0.3.2 in `CandleEmbedder::with_config` with a direct `ureq`
  download helper (`embeddings::download::get_candle_bert_paths`). The
  old path passed huggingface.co's relative 307 Location headers verbatim
  to `ureq::get`, which failed URL parse with `Bad URL: failed to parse
  URL: RelativeUrlWithoutBase`. Plain `ureq::get` follows the relative
  redirect correctly against the request base. Fixes the pre-existing
  env-block that forced the v0.8.0 Item 1 retrieval gate to close by
  constructive argument instead of Recall@10 / MRR / Precision@10
  metrics.

### Changed

- **Dropped `hf-hub` workspace dependency.** Only consumer was the
  Candle embedder; replaced with the existing `download.rs` ureq
  plumbing.
- **Candle model cache moved to `~/.cache/memd/models/`.** Previously
  the Candle embedder used `hf-hub`'s cache at `~/.cache/huggingface/hub`
  (or `$HF_HOME/hub`). A host upgrading from v0.8.0 with a warm hf-hub
  cache but no network access will re-download `config.json`,
  `tokenizer.json`, and `model.safetensors` (~91MB) on first use. Once
  downloaded, the memd cache is stable across versions.
- **`download_file` writes atomically with first-writer-wins publish.**
  Downloads now stream into `<target>.partial.<pid>.<thread>.<counter>`,
  call `sync_all()`, then publish via `hard_link` — which fails
  atomically with `AlreadyExists` when a racing caller has already
  published the same target. The loser cleans up its tmp and keeps the
  winner's bytes; `rename` is deliberately avoided because it would
  silently clobber a concurrent publisher on Unix. An interrupted
  download, a crash mid-stream, or two racing processes/threads can
  now never leave a half-written `config.json` / safetensors blob at
  the canonical cache path where `verify_file_size` would either wedge
  boot or (worse) let a truncated model through. Applies to both the
  new Candle path and the existing ONNX `get_model_path_for` /
  `get_tokenizer_path_for` callers (Codex LOW round-1 + MEDIUM round-2).

## [0.8.0] - 2026-04-20

Post-nanomem cleanup release. Closes the remaining items from the
nanomem-inspired follow-up handoff (Items 2, 3, 4, 5, 6, and the
Item 2 PRAGMA-detection NIT), plus a verification-gate write-up for
F-track. Item 2 is a one-way schema migration; downgrade is not
supported.

### Changed

- **Schema migration (one-way): chunks UNIQUE is now tenant-scoped** (Item 2). The legacy `UNIQUE(segment_id, ordinal)` was a pre-existing bug: `PersistentStore::next_segment_id()` allocates segment IDs per-tenant, so tenant_a's (segment=1, ordinal=0) and tenant_b's first write would collide on the global UNIQUE and `INSERT OR REPLACE` would silently overwrite the first-written row's metadata. The constraint is now `UNIQUE(tenant_id, segment_id, ordinal)`. Legacy databases are migrated automatically on open via a rebuild-in-place in `migrate_chunks_unique_to_tenant_scoped`. `segment_id` is no longer globally meaningful; `MetadataStore::get_by_segment` takes a `tenant_id` parameter accordingly, and `idx_chunks_segment` is now `(tenant_id, segment_id)`.
- **PostWriteEvent moved to its own module** (Item 6). Relocated from `mcp::handlers` to `mcp::post_write_hooks`, with `PostWriteEvent::from_imported_chunk` adapter. Public path `memd::mcp::PostWriteEvent` preserved; `memd::mcp::handlers::PostWriteEvent` retained via `pub use` re-export.
- **chunks UNIQUE migration detection uses `PRAGMA index_list` / `pragma_index_info`** (Item 2 NIT). `migrate_chunks_unique_to_tenant_scoped` now inspects the concrete UNIQUE index columns SQLite reports instead of grepping the `CREATE TABLE` text. Tolerates DDL variations (`UNIQUE (segment_id, ordinal)` with a space, `CONSTRAINT name UNIQUE(...)`, casing) that the substring match would have missed — a legitimate memd legacy DB written by a non-default client would otherwise skip the rebuild. Filter is `origin='u'` only, so a foreign chunks schema with a manual `CREATE UNIQUE INDEX` (origin='c') is left alone (Codex NIT round-1 MEDIUM).
- **OMF supersession round-trip** (Item 5). `import_omf` now preserves the supersession graph across memd↔memd round-trips. Previously the importer dropped `extensions.memd.lifecycle.supersedes` / `superseded_by` because source-side chunk IDs had no meaning in the destination. The importer now runs two passes: pass 1 writes each chunk (assigning a fresh dest-side id) and records a `source_chunk_id → target_chunk_id` map; pass 2 replays each `supersedes` edge through `MetadataStore::atomic_supersede` using the translated dest-side IDs. Edges whose other side is not present in the document (partial export) are silently dropped, never translated to dangling IDs. Edge replay is gated by the F3 trust gate — untrusted sources (`source.app != 'memd'` or version mismatch) cannot fabricate supersession state. A pre-flight invariant check rejects malformed trusted documents (forked supersession graph, duplicate source chunk_ids, non-UUID `chunk_id` / `supersedes` / `superseded_by`) before any chunk is written, so a broken input can't half-apply. `preview_omf_import` runs the same pre-flight so it shares fail-closed behavior with real import.

### Added

- **`memd export-markdown` auto-discovers `--data-dir` from `.memd/tenant_scope.json`** (Item 4). When invoked from inside an initialized project, picks up the daemon's data dir from the nearest-ancestor scope config written by `memd init` (without forcing `--data-dir` on every call). Discovery *augments* the `$HOME/.memd/data` fallback rather than replacing it, so the containment guard still refuses an outdir inside the home default even if discovery finds a different path. Also persists `data_dir` in `tenant_scope.json` for every scope mode (not just `global`), and absolutizes `--memd-data-dir` at init time.
- **G3 symlink-escape guard on `memd export-markdown` writes** (Item 3). `reject_if_any_symlink_inside_outdir` refuses before any filesystem write if any existing component inside outdir is a symlink — closing a pre-existing-symlink-plant escape where an attacker could redirect the write to a path of their choosing. Outdir itself can be a symlink (users may legitimately point at a symlinked exports dir); only components BELOW outdir are refused.

### Migration notes

- **Downgrade is one-way.** A pre-Item-2 binary opening a post-migration database would see the new `UNIQUE(tenant_id, segment_id, ordinal)` constraint but its code paths still treat `segment_id` as globally unique — no silent corruption is expected, but any compaction / audit tooling from the older binary could misread tenant-scoped segments as cross-tenant rows. Roll forward, not back.

## [0.7.0] - 2026-04-18

Track C release. Lands the temporal-fields + sweep + expiry-control surface on top of
Track A (lifecycle overlay) and Track B (visibility filter + HNSW exclusion) from 0.6.0.
Every task (C1–C6) went through Codex CLI review; all HIGH findings on races,
boundary semantics, and TOCTOU were addressed before sign-off.

### Added

- **`memory.add(_batch)` accepts `expires_at_ms`, `review_after_ms`, and `mode`** (C1).
  Temporal overlay fields are persisted via `PersistentStore::add_chunk_with_lifecycle`
  in the same logical op as the chunk write, so retrieval visibility (C2) and the sweeps
  (C3/C4) see them on the next read. `mode` is accepted now for Track E forward-compat.
- **`memory.set_expiry` MCP tool** (C6). Atomic out-of-band update of a chunk's
  `expires_at_ms` / `review_after_ms`. JSON triple-state: field absent → leave, `null` →
  clear, integer → set. Refuses deleted / unknown / cross-tenant chunks via a guarded
  SQL `UPDATE` with rowcount check, so `{"updated": true}` is a load-bearing claim.
- **`ExpirySweep`** (C3) — compaction-level sweep that promotes rows past their
  `expires_at_ms` to `status=Expired`. Uses a race-safe `mark_expired_if_final` that
  re-checks both the status and retention predicate at UPDATE time.
- **`HistoryPromotion`** (C4) — compaction-level job that demotes long-stale
  Superseded/Expired rows to `MemoryTier::History` based on the overlay-idle clock
  (`lifecycle_updated_at_ms`, NOT `timestamp_created`). Uses a guarded
  `promote_to_history_if_stale` with three predicates re-checked at UPDATE time.
- **Wired into `CompactionRunner::run_compaction`** (C5). Both sweeps run before the
  excluded-ID gather so their transitions flow into Track B2's HNSW-rebuild exclusion.
  `CompactionResult` gains `expired_count` and `promoted_count`. `CompactionConfig`
  gains `expiry_sweep_enabled`, `history_promotion_enabled`, and `history_promotion_age_ms`
  (default 90 days). The runner owns a single centralised `tenant_memory_version` bump
  per cycle, narrowly scoped to C3/C4 overlay transitions.

### Changed

- **Lazy retrieval hiding matches the sweep boundary.** `list_expired_before` now uses
  `expires_at_ms <= now` (was `<`), mirroring `VisibilityPolicy::is_visible_at`.

### Notes

- `VisibilityPolicy::is_visible_at` was already in place in 0.6.0 and handles Track C2's
  lazy retrieval hiding. 0.7.0 adds end-to-end tests for the MCP path now that C1 writes
  the overlay field.
- `memory.set_expiry` guard chain: preflight-via-atomic-UPDATE with
  `AND status != 'deleted'` and tenant filter, so TOCTOU windows are closed.

### Test coverage

21 new tests in `crates/memd/tests/expiry_and_history.rs` plus two lib tests in
`crates/memd/src/compaction/` and one in `crates/memd/src/store/persistent.rs`. Race
guard paths are exercised directly (not just through the pre-filter) for both
`mark_expired_if_final` and `promote_to_history_if_stale`.

## [0.6.0] - 2026-04-18

Lifecycle release. Landed Track A of the nanomem-inspired features plan — atomic chunk
supersession, lifecycle overlay on `memory.get`, and the SQLite overlay columns that
underpin the remaining tracks (B–G). Addresses all five findings from Codex CLI's round-1
review of the merge (2 HIGH + 2 MEDIUM + 1 LOW).

### Breaking changes

- **`memory.get` response shape changed.** Pre-0.6 returned a bare `MemoryChunk | null`.
  0.6 returns a discriminated envelope so callers can distinguish "not found" from
  "hidden by visibility policy" and retrieve lifecycle metadata without a second round
  trip:
  - `{found: false}` — chunk does not exist for this tenant.
  - `{found: true, chunk, lifecycle, status}` — chunk is visible; payload + overlay
    included.
  - `{found: true, hidden: true, status, tier, hidden_reason}` — chunk exists but is
    hidden by the lifecycle visibility policy. `hidden_reason` is one of `superseded`,
    `expired`, `history`, or `error`. For `superseded`, `expired`, and `history` the
    caller can retry with the matching `include_*` flag to unhide; `error` has no
    include knob (it flags a corrupted / unrecoverable row). `deleted` rows never
    surface through `memory.get` at all — they return `{found: false}`.
  - Clients pinned to the old shape must update. The in-tree `evals/harness` conformance
    suite was updated in this release.

### Added

- **`memory.supersede` MCP tool.** Atomically replaces an existing chunk with a new one
  and records the supersession edge in a single SQLite transaction. Old chunk becomes
  `status=Superseded`, new chunk gets `supersedes = old_id` and the old chunk gets
  `superseded_by = new_id`. Preflight rejects missing / deleted / non-head `old_id`,
  detects pre-existing cycles in the `superseded_by` chain before touching disk, and
  rolls back compensating-tombstone-style if the link transaction fails after the new
  chunk is already written.
- **Lifecycle overlay on `memory.get`.** `include_superseded`, `include_expired`, and
  `include_history` flags control whether hidden rows surface with their payload. The
  response always advertises `hidden_reason` on a hidden envelope so agents can retry
  with the right knob.
- **Lifecycle metadata columns + access-path indexes on `chunks`.** Seven new
  columns added by the A3 migration: `tier`, `supersedes`, `superseded_by`,
  `expires_at_ms`, `review_after_ms`, `lifecycle_updated_at_ms`, `canonical_text`
  (`status` already existed). Four new indexes: `idx_chunks_expiry` (partial, on
  `expires_at_ms IS NOT NULL`), `idx_chunks_supersedes` (partial, on
  `supersedes IS NOT NULL`), `idx_chunks_tier_status` (full), and
  `idx_chunks_canonical` (partial, on `canonical_text IS NOT NULL`). Legacy rows
  migrate with safe defaults (`Final` / `LongTerm`).
- **`ChunkStatus::Superseded` and `ChunkStatus::Expired`** with fail-closed `FromStr`.
- **Visibility policy primitives.** `VisibilityPolicy` with `is_visible` / `is_visible_at`;
  `MemoryTier`, `LifecycleDelta` (triple-state clear semantics), `ResolvedChunk`.

### Fixed

- **HIGH-1: `memory.supersede` is now actually atomic.** Pre-fix, a crash between the
  new-chunk write and the `atomic_supersede` SQL transaction left an orphan replacement
  visible in retrieval with no supersession edge. Fix: `atomic_supersede`'s UPDATE now
  carries a `superseded_by IS NULL` guard (head-only at SQL level), and
  `supersede_chunk` rolls the orphan back via the full `Store::delete_chunk` path if
  the link transaction fails after the new chunk was persisted. That path appends a
  WAL delete record (so restart recovery cannot resurrect the orphan), marks the
  segment tombstone, removes the chunk from the hybrid / sparse / dense / tiered
  indexes, and invalidates the cache — leaving no trace of the failed write.
- **HIGH-2: `memory.get` wire envelope (see Breaking changes above).**
- **MEDIUM-3: hidden `memory.get` envelope omitted the hiding cause.** Callers now get
  `hidden_reason` ∈ {`superseded`, `expired`, `history`, `error`} with precedence
  matching `VisibilityPolicy::is_visible_at` exactly (status → tier → wall-clock
  expiry) so the flag it names actually unhides the row.
- **MEDIUM-4: `supersede_chunk` accepted any non-deleted `old_id`.** A double-supersede
  on the same chunk used to fork the graph with two live successors. Now enforced as a
  two-layer invariant: preflight rejects when `old_id.superseded_by` is already set;
  SQL-layer UPDATE filters on `superseded_by IS NULL` so a concurrent race past the
  preflight rolls back rather than overwriting.
- **LOW-5: cycle detection in `supersede_chunk`.** Replaced bounded 64-hop
  return-to-start walk with a `HashSet<ChunkId>` visited-set scan that catches any
  revisit at any depth. A 65-hop cycle, or a cycle that re-enters mid-chain, used to
  pass; both now fail.

### Testing

- Four new lifecycle tests: `supersede_chunk_rejects_double_supersede_on_same_old_id`,
  `supersede_chunk_detects_non_start_cycle_mid_chain`,
  `supersede_chunk_walks_long_chain_past_old_64_hop_bound`,
  `memory_get_hidden_envelope_carries_hidden_reason`.
- Full suite: 699 tests pass (up from 695 on the 0.5.0 merge base).

### Review

- Codex CLI round-1 review of the Track A merge (session
  `019da31a-5f94-7942-ab14-40d84012f7b5`): REQUEST_CHANGES, 5 findings. All addressed
  in this release and re-reviewed; artifact `019da326-b1b3-7262-8d68-3734fdcac6f7`
  in memd.

## [0.5.0] - 2026-04-18

Retrieval durability release. Two production-observed failure modes in the persistent store are
fixed, and a startup-time HNSW backfill closes the cold-start gap that left pre-crash data
semantically invisible. Every change was reviewed by Codex CLI; all flagged issues addressed
before cutting.

### Fixed

- **`get_chunk` no longer silently returns "not found" when the in-memory segment cache drifts.**
  Observed in production: after an 8-day daemon run, a freshly-added chunk had a valid metadata
  row (status=final, correct segment/ordinal) and its segment files existed on disk, yet
  `memory.get` returned `null` and semantic search returned no results. Root read: a missing
  entry in `tenant.segments` was being collapsed into `Ok(None)`. The read path now opens the
  segment on demand, emits a `warn!` for observability, repopulates the cache via
  `entry().or_insert()` (race-safe against concurrent rollover), and propagates a real
  `StorageError` on open failure instead of masking it as "chunk not found". The underlying
  registration race that produces the drift is not yet root-caused — the defensive fix is
  observable via the new log so the next recurrence is diagnosable.

### Added

- **HNSW startup backfill for cold tenants.** `DenseSearcher` persists HNSW state only on
  graceful shutdown (Drop / explicit shutdown); any non-graceful restart — crash, SIGKILL,
  systemd restart loop — left the in-memory dense index empty for every tenant on next boot
  while `load_segments()` still rehydrated segment readers. Reads worked but semantic search
  returned nothing for pre-crash data. New `PersistentStore::backfill_hnsw_for_cold_tenants`
  async method plus free-fn `run_hnsw_backfill` iterate each tenant's active metadata rows,
  filter via the new `DenseSearcher::contains_chunk` per-chunk membership test, and re-index
  the missing chunks in batches of 64 via `hybrid.index_batch` (or `dense.index_batch` when
  hybrid is off). New `PersistentStoreConfig::backfill_hnsw_on_startup` (default `true`, env
  override `MEMD_BACKFILL_HNSW_ON_STARTUP`) triggers a one-shot background task on the ambient
  Tokio runtime at `open()`. Non-blocking — the daemon starts serving immediately; semantic
  search on older chunks degrades until the task completes.
- **`DenseSearcher::contains_chunk`.** Per-chunk HNSW-mapping membership check. Codex-reviewed:
  count-based heuristics (`index_len >= active_count`) silently skip stale tenants because
  HNSW's `next_id` never decrements on delete, so a tenant with delete + re-add + crash can
  satisfy the count check while still missing live chunks.

### Testing

- Three new integration tests in `crates/memd/tests/bug_b_rollover_read.rs` (rollover/restart
  read round-trips) plus one white-box test `get_chunk_recovers_when_segments_cache_loses_entry`
  in the persistent-store tests module that deliberately removes a finalized reader from
  `tenant.segments` and asserts on-demand recovery plus cache repopulation.
- Four new backfill tests including
  `backfill_hnsw_detects_staleness_via_per_chunk_membership_not_counts`, which pins the
  delete-skew regression Codex flagged during review.
- Full suite: 623 lib + 30 integration tests pass.

### Review

- Codex CLI reviewed both fixes across multiple rounds: Phase 1 caught silent-`Ok(None)`,
  unsafe unconditional cache insert, and tests that did not exercise the new path. Phase 2
  caught the count-heuristic blind spot and the `LIMIT/OFFSET` race under concurrent writes
  (replaced with a single `metadata.list` snapshot). All flagged issues addressed before
  landing.

## [0.4.0] - 2026-04-16

This release is the product of a deep four-phase audit-and-remediation arc across safety, adoption,
concurrency, and durability. Every phase was independently reviewed by Codex CLI; all Codex-flagged
release-blocking issues were fixed before cutting.

### Security / correctness

- **Trust-boundary rewrite.** `derive_artifact_trust_tier` no longer honours agent-supplied
  `verification_status` / `approval_state` / `ArtifactKind::Verification` / non-empty `validation`
  as trust-promotion inputs. `PromotionState::Verified` — and thus `TrustTier::VerifiedRecord` — is
  reserved for artifacts explicitly promoted by `promote_if_countersigned`, which requires a
  distinct-writer countersignature (different `agent_id`) with `supports_claim = true`. The
  "trust-theater" channel where any single agent could self-label as verified is closed.
- **`artifact.verify` renamed to `artifact.find_related`.** The tool is a retrieval helper, not a
  trust primitive; the docs and name now reflect that. The legacy `artifact.verify` alias continues
  to work with a deprecation warning.
- **Digest forgery mitigation.** `artifact.create` rejects `artifact_kind = "digest"` — digests are
  server-generated and have deterministic IDs; accepting agent-authored digests let callers
  overwrite canonical `project_brief` / `failure_library` / etc. artifacts.
- **Explicit writer identity.** Removed the forgeable process-global `SESSION_DEFAULT_AGENT_ID`.
  Artifact writes must carry an explicit `agent_id` to participate in countersignature promotion;
  anonymous writes stay anonymous and cannot produce false `VerifiedRecord` trust.
- **Tenant isolation hardened.** The legacy cross-tenant `project_id` fallback is now opt-in via
  `server.allow_cross_tenant_project_fallback` (default `false`). `TenantId::validate` runs at
  every storage-path boundary. `README.md` was tightened to state explicitly that `tenant_id` is
  NOT an authentication boundary.
- **Persistence durability.** `SegmentWriter::flush_payload` (flush + `sync_data`) runs before
  every SQLite metadata commit. `next_segment_id` now scans all `seg_*` directories on disk so
  unfinalized orphans cannot be silently overwritten. `recover_from_wal` finalizes the active
  segment before WAL truncation so a second crash after recovery cannot strand metadata.
- **HNSW compaction is real.** `DenseSearcher::rebuild_hnsw_for_tenant` actually swaps the rebuilt
  Hnsw graph into the live `RwLock<Hnsw<...>>` and marks the excluded internal-ids invalid in the
  embedding cache, so save/load after compaction no longer resurrect removed points.
- **BM25 reopens correctly.** `Bm25Index::with_path` uses `Index::open_or_create` instead of
  `Index::create_in_dir`. Hybrid search no longer silently disables itself on every daemon restart.
- **MCP protocol compliance.** Stdio notifications no longer receive responses (JSON-RPC §4.1);
  HTTP notifications that error return 202 Accepted with an empty body; `ping` returns an empty
  object `{}` with the request id echoed. Dropped the unimplemented `text/event-stream`
  advertisement from the startup banner.
- **Structural code-nav.** `code.find_callers` resolves the caller via `caller_symbol_id` (was
  looking up the wrong field and returning garbage names for multi-hop). `link_callees` is
  implemented end-to-end with an ambiguity guard; error-recovery tree-sitter spans no longer
  pollute the symbol index.
- **Chunking.** Merge-too-small-final-chunk path uses `max(old_end, new_end)` and filters overlap
  sentences, so character offsets stay consistent and no text is duplicated.
- **Cross-encoder scoring.** Logits are interpreted by shape: 1-dim regression → sigmoid, 2-dim
  `[not_relevant, relevant]` → softmax (`[1]`), other shapes warn and fall back. Previously a 2-class
  export silently inverted ranking.

### Adoption

- **Default `tenant_id` resolution.** `tenant_id` is now optional on every tool. The resolver
  falls through explicit param → `$MEMD_DEFAULT_TENANT` → `~/.memd/default_tenant` file → literal
  `"default"`. Agents that don't know their tenant still land writes in a stable local tenant.
- **Shrunk required fields on `task.*`.** `task.start` requires only `{goal}`; `task.finish` only
  `{task_id}`; analogous shrinking for `task.progress`, `task.run_start`, `task.run_finish`,
  `task.add_evidence`. Previously-required fields like `motivation`, `hypothesis`,
  `scientific_question`, `confidence`, `supports_claim` became optional with sensible defaults.
- **Split `artifact.create` into focused tools.** Four new tools with tight 3-8 field schemas:
  `artifact.review`, `artifact.revision`, `artifact.decision`, `artifact.verification`. Each
  injects the appropriate `artifact_kind` server-side and rejects a conflicting override.
  `artifact.create` stays registered with a deprecation warning.
- **Search consolidated.** `memory.search` with `mode` is the primary retrieval surface; the SKILL
  guide leads with it. `context.search_context_documents` now logs a deprecation warning on use.
  `task.search` / `artifact.search` remain first-class through v0.5 because their output shapes
  differ meaningfully.
- **Cut write amplification.** New `build_task_projections_minimal` emits 1 canonical + 1 summary
  projection. `task.progress` and `task.add_evidence` use the minimal path by default. `task.start`
  / `task.finish` / `task.run_*` keep the full fanout because their kind-specific projections
  carry tool/command content that filter paths rely on.

### Concurrency / performance

- **HTTP request concurrency unlocked.** Dropped the outer `Arc<AsyncMutex<McpServer<S>>>`. The
  server is now shared directly as `Arc<McpServer<S>>` and every handler takes `&self`. Concurrent
  MCP requests (reads, writes, and mixes) no longer serialize on a single mutex.
- **Custom SQLite connection pool.** Replaced the single `Mutex<Connection>` with a bounded pool
  (`SqliteConnectionPool`, default 16, `$MEMD_SQLITE_POOL_MAX` to override). Concurrent readers
  parallelise naturally; writers serialise at SQLite's own WAL-mode locking. Sidesteps the
  `r2d2_sqlite` / `libsqlite3-sys` version conflict with `rusqlite 0.38`.
- **Unified per-tenant `memory_version`.** The warm-tier version counter is now actually bumped on
  every add/delete via `HybridSearcher::bump_tenant_memory_version`. The semantic cache invalidates
  correctly on writes for the first time.
- **Tombstone visibility.** `SegmentReader.tombstones` is now `Arc<RwLock<TombstoneSet>>`;
  `mark_deleted` takes `&self`. Delete callers dropped from `segments.write()` to `segments.read()`
  so concurrent readers on other segments (and even on the same segment's payload) don't block.
- **Background digest sweeper.** Phase 3.4's writer-side dirty tracker is now drained on a timer
  (`$MEMD_DIGEST_SWEEP_INTERVAL_SEC`, default 10s). `memory.compact` remains a manual hook.
  Sweeper re-marks keys on regeneration failure so transient errors retry.
- **Writer-side digest invalidation.** `task.add_evidence`, `task.finish`, `artifact.create` (by
  kind, plus the validation-bearing cross-family case) mark the affected (tenant, project, role)
  digest scopes dirty via a new `task_memory::digest_dirty` tracker. Reader-path regen still works
  as a defensive fallback.

### Observability

- **Rejection metrics.** `MetricsCollector::record_rejection(tool, reason)` surfaces per-tool /
  per-reason rejection counts via `memory.metrics`. Every failing tool call is now visible in
  aggregate rather than lost to the individual error response.

### Tests

- 618 library tests, 7 HTTP integration tests, 624 with the `cross-encoder-reranker` feature (5
  ignored, requiring the real ONNX runtime). Phase 4 added a black-box HTTP integration harness
  that spawns real daemons against tempdirs and exercises the full lifecycle (initialize → task.* →
  artifact.* focused tools → memory.search → task.resume → concurrent read/write).

### Deferred to future releases

- Real session identity (cryptographic / OAuth writer attribution). Today's agent_id is explicit
  but unauthenticated.
- Full streamable-HTTP / SSE for long-running tools. `memory.compact` + `artifact.find_highlights`
  are the natural candidates.
- `hnsw_rs` → `usearch` migration. The current `ArcSwap`-style swap handles v0.4 needs; a
  native-delete index could simplify the pipeline further.

## [0.3.0] - 2026-04-10

### Added
- Explicit trust-boundary metadata on search and digest-style MCP responses.
- `artifact.verify` for grounding claims against canonical artifacts.
- Structural runtime wiring so `code.find_*` tools are initialized in normal server startup.
- A checked-in structural benchmark fixture for the eval harness.

### Changed
- The compiled wiki prototype now renders trust tiers and grounding links when current memd metadata is available.
- The packaged memd skill binary has been rebuilt from the current release tree.

### Fixed
- Structural benchmark runs no longer depend on a missing fixture or an uninitialized structural index.
- The local shared HTTP daemon can now be restarted from the current installed binary to expose the latest shipped behavior.

## [0.2.0] - 2026-04-01

### Added
- Shared local HTTP daemon support for multi-session MCP access.
- Structured task and artifact workflows for progress tracking, evidence capture, review, and thread-level collaboration.
- Summary-first project briefs, task resumes, and failure, decision, evidence, and highlight digests.
- Additional MCP tools for context retrieval, structural code queries, and debug inspection.

### Changed
- Retrieval can widen by `project_id` across older local tenant histories when needed.
- The release surface and bundled skill assets are aligned with the current `main` branch.

### Fixed
- Search-style retrieval now skips unreadable finalized chunks instead of aborting on CRC-related storage errors.
- The packaged Linux skill binary has been refreshed to the current release build.
