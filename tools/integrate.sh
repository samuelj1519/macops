#!/bin/bash
set -euo pipefail

if [[ $# != 1 ]]; then
    echo 'Usage: bash tools/integrate.sh CANDIDATE_COMMIT' >&2
    exit 2
fi

root="$(git rev-parse --show-toplevel)"
cd "$root"
branch="$(git symbolic-ref --quiet HEAD)"
start="$(git rev-parse HEAD)"
base="$(git rev-parse refs/heads/main)"
candidate="$(git rev-parse --verify --end-of-options "$1^{commit}")"
git diff --cached --check
if [[ -n "$(git status --porcelain)" ]] || ! git merge-base --is-ancestor "$base" "$candidate" || [[ "$base" == "$candidate" ]]; then
    echo 'Integration requires a clean worktree and a candidate that advances main.' >&2
    exit 1
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/macops-integration.XXXXXX")"
scratch="$(cd "$scratch" && pwd -P)"
trap 'rm -rf -- "$scratch"' EXIT
export RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER='' RUNNER_TEMP="$scratch"

git clone --quiet --no-hardlinks --no-checkout -- "$root" "$scratch/source"
git -C "$scratch/source" checkout --quiet --detach "$candidate"
git -C "$scratch/source" worktree add --quiet --detach "$scratch/baseline" "$base"
check="$scratch/source/tools/check.sh"

printf 'Checking baseline %s\n' "$base"
CARGO_TARGET_DIR="$scratch/baseline-build" bash "$check" coverage "$scratch/baseline"
printf 'Checking candidate %s\n' "$candidate"
CARGO_TARGET_DIR="$scratch/candidate-build" bash "$check" full
python3 -B "$scratch/source/tools/check_coverage.py" \
    "$scratch/candidate-build/coverage/workspace.json" \
    --baseline "$scratch/baseline-build/coverage/gate.json"

test "$(git symbolic-ref --quiet HEAD)" = "$branch"
test "$(git rev-parse HEAD)" = "$start"
test "$(git rev-parse refs/heads/main)" = "$base"
test -z "$(git status --porcelain)"
git switch main
test "$(git rev-parse HEAD)" = "$base"
test -z "$(git status --porcelain)"
git merge --ff-only "$candidate"
test "$(git rev-parse HEAD)" = "$candidate"
test -z "$(git status --porcelain)"
