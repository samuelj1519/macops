"""Replay UV discovery namespace drift against an absolute compiled artifact."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def snapshot(root):
    if not root.exists():
        return None
    result = {}
    for path in [root, *sorted(root.rglob("*"))]:
        meta = path.lstat()
        content = os.readlink(path) if path.is_symlink() else (
            hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
        )
        result[str(path.relative_to(root))] = (meta.st_mode, meta.st_mtime_ns, content)
    return result


def fixture(tool_dir, bin_dir, version):
    payload = tool_dir / "demo-tool"
    dist = payload / f"lib/python3.12/site-packages/demo_tool-{version}.dist-info"
    dist.mkdir(parents=True)
    (payload / "bin").mkdir()
    bin_dir.mkdir(parents=True)
    (dist / "METADATA").write_text(f"Name: demo-tool\nVersion: {version}\n")
    (payload / "pyvenv.cfg").write_text("home = /synthetic-python\n")
    (payload / "bin/demo-tool").write_text("#!/synthetic-python\nexit 99\n")
    (bin_dir / "demo-tool").symlink_to(payload / "bin/demo-tool")
    (payload / "uv-receipt.toml").write_text(
        "[tool]\nrequirements=[{name='demo-tool'}]\n"
        f"entrypoints=[{{name='demo-tool',install-path='{bin_dir}/demo-tool'}}]\n"
    )


def declaration(owner, tool_dir, bin_dir):
    return {
        "id": owner, "name": "Demo", "kind": "cli", "official_url": "https://example.org",
        "provider": {"type": "uv", "package": "Demo_Tool", "binding": {
            "tool_dir": str(tool_dir), "bin_dir": str(bin_dir),
        }},
    }


def run_case(binary, declared, change, events):
    with tempfile.TemporaryDirectory(prefix="macops-uv-discovery-", dir="/private/tmp") as temporary:
        root = Path(temporary)
        home, state, cache = root / "home", root / "state", root / "cache"
        (root / "settings/manifests").mkdir(parents=True)
        tool_dir, bin_dir = home / ".local/share/uv/tools", home / ".local/bin"
        other_dir, other_bin = home / "B/tools", home / "B/bin"
        fixture(tool_dir, bin_dir, "1")
        fixture(other_dir, other_bin, "2")
        fake_bin = home / "executables"
        fake_bin.mkdir()
        sentinel = home / "native-called"
        for name in ["uv", "python", "python3", "keyring"]:
            path = fake_bin / name
            path.write_text(f"#!/bin/sh\nprintf called >> '{sentinel}'\nexit 99\n")
            path.chmod(0o755)
        catalog = root / "catalog.json"
        packages = [declaration("owner:b", other_dir, other_bin)]
        if declared:
            packages.append(declaration("owner:a", tool_dir, bin_dir))
        catalog.write_text(json.dumps({"schema_version": 1, "packages": packages}))

        def run(arguments, expected=0):
            command = [str(binary), "--json", "--home", str(home), "--settings", str(root / "settings"),
                       "--state-dir", str(state), "--cache-dir", str(cache), "--catalog", str(catalog),
                       *arguments]
            result = subprocess.run(command, capture_output=True, text=True, timeout=4,
                                    env={"HOME": "/synthetic-ambient-home", "PATH": f"{fake_bin}:/usr/bin:/bin"})
            value = json.loads(result.stdout)
            events.append({"declared": declared, "change": change, "args": arguments,
                           "code": result.returncode, "value": value})
            assert result.returncode == expected, (arguments, result.returncode, value, result.stderr)
            assert not sentinel.exists()
            return value

        def item(value, identity):
            rows = [row for row in value["data"]["packages"] if row["id"] == identity]
            assert len(rows) == 1, rows
            return rows[0]

        before_tree = snapshot(home)
        before = run(["discover", "uv"])
        identity = next(row["id"] for row in before["data"]["packages"] if row["scope"] == str(tool_dir))
        assert item(before, identity)["status"] == "present"
        assert item(before, identity)["management"] == "native"
        assert snapshot(home) == before_tree and not state.exists() and not cache.exists()
        run(["adopt", identity, "--yes"])
        receipt_bytes = (state / "receipts.json").read_bytes()
        stable_state = snapshot(state)
        stable = item(run(["discover", "uv"]), identity)
        assert stable["status"] == "present" and stable["management"] == "adopted"
        assert stable["baseline"]["comparison"] == "same"
        old = home / "old-namespace"
        source = tool_dir if change.startswith("root") else bin_dir if change.startswith("bin") else tool_dir / "demo-tool"
        source.rename(old)
        if change == "root-replaced":
            tool_dir.mkdir()
            (old / "demo-tool").rename(tool_dir / "demo-tool")
        elif change == "bin-replaced":
            bin_dir.mkdir()
            (old / "demo-tool").rename(bin_dir / "demo-tool")
        changed_tree = snapshot(home)
        value = run(["discover", "uv"])
        current = item(value, identity)
        assert current["status"] == "unknown", "DISCOVER_REBOUND_ADOPTED_NAMESPACE"
        assert current["management"] == "adopted" and current["releases"] == []
        assert current["baseline"]["comparison"] == "unknown"
        assert any(report["roots"] == [str(tool_dir), str(bin_dir)] and report["state"] == "invalid"
                   for report in value["data"]["coverage"])
        assert any(report["roots"] == [str(other_dir), str(other_bin)] and report["state"] == "complete"
                   for report in value["data"]["coverage"])
        assert any(row["scope"] == str(other_dir) and row["status"] == "present" for row in value["data"]["packages"])
        listed = run(["list", "--kind", "applications"], 3)
        assert next(row for row in listed["data"]["resources"] if row["id"] == identity)["status"] == "unknown"
        owner = "owner:a" if declared else identity
        assert run(["doctor", owner], 3)["data"][0]["observation"]["status"] == "unknown"
        run(["--dry-run", "adopt", identity], 3)
        assert snapshot(home) == changed_tree and snapshot(state) == stable_state
        assert (state / "receipts.json").read_bytes() == receipt_bytes and not cache.exists()
        assert not (tool_dir / ".lock").exists() and not (other_dir / ".lock").exists()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    assert arguments.binary.is_absolute() and arguments.binary.is_file()
    events = []
    for declared in [False, True]:
        for change in ["root-replaced", "bin-replaced", "root-missing", "bin-missing", "tool-missing"]:
            run_case(arguments.binary, declared, change, events)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps({"binary": str(arguments.binary), "sha256": hashlib.sha256(
        arguments.binary.read_bytes()).hexdigest(), "events": events, "native_calls": 0,
        "state_and_payload_unchanged_by_reads": True}, indent=2) + "\n")
    print(f"PASS {len(events)} calls; 10 owner/namespace cases; zero native calls; frozen receipts retained")


if __name__ == "__main__":
    main()
