#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
cargo test --workspace --all-targets --all-features --locked --offline
cargo test --workspace --doc --all-features --locked --offline
python3 -B tools/check_public_source.py
python3 -B -m unittest discover -s tools/tests -v
typos
rg --files --hidden -0 -g '*.toml' -g '!.git' -g '!target/**' -g '!artifacts/**' -g '!.worktrees/**' | xargs -0 taplo fmt --check
rg --files --hidden -0 -g '*.toml' -g '!.git' -g '!target/**' -g '!artifacts/**' -g '!.worktrees/**' | xargs -0 taplo check
cargo deny --all-features --offline --locked check
mkdir -p target/coverage
cargo llvm-cov --workspace --all-targets --all-features --locked --offline --json --summary-only --output-path target/coverage/workspace.json
python3 -B tools/check_coverage.py target/coverage/workspace.json --output target/coverage/gate.json
cargo build --release --locked --offline
