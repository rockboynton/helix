"""Validate disposable acceptance fixtures without touching a user's repos."""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_file_location("fixture", Path(__file__).with_name("neohelix-test-fixture.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


@unittest.skipUnless(shutil.which("git"), "git required")
class FixtureTests(unittest.TestCase):
    def make(self, with_jj=False):
        root = fixture.create(with_jj)
        self.addCleanup(shutil.rmtree, root)
        return root

    def test_git_snapshots_and_media(self):
        root = self.make()
        status = fixture.run(root / "git", "git", "status", "--short")
        for name in ["src/lib.rs", "src/added.rs", "obsolete.md", "src/old_name.rs", "src/renamed.rs"]:
            self.assertIn(name, status)
        self.assertTrue((root / "before/obsolete.md").is_file())
        self.assertFalse((root / "after/obsolete.md").exists())
        self.assertIn("hello {name}", (root / "before/src/lib.rs").read_text())
        self.assertIn("welcome {name}", (root / "after/src/lib.rs").read_text())
        self.assertTrue((root / "media/sample.png").read_bytes().startswith(b"\x89PNG\r\n\x1a\n"))
        self.assertEqual(fixture.run(root / "git", "git", "remote"), "")

    def test_cli_optional_large_file(self):
        result = subprocess.run([sys.executable, str(Path(fixture.__file__)), "--without-jj", "--large-mib", "1"],
                                check=True, capture_output=True, text=True)
        root = Path(result.stdout.strip())
        self.addCleanup(shutil.rmtree, root)
        self.assertEqual((root / "large.txt").stat().st_size, 1024 * 1024)
        self.assertFalse((root / "jj").exists())

    @unittest.skipUnless(shutil.which("jj"), "jj required")
    def test_native_jj_patch_and_history(self):
        root = self.make(with_jj=True)
        self.assertIn("Modified regular file src/lib.rs:", (root / "patches/jj.patch").read_text())
        history = fixture.run(root / "jj", "jj", "--config", 'signing.behavior="drop"',
                              "log", "--no-graph", "-r", "@ | @-", "-T", 'description ++ "\\n"')
        self.assertIn("Fixture baseline", history)

    @unittest.skipUnless(os.environ.get("HX"), "set HX to check both patch layouts with the real renderer")
    def test_generated_patches_render(self):
        root = self.make(with_jj=bool(shutil.which("jj")))
        repo = Path(__file__).resolve().parents[1]
        user = root / "config/helix"
        user.mkdir(parents=True)
        (user / "helix.scm").write_text("(provide configure!) (define (configure!) #t)")
        (user / "init.scm").write_text('(require "helix.scm") (configure!)')
        env = dict(os.environ, HELIX_STEEL_CONFIG=str(user), HELIX_RUNTIME=str(repo / "runtime"),
                   XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"),
                   XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
                   STEEL_HOME=str(root / "steel"), HELIX_DIFF_CACHE="0", HELIX_DIFF_WORKER="0")
        for patch in sorted((root / "patches").glob("*.patch")):
            for layout in ["inline", "side-by-side"]:
                result = subprocess.run([str(Path(os.environ["HX"]).resolve()), "--render-diff", "--patch",
                                         "--diff-layout", layout, "--color", "never", str(patch)],
                                        cwd=root, env=env, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, (patch.name, layout, result.stderr))
                self.assertIn("welcome", result.stdout)


if __name__ == "__main__":
    unittest.main()
