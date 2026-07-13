# Release provenance archive

`v0.50.0-legacy.yaml` is retained only as historical process evidence. It
contains internally inconsistent version labels, machine-local paths, and
commands that were documented rather than executed. It is not a release
manifest and must not be copied forward.

New release provenance is produced by the clean-clone benchmark and package
gates. The release record must identify one clean commit, exact build inputs,
binary digest, executed commands, test outputs, and published artifact checks.
Store each record under its release version; do not recreate a mutable
top-level `run_manifest.yaml`.
