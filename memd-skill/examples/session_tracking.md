# Task Lifecycle Across Sessions

This example shows how one agent can start work and another agent can later
recover the useful history with the `memd` CLI.

Tenant: `ecommerce-api`
Project: `auth`
Task tag: `jwt-auth`

## Session 1: Retrieve First

```bash
memd agent-context \
  --tenant-id ecommerce-api \
  --project-id auth \
  --query "JWT authentication key rotation prior work" \
  --k 2 \
  --token-budget 700 \
  --format markdown \
  --output .memd/context.md \
  --log-dir .memd/search-logs
```

## Session 1: Record Progress

```bash
memd add \
  --tenant-id ecommerce-api \
  --project-id auth \
  --chunk-type summary \
  --tags kind:progress,task:jwt-auth \
  --text "Mapped auth middleware and token issuance touchpoints. Symmetric-key design would complicate service-to-service trust boundaries. Next step: prototype RS256 issuance and validation."
```

## Session 1: Record a Run

```bash
memd add \
  --tenant-id ecommerce-api \
  --project-id auth \
  --chunk-type trace \
  --tags kind:run,task:jwt-auth,tool:cargo-test,status:failed \
  --text "cargo test auth::jwt -- --nocapture: 7 tests passed, 1 expiration edge case failed. The failure appears when local timezone offsets are mixed with UTC claims."
```

## Session 1: Record Evidence

```bash
memd add \
  --tenant-id ecommerce-api \
  --project-id auth \
  --chunk-type research \
  --tags kind:evidence,task:jwt-auth,supports:true \
  --text "The expiration failure reproduces only when local offsets are mixed with UTC claims; UTC normalization fixes the failing case."
```

## Session 2: Resume

```bash
memd search \
  --tenant-id ecommerce-api \
  --project-id auth \
  --query "JWT auth expiration UTC evidence" \
  --compact \
  --token-budget 2000 \
  --format markdown
```

## Finish

```bash
memd add \
  --tenant-id ecommerce-api \
  --project-id auth \
  --chunk-type summary \
  --tags kind:finish,task:jwt-auth \
  --text "Implemented RS256 JWT validation with UTC-normalized expiration checks. Validation: cargo test auth::jwt passed. Remaining risk: deployment key-rotation runbook still needs review."
```
