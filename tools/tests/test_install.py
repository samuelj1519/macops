import os
import platform
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WRAPPER = b'#!/bin/sh\nexec "$HOME/.local/libexec/macops" "$@"\n'


@unittest.skipUnless(
    platform.system() == "Darwin" and platform.machine() == "arm64",
    "installer targets Apple Silicon macOS",
)
class InstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        releases = tempfile.TemporaryDirectory(prefix="macops-test-releases-")
        cls.addClassCleanup(releases.cleanup)
        cls.releases = Path(releases.name)
        for version in ["1.2.3", "1.2.4"]:
            binary = cls.releases / "macops"
            subprocess.run(
                ["/usr/bin/cc", "-x", "c", "-o", binary, "-"],
                input=f'#include <stdio.h>\nint main(void) {{ puts("macops {version}"); }}\n',
                text=True,
                check=True,
            )
            subprocess.run(
                ["/bin/sh", ROOT / "tools/package_release.sh", f"v{version}", binary, cls.releases / version],
                capture_output=True,
                check=True,
            )

    def setUp(self):
        sandbox = tempfile.TemporaryDirectory(prefix="macops-installer-test-")
        self.addCleanup(sandbox.cleanup)
        root = Path(sandbox.name)
        self.home = root / "fixture home"
        self.home.mkdir()
        self.bin = self.home / ".local/bin"
        self.target = self.home / ".local/libexec/macops"
        self.assets = root / "assets"
        commands = root / "commands"
        commands.mkdir()
        curl = commands / "curl"
        curl.write_text(
            '#!/bin/sh\n'
            'set -eu\n'
            'while [ "$1" != --output ]; do shift; done\n'
            'case "$3" in\n'
            '  https://github.com/samuelj1519/macops/releases/latest/download/SHA256SUMS) name=SHA256SUMS ;;\n'
            '  "https://github.com/samuelj1519/macops/releases/download/$TEST_TAG/macops-$TEST_TAG-darwin-arm64.tar.gz") name=${3##*/} ;;\n'
            '  *) exit 23 ;;\n'
            'esac\n'
            'printf "%s\\n" "$3" >> "$TEST_REQUESTS"\n'
            'cp "$TEST_ASSETS/$name" "$2"\n'
        )
        curl.chmod(0o755)
        self.requests = root / "requests"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "TMPDIR": sandbox.name,
            "PATH": str(commands) + ":/usr/bin:/bin",
            "TEST_REQUESTS": str(self.requests),
            "TEST_ASSETS": str(self.assets),
        }
        self.prepare_release("1.2.3")

    def prepare_release(self, version):
        if self.assets.exists():
            shutil.rmtree(self.assets)
        shutil.copytree(self.releases / version, self.assets)
        self.env["TEST_TAG"] = f"v{version}"

    def install(self):
        return subprocess.run(
            [ROOT / "install.sh", "--release"], env=self.env, capture_output=True, text=True,
        )

    def assert_version(self, version):
        for name in ["mo", "macops"]:
            actual = subprocess.check_output([self.bin / name, "--version"], env=self.env, text=True)
            self.assertEqual(actual.strip(), f"macops {version}")

    def test_fresh_install_and_update_preserve_existing_wrappers(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('export PATH="$HOME/.local/bin:$PATH"', result.stdout)
        self.assert_version("1.2.3")
        for name in ["mo", "macops"]:
            path = self.bin / name
            self.assertEqual(path.readlink(), Path("../libexec/macops"))
            path.unlink()
            path.write_bytes(WRAPPER)
            path.chmod(0o755)
        self.prepare_release("1.2.4")
        for _ in range(2):
            result = self.install()
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version("1.2.4")
        self.assertEqual((self.bin / "mo").read_bytes(), WRAPPER)
        self.assertEqual((self.bin / "macops").read_bytes(), WRAPPER)

    def test_checksum_mismatch_preserves_working_install(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        original = self.target.read_bytes()
        self.prepare_release("1.2.4")
        (self.assets / "macops-v1.2.4-darwin-arm64.tar.gz").write_bytes(b"damaged download")
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("校验失败", result.stderr)
        self.assertEqual(self.target.read_bytes(), original)
        self.assert_version("1.2.3")

    def test_conflicts_stop_before_download(self):
        self.bin.mkdir(parents=True)
        alias = self.bin / "mo"
        alias.write_bytes(b"another command")
        collision = self.install()
        self.assertNotEqual(collision.returncode, 0)
        self.assertIn("命令名已经被占用", collision.stderr)
        self.assertEqual(alias.read_bytes(), b"another command")
        self.assertFalse(self.target.exists())
        alias.unlink()

        self.target.parent.mkdir()
        self.target.write_bytes(b"another program")
        unclaimed = self.install()
        self.assertNotEqual(unclaimed.returncode, 0)
        self.assertIn("找不到对应的 mo 或 macops 命令", unclaimed.stderr)
        self.assertEqual(self.target.read_bytes(), b"another program")
        self.assertFalse(self.requests.exists())

    def test_partial_link_install_can_be_retried(self):
        self.bin.mkdir(parents=True)
        (self.bin / "mo").symlink_to("../libexec/macops")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version("1.2.3")
