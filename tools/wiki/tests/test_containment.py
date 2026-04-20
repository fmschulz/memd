"""Tests for the containment guard (export-markdown rules ported from Rust).

Mirror of the Rust test matrix in
``crates/memd/src/cli.rs::reject_if_any_symlink_inside_outdir_*`` and
``discover_project_data_dir_from`` / ``resolve_export_markdown_data_dirs_from``.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.containment import (  # noqa: E402
    OutdirContainmentError,
    check_outdir_containment,
    discover_project_data_dir_from,
    normalize_absolute,
    path_is_inside,
    reject_if_any_symlink_inside_outdir,
    resolve_forbidden_data_dirs,
)


class NormalizeAbsoluteTests(unittest.TestCase):
    def test_absolute_input_kept(self) -> None:
        self.assertEqual(
            normalize_absolute(Path("/tmp/foo/bar")),
            Path("/tmp/foo/bar"),
        )

    def test_relative_joined_against_cwd(self) -> None:
        result = normalize_absolute(Path("foo/bar"), cwd=Path("/home/me"))
        self.assertEqual(result, Path("/home/me/foo/bar"))

    def test_collapses_curdir(self) -> None:
        self.assertEqual(
            normalize_absolute(Path("/tmp/./foo/./bar")),
            Path("/tmp/foo/bar"),
        )

    def test_collapses_parent_segments(self) -> None:
        self.assertEqual(
            normalize_absolute(Path("/tmp/foo/../bar")),
            Path("/tmp/bar"),
        )

    def test_parent_never_pops_past_root(self) -> None:
        self.assertEqual(
            normalize_absolute(Path("/../../..")),
            Path("/"),
        )


class PathIsInsideTests(unittest.TestCase):
    def test_same_path_is_inside(self) -> None:
        self.assertTrue(path_is_inside(Path("/a/b"), Path("/a/b")))

    def test_child_is_inside(self) -> None:
        self.assertTrue(path_is_inside(Path("/a/b/c"), Path("/a/b")))

    def test_sibling_is_not_inside(self) -> None:
        self.assertFalse(path_is_inside(Path("/a/c"), Path("/a/b")))

    def test_prefix_confusion_not_inside(self) -> None:
        # `/a/bc` starts with `/a/b` as a string but not as a path
        self.assertFalse(path_is_inside(Path("/a/bc"), Path("/a/b")))


class DiscoverProjectDataDirTests(unittest.TestCase):
    def test_no_scope_file_returns_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(discover_project_data_dir_from(Path(tmp)))

    def test_absolute_data_dir_returned_verbatim(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": "/abs/data/dir"}), encoding="utf-8"
            )
            self.assertEqual(
                discover_project_data_dir_from(project),
                Path("/abs/data/dir"),
            )

    def test_relative_data_dir_joined_against_project_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": "local/data"}), encoding="utf-8"
            )
            self.assertEqual(
                discover_project_data_dir_from(project),
                project / "local" / "data",
            )

    def test_found_but_malformed_json_stops_walk(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "project"
            outer = Path(tmp)
            project.mkdir()
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                "{not valid json}", encoding="utf-8"
            )
            # Outer has a valid one — discovery must NOT fall through
            (outer / ".memd").mkdir()
            (outer / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": "/should/not/surface"}), encoding="utf-8"
            )
            self.assertIsNone(discover_project_data_dir_from(project))

    def test_missing_data_dir_stops_walk(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "project"
            outer = Path(tmp)
            project.mkdir()
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"other_key": "present"}), encoding="utf-8"
            )
            (outer / ".memd").mkdir()
            (outer / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": "/should/not/surface"}), encoding="utf-8"
            )
            self.assertIsNone(discover_project_data_dir_from(project))


class ResolveForbiddenDataDirsTests(unittest.TestCase):
    def test_explicit_only_overrides_everything(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": str(project / "discovered")}),
                encoding="utf-8",
            )
            result = resolve_forbidden_data_dirs(
                explicit=Path("/explicit/only"),
                start=project,
                home=Path("/fake-home"),
            )
            self.assertEqual(result, [Path("/explicit/only")])

    def test_default_returns_home_when_no_scope(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = resolve_forbidden_data_dirs(
                explicit=None,
                start=Path(tmp),
                home=Path("/fake-home"),
            )
            self.assertEqual(result, [Path("/fake-home/.memd/data")])

    def test_default_includes_discovered_and_home(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": "/project/data"}), encoding="utf-8"
            )
            result = resolve_forbidden_data_dirs(
                explicit=None,
                start=project,
                home=Path("/fake-home"),
            )
            self.assertEqual(
                result,
                [Path("/project/data"), Path("/fake-home/.memd/data")],
            )

    def test_dedup_when_discovered_equals_home_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            (project / ".memd").mkdir()
            (project / ".memd" / "tenant_scope.json").write_text(
                json.dumps({"data_dir": "/fake-home/.memd/data"}),
                encoding="utf-8",
            )
            result = resolve_forbidden_data_dirs(
                explicit=None,
                start=project,
                home=Path("/fake-home"),
            )
            self.assertEqual(result, [Path("/fake-home/.memd/data")])


class CheckOutdirContainmentTests(unittest.TestCase):
    def test_clean_outdir_returns_normalized_path(self) -> None:
        result = check_outdir_containment(
            Path("/tmp/project/wiki"),
            [Path("/home/me/.memd/data")],
        )
        self.assertEqual(result, Path("/tmp/project/wiki"))

    def test_outdir_inside_home_default_refused(self) -> None:
        with self.assertRaises(OutdirContainmentError) as ctx:
            check_outdir_containment(
                Path("/home/me/.memd/data/wiki"),
                [Path("/home/me/.memd/data")],
            )
        self.assertIn("memd data directory", ctx.exception.reason)

    def test_outdir_equal_to_data_dir_refused(self) -> None:
        with self.assertRaises(OutdirContainmentError):
            check_outdir_containment(
                Path("/home/me/.memd/data"),
                [Path("/home/me/.memd/data")],
            )

    def test_outdir_inside_discovered_scope_refused(self) -> None:
        with self.assertRaises(OutdirContainmentError):
            check_outdir_containment(
                Path("/project/data/wiki"),
                [Path("/project/data"), Path("/home/me/.memd/data")],
            )

    def test_relative_outdir_normalized_against_cwd(self) -> None:
        result = check_outdir_containment(
            Path("wiki"),
            [Path("/home/me/.memd/data")],
            cwd=Path("/tmp/project"),
        )
        self.assertEqual(result, Path("/tmp/project/wiki"))


class RejectIfAnySymlinkInsideOutdirTests(unittest.TestCase):
    """Mirror of the Rust test matrix in cli.rs."""

    def test_accepts_regular_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp)
            (outdir / "sub").mkdir()
            (outdir / "sub" / "file.md").write_text("ok", encoding="utf-8")
            # Should not raise
            reject_if_any_symlink_inside_outdir(outdir / "sub" / "file.md", outdir)

    def test_tolerates_nonexistent_components(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp)
            # `outdir/sub/new.md` does not exist; guard should not raise
            reject_if_any_symlink_inside_outdir(outdir / "sub" / "new.md", outdir)

    def test_refuses_leaf_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp)
            victim = outdir / "victim.md"
            target = Path(tmp) / "outside.md"
            target.write_text("external", encoding="utf-8")
            os.symlink(target, victim)
            with self.assertRaises(OutdirContainmentError) as ctx:
                reject_if_any_symlink_inside_outdir(victim, outdir)
            self.assertIn("symlink", ctx.exception.reason)

    def test_refuses_intermediate_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp)
            # outdir/sub -> /tmp/elsewhere; writing outdir/sub/file.md would
            # redirect outside the tree.
            elsewhere = Path(tmp) / "elsewhere"
            elsewhere.mkdir()
            os.symlink(elsewhere, outdir / "sub")
            with self.assertRaises(OutdirContainmentError):
                reject_if_any_symlink_inside_outdir(
                    outdir / "sub" / "file.md", outdir
                )

    def test_permits_symlinked_outdir_itself(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            backing = Path(tmp) / "real-outdir"
            backing.mkdir()
            link = Path(tmp) / "link-outdir"
            os.symlink(backing, link)
            # Pre-populate a real file inside the symlinked outdir.
            (backing / "file.md").write_text("ok", encoding="utf-8")
            # Guard should NOT consider the outdir symlink itself.
            reject_if_any_symlink_inside_outdir(link / "file.md", link)

    def test_internal_error_when_target_not_inside_outdir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "a"
            outdir.mkdir()
            stray = Path(tmp) / "b" / "file.md"
            with self.assertRaises(OutdirContainmentError) as ctx:
                reject_if_any_symlink_inside_outdir(stray, outdir)
            self.assertIn("not inside outdir", ctx.exception.reason)

    def test_unnormalized_target_does_not_spuriously_refuse(self) -> None:
        """Codex r1 HIGH: raw `outdir/../outdir/file` must normalize before relative_to."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp) / "wiki"
            outdir.mkdir()
            # A target with `..` that normalizes to inside outdir.
            tricky = outdir / ".." / "wiki" / "file.md"
            reject_if_any_symlink_inside_outdir(tricky, outdir)

    def test_relative_outdir_normalized(self) -> None:
        """Relative outdir inputs are absolutized before comparison."""
        cwd_backup = os.getcwd()
        try:
            with tempfile.TemporaryDirectory() as tmp:
                os.chdir(tmp)
                Path("wiki").mkdir()
                Path("wiki/file.md").write_text("ok", encoding="utf-8")
                # Both paths relative; guard must still work.
                reject_if_any_symlink_inside_outdir(
                    Path("wiki/file.md"), Path("wiki")
                )
        finally:
            os.chdir(cwd_backup)

    def test_enotdir_surfaces_as_refusal(self) -> None:
        """MEDIUM fold: abnormal OS errors must fail closed, not be silently swallowed."""
        with tempfile.TemporaryDirectory() as tmp:
            outdir = Path(tmp)
            # Create a regular file at outdir/sub; a lookup of
            # outdir/sub/x raises ENOTDIR, which must surface as a
            # refusal per the Rust contract.
            (outdir / "sub").write_text("file-not-dir", encoding="utf-8")
            with self.assertRaises(OutdirContainmentError) as ctx:
                reject_if_any_symlink_inside_outdir(
                    outdir / "sub" / "x" / "y.md", outdir
                )
            # ENOTDIR or similar — the wrapper message begins with
            # "cannot verify" unless the intermediate was a plain symlink.
            # Here `sub` is a regular file, so the walk hits lstat on
            # `sub/x` which raises ENOTDIR.
            self.assertIn("cannot verify", ctx.exception.reason)


if __name__ == "__main__":
    unittest.main()
