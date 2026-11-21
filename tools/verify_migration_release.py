#!/usr/bin/env python3
"""Exercise one exact macops artifact with synthetic data and original recovery fixtures."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot(root):
    result = {}
    for path in [root, *sorted(root.rglob("*"))]:
        info = path.lstat()
        value = {"mode": stat.S_IMODE(info.st_mode), "inode": info.st_ino, "device": info.st_dev}
        if path.is_symlink():
            value["link"] = os.readlink(path)
        elif path.is_file():
            value["sha256"] = digest(path)
        result[str(path.relative_to(root))] = value
    return result


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value)


class Runner:
    def __init__(self, binary):
        self.binary = binary.resolve(strict=True)
        self.results = []
        self.fixture_roots = []
        self.environment = {"HOME": os.environ["HOME"], "PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"}

    def call(self, home, args, expected=0, extra=()):
        command = [str(self.binary), "--home", str(home), "--json", *extra, *args]
        result = subprocess.run(command, env=self.environment, capture_output=True, text=True, timeout=15)
        value = json.loads(result.stdout)
        self.results.append({"home": str(home), "args": args, "extra": list(extra), "exit": result.returncode, "response": value})
        assert result.returncode == expected, (command, result.returncode, value, result.stderr)
        return value


def new_home(runner):
    retained = Path(tempfile.mkdtemp(prefix="macops-migration-release-", dir="/private/tmp"))
    runner.fixture_roots.append(str(retained))
    home = retained / "home"
    home.mkdir(mode=0o700)
    settings = home / ".local/settings"
    values = {
        ".agents/managed-assets.json": '{"vendor":{"version":"1","unknown":true}}\n',
        ".agents/invocation-overlays.json": '{"schema_version":1,"files":{}}\n',
        ".agents/cache/managed-updates.json": '{"legacy_cache":{"revision":"1"}}\n',
        ".local/settings/manifests/skill-profiles.json": '{}\n',
        ".local/settings/manifests/managed-agent-assets.json": '{"schema_version":1,"assets":{}}\n',
        ".local/settings/manifests/agent-invocation-policy.json": '{"schema_version":1,"standalone_manual_only":[]}\n',
        ".local/settings/manifests/agent-source-channels.json": '{"schema_version":1,"source_ids":{}}\n',
        ".local/settings/manifests/mac-apps.tsv": "path\tbundle_id\tversion\towner\tsource\n",
        ".local/settings/dirty.txt": "preserve dirty source\n",
        ".local/settings/Cargo.lock": "a dependency lock, not a running writer\n",
    }
    for relative, content in values.items():
        write(home / relative, content)
    (home / ".agents/skill-profiles.json").symlink_to("../.local/settings/manifests/skill-profiles.json")
    protected = []
    for index, profile in enumerate([".claude", ".claude-mi", ".claude-kimi", ".claude-glm", ".claude-zen"]):
        path = home / profile / "settings.json"
        write(path, json.dumps({"env": {"ANTHROPIC_MODEL": f"custom-{index}"}, "unknown": True}) + "\n")
        protected.append(path)
    for relative in [".codex/config.toml", ".local/share/mise/installs/claude-code/1/bin/claude", ".local/share/mise/installs/claude-code/2/bin/claude"]:
        path = home / relative
        write(path, f"untouched sentinel: {relative}\n")
        protected.append(path)
    protected_before = {str(p): (p.read_bytes(), p.stat().st_ino, stat.S_IMODE(p.stat().st_mode)) for p in protected}
    before = snapshot(home)
    preview = runner.call(home, ["migrate", "preflight"])
    assert preview["data"]["ready"] is True
    assert snapshot(home) == before
    runner.call(home, ["migrate", "run", "--dry-run"])
    assert snapshot(home) == before
    unknown = home / ".agents/unknown-governance.json"
    unknown.write_text("{}")
    blocked = runner.call(home, ["migrate", "preflight"], expected=3)
    assert blocked["data"]["ready"] is False
    runner.call(home, ["migrate", "run", "--yes"], expected=3)
    assert not (home / ".local/state/macops").exists()
    unknown.unlink()
    original = {str(home / p): (home / p).read_bytes() for p in values}
    result = runner.call(home, ["migrate", "run", "--yes"])
    operation = result["data"]["migration_id"]
    state = home / ".local/state/macops"
    assert not (state / "migration-active.json").exists()
    assert not (state / "migration.pending.json").exists()
    assert (state / "managed-assets.json").read_bytes() == values[".agents/managed-assets.json"].encode()
    assert stat.S_IMODE((state / "managed-assets.json").stat().st_mode) == 0o600
    runner.call(home, ["migrate", "verify"])
    write(state / "managed-assets.json", '{"new_business":{"revision":"2"}}\n')
    runner.call(home, ["migrate", "verify"])
    runner.call(home, ["migrate", "run"])
    runner.call(home, ["migrate", "recover", operation, "--yes"])
    for name, content in original.items():
        assert Path(name).read_bytes() == content
    for name, value in protected_before.items():
        path = Path(name)
        assert (path.read_bytes(), path.stat().st_ino, stat.S_IMODE(path.stat().st_mode)) == value
    (home / ".agents/cache/managed-updates.json").unlink()
    (home / ".cache/macops/managed-updates.json").unlink()
    changed_cache = runner.call(home, ["migrate", "verify"], extra=["--cache-dir", str(retained / "fresh-cache")])
    assert changed_cache["data"]["cache_miss"] is True
    write(settings / "dirty.txt", "late source divergence\n")
    divergent = runner.call(home, ["migrate", "verify"], expected=3)
    assert divergent["error"]["code"] == "migration_source_diverged"
    assert (state / "managed-assets.json").read_text() == '{"new_business":{"revision":"2"}}\n'
    return str(retained)


def recovery(runner, fixture):
    home = Path(fixture["home"])
    original = {p["dataset"]: Path(p["source"]).read_bytes() for p in fixture["copied"]}
    source_files = {p["source"]: (Path(p["source"]).read_bytes(), Path(p["source"]).stat().st_ino, stat.S_IMODE(Path(p["source"]).stat().st_mode)) for p in fixture["copied"]}
    root = home / ".local/state/macops"
    before = snapshot(home)
    runner.call(home, ["migrate", "recover", fixture["id"], "--dry-run"])
    assert snapshot(home) == before
    result = runner.call(home, ["migrate", "recover", fixture["id"], "--yes"])
    assert result["data"]["result"] == ("committed" if fixture["committed"] else "rolled-back")
    for item in fixture["targets"]:
        path = Path(item["target"]) if fixture["committed"] else Path(fixture["stage"]) / item["slot"]
        assert path.read_bytes() == original[item["dataset"]]
        assert path.stat().st_ino == item["identity"]["inode"]
        if not fixture["committed"]:
            assert not Path(item["target"]).exists()
    for source, value in source_files.items():
        path = Path(source)
        assert (path.read_bytes(), path.stat().st_ino, stat.S_IMODE(path.stat().st_mode)) == value
    assert not (root / "migration-active.json").exists()
    assert not (root / "migration.pending.json").exists()
    runner.call(home, ["migrate", "recover", fixture["id"], "--yes"])
    if fixture["committed"]:
        runner.call(home, ["migrate", "verify"])
        missing = runner.call(home, ["migrate", "recover", "OP-999-999", "--yes"], expected=3)
        assert missing["error"]["code"] == "migration_not_found"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--fixtures", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    runner = Runner(args.binary)
    windows = []
    status = "FAIL"
    try:
        new_home(runner)
        if args.fixtures:
            for fixture in json.loads(args.fixtures.read_text()):
                recovery(runner, fixture)
                windows.append(fixture["window"])
        status = "PASS"
    finally:
        report = {"status": status, "binary": str(runner.binary), "binary_sha256": digest(runner.binary), "retained_fixtures": runner.fixture_roots, "recovery_windows": windows, "calls": len(runner.results), "results": runner.results}
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "results"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
