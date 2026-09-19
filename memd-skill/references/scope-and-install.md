# Tenant and Project Scope

For one trusted machine or trust domain, prefer one stable shared tenant and use
`project_id` for narrower retrieval. Avoid per-session tenant names unless the
work requires isolation.

If `.memd/project_scope.json` exists, use its pinned `tenant_id` and
`project_id` instead of guessing from the directory.

`memd call` and `memd batch` inherit both fields only when the JSON request
omits `tenant_id`. An explicit JSON `tenant_id` is intentionally tenant-wide
unless that request also includes `project_id`. Scope is resolved before warm
worker routing. A malformed or unreadable scope file fails closed for an
unscoped request; `batch --continue-on-error` preserves a per-line failure
receipt while explicitly scoped lines continue.

Initialize a repository:

```bash
memd init --tenant-id "$TENANT_ID" --project-id "$PROJECT_ID"
```

This writes `.memd/memory_guardrails.md`, `.memd/tenant_scope.json`, and
`.memd/project_scope.json`, and can upsert CLI guardrail blocks into local
`AGENTS.md` and `CLAUDE.md`.

Automatic session startup does not require `memd init`. It reuses a valid
project or legacy scope, or creates `.memd/project_scope.json` when neither
provides one. Let memd write this file; partial JSON fails to parse. See
[Automatic session-start](session-start.md#automatic-session-start).
Run `memd init` only when you want the full guardrail suite for a repo.

## Verify the install

Install memd 1.8.0 or later for `memd experience` and detailed consolidation
review. See [Experience cases](experience.md).

```bash
memd doctor
```

Reports binary path/version, data directory, global agent rules (Claude,
Codex, Cursor), the Claude `SessionStart` hook, and the current project's
`.memd` scope. Use `--format json` for machine-readable output.

`memd doctor --strict` exits non-zero when any check fails. Use it in scripts.
On a fresh store, the data dir and project scope checks read as failing until
your first `session-start`. For store-content health (rejected writes, hit-rate,
noise), run `memd report --strict`.
