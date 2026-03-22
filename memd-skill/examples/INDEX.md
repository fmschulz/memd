# memd Example Index

These examples show how to use `memd` as a shared knowledge-artifact memory system.

## Example 1: Task Lifecycle Across Sessions

File: [session_tracking.md](session_tracking.md)

Use this when:

- one agent starts work and another agent resumes it later
- you want consistent reporting of motivation, runs, failures, and outcomes
- you need `task.get` and `task.search` to reconstruct prior work

Key tools:

- `task.start`
- `task.progress`
- `task.run_start`
- `task.run_finish`
- `task.add_evidence`
- `task.finish`
- `task.get`
- `task.search`

## Example 2: Cross-Agent Experiment and Decision Tracking

File: [decision_tracking.md](decision_tracking.md)

Use this when:

- multiple agents are iterating on the same scientific or engineering question
- you need to compare what worked and what failed
- you want evidence and rationale to survive handoffs

Key tools:

- `task.search` with exact filters
- `task.add_evidence`
- `task.finish`
- `memory.search` for broader context

## Example 3: Codebase Indexing with Task Tracking

File: [codebase_indexing.md](codebase_indexing.md)

Use this when:

- you are indexing a repository into raw `memory.*` chunks
- you still want the indexing job itself tracked as a task artifact
- later agents need to know what was indexed, with which parameters, and what coverage gaps remain

Key tools:

- `task.start`
- `task.run_start`
- `memory.add_batch`
- `task.run_finish`
- `task.finish`

## Choosing Between `task.*` and `memory.*`

Use `task.*` when the work has:

- a goal
- motivation
- a hypothesis or question
- runs and parameters
- evidence
- outcomes

Use `memory.*` when the content is a raw chunk such as:

- source code
- documentation
- codified context
- ad hoc notes

In practice, many workflows use both:

1. record the task lifecycle with `task.*`
2. store raw source artifacts with `memory.add` / `memory.add_batch`
3. query structured history with `task.search`
4. query broad context with `memory.search`
