#!/bin/bash
set -euo pipefail

flags=(--workspace --all-features --locked --offline)
logs="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/macops-tests.XXXXXX")"
trap 'rm -f "$logs/unit.log" "$logs/integration.log"; rmdir "$logs"' EXIT

RUST_TEST_THREADS=1 "$@" "${flags[@]}" --lib --bins --examples --benches >"$logs/unit.log" 2>&1 &
unit_pid=$!
RUST_TEST_THREADS=4 "$@" "${flags[@]}" --test '*' >"$logs/integration.log" 2>&1 &
integration_pid=$!

status=0
wait "$unit_pid" || status=$?
wait "$integration_pid" || status=$?
cat "$logs/unit.log" "$logs/integration.log"
exit "$status"
