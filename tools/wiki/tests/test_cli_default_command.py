"""`memd-wiki` with no subcommand runs `build` with build's defaults."""

from __future__ import annotations

import unittest

from compiled_wiki.cli import parse_args


class DefaultCommandTest(unittest.TestCase):
    def test_no_subcommand_parses_as_build(self) -> None:
        bare = parse_args([])
        explicit = parse_args(["build"])
        self.assertEqual(bare.command, "build")
        self.assertEqual(vars(bare), vars(explicit))

    def test_explicit_subcommand_is_kept(self) -> None:
        self.assertEqual(parse_args(["lint"]).command, "lint")


if __name__ == "__main__":
    unittest.main()
