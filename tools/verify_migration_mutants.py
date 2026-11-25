#!/usr/bin/env python3
"""Compile incorrect migration logic and require named behavioral oracles to fail."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

EXCLUDED = {".git", "artifacts", "target", "__pycache__"}
GENERATED = {".pyc", ".profraw", ".profdata"}


def files(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob("*") if p.is_file() and not EXCLUDED.intersection(p.relative_to(root).parts) and p.suffix not in GENERATED and p.name != ".DS_Store"}


def replace(text, before, after):
    assert text.count(before) == 1, (before, text.count(before))
    return text.replace(before, after, 1)


def function_range(text, name):
    start = text.index(name)
    opening = text.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return start, opening, end


def in_function(text, name, before, after):
    start, _, end = function_range(text, name)
    return text[:start] + replace(text[start:end], before, after) + text[end:]


def relearn_root(text):
    _, opening, end = function_range(text, "pub fn create(")
    body = """{
        let mut dir=directory(Path::new("/"))?;
        for part in self.path.components().skip(1) {
            let name=part.as_os_str();
            if dir.metadata(name)?.is_none() {
                rustix::fs::mkdirat(dir.file(),name,Mode::RWXU).map_err(os)?;
                dir.file().sync_all()?;
            }
            dir=dir.child(name)?;
        }
        private(&dir)?;Ok(dir)
    }"""
    return text[:opening] + body + text[end:]


CASES = [
    ("unknown-governance", "crates/app/src/policy/migration.rs", lambda t: replace(t, "    Classification::Block\n}", "    Classification::Runtime\n}"), "cli", "migration::migration_unknown_governance_target_conflicts_and_special_inputs_block_without_writes"),
    ("old-source-proof", "crates/app/src/migration/migration_scan.rs", lambda t: replace(t, "current.converted.get(&dataset) != ledger.converted.get(&dataset)", "false"), "lib", "migration::migration_tx::tests::changed_source_before_commit_rolls_back_without_creating_authority"),
    ("late-archive-divergence", "crates/app/src/migration/migration_scan.rs", lambda t: replace(t, "current.archives != ledger.archives || ", ""), "cli", "migration::migration_indexes_dependency_lock_and_dirty_link_bytes_without_executing_or_following"),
    ("missing-old-dataset", "crates/app/src/migration/mod.rs", lambda t: replace(t, "    for dataset in crate::policy::governance::DATASETS {", "    for dataset in crate::policy::governance::DATASETS {\n        if matches!(ledger.converted.get(&dataset),Some(Transfer::Missing)){continue;}"), "cli", "migration::migration_verify_validates_new_business_datasets_even_when_originally_missing"),
    ("forward-overwrite", "crates/app/src/migration/migration_tx.rs", lambda t: in_function(t, "fn publish_with(", "rustix::fs::RenameFlags::NOREPLACE", "rustix::fs::RenameFlags::empty()"), "lib", "migration::migration_tx::tests::atomic_no_replace_preserves_last_window_foreign_files_in_both_directions"),
    ("rollback-overwrite", "crates/app/src/migration/migration_tx.rs", lambda t: in_function(t, "fn restore_with(", "rustix::fs::RenameFlags::NOREPLACE", "rustix::fs::RenameFlags::empty()"), "lib", "migration::migration_tx::tests::atomic_no_replace_preserves_last_window_foreign_files_in_both_directions"),
    ("inode-recapture", "crates/app/src/migration/migration_fs.rs", lambda t: in_function(t, "pub fn matches(", " || meta.identity != *id", ""), "lib", "migration::migration_tx::tests::either_lost_or_replaced_registration_record_is_unknown_and_keeps_the_other_blocker"),
    ("lost-record-blessed", "crates/app/src/migration/migration_tx.rs", lambda t: in_function(t, "fn position(", "found.ok_or_else(fs::error)", "Ok(found.unwrap_or(Position::Complete))"), "lib", "migration::migration_tx::tests::either_lost_or_replaced_registration_record_is_unknown_and_keeps_the_other_blocker"),
    ("active-marker-ignored", "crates/app/src/migration/migration_tx.rs", lambda t: in_function(t, "pub fn pending_in(", "[ACTIVE, MARKER]", "[MARKER, MARKER]"), "lib", "migration::migration_tx::tests::either_lost_or_replaced_registration_record_is_unknown_and_keeps_the_other_blocker"),
    ("precommit-source-fence", "crates/app/src/migration/migration_tx.rs", lambda t: in_function(t, "fn publish_ledger(", "        migration_scan::verify_sources(ctx, &self.manifest.ledger)?;\n", ""), "lib", "migration::migration_tx::tests::changed_source_before_commit_rolls_back_without_creating_authority"),
    ("destination-recapture", "crates/app/src/migration/migration_fs.rs", relearn_root, "lib", "migration::migration_tx::tests::frozen_destination_roots_cannot_be_replaced_or_relearned_before_staging"),
]


def build(root, target, kind, log):
    env = dict(os.environ, RUSTC_WRAPPER="", CARGO_TARGET_DIR=str(target))
    selection = ["--lib"] if kind == "lib" else ["--test", "cli"]
    command = ["cargo", "test", *selection, "--all-features", "--locked", "--offline", "--no-run", "--message-format=json"]
    result = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True)
    log.write_text(result.stdout + "\n" + result.stderr)
    assert result.returncode == 0, ("INVALID_BUILD", str(log))
    candidates = []
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            artifact = message["target"]
            if kind == "lib" and "lib" in artifact["kind"] or kind == "cli" and artifact["name"] == "cli":
                candidates.append(Path(message["executable"]))
    assert len(candidates) == 1, candidates
    return candidates[0]


def run(executable, test, log, expected_pass):
    result = subprocess.run([str(executable), test, "--exact"], capture_output=True, text=True, timeout=60)
    output = result.stdout + "\n" + result.stderr
    log.write_text(output)
    assert "running 1 test" in output, ("INVALID_ORACLE", str(log))
    assert (result.returncode == 0) == expected_pass, ("SURVIVED" if not expected_pass else "BASELINE_FAILED", str(log))
    if not expected_pass:
        assert "FAILED" in output and ("assertion" in output or "Error:" in output), ("INVALID_FAILURE", str(log))
    return result.returncode


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--build", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    original = files(args.root)
    args.source.mkdir(exist_ok=False)
    args.output.mkdir(parents=True, exist_ok=False)
    for relative in original:
        destination = args.source / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(args.root / relative, destination)
    baselines = {}
    for kind in ["lib", "cli"]:
        baselines[kind] = build(args.source, args.build, kind, args.output / f"baseline-{kind}-build.log")
    for name, _, _, kind, test in CASES:
        run(baselines[kind], test, args.output / f"{name}-baseline.log", True)
    results = []
    for name, relative, mutate, kind, test in CASES:
        destination = args.source / relative
        correct = destination.read_text()
        wrong = mutate(correct)
        assert correct != wrong
        directory = args.output / name
        directory.mkdir()
        destination.write_text(wrong)
        shutil.copyfile(destination, directory / "mutated.rs")
        executable = build(args.source, args.build, kind, directory / "build.log")
        artifact_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
        shutil.copyfile(executable, directory / "test-artifact")
        code = run(executable, test, directory / "run.log", False)
        results.append({"name": name, "path": relative, "test": test, "kind": kind, "compiled": True, "test_artifact_sha256": artifact_hash, "exit": code, "detected": True})
        destination.write_text(correct)
        assert files(args.source) == original
        print(json.dumps({"name": name, "status": "DETECTED"}), flush=True)
    assert files(args.root) == original
    (args.output / "summary.json").write_text(json.dumps({"status": "PASS", "input_files": original, "count": len(results), "results": results}, indent=2) + "\n")


if __name__ == "__main__":
    main()
