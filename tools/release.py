#!/usr/bin/env python3
"""Verify the main CI run for a release or check the published archive."""
import argparse
import json
import re
import subprocess
import tarfile
import tempfile
import time
from pathlib import Path


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def output(*args):
    return run(*args, stdout=subprocess.PIPE).stdout.strip()


def require_successful_main(run_info, commit):
    expected = {"headSha": commit, "headBranch": "main", "event": "push",
                "status": "completed", "conclusion": "success"}
    if any(run_info.get(field) != value for field, value in expected.items()):
        raise ValueError("CI must pass on the exact main push")
    jobs = run_info["jobs"]
    for name in ("checks", "quality"):
        matching = [job for job in jobs if job["name"] == name]
        if len(matching) != 1 or matching[0].get("conclusion") != "success":
            raise ValueError(f"main CI job did not pass: {name}")


def require_ci(repo, commit):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("a full commit SHA is required")
    for _ in range(12):
        runs = json.loads(output("gh", "run", "list", "--repo", repo, "--workflow", "ci.yml",
                                 "--branch", "main", "--event", "push", "--commit", commit,
                                 "--limit", "1", "--json", "databaseId"))
        if runs:
            run_id = str(runs[0]["databaseId"])
            break
        time.sleep(5)
    else:
        raise ValueError("no main CI run for this commit")

    run("gh", "run", "watch", run_id, "--repo", repo, "--interval", "15", "--exit-status")
    info = json.loads(output("gh", "run", "view", run_id, "--repo", repo,
                             "--json", "headSha,headBranch,event,status,conclusion,jobs,url"))
    require_successful_main(info, commit)
    print(info["url"])


def verify(repo, tag):
    if not re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", tag):
        raise ValueError("a version tag such as v0.4.1 is required")
    ref = f"refs/tags/{tag}"
    commit = output("git", "rev-parse", "--verify", ref + "^{commit}")
    refs = dict(tuple(line.split()[::-1]) for line in output(
        "git", "ls-remote", f"https://github.com/{repo}.git", ref, ref + "^{}").splitlines())
    if refs.get(ref + "^{}", refs.get(ref)) != commit:
        raise ValueError("local and published tags refer to different commits")
    archive = f"macops-{tag}-darwin-arm64.tar.gz"
    entry = json.loads(output("gh", "release", "view", tag, "--repo", repo,
                              "--json", "tagName,url,isDraft,isPrerelease,assets"))
    if (entry["tagName"] != tag or entry["isDraft"] or entry["isPrerelease"]
            or {asset["name"] for asset in entry["assets"]} != {archive, "SHA256SUMS"}):
        raise ValueError("unexpected release metadata or assets")
    with tempfile.TemporaryDirectory(prefix="macops-release-") as scratch:
        root = Path(scratch).resolve()
        run("gh", "release", "download", tag, "--repo", repo, "--dir", str(root),
            "--pattern", archive, "--pattern", "SHA256SUMS")
        checksum = (root / "SHA256SUMS").read_text().splitlines()
        if len(checksum) != 1 or not re.fullmatch(r"[0-9a-f]{64}  " + re.escape(archive), checksum[0]):
            raise ValueError("unexpected release checksum entries")
        run("shasum", "-a", "256", "-c", "SHA256SUMS", cwd=root)
        with tarfile.open(root / archive, "r:gz") as tar:
            members = tar.getmembers()
            if sorted(m.name for m in members) != ["LICENSE", "macops"] or any(not m.isfile() for m in members):
                raise ValueError("unexpected release archive contents")
            binary = root / "macops"
            binary.write_bytes(tar.extractfile("macops").read())
            binary.chmod(0o700)
        if "Mach-O 64-bit executable arm64" not in output("file", "-b", str(binary)):
            raise ValueError("unexpected release architecture")
        if output(str(binary), "--version") != f"macops {tag[1:]}":
            raise ValueError("binary version does not match release")
        home = root / "home"
        home.mkdir()
        probe = json.loads(output("env", "-i", f"HOME={home}", "PATH=/usr/bin:/bin",
                                  str(binary), "--json", "migrate", "preflight"))
        if probe["status"] != "ok" or probe["data"]["read_only"] is not True or any(home.iterdir()):
            raise ValueError("release binary failed the read-only CLI probe")
    print(entry["url"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("require-ci", "verify"))
    parser.add_argument("subject")
    parser.add_argument("--repo", required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        parser.error("--repo requires OWNER/REPO")
    try:
        action = require_ci if args.action == "require-ci" else verify
        action(args.repo, args.subject)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, tarfile.TarError) as error:
        parser.exit(1, f"release verification: {error}\n")


if __name__ == "__main__":
    main()
