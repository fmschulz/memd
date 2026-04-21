"""Pinned MCP tool contract memd-wiki depends on.

Per the Item 7 plan §9.4, memd-wiki's compiler calls exactly 7 tools.
A memd-side rename or schema change must break a pinned test here
rather than silently degrade the wiki.

The declared contract captures:

- the tool ``name`` the compiler calls,
- the arg keys the compiler passes that memd MUST accept (verified
  against the live ``tools/list`` ``inputSchema.properties``),
- the arg keys memd MUST treat as required (verified against
  ``inputSchema.required`` when present; empty means "memd accepts
  us supplying them as optional").

Kept as plain data so unit tests can assert the contract matches the
compiler's actual call sites without a running daemon, and the
integration test can assert the live daemon honors it.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ToolExpectation:
    name: str
    args_we_pass: tuple[str, ...]
    required_by_us: tuple[str, ...]


# Order mirrors compiler.py's call sequence so drift is easy to diff.
REQUIRED_MCP_TOOLS: tuple[ToolExpectation, ...] = (
    ToolExpectation(
        name="context.brief_project",
        args_we_pass=("tenant_id", "project_id", "k"),
        required_by_us=("project_id",),
    ),
    ToolExpectation(
        name="artifact.find_failures",
        args_we_pass=("tenant_id", "project_id", "k"),
        required_by_us=(),
    ),
    ToolExpectation(
        name="artifact.find_decisions",
        args_we_pass=("tenant_id", "project_id", "k"),
        required_by_us=(),
    ),
    ToolExpectation(
        name="artifact.find_evidence",
        args_we_pass=("tenant_id", "project_id", "k"),
        required_by_us=(),
    ),
    ToolExpectation(
        name="artifact.find_highlights",
        args_we_pass=("tenant_id", "project_id", "k"),
        required_by_us=(),
    ),
    ToolExpectation(
        name="task.resume",
        args_we_pass=("tenant_id", "task_id", "k"),
        required_by_us=("task_id",),
    ),
    ToolExpectation(
        name="artifact.list_thread",
        args_we_pass=("tenant_id", "thread_id"),
        required_by_us=(),
    ),
    # v2 phase 2: the compiler pulls every wiki_page artifact for the
    # configured project and resolves each one's grounding refs back
    # to the cited canonical artifact via artifact.get.
    ToolExpectation(
        name="artifact.search",
        args_we_pass=("tenant_id", "k", "filters"),
        required_by_us=(),
    ),
    ToolExpectation(
        name="artifact.get",
        args_we_pass=("tenant_id", "artifact_id"),
        required_by_us=("artifact_id",),
    ),
)
