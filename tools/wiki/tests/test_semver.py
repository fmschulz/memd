from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki._semver import (  # noqa: E402
    Semver,
    SemverParseError,
    compare_major_minor,
    parse,
)


class SemverParseTests(unittest.TestCase):
    def test_parses_plain_triple(self) -> None:
        self.assertEqual(parse("0.9.0").as_tuple(), (0, 9, 0))

    def test_parses_multi_digit_components(self) -> None:
        self.assertEqual(parse("1.10.12").as_tuple(), (1, 10, 12))

    def test_strips_prerelease_suffix(self) -> None:
        self.assertEqual(parse("0.9.0-rc1").as_tuple(), (0, 9, 0))

    def test_strips_build_metadata(self) -> None:
        self.assertEqual(parse("0.9.0+abc123").as_tuple(), (0, 9, 0))

    def test_strips_both_suffixes(self) -> None:
        self.assertEqual(parse("0.9.0-rc1+abc123").as_tuple(), (0, 9, 0))

    def test_raises_on_two_component_version(self) -> None:
        with self.assertRaises(SemverParseError):
            parse("0.9")

    def test_raises_on_four_component_version(self) -> None:
        with self.assertRaises(SemverParseError):
            parse("0.9.0.1")

    def test_raises_on_empty(self) -> None:
        with self.assertRaises(SemverParseError):
            parse("")

    def test_raises_on_non_string(self) -> None:
        with self.assertRaises(SemverParseError):
            parse(42)  # type: ignore[arg-type]

    def test_raises_on_non_integer_component(self) -> None:
        with self.assertRaises(SemverParseError):
            parse("0.x.0")

    def test_raises_on_negative_component(self) -> None:
        with self.assertRaises(SemverParseError):
            parse("-1.0.0")


class CompareMajorMinorTests(unittest.TestCase):
    def test_equal_major_minor_is_zero_even_on_patch_diff(self) -> None:
        self.assertEqual(
            compare_major_minor(Semver(0, 9, 0), Semver(0, 9, 5)),
            0,
        )

    def test_major_minor_mismatch_distinguishes_0_10_from_0_1(self) -> None:
        # Regression guard: string-prefix comparison would treat "0.10" and
        # "0.1" as the same prefix; parsed semver must distinguish them.
        self.assertNotEqual(
            compare_major_minor(Semver(0, 10, 0), Semver(0, 1, 0)),
            0,
        )
        self.assertEqual(
            compare_major_minor(Semver(0, 10, 0), Semver(0, 1, 0)),
            1,
        )

    def test_minor_less_than(self) -> None:
        self.assertEqual(
            compare_major_minor(Semver(0, 9, 0), Semver(0, 10, 0)),
            -1,
        )

    def test_major_greater_than(self) -> None:
        self.assertEqual(
            compare_major_minor(Semver(1, 0, 0), Semver(0, 99, 99)),
            1,
        )


if __name__ == "__main__":
    unittest.main()
