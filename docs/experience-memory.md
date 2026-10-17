# Experience memory

An experience case connects a problem to attempted repairs and their check
results. A supported lesson states where the repair applies. Cases use
memd's canonical task-artifact store and give later sessions a concise account
to inspect. Repository files hold source code, full logs, plans, and handoffs.

The experience commands require memd 1.8.0 or later. `experience note` needs
memd 1.9.0 or later. See the [worked procedure](experience-how-to.md)
and [CLI reference](cli-reference.md#experience).

There are two ways to record a case. `experience note FILE` turns one case file
into the whole record in a single call, and is the normal way for an agent
that has solved a problem. The step-by-step commands (`record`, `check`) are
the audit path: they let memd run the verifying command itself.

## A case preserves the reasoning trail

<figure class="memd-figure">
--8<-- "docs/figures/experience-case.svg"
<figcaption>One case: a problem, the attempts with their checks, the lesson that a passing check supports, and a later correction.</figcaption>
</figure>

Each attempt names its problem and preceding attempt. A check names the exact
attempt it tested. A lesson names a supporting check and states its symptom
terms, machine, tool, and version constraints. A correction supersedes a
lesson without deleting the earlier record. Competing corrections remain
visible as a conflict; recall does not choose between applicable conflicting
lessons.

`experience get` preserves failed checks in history. The general `task.resume`
view takes current blockers and follow-ups from the latest task-state snapshot,
using observation time when available. An old failure remains inspectable
without becoming a current blocker again.

## A case file records a solved problem in one call

`experience note case.json` reads a JSON case file (or `-` for stdin) and stores
a problem, one attempt for each failed approach and one for the fix, a check
on the fix, and a lesson. The same file is the exchange format for tools that
carry cases between machines.

```json
{
  "problem": "uv cache is read-only in the Codex sandbox",
  "symptoms": ["uv", "cache", "read-only", "os error 30"],
  "failed": ["retry with the default cache"],
  "fix": "set UV_CACHE_DIR to a turn-local directory",
  "check": {"method": "observed", "result": "uv run returned 0", "command": "uv run ..."},
  "applies_when": {"tool": "uv", "sandbox": "codex workspace-write"},
  "evidence": ["docs/handoffs/2026-10-17_x.md"],
  "visibility": "shared"
}
```

| Field | Rule |
| --- | --- |
| `problem` | Required, 1 to 300 characters. |
| `symptoms` | Required, 1 to 12 distinctive words or phrases, each up to 80 characters. They drive recall. |
| `failed` | Optional, up to 8 approaches that did not work, each up to 300 characters. |
| `fix` | Required, up to 1000 characters. It becomes the lesson guidance. |
| `check` | Required. `method` is `observed` or `executed`; `result` is up to 300 characters; `command` is optional; `receipt_id` is optional and only valid with `executed`. |
| `applies_when` | Optional, up to 8 keys (`^[a-z][a-z0-9_]{0,31}$`) with values up to 80 characters. `tool`, `version`, and `machine` become exact recall conditions, and `version` needs `tool`. Other keys (`os`, `sandbox`, `harness`, ...) are stored on the lesson as `context`; recall shows them and does not filter on them. |
| `evidence` | Optional, up to 8 paths or URLs, each up to 300 characters. They are stored as `links` and never copied. |
| `visibility` | `shared` (default) or `project`. |

The file is strict: an unknown field is an error, an explicit `null` for an
optional field (`check.command`, `check.receipt_id`, `failed`, `applies_when`,
`evidence`, `visibility`) is an error, and the body may not exceed 32 KiB. `--emit` validates the file and prints the canonical JSON without
opening a store, so it needs no tenant, project, or writable directory.
`--shared-id ID` stores `agentdash:ID` in the provenance of every record it
writes. A call is a replay only when it repeats a stored submission. With an
ID, that is a case that carries the same ID; the content is not compared and a
different ID always makes a new case. Without an ID, it is a case in the same
project with the same problem, symptoms, fix, check, and `applies_when`
(`failed`, `evidence`, and `visibility` are not compared). A replay returns the
stored case (`"replay": true`) and writes nothing. The output names
`problem_id`, `attempt_ids`, `check_id`, `lesson_id`, and `evidence`.

### Observed and executed evidence

memd does not run the command in a case file, so a check there is the agent's
report. The lesson is labelled by how its check was established:

- `evidence: "executed"`: a check receipt that `experience check` produced by
  running the command, as described below.
- `evidence: "observed"`: an agent-reported check (`observed_check` in `get`
  output, with status `observed`). It stores the result and optional command.
  It has no exit code, no output hashes, and no source fingerprint, so it
  never counts as a verified success.

A case with `method: "executed"` and a `receipt_id` must name a passing receipt
that is already in this store. The lesson is added to that receipt's problem
and reads `executed`; the file's `failed` list is not recorded again, because
that problem already has its attempts. Other guidance, another receipt, or
another shared ID adds a further lesson to that problem. A case with
`method: "executed"` and no receipt is only the agent's word, so it is stored
as observed.

`find` and `get` show the label on each lesson (`evidence`). Treat an
observed lesson as a lead to recheck.

## Provenance belongs to the caller

Before dispatching to a warm worker, the CLI captures the calling directory,
host, Git revision and dirty state, and available native session, thread,
harness, and reported model fields. Missing values stay absent. It does
not infer a model from a default configuration or read a transcript to discover
identity. Native session IDs remain distinct from semantic task IDs.

The optional native session-start adapter can also return agent and parent IDs
from its hook payload. Those fields are not automatically bound to later CLI
writes. Pass a repository evidence link in `provenance.path`, `uri`, `repo`,
or `commit`; CLI enrichment preserves these fields while setting
`provenance.execution` from the current caller.

Search episodes carry the requesting thread even when several sessions share
one warm worker. Structured writes carry execution context in
`provenance.execution`. Check receipts also carry their execution context and,
where available, a digest of the tracked Git diff. Environment values and hook
payloads report identity; they do not authenticate it.

## What a check proves

`experience check` runs the explicitly supplied program in the requesting
CLI process. The worker stores the receipt. The receipt includes argv, working
directory, start and finish times, exit code, timeout status, output hashes,
and source fingerprints. Standard output and standard error are hashed as
streams; their text is not stored. Arguments and paths are stored, so secrets
must not appear in them.

A receipt supports a lesson only when the command exits successfully, both
output hashes are complete, and the before/after source fingerprints are
present and equal. Without explicit evidence paths, the fingerprint covers
the Git revision and tracked diff. Untracked files require `--evidence`;
explicit evidence covers only the named files. A successful command with
unknown or changed source receives `source_unverified` status.
Matching snapshots do not detect a change that was made and reverted between
them.

The receipt establishes the recorded process result and stability of the
covered source. Whether the check was sufficient, and whether its result
supports the guidance, still requires judgment. The lower-level
`experience.record_check` operation
accepts caller-reported historical receipts. Imports preserve those reports
without rerunning checks. Experience artifacts remain `Canonical`, and never
gain `VerifiedRecord` trust merely because a receipt says `passed`.

The command deadline includes process completion and output collection.
On Unix, timeout and handled interruption terminate the verifier's process
group. A descendant that deliberately leaves that group, or a client killed
with `SIGKILL`, is outside that cleanup guarantee. This executor is not a
sandbox.

## Recall checks applicability before returning guidance

`experience find` searches current lessons in the requested tenant. Symptom
matching uses lexical overlap after removing common words. Machine,
tool, and version conditions use exact string equality. The CLI defaults the
target machine to its own hostname; a missing required tool or version causes
an explicit abstention. Unspecified lesson conditions impose no restriction.

Lessons from the requested project always qualify. Lessons from another
project qualify when the lesson has `visibility: "shared"`; pass
`--project-only` to leave them out. Tenant boundaries still apply.
`experience get` reads a case from another project when the case has a current
shared lesson; any other case needs its own project. The other project sees
only the shared lessons and corrections and the checks that support them. It
also gets the problem, every attempt, and the other checks with their
receipts, so share only guidance that suits that.
Recall returns `matches`, or `abstain` with `no_match`,
`unknown_required_condition`, or `conflict`. It does not run stored commands.
Ordinary hybrid `search` can still return projected case text as a candidate;
use `experience find` when applicability and correction handling are required.

## Reuse and success are separate observations

The passive Codex outcome scanner records `observed_used` when it recognizes a
retrieved item in a later tool action. That observation carries no success
credit and does not improve retrieval ranking. It uses the action's recorded
time and avoids counting the same action again when a transcript grows.
Recognizable legacy scanner acceptances remain in history but are excluded
from outcome priors.

An explicit task outcome can still name the memories that affected a verified
result. The quality of that attribution depends on the caller's evidence.
Retrieval counts, reuse counts, and successful checks answer different
questions; none alone establishes that memory improved an agent's work.

## Case transfer

A case file (above) is also the unit for an external exchange, such as a shared
service that stores cases for many machines. The service holds its own copy;
`experience note FILE --shared-id ID` keeps a local memd copy that carries the
service's ID in its provenance. memd has no code for any particular service.

The versioned `memd-experience` bundle preserves canonical artifact IDs,
timestamps, provenance, attempts, checks, lessons, and corrections. Import
validates the causal history, scope, envelope, and collisions before writing.
An exact replay inserts nothing. Search projections are regenerated and may
have different chunk IDs; retrieval episodes and outcome events are not part
of this bundle. OMF remains the separate interchange for raw memory chunks.

Import does not retarget tenants or projects, execute argv, authenticate the
origin, or promote trust. A storage failure during a multi-artifact import may
leave a partial import; replaying the same bundle resumes it. Keep a local
store on each machine and transfer bundles explicitly. Continuous replication
and automatic merge of divergent histories are not implemented.

The [validation notebook](https://github.com/fmschulz/memd/blob/main/evals/bench/experience-memory/validation.ipynb)
records execution of deterministic cases, real CLI commands, and transfer
between two stores on one machine. Those checks establish the tested
behaviors, not an improvement in open-ended agent accuracy or latency.
