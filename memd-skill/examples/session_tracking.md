# Task Lifecycle Across Sessions

This example shows how one agent can start work and another agent can later recover the full task history from `memd`.

## Scenario

Tenant: `ecommerce-api`

Goal: implement and validate JWT authentication.

The important part is not just storing random notes. The important part is that every agent emits the same artifact types, so later agents can recover:

- the motivation
- the hypothesis
- the parameters used in runs
- what already failed
- what evidence supported the decision

## Session 1: Start the Task

```json
{
  "name": "task.start",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "project_id": "auth",
    "goal": "Implement JWT authentication with key rotation support",
    "motivation": "The API needs stateless auth and auditability across services",
    "hypothesis": "RS256 with short-lived access tokens and refresh tokens will satisfy security and operability requirements",
    "scientific_question": "Which JWT design gives acceptable security without excessive operational burden?",
    "dataset_refs": [
      {"name": "auth_requirements", "version": "2026-03"}
    ],
    "expected_outputs": [
      "jwt service implementation",
      "validation summary",
      "deployment notes"
    ]
  }
}
```

## Session 1: Record a Meaningful Checkpoint

```json
{
  "name": "task.progress",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "task_id": "<task_id>",
    "project_id": "auth",
    "summary": "Mapped the current auth middleware and identified token issuance touchpoints",
    "blockers": [
      "Key storage strategy is still undecided"
    ],
    "failed_attempts": [
      "A symmetric-key design would complicate service-to-service trust boundaries"
    ],
    "next_step": "Prototype RS256 issuance and validation flow"
  }
}
```

## Session 1: Record a Run

Before the substantive run:

```json
{
  "name": "task.run_start",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "task_id": "<task_id>",
    "project_id": "auth",
    "tool_name": "cargo-test",
    "command": "cargo test auth::jwt -- --nocapture",
    "why_chosen": "Need fast feedback on token issuance and expiration behavior",
    "parameters": {
      "module": "auth::jwt"
    },
    "inputs": [
      "src/auth/jwt.rs",
      "tests/auth_jwt.rs"
    ],
    "summary": "Validate initial JWT implementation"
  }
}
```

After the run:

```json
{
  "name": "task.run_finish",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "task_id": "<task_id>",
    "project_id": "auth",
    "status": "completed",
    "tool_name": "cargo-test",
    "command": "cargo test auth::jwt -- --nocapture",
    "outputs": [
      "7 tests passed",
      "1 expiration edge case failed"
    ],
    "metrics": {
      "tests_passed": 7,
      "tests_failed": 1
    },
    "notes": "The timezone normalization edge case still fails",
    "validation": [
      "Token signing works",
      "Expiration handling still needs normalization fixes"
    ]
  }
}
```

## Session 1: Record Evidence

```json
{
  "name": "task.add_evidence",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "task_id": "<task_id>",
    "project_id": "auth",
    "summary": "The expiration failure only appears when local timezone offsets are mixed with UTC claims",
    "evidence_kind": "test_failure",
    "supports_claim": true,
    "metric_name": "failed_case_count",
    "metric_value": 1
  }
}
```

## Session 2: Another Agent Resumes Work

The new agent should search first, then inspect the canonical history.

```json
{
  "name": "task.search",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "query": "JWT timezone expiration failure",
    "k": 10,
    "filters": {
      "project_id": "auth"
    }
  }
}
```

Then:

```json
{
  "name": "task.get",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "task_id": "<task_id>"
  }
}
```

That second agent can now recover:

- why JWT was chosen
- what was already tested
- which edge case failed
- what evidence supports the bug hypothesis
- what next step was planned

## Session 2: Finish the Task

```json
{
  "name": "task.finish",
  "arguments": {
    "tenant_id": "ecommerce-api",
    "task_id": "<task_id>",
    "project_id": "auth",
    "what_worked": [
      "RS256 token issuance and validation passed",
      "UTC normalization fixed expiration handling"
    ],
    "what_failed": [
      "The initial implementation mixed local server time and UTC claims"
    ],
    "validation": [
      "All auth::jwt tests passed after normalization",
      "Refresh flow remained backward compatible"
    ],
    "uncertainty": [
      "Operational key rotation still needs a production rollout checklist"
    ],
    "followups": [
      "Document key rotation procedures",
      "Add integration tests for multi-service validation"
    ],
    "confidence": 0.88
  }
}
```

## Why This Pattern Works

This pattern enforces consistent reporting. Different agents can use the same tenant and still recover a shared, structured picture of:

- intent
- rationale
- runs
- parameters
- evidence
- failures
- outcomes

That is the whole point of the knowledge artifact schema.
