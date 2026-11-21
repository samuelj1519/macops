#!/usr/bin/env python3
"""Check tracked public source without printing matching values."""
import os
from pathlib import Path
import re
import subprocess
import sys

TOKEN = re.compile(
    r"\b(?:gh[pousr]_[A-Za-z0-9]{25,}|github_pat_[A-Za-z0-9_]{30,}|"
    r"sk-(?:proj-)?[A-Za-z0-9_-]{28,}|(?:AKIA|ASIA)[A-Z0-9]{16}|"
    r"xox[baprs]-[A-Za-z0-9-]{20,}|AIza[A-Za-z0-9_-]{35})\b"
    r"|-----BEGIN [A-Z ]*PRIVATE KEY-----"
)
EMAIL = re.compile(r"(?<![\w@])[\w.%+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})")
LINUX_HOME = re.compile(r"(?<![\w/}])/home/([A-Za-z0-9][\w.-]*)/")
USER_HOME = re.compile(r'''(?<![\w/}])/Users/([\w.-]+)''', re.I)
CREDENTIAL_URL = re.compile(r"https?://[^/\s@]+:[^/\s@]+@[A-Za-z0-9][A-Za-z0-9.-]*")
EXAMPLE_DOMAINS = {"example.com", "example.net", "example.org", "users.noreply.github.com"}
EXAMPLE_ACCOUNTS = {"example", "another-account"}
FIXTURE_URLS = {"https://user:secret@" + "registry.npmjs.org", "https://user:SECRET@" + "host"}


def findings(name, lines):
    test_fixture = name.startswith("crates/app/tests/")
    for number, source in enumerate(lines, 1):
        for value in FIXTURE_URLS if test_fixture else ():
            source = source.replace(value, "")
        if TOKEN.search(source):
            yield number, "credential pattern"
        if CREDENTIAL_URL.search(source):
            yield number, "credential in URL"
        if any(m[1].lower() not in EXAMPLE_DOMAINS and m[0].lower() != "git@github.com" for m in EMAIL.finditer(source)):
            yield number, "personal email"
        if (not test_fixture and LINUX_HOME.search(source)) or any(not test_fixture or m[1].lower() not in EXAMPLE_ACCOUNTS for m in USER_HOME.finditer(source)):
            yield number, "literal account path"


def main():
    root = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
    files = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).split(b"\0")
    failed = False
    for name in sorted({os.fsdecode(path) for path in files if path}):
        path = root / name
        if (
            path.is_symlink()
            or any(part in {"artifacts", "target", ".ssh", ".codex", ".claude"} for part in Path(name).parts)
            or path.name == ".env"
            or path.name.startswith(".env.")
            or path.suffix.lower() in {".pem", ".key", ".p12", ".pfx", ".sqlite", ".db"}
        ):
            print(f"{name}: local data or sensitive file type", file=sys.stderr)
            failed = True
            continue
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError):
            print(f"{name}: unreadable or non-text file", file=sys.stderr)
            failed = True
            continue
        for line, reason in findings(name, lines):
            print(f"{name}:{line}: {reason}", file=sys.stderr)
            failed = True
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
