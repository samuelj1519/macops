#!/bin/bash
set -euo pipefail

mode=${1:-full}
root="$(dirname "$0")/.."
case "$mode" in
    focus|focus-coverage) cd "$root" ;;
    *) cd "${2:-$root}" ;;
esac

quick() {
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
}

tests() {
    bash tools/run_ci_tests.sh cargo test
    cargo test --workspace --doc --all-features --locked --offline
}

focus() {
    local tool=$1
    shift
    if [[ $# != 2 || -z "$2" || "$2" == -* ]]; then
        echo 'Usage: bash tools/check.sh {focus|focus-coverage} {lib|cli} TEST_FILTER' >&2
        return 2
    fi
    local threads
    local target
    case "$1" in
        lib) threads=1; target=(--lib) ;;
        cli) threads=4; target=(--test cli) ;;
        *) echo 'Choose lib or cli for the test target.' >&2; return 2 ;;
    esac
    local filter=$2
    local flags=(--workspace --all-features --locked --offline)
    if [[ "$tool" == coverage ]]; then
        echo 'Focused coverage is diagnostic. Check that the filter runs tests.' >&2
        local build="${CARGO_TARGET_DIR:-target}/focus-coverage"
        CARGO_TARGET_DIR="$build" CARGO_LLVM_COV_TARGET_DIR="$build/llvm-cov-target" \
            CARGO_LLVM_COV_BUILD_DIR="$build/llvm-cov-target" RUST_TEST_THREADS="$threads" \
            cargo llvm-cov "${flags[@]}" "${target[@]}" --show-missing-lines -- "$filter"
    else
        RUST_TEST_THREADS="$threads" cargo test "${flags[@]}" "${target[@]}" -- "$filter"
    fi
}

coverage() {
    local reports="${CARGO_TARGET_DIR:-target}/coverage"
    cargo llvm-cov clean --workspace
    mkdir -p "$reports"
    bash tools/run_ci_tests.sh cargo llvm-cov --no-report
    cargo llvm-cov report --json --summary-only --output-path "$reports/workspace.json"
    python3 -B tools/check_coverage.py "$reports/workspace.json" --output "$reports/gate.json"
}

static_checks() {
    python3 -B tools/check_public_source.py
    python3 -B -m unittest discover -s tools/tests -v
    typos
    git ls-files -z -- '*.toml' | xargs -0 taplo fmt --check
    git ls-files -z -- '*.toml' | xargs -0 taplo check
    cargo deny --all-features --offline --locked check
}

case "$mode" in
    quick) quick ;;
    static) static_checks ;;
    focus) focus tests "${@:2}" ;;
    focus-coverage) focus coverage "${@:2}" ;;
    tests) tests ;;
    coverage) coverage ;;
    quality) static_checks; coverage ;;
    full)
        quick
        static_checks
        tests
        coverage
        cargo build --release --locked --offline
        ;;
    *)
        echo 'Usage: bash tools/check.sh [quick|static|tests|coverage|quality|full] [SOURCE_DIR]' >&2
        echo '       bash tools/check.sh {focus|focus-coverage} {lib|cli} TEST_FILTER' >&2
        exit 2
        ;;
esac
