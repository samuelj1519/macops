import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("coverage_gate", SOURCE / "tools/check_coverage.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class CoverageBoundary(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="macops-gate-", dir="/private/tmp")
        self.root = Path(self.temp.name)
        self.policy = json.loads((SOURCE / "quality-policy.json").read_text())
        files = set(self.policy["pure_files"] + self.policy["required_pure"] + self.policy["io_files"] + self.policy["declaration_only"])
        for name in files:
            path = self.root / "crates/app/src" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("// Synthetic source inventory for report validation.\n")
        summary = {metric: {"count": 100, "covered": 100, "percent": 100.0} for metric in gate.METRICS}
        self.report = {"data": [{"files": [{"filename": str(self.root / "crates/app/src" / name), "summary": copy.deepcopy(summary)} for name in sorted(files) if name not in self.policy["declaration_only"]]}]}
        self.addCleanup(self.temp.cleanup)

    def verify(self, report=None, policy=None):
        return gate.validate(self.root, self.report if report is None else report, self.policy if policy is None else policy)

    def test_valid_application_and_canonical_test_paths(self):
        self.assertEqual(self.verify()["failures"], [])
        record = copy.deepcopy(self.report["data"][0]["files"][0])
        record["filename"] = str(self.root / "crates/app/src/../tests/unit/example.rs")
        self.report["data"][0]["files"].append(record)
        self.assertEqual(self.verify()["failures"], [])

    def test_missing_new_duplicate_and_alias_sources_fail(self):
        for variant in ["missing", "duplicate", "alias", "foreign"]:
            report = copy.deepcopy(self.report)
            files = report["data"][0]["files"]
            if variant == "missing":
                files.pop()
            else:
                extra = copy.deepcopy(files[0])
                if variant == "alias":
                    p = Path(extra["filename"])
                    extra["filename"] = str(p.parent / "../src" / p.name)
                if variant == "foreign":
                    extra["filename"] = "/private/tmp/foreign/src.rs"
                files.append(extra)
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                self.verify(report)
        (self.root / "crates/app/src/unclassified.rs").write_text("fn hidden() {}\n")
        with self.assertRaises(ValueError):
            self.verify()

    def test_empty_and_inconsistent_reports_fail(self):
        for report in [{}, {"data": []}, {"data": [{"files": []}]}]:
            with self.assertRaises((ValueError, KeyError)):
                self.verify(report)
        for change in [{"count": 0, "covered": 0, "percent": 0}, {"count": -1}, {"covered": 101}, {"covered": True}, {"percent": float("nan")}, {"percent": 99.99}]:
            report = copy.deepcopy(self.report)
            report["data"][0]["files"][0]["summary"]["lines"].update(change)
            with self.assertRaises(ValueError):
                self.verify(report)

    def test_one_pure_module_cannot_hide_behind_the_aggregate(self):
        report = copy.deepcopy(self.report)
        record = next(f for f in report["data"][0]["files"] if f["filename"].endswith("/domain.rs"))
        record["summary"]["lines"] = {"count": 10000, "covered": 9999, "percent": 99.99}
        result = self.verify(report)
        self.assertTrue(any("pure domain.rs lines" in message for message in result["failures"]))

    def test_effects_and_policy_reductions_fail(self):
        policy = copy.deepcopy(self.policy)
        policy["overall"]["lines"] = 89
        with self.assertRaises(ValueError):
            self.verify(policy=policy)
        source = self.root / "crates/app/src/domain.rs"
        source.write_text("use std::fs;\n")
        with self.assertRaises(ValueError):
            self.verify()

    def test_required_pure_cannot_be_reclassified_or_inline_tests_hidden(self):
        policy = copy.deepcopy(self.policy)
        policy["pure_directories"] = []
        policy["io_files"].extend(policy["required_pure"])
        result = self.verify(policy=policy)
        self.assertEqual(len(result["pure"]), len(self.policy["pure_files"] + self.policy["required_pure"]))
        module = policy["required_pure"][0]
        policy["declaration_only"].append(module)
        with self.assertRaises(ValueError):
            self.verify(policy=policy)
        source = self.root / "crates/app/src/domain.rs"
        for text in ["#[cfg(test)] mod tests { #[test] fn witness() {} }", "#[cfg(test)] fn fixture_helper() -> bool { true }", "use std::{fs}; fn policy() { fs::read(\"x\"); }"]:
            source.write_text(text)
            with self.assertRaises(ValueError):
                self.verify()
        source.write_text("// restored\n")
        (self.root / "crates/app/src/cli.rs").write_text("fn hidden() {}\n")
        with self.assertRaises(ValueError):
            self.verify()


if __name__ == "__main__":
    unittest.main()
