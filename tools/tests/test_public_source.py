import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from check_public_source import findings


class PublicSourceTests(unittest.TestCase):
    def test_reports_sensitive_shape_without_echoing_the_value(self):
        token = "ghp" + "_" + "a" * 32
        self.assertEqual(list(findings("README.md", [token])), [(1, "credential pattern")])

    def test_rejects_literal_user_paths_outside_fixtures(self):
        path = "/Users" + "/example/Documents"
        self.assertEqual(list(findings("README.md", [path])), [(1, "literal account path")])
        self.assertEqual(list(findings("crates/app/tests/cli.rs", [path])), [])

    def test_fixture_exceptions_do_not_hide_arbitrary_credentials(self):
        url = "https://example:other@" + "invalid.test/path"
        reasons = {reason for _, reason in findings("crates/app/tests/cli.rs", [url])}
        self.assertIn("credential in URL", reasons)
