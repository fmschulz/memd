"""Step 8: assert memd-wiki's MCP tool contract matches reality.

Two tests:

1. ``ContractMatchesCompilerTests`` — the declared contract in
   ``mcp_contract.REQUIRED_MCP_TOOLS`` lists every tool the compiler
   actually calls, with matching arg keys. Drift between compiler.py
   and the contract declaration is caught here.

2. ``LiveServerContractTests`` — network-gated integration test. Spins
   up no server; just queries the already-running memd HTTP daemon if
   one is reachable. Asserts each declared tool is present in
   ``tools/list`` and that its ``inputSchema.properties`` accepts
   every arg we pass, and that its ``inputSchema.required`` is a
   subset of our ``required_by_us`` expectation (memd can be stricter
   than us expects, never weaker).
"""

from __future__ import annotations

import json
import os
import sys
import unittest
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.mcp_contract import (  # noqa: E402
    REQUIRED_MCP_TOOLS,
    ToolExpectation,
)


class ContractMatchesCompilerTests(unittest.TestCase):
    """Grep the compiler source for call_tool(...) sites and match them."""

    def test_contract_covers_all_compiler_call_sites(self) -> None:
        import re

        compiler_src = (
            Path(__file__).resolve().parents[1]
            / "compiled_wiki"
            / "compiler.py"
        ).read_text(encoding="utf-8")
        # Match call_tool("<name>", {...})
        calls = set(re.findall(r'call_tool\(\s*"([^"]+)"', compiler_src))
        declared = {t.name for t in REQUIRED_MCP_TOOLS}
        self.assertEqual(
            calls,
            declared,
            f"compiler.py calls {calls} but contract declares {declared}",
        )

    def test_every_expectation_has_non_empty_args_we_pass(self) -> None:
        for exp in REQUIRED_MCP_TOOLS:
            self.assertTrue(
                exp.args_we_pass,
                f"tool {exp.name} declares no args we pass",
            )

    def test_required_by_us_is_subset_of_args_we_pass(self) -> None:
        for exp in REQUIRED_MCP_TOOLS:
            self.assertTrue(
                set(exp.required_by_us) <= set(exp.args_we_pass),
                f"tool {exp.name}: required_by_us {exp.required_by_us} "
                f"is not a subset of args_we_pass {exp.args_we_pass}",
            )


def _probe_live_memd() -> list[dict] | None:
    """Return the tool list from the local memd daemon if reachable.

    Returns None when the daemon is not running so the test can skip
    cleanly on CI / environments without a live server.
    """
    url = os.environ.get("MEMD_WIKI_TEST_URL", "http://127.0.0.1:8787/mcp")
    # Step 1: initialize.
    init_body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "memd-wiki-contract-test", "version": "0.0.1"},
            },
        }
    ).encode("utf-8")
    try:
        req = urllib.request.Request(
            url,
            data=init_body,
            headers={
                "Accept": "application/json, text/event-stream",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=2.0):
            pass
    except (urllib.error.URLError, TimeoutError, OSError):
        return None

    # Step 2: tools/list.
    list_body = json.dumps(
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
    ).encode("utf-8")
    try:
        req = urllib.request.Request(
            url,
            data=list_body,
            headers={
                "Accept": "application/json, text/event-stream",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=2.0) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError):
        return None
    tools = data.get("result", {}).get("tools")
    if not isinstance(tools, list):
        return None
    return tools


class LiveServerContractTests(unittest.TestCase):
    """Network-gated: skipped when no local memd daemon is reachable."""

    @classmethod
    def setUpClass(cls) -> None:
        tools = _probe_live_memd()
        if tools is None:
            raise unittest.SkipTest(
                "memd daemon not reachable at MEMD_WIKI_TEST_URL or default; "
                "skipping live contract test"
            )
        cls.tools_by_name = {t["name"]: t for t in tools if isinstance(t, dict)}

    def test_all_declared_tools_present(self) -> None:
        missing = [t.name for t in REQUIRED_MCP_TOOLS if t.name not in self.tools_by_name]
        self.assertEqual(missing, [], f"memd tools/list missing {missing}")

    def test_each_tool_accepts_every_arg_we_pass(self) -> None:
        for exp in REQUIRED_MCP_TOOLS:
            tool = self.tools_by_name[exp.name]
            schema = tool.get("inputSchema") or {}
            props = set((schema.get("properties") or {}).keys())
            unknown = [arg for arg in exp.args_we_pass if arg not in props]
            self.assertEqual(
                unknown,
                [],
                f"tool {exp.name} rejects {unknown}; live schema properties: {sorted(props)}",
            )

    def test_server_required_is_subset_of_our_expected_required(self) -> None:
        """memd may be stricter than our contract, but must never require args we don't send."""
        for exp in REQUIRED_MCP_TOOLS:
            tool = self.tools_by_name[exp.name]
            schema = tool.get("inputSchema") or {}
            server_required = set(schema.get("required") or [])
            args_we_send = set(exp.args_we_pass)
            missing_from_our_calls = server_required - args_we_send
            self.assertEqual(
                missing_from_our_calls,
                set(),
                f"tool {exp.name}: memd requires {missing_from_our_calls} "
                f"but compiler does not pass them",
            )


if __name__ == "__main__":
    unittest.main()
