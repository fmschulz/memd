# Codebase Indexing with Task Tracking

This example stores source/documentation chunks and records the indexing job
itself with CLI memories.

Tenant: `repo-memory`
Project: `payments-api`
Task tag: `indexing`

## Record Indexing Intent

Indexing intent is short-lived by default; tag verified indexing results as
`kind:evidence` or `kind:finish` if later agents should keep them.

```bash
memd add \
  --tenant-id repo-memory \
  --project-id payments-api \
  --chunk-type summary \
  --tags kind:progress,task:indexing \
  --text "Index payments-api source and docs so later agents can find payment routing, retry, and idempotency context."
```

## Add Source Chunks

```bash
memd add \
  --tenant-id repo-memory \
  --project-id payments-api \
  --chunk-type code \
  --tags ctx:file:src/payments/router.rs,lang:rust \
  --source-path src/payments/router.rs \
  --text "$(sed -n '1,220p' src/payments/router.rs)"
```

```bash
memd add \
  --tenant-id repo-memory \
  --project-id payments-api \
  --chunk-type doc \
  --tags ctx:file:docs/payments.md \
  --source-path docs/payments.md \
  --text "$(sed -n '1,220p' docs/payments.md)"
```

For larger repositories, wrap this pattern in a script and keep chunks bounded
by file section or symbol.

## Record Coverage

```bash
memd add \
  --tenant-id repo-memory \
  --project-id payments-api \
  --chunk-type summary \
  --tags kind:finish,task:indexing \
  --text "Indexed payment router and payments docs. Coverage gap: retry worker and reconciliation jobs still need indexing."
```

## Search Later

```bash
memd search \
  --tenant-id repo-memory \
  --project-id payments-api \
  --query "payment idempotency retry routing" \
  --compact \
  --token-budget 2000 \
  --format markdown
```
