#!/usr/bin/env python3
"""Seed prior task experience for the multi-turn token-savings benchmark."""

from __future__ import annotations

import json
import urllib.request


URL = "http://127.0.0.1:8787/mcp"
TENANT_ID = "bench_mt_tokens"
PROJECT_ID = "timezone_boundary"
EXPERIENCE_ID = "mt-timezone-boundary-v1"
EXPERIENCE_ID_V2 = "mt-timezone-boundary-v2"
PAGINATION_PROJECT_ID = "pagination_cursor"
CACHE_PROJECT_ID = "cache_key_scope"
SCHEMA_PROJECT_ID = "schema_defaults"
STREAM_PROJECT_ID = "stream_backpressure"
PAGINATION_EXPERIENCE_ID = "mt-pagination-cursor-v1"
CACHE_EXPERIENCE_ID = "mt-cache-key-scope-v1"
SCHEMA_EXPERIENCE_ID = "mt-schema-defaults-v1"
STREAM_EXPERIENCE_ID = "mt-stream-backpressure-v1"


def call_tool(name: str, arguments: dict) -> dict:
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    raw = urllib.request.urlopen(
        urllib.request.Request(
            URL,
            data=json.dumps(req).encode(),
            headers={"content-type": "application/json"},
        ),
        timeout=30,
    ).read()
    obj = json.loads(raw)
    if "error" in obj:
        raise RuntimeError(obj["error"])
    return json.loads(obj["result"]["content"][0]["text"])


def experience_exists(project_id: str, experience_id: str) -> bool:
    result = call_tool(
        "task.search",
        {
            "tenant_id": TENANT_ID,
            "filters": {"project_id": project_id},
            "query": experience_id,
            "compact": True,
            "k": 3,
            "token_budget": 1200,
        },
    )
    return experience_id in json.dumps(result)


def seed_experience(
    *,
    experience_id: str,
    version: str,
    project_id: str,
    goal: str,
    motivation: str,
    hypothesis: str,
    progress_summary: str,
    failed_attempts: list[str],
    evidence_summary: str,
    what_worked: list[str],
    followups: list[str],
) -> dict:
    if experience_exists(project_id, experience_id):
        return {
            "experience_id": experience_id,
            "project_id": project_id,
            "skipped": True,
        }

    start = call_tool(
        "task.start",
        {
            "tenant_id": TENANT_ID,
            "project_id": project_id,
            "agent_id": "seed_experiences",
            "goal": goal,
            "motivation": motivation,
            "hypothesis": hypothesis,
            "scientific_question": (
                "Can a later agent use this prior experience to avoid repeated "
                "diagnosis on a related fixture?"
            ),
            "expected_outputs": [
                experience_id,
                "root cause",
                "failed attempts",
                "repair rule",
                "verification command",
            ],
            "dataset_refs": [
                {"name": "memd-multiturn-token-savings", "version": version}
            ],
        },
    )
    task_id = start["task_id"]
    call_tool(
        "task.progress",
        {
            "tenant_id": TENANT_ID,
            "project_id": project_id,
            "agent_id": "seed_experiences",
            "task_id": task_id,
            "summary": progress_summary,
            "blockers": [],
            "failed_attempts": failed_attempts,
            "next_step": "Apply the repair rule and run python3 -m unittest -q.",
        },
    )
    call_tool(
        "task.add_evidence",
        {
            "tenant_id": TENANT_ID,
            "project_id": project_id,
            "agent_id": "seed_experiences",
            "task_id": task_id,
            "evidence_kind": "prior_experience",
            "summary": evidence_summary,
            "supports_claim": True,
            "metrics": {"experience_id": experience_id},
        },
    )
    call_tool(
        "task.finish",
        {
            "tenant_id": TENANT_ID,
            "project_id": project_id,
            "agent_id": "seed_experiences",
            "task_id": task_id,
            "status": "completed",
            "what_worked": what_worked,
            "what_failed": failed_attempts,
            "validation": ["python3 -m unittest -q"],
            "followups": followups,
            "confidence": 0.95,
        },
    )
    return {
        "experience_id": experience_id,
        "project_id": project_id,
        "task_id": task_id,
        "artifact_id": start["artifact_id"],
    }


