# Trust boundary

`memd` separates retrieved candidates, canonical records, and recorded
verification claims. These are evidence categories inside a trusted local
store. They do not authenticate the identity of an agent or reviewer.

## Retrieval and canonical records

Search and digest helpers return candidates. Canonical artifacts preserve
the claims, references, and provenance that a consumer can inspect. Digests
are derived summaries and can become stale. `artifact.find_related` finds
overlapping evidence; a retrieval hit does not establish a claim.

The general artifact workflow permits promotion to `VerifiedRecord` when a
verification record supplies a different `agent_id` from its parent and sets
`supports_claim = true`. Those IDs are caller supplied. This rule records a
claimed independent review; it cannot establish that two distinct people or
agents performed it. Consumers must assess the evidence and its origin.

## Experience checks

An explicit `experience check` runs the supplied argv in the requesting CLI
and records the exit result, timing, output hashes, and source fingerprints.
A supporting receipt requires a successful exit, complete output hashes,
and present, equal before/after fingerprints. This establishes only the
recorded check result over the covered source.

The lower-level `experience.record_check` API accepts caller-reported
historical receipts. Imported receipts are also reports: import does not
rerun the command or authenticate its origin. Experience records always retain
`Canonical` promotion state. Import rejects envelopes that try to attach
verification or approval authority to them.

Hostnames, environment-supplied native IDs, model names, and hook fields are
provenance observations. They are useful for tracing work, but are not
credentials or signatures. Read the [experience model](experience-memory.md)
for source coverage, command cleanup, and transfer limits.

## Local security posture

- The warm worker creates its Unix socket in a `0700` runtime directory and
  chmods the socket file to `0600` before accepting connections.
- Embedding model and tokenizer downloads for all-MiniLM-L6-v2 and
  Qwen3-Embedding-0.6B are pinned to immutable Hugging Face commit revisions
  and verified against compiled-in SHA-256 digests.
- Corrupted or tampered embedding model/tokenizer files are rejected and never
  loaded.
- CLI writers share a data-directory writer lock. This coordinates processes;
  it does not authenticate the claims they write.
- Explicit check commands run with the caller's local permissions. The
  executor is not a sandbox, and stored argv must not contain secrets.

See the [task memory schema](scientific-task-memory/schema/README.md) for the
full canonical-artifact envelope and how trust tiers are persisted.
