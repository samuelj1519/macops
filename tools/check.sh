#!/bin/bash
set -euo pipefail

mode=${1:-full}
cd "${2:-$(dirname "$0")/..}"

quick() {
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
}

tests() {
    bash tools/run_ci_tests.sh cargo test
    cargo test --workspace --doc --all-features --locked --offline
}

coverage() {
    local reports="${CARGO_TARGET_DIR:-target}/coverage"
    cargo llvm-cov clean --workspace
    mkdir -p "$reports"
    bash tools/run_ci_tests.sh cargo llvm-cov --no-report
    cargo llvm-cov report --json --summary-only --output-path "$reports/workspace.json"
    python3 -B tools/check_coverage.py "$reports/workspace.json" --output "$reports/gate.json"
}

quality() {
    python3 -B tools/check_public_source.py
    python3 -B -m unittest discover -s tools/tests -v
    typos
    git ls-files -z -- '*.toml' | xargs -0 taplo fmt --check
    git ls-files -z -- '*.toml' | xargs -0 taplo check
    cargo deny --all-features --offline --locked check
    coverage
}

case "$mode" in
    quick) quick ;;
    tests) tests ;;
    coverage) coverage ;;
    quality) quality ;;
    full)
        quick
        tests
        quality
        cargo build --release --locked --offline
        ;;
    *)
        echo 'Usage: bash tools/check.sh [quick|tests|coverage|quality|full] [SOURCE_DIR]' >&2
        exit 2
        ;;
esac