def main() -> int:
    seeded = [
        seed_experience(
            experience_id=EXPERIENCE_ID,
            version="pilot1",
            project_id=PROJECT_ID,
            goal=(
                "Record prior debugging experience for timezone-boundary "
                "scheduler failures."
            ),
            motivation=(
                "Later benchmark agents should be able to retrieve a concise "
                "repair rule instead of rediscovering the same failed attempts."
            ),
            hypothesis=(
                "A one-hour boundary-time failure is solved by normalizing "
                "timezone-aware inputs exactly once."
            ),
            progress_summary=(
                f"Experience {EXPERIENCE_ID}: symptom signature is an event or "
                "reminder exactly one hour wrong around spring-forward or "
                "fall-back boundary tests. Failed attempts ruled out database "
                "ordering and string formatting. Root cause: an ISO timestamp "
                "that already carried a UTC offset was converted to UTC twice. "
                "Repair rule: Normalize to timezone-aware UTC at input boundary "
                "only, then perform reminder arithmetic on the normalized "
                "instant."
            ),
            failed_attempts=[
                "Database ordering was checked and was not causal.",
                "Formatting changes did not affect the failing assertion.",
            ],
            evidence_summary=(
                f"{EXPERIENCE_ID}: Normalize to timezone-aware UTC at input "
                "boundary only. Do not subtract the parsed offset manually "
                "before calling astimezone(timezone.utc), because that converts "
                "to UTC twice. Verification command: python3 -m unittest -q."
            ),
            what_worked=[
                (
                    f"{EXPERIENCE_ID}: Normalize to timezone-aware UTC at input "
                    "boundary only."
                ),
                (
                    "The prior failure was caused by manually subtracting an "
                    "offset and then also calling astimezone(timezone.utc)."
                ),
            ],
            followups=[
                "For transfer tasks, inspect timestamp normalization before broad rewrites."
            ],
        ),
        seed_experience(
            experience_id=EXPERIENCE_ID_V2,
            version="pilot2",
            project_id=PROJECT_ID,
            goal=(
                "Record prior debugging experience for a dispatch scheduler "
                "where timezone normalization is hidden behind policy and "
                "export-order symptoms."
            ),
            motivation=(
                "The harder transfer fixture should reward agents that retrieve "
                "failed-attempt history before rewriting unrelated policy, audit, "
                "or formatting code."
            ),
            hypothesis=(
                "Boundary export-order and reminder failures are solved by fixing "
                "one shared offset-bearing ISO parser and by using timedelta for "
                "reminder offsets."
            ),
            progress_summary=(
                f"Experience {EXPERIENCE_ID_V2}: symptom signature includes "
                "dispatch exports sorted in the wrong order near a DST boundary, "
                "reminders failing when they cross an hour boundary, and UTC "
                "contract fields shifted by the original offset. Failed attempts: "
                "blackout policy and audit key hypotheses were not causal; "
                "technician sorting and output formatting were not causal; cache "
                "or database ordering was not causal. Root cause: an "
                "offset-bearing ISO timestamp was converted to UTC twice in a "
                "shared parsing helper. Repair rule: normalize to timezone-aware "
                "UTC at input boundary only, never subtract the parsed offset "
                "manually before astimezone(timezone.utc). Use timedelta for "
                "reminder offsets instead of replace(minute=...)."
            ),
            failed_attempts=[
                "Blackout policy and audit key hypotheses were not causal.",
                "Technician sorting and output formatting were not causal.",
                "Cache or database ordering was not causal.",
                "Changing downstream UTC string rendering did not fix the contract shift.",
            ],
            evidence_summary=(
                f"{EXPERIENCE_ID_V2}: offset-bearing ISO timestamp was converted "
                "to UTC twice in the shared parser. Fix the parser so aware "
                "inputs call astimezone(timezone.utc) exactly once; keep local "
                "wall-clock policy checks local; use timedelta for reminder "
                "offsets. Blackout policy and audit key hypotheses were not "
                "causal. Verification command: python3 -m unittest -q."
            ),
            what_worked=[
                (
                    f"{EXPERIENCE_ID_V2}: normalize offset-bearing inputs at the "
                    "input boundary exactly once."
                ),
                "Use timedelta for reminder offsets that cross hour or day boundaries.",
                "Leave local wall-clock policy and audit-key behavior unchanged.",
            ],
            followups=[
                "For dispatch transfer tasks, inspect shared time parsing before rewriting policy or formatting modules."
            ],
        ),
        seed_experience(
            experience_id=PAGINATION_EXPERIENCE_ID,
            version="suite5",
            project_id=PAGINATION_PROJECT_ID,
            goal=(
                "Record prior debugging experience for retry-safe pagination "
                "cursor advancement."
            ),
            motivation=(
                "Later benchmark agents should retrieve that cursor movement "
                "belongs after durable writes, avoiding source API or idempotent "
                "store rewrites."
            ),
            hypothesis=(
                "A retry that skips records after a transient write failure is "
                "caused by advancing the cursor before the page write succeeds."
            ),
            progress_summary=(
                f"Experience {PAGINATION_EXPERIENCE_ID}: symptom signature is a "
                "backfill retry that skips the failed page or starts the next "
                "attempt after records that were never written. Failed attempts: "
                "API page boundaries and idempotent upserts were not causal; "
                "changing page size only moved the skipped records. Root cause: "
                "the worker advanced state.cursor before store.upsert_many "
                "completed. Repair rule: advance the cursor only after the page "
                "write succeeds; on transient write failure, leave cursor "
                "unchanged so the same page is retried."
            ),
            failed_attempts=[
                "API page boundaries and idempotent upserts were not causal.",
                "Changing page size only moved which records were skipped.",
                "Adding duplicate filtering did not recover records that were never written.",
            ],
            evidence_summary=(
                f"{PAGINATION_EXPERIENCE_ID}: advance the cursor only after the "
                "page write succeeds. On TransientWriteError, leave state.cursor "
                "unchanged and retry the same page. API page boundaries and "
                "idempotent upserts were not causal. Verification command: "
                "python3 -m unittest -q."
            ),
            what_worked=[
                "Move state.set_cursor(next_cursor) after store.upsert_many(page).",
                "Return retry without changing cursor when the write fails.",
                "Leave source API pagination and idempotent upsert behavior unchanged.",
            ],
            followups=[
                "For cursor transfer tasks, inspect durable-write ordering before rewriting pagination helpers."
            ],
        ),
        seed_experience(
            experience_id=CACHE_EXPERIENCE_ID,
            version="suite5",
            project_id=CACHE_PROJECT_ID,
            goal="Record prior debugging experience for tenant-scoped cache keys.",
            motivation=(
                "Later benchmark agents should retrieve that cross-tenant leakage "
                "comes from cache key composition, not authorization or flag defaults."
            ),
            hypothesis=(
                "A tenant seeing another tenant's feature flag is caused by a "
                "cache key that omits tenant_id."
            ),
            progress_summary=(
                f"Experience {CACHE_EXPERIENCE_ID}: symptom signature is two "
                "tenants sharing a project_id and flag name but receiving the "
                "first tenant's cached flag value. Failed attempts: authorization "
                "and flag defaults were not causal; store rows were correct; "
                "project-level cache invalidation only masked the issue. Root "
                "cause: cache key omitted tenant_id. Repair rule: include "
                "tenant_id, project_id, and flag_name in the cache key."
            ),
            failed_attempts=[
                "Authorization and flag defaults were not causal.",
                "Store rows were correct for both tenants.",
                "Project-level cache invalidation only masked the issue.",
            ],
            evidence_summary=(
                f"{CACHE_EXPERIENCE_ID}: cache key omitted tenant_id. Build keys "
                "from tenant_id, project_id, and flag_name so shared project ids "
                "do not leak cached values across tenants. Authorization and flag "
                "defaults were not causal. Verification command: python3 -m unittest -q."
            ),
            what_worked=[
                "Include tenant_id in flag_cache_key.",
                "Leave can_read authorization checks unchanged.",
                "Leave missing-flag default behavior unchanged.",
            ],
            followups=[
                "For tenant-leak transfer tasks, inspect cache key composition before rewriting authorization."
            ],
        ),
        seed_experience(
            experience_id=SCHEMA_EXPERIENCE_ID,
            version="suite5",
            project_id=SCHEMA_PROJECT_ID,
            goal="Record prior debugging experience for required schema defaults.",
            motivation=(
                "Later benchmark agents should retrieve that old rows need a "
                "backfill when a new required field is added."
            ),
            hypothesis=(
                "A report crash after migration is caused by a required column "
                "that lacked a backfill default for existing rows."
            ),
            progress_summary=(
                f"Experience {SCHEMA_EXPERIENCE_ID}: symptom signature is a "
                "report or export crashing only on pre-migration rows after a "
                "new required field is added. Failed attempts: report formatting "
                "and ingest defaults were not causal; new rows behaved correctly; "
                "the migration declared the field required but did not backfill "
                "old rows. Root cause: required column lacked a backfill default "
                "for existing rows. Repair rule: set a default value on every "
                "existing row before enforcing the required column."
            ),
            failed_attempts=[
                "Report formatting and ingest defaults were not causal.",
                "New rows behaved correctly after the migration.",
                "Changing report aggregation masked but did not fix missing fields.",
            ],
            evidence_summary=(
                f"{SCHEMA_EXPERIENCE_ID}: required column lacked a backfill "
                "default for existing rows. Backfill existing rows before "
                "enforcing the required field; keep reporting and ingest defaults "
                "unchanged. Verification command: python3 -m unittest -q."
            ),
            what_worked=[
                "Backfill missing tier='standard' on existing rows.",
                "Then mark tier as required.",
                "Leave report aggregation and ingest helpers unchanged.",
            ],
            followups=[
                "For schema transfer tasks, inspect migration backfill before changing reports."
            ],
        ),
        seed_experience(
            experience_id=STREAM_EXPERIENCE_ID,
            version="suite5",
            project_id=STREAM_PROJECT_ID,
            goal="Record prior debugging experience for stream backpressure flush ordering.",
            motivation=(
                "Later benchmark agents should retrieve that missing final chunks "
                "come from drain/flush ordering, not filters or formatters."
            ),
            hypothesis=(
                "A stream exporter truncating final chunks is caused by flushing "
                "before pending buffered chunks are drained."
            ),
            progress_summary=(
                f"Experience {STREAM_EXPERIENCE_ID}: symptom signature is an "
                "export that contains full chunks written under backpressure but "
                "drops the final partial buffer. Failed attempts: filtering and "
                "formatting were not causal; changing chunk text did not restore "
                "the final records. Root cause: exporter called flush before "
                "draining pending chunks. Repair rule: drain pending chunks "
                "before the final flush, and when backpressure fires, drain "
                "before flushing the durable writer."
            ),
            failed_attempts=[
                "Filtering and formatting were not causal.",
                "Changing chunk text did not restore final records.",
                "Writer internals correctly retained pending chunks until drain.",
            ],
            evidence_summary=(
                f"{STREAM_EXPERIENCE_ID}: drain pending chunks before the final "
                "flush. Filtering and formatting were not causal; the writer "
                "kept chunks pending until drain. Verification command: "
                "python3 -m unittest -q."
            ),
            what_worked=[
                "Call writer.drain() before the final writer.flush().",
                "On backpressure, drain pending chunks before flushing.",
                "Leave filtering, formatting, and writer internals unchanged.",
            ],
            followups=[
                "For stream transfer tasks, inspect drain/flush ordering before rewriting parser or filter code."
            ],
        ),
    ]
    print(json.dumps({"seeded": seeded}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
