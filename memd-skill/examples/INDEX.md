# memd Example Index

These examples use the skill + CLI workflow.

## Task Lifecycle Across Sessions

File: [session_tracking.md](session_tracking.md)

Use this when one agent starts work and another resumes later.

Key commands:

- `memd agent-context`
- `memd search`
- `memd add --chunk-type summary --tags kind:progress`
- `memd add --chunk-type trace --tags kind:run`
- `memd add --chunk-type research --tags kind:evidence`
- `memd add --chunk-type summary --tags kind:finish`

## Cross-Agent Experiment and Decision Tracking

File: [decision_tracking.md](decision_tracking.md)

Use this when multiple agents compare what worked, what failed, and why.

Key commands:

- `memd search --mode find-decisions`
- `memd search --mode find-evidence`
- `memd add --chunk-type decision --tags kind:decision`
- `memd add --chunk-type research --tags kind:evidence`

## Codebase Indexing with Task Tracking

File: [codebase_indexing.md](codebase_indexing.md)

Use this when you are storing source or documentation chunks and still want the
indexing job itself to be recoverable later.

Key commands:

- `memd add --chunk-type code`
- `memd add --chunk-type doc`
- `memd add --chunk-type summary --tags kind:progress`
- `memd search`

## Choosing Chunk Types

Use `summary` for task progress and finishes; routine `kind:progress`
summaries are short-lived by default, while `kind:finish` or explicit
`priority:N` marks reusable outcomes.
Use `trace` for commands, runs, logs, and tool outputs.
Use `research` for evidence and analysis.
Use `decision` for explicit choices and rationale.
Use `code` or `doc` for raw indexed source material.
