#!/usr/bin/env python3
"""Compile deliberately wrong scratch copies and require behavior tests to reject them."""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def remove_block(source, needle):
    start = source.index(needle)
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[:start] + source[end:]


def mutate(root, name):
    directory = root / "crates/app/src"
    if name == "gate":
        path = directory / "operations.rs"
        source = path.read_text()
        start = source.index("    let mut gate =", source.index("pub fn worker("))
        end = source.index("    let journal:", start)
        path.write_text(source[:start] + source[end:])
        return ["--test", "cli", "valid_worker_journal_without_gate_never_executes"]
    if name == "cache-identity":
        path = directory / "cache.rs"
        path.write_text(remove_block(path.read_text(), "        if !crate::policy::cache::candidate_matches("))
        return ["--lib", "cache::tests::changed_inode_is_quarantined_and_never_deleted"]
    if name == "home":
        path = directory / "process.rs"
        source = path.read_text()
        old = 'c.env("HOME", paths::read_path(&scope.home)?)\n        .current_dir(&scope.home);'
        if source.count(old) != 1:
            raise RuntimeError("HOME mutation target changed")
        path.write_text(source.replace(old, "c.current_dir(&scope.home);"))
        return ["--test", "cli", "query_and_installer_receive_frozen_home"]
    raise ValueError(name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    results = []
    for name in ["gate", "cache-identity", "home"]:
        with tempfile.TemporaryDirectory(prefix=f"macops-mutant-{name}-", dir="/private/tmp") as temp:
            root = Path(temp)
            for filename in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rustfmt.toml", "clippy.toml"]:
                shutil.copy2(args.source / filename, root / filename)
            shutil.copytree(args.source / ".cargo", root / ".cargo")
            shutil.copytree(args.source / "crates", root / "crates")
            before = {str(p.relative_to(root)): p.read_bytes() for p in root.rglob("*") if p.is_file()}
            selection = mutate(root, name)
            patches = []
            for relative, content in before.items():
                changed = (root / relative).read_bytes()
                if changed != content:
                    patches.extend(difflib.unified_diff(content.decode().splitlines(True), changed.decode().splitlines(True), fromfile=relative, tofile=relative))
            (args.output / f"{name}.patch").write_text("".join(patches))
            command = ["cargo", "test", "--manifest-path", str(root / "Cargo.toml"), "--locked", "--offline", *selection, "--", "--exact"]
            log = args.output / f"{name}.log"
            with log.open("w") as stream:
                result = subprocess.run(command, cwd=root, env={**os.environ, "RUSTC_WRAPPER": ""}, stdout=stream, stderr=subprocess.STDOUT)
            content = log.read_text()
            killed = result.returncode != 0 and f"test {selection[-1]} ... FAILED" in content and "test result: FAILED" in content
            results.append({"name": name, "exit": result.returncode, "status": "killed" if killed else "invalid-or-survived", "log_sha256": hashlib.sha256(log.read_bytes()).hexdigest(), "source_sha256": {p: hashlib.sha256(b).hexdigest() for p,b in before.items()}, "command": command})
            print(f"{name}: {results[-1]['status']}", flush=True)
    (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if all(row["status"] == "killed" for row in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
