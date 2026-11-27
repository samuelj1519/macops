import copy
import os
import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path

TOOLS = Path(__file__).resolve().parents[1]
require_successful_main = runpy.run_path(str(TOOLS / "release.py"))["require_successful_main"]
compare_baseline = runpy.run_path(str(TOOLS / "check_coverage.py"))["compare_baseline"]


class WorkflowTests(unittest.TestCase):
    def test_focused_checks_select_the_target_and_isolate_coverage(self):
        with tempfile.TemporaryDirectory() as raw:
            area = Path(raw)
            command = area / "cargo"
            command.write_text(
                '#!/bin/sh\nprintf "%s|%s|%s|%s|%s\\n" "$RUST_TEST_THREADS" '
                '"$CARGO_TARGET_DIR" "$CARGO_LLVM_COV_TARGET_DIR" "$CARGO_LLVM_COV_BUILD_DIR" "$*"\n'
            )
            command.chmod(0o700)
            env = {
                **os.environ, "PATH": f'{area}:{os.environ["PATH"]}',
                "CARGO_TARGET_DIR": str(area / "target"),
                "CARGO_LLVM_COV_TARGET_DIR": "formal-target", "CARGO_LLVM_COV_BUILD_DIR": "formal-build",
            }

            def check(*args):
                return subprocess.run(
                    ["bash", str(TOOLS / "check.sh"), *args], env=env,
                    capture_output=True, text=True, timeout=10,
                )

            flags = "--workspace --all-features --locked --offline"
            for args, threads, build, invoked in [
                (("focus", "lib", "terminal::"), 1, "target", f"test {flags} --lib -- terminal::"),
                (("focus-coverage", "cli", "skill_browser"), 4, "target/focus-coverage", f"llvm-cov {flags} --test cli --show-missing-lines -- skill_browser"),
            ]:
                with self.subTest(args=args):
                    result = check(*args)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    coverage = str(area / build / "llvm-cov-target")
                    cov_dirs = f"{coverage}|{coverage}" if args[0] == "focus-coverage" else "formal-target|formal-build"
                    self.assertEqual(result.stdout.strip(), f"{threads}|{area / build}|{cov_dirs}|{invoked}")
            for args in [("focus", "lib"), ("focus", "lib", ""), ("focus", "cli", "--list"), ("focus-coverage", "any", "term")]:
                with self.subTest(args=args):
                    result = check(*args)
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(result.stdout, "")

    def test_release_requires_both_successful_jobs_on_exact_main_push(self):
        commit = "a" * 40
        info = {
            "headSha": commit, "headBranch": "main", "event": "push",
            "status": "completed", "conclusion": "success",
            "jobs": [{"name": name, "conclusion": "success"} for name in ("checks", "quality")],
        }
        require_successful_main(info, commit)
        for fields in (
            {"headSha": "b" * 40}, {"headBranch": "feature"}, {"event": "pull_request"},
            {"status": "in_progress"}, {"conclusion": "failure"}, {"jobs": info["jobs"][:1]},
            {"jobs": [*info["jobs"][:1], {"name": "quality", "conclusion": "skipped"}]},
            {"jobs": [*info["jobs"], info["jobs"][0]]},
        ):
            with self.subTest(fields=fields), self.assertRaises(ValueError):
                require_successful_main({**info, **fields}, commit)

    def test_coverage_comparison_keeps_baseline_gate_and_all_metrics(self):
        baseline = {"failures": [], "overall": {"lines": 90, "functions": 92, "regions": 85}}
        candidate = copy.deepcopy(baseline)
        compare_baseline(candidate, baseline)
        self.assertEqual(candidate["failures"], [])
        candidate["overall"]["regions"] = 84.99
        compare_baseline(candidate, baseline)
        self.assertIn("< baseline 85.0000%", candidate["failures"][0])
        with self.assertRaises(ValueError):
            compare_baseline(candidate, {**baseline, "failures": ["failed source gate"]})
        with self.assertRaises(ValueError):
            compare_baseline(candidate, {**baseline, "overall": {**baseline["overall"], "lines": float("nan")}})

    def test_integrate_only_fast_forwards_after_both_checks_and_comparison(self):
        for failure in ("", "full", "compare", "move-main"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as raw:
                area = Path(raw).resolve()
                repo = area / "repo"
                repo.mkdir()
                env = {**os.environ, "VERIFY_LOG": str(area / "calls"), "FAIL_CHECK": failure, "LIVE_REPO": str(repo)}

                def git(*args):
                    return subprocess.check_output(["git", *args], cwd=repo, text=True).strip()

                git("init", "--quiet", "-b", "main")
                git("config", "core.hooksPath", str(area / "empty-hooks"))
                git("config", "user.name", "Fixture")
                git("config", "user.email", "fixture@example.org")
                git("commit", "--quiet", "--allow-empty", "-m", "base")
                base = git("rev-parse", "HEAD")
                git("switch", "--quiet", "-c", "task")
                (repo / "tools").mkdir()
                (repo / "tools/check.sh").write_text(
                    '#!/bin/bash\nset -eu\ncd "${2:-$(dirname "$0")/..}"\n'
                    'printf "%s %s\\n" "$1" "$(git rev-parse HEAD)" >> "$VERIFY_LOG"\n'
                    'test "$1" != "$FAIL_CHECK"\n'
                    'if [[ "$1" = full && "$FAIL_CHECK" = move-main ]]; then\n'
                    '    git -C "$LIVE_REPO" update-ref refs/heads/main "$(git rev-parse HEAD)"\n'
                    'fi\n'
                )
                (repo / "tools/check_coverage.py").write_text(
                    'import os\nwith open(os.environ["VERIFY_LOG"], "a") as log:\n    log.write("compare\\n")\n'
                    'raise SystemExit(os.environ["FAIL_CHECK"] == "compare")\n'
                )
                git("add", "tools")
                git("commit", "--quiet", "-m", "candidate")
                candidate = git("rev-parse", "HEAD")
                result = subprocess.run(
                    ["bash", str(TOOLS / "integrate.sh"), candidate], cwd=repo, env=env,
                    capture_output=True, text=True, timeout=20,
                )
                calls = [f"coverage {base}", f"full {candidate}"]
                if failure != "full":
                    calls.append("compare")
                self.assertEqual((area / "calls").read_text().splitlines(), calls)
                self.assertEqual(result.returncode == 0, not failure, result.stderr)
                self.assertEqual(git("branch", "--show-current"), "task" if failure else "main")
                current_main = candidate if failure in ("", "move-main") else base
                self.assertEqual(git("rev-parse", "main"), current_main)
