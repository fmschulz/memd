# Record Operational Facts

Use `memd add` after independently verifying an operational fact that has no
home in a readable repository. Include the affected machine or scope, the
observation, its evidence, and an action for the next agent. For example, set
`OPERATIONAL_FACT` to that verified observation before running:

```bash
memd add \
  --chunk-type summary \
  --tags kind:evidence \
  --text "$OPERATIONAL_FACT"
```

Record implementation decisions, test output, and task completion in `tasks/`
or a handoff. The CLI supports structured task history for workflows that
explicitly choose it; this skill does not require duplicating repository state.
When the reusable unit is a failed approach, checked repair, and conditional
lesson, keep the complete evidence in the repository and record an
[experience case](experience.md) that links to it.

For an event-time memory, such as an incident or deploy, store the event time
(ms since epoch) so recall can render it.
Never bake dates into the text itself (they pollute retrieval); pass
`event_time_ms` instead. JSON surface only (`call` / `batch`):

```bash
memd call memory.add \
  --json '{"type":"message","text":"Remote mount /mnt/research became read-only after the host restarted.","event_time_ms":1749168000000,"tags":["kind:evidence"]}'
```

The same field works per-line in `memd batch` (`memory.add` /
`memory.add_batch` arguments). The event time is independent of when the memory
is written.

### Preserve physical write identities

Long inputs can split into several physical chunks. `memd add` and
`memory.add` return the backward-compatible primary `chunk_id` plus the full,
ordered `stored_chunk_ids` list. Preserve the full list when later retrieval,
supersession, or outcome attribution needs exact identities. Likewise,
`memory.supersede` returns `new_stored_chunk_ids` for every replacement child.

`memory.add_batch` returns one primary ID per logical input but not its split
children. Use individual `memory.add` calls when complete physical attribution
matters.
