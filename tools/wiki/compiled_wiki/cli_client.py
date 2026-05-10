from __future__ import annotations

import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import __version__ as _CLIENT_PACKAGE_VERSION
from .compat import (
    CompatResult,
    ServerIncompatibleError,
    check_server_compat,
)


class MemdCliError(RuntimeError):
    """Raised when the memd executable returns an invalid or failed response."""


@dataclass
class MemdCliClient:
    memd_bin: str = "memd"
    data_dir: Path | None = None
    timeout: float = 30.0
    client_version: str = _CLIENT_PACKAGE_VERSION
    check_compat: bool = True
    _initialized: bool = field(default=False, init=False, repr=False)
    executable_version: str | None = field(default=None, init=False)
    compat_result: CompatResult | None = field(default=None, init=False)

    def initialize(self) -> None:
        if self._initialized:
            return
        proc = self._run(["--version"])
        version = _parse_version(proc.stdout)
        self.executable_version = version
        if self.check_compat:
            self.compat_result = check_server_compat(version, self.client_version)
            if self.compat_result.severity == "fail":
                raise ServerIncompatibleError(
                    client_version=self.compat_result.client_version,
                    server_version=self.compat_result.server_version,
                )
            if self.compat_result.severity == "warn":
                print(
                    f"memd-wiki: warning: {self.compat_result.message}",
                    file=sys.stderr,
                )
        self._initialized = True

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        self.initialize()
        cmd = self._base_args() + [
            "call",
            name,
            "--json",
            json.dumps(arguments, sort_keys=True),
        ]
        proc = self._run(cmd)
        return self._parse_payload(proc.stdout, name)

    def _base_args(self) -> list[str]:
        args: list[str] = []
        if self.data_dir is not None:
            args.extend(["--data-dir", str(self.data_dir)])
        return args

    def _run(self, args: list[str]) -> subprocess.CompletedProcess[str]:
        cmd = [self.memd_bin] + args
        try:
            return subprocess.run(
                cmd,
                check=True,
                capture_output=True,
                text=True,
                timeout=self.timeout,
            )
        except FileNotFoundError as exc:
            raise MemdCliError(f"memd executable not found: {self.memd_bin}") from exc
        except subprocess.TimeoutExpired as exc:
            raise MemdCliError(
                f"memd command timed out after {self.timeout}s: {' '.join(cmd)}"
            ) from exc
        except subprocess.CalledProcessError as exc:
            stderr = (exc.stderr or "").strip()
            stdout = (exc.stdout or "").strip()
            detail = stderr or stdout or f"exit code {exc.returncode}"
            raise MemdCliError(f"memd command failed: {detail}") from exc

    @staticmethod
    def _parse_payload(raw: str, name: str) -> Any:
        stripped = raw.strip()
        if not stripped:
            raise MemdCliError(f"operation {name} returned no output")
        try:
            return json.loads(stripped)
        except json.JSONDecodeError as exc:
            raise MemdCliError(
                f"operation {name} returned invalid JSON: {stripped[:200]!r}"
            ) from exc


def _parse_version(output: str) -> str | None:
    match = re.search(r"\b(\d+\.\d+\.\d+)\b", output)
    return match.group(1) if match else None
