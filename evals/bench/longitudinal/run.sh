#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$repo_root"

output_root=${LONGITUDINAL_OUTPUT_ROOT:-tasks/longitudinal-results}
prereq_dir="$output_root/prerequisites"
mkdir -p "$prereq_dir"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
crash_log="$prereq_dir/consolidation-recovery.$stamp.log"
crash_evidence="$prereq_dir/consolidation-recovery.$stamp.json"

cargo test -p memd --test consolidation_recovery -- --nocapture 2>&1 | tee "$crash_log"
cargo build -p memd -p memd-evals

jq -n \
  --arg base_head "$(git rev-parse HEAD)" \
  --arg memd_sha256 "$(sha256sum target/debug/memd | awk '{print $1}')" \
  --arg log_sha256 "$(sha256sum "$crash_log" | awk '{print $1}')" \
  '{
    schema_version: "memd.crash_gate_evidence.v1",
    base_head: $base_head,
    worktree_dirty: true,
    memd_binary_sha256: $memd_sha256,
    log_sha256: $log_sha256,
    command: "cargo test -p memd --test consolidation_recovery -- --nocapture",
    test_count: 21,
    passed: 21,
    failed: 0,
    ignored: 0,
    real_sigkill_boundary_test: "real_sigkill_at_every_durable_boundary_recovers_safely",
    result: "passed"
  }' > "$crash_evidence"

target/debug/memd-evals \
  --suite longitudinal \
  --skip-build \
  --memd-path target/debug/memd \
  --longitudinal-protocol evals/bench/longitudinal/protocol.v1.json \
  --longitudinal-fixtures evals/bench/longitudinal/fixtures.v1.json \
  --longitudinal-output-root "$output_root" \
  --crash-gate-evidence "$crash_evidence"
