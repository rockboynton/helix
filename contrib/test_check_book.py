"""Regression tests for the offline mdBook link checker."""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("check_book", Path(__file__).with_name("check-book.py"))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class BookLinks(unittest.TestCase):
    def test_relative_root_encoded_fragments_and_external_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "chapter").mkdir()
            (root / "index.html").write_text(
                '<a href="chapter/page.html#some%20id">chapter</a>'
                '<a href="https://example.com/missing">external</a>')
            (root / "chapter/page.html").write_text(
                '<h1 id="some id">Heading</h1><a href="/">home</a>'
                '<a href="../index.html">home</a><a href="#some%20id">self</a>')
            with contextlib.redirect_stdout(io.StringIO()) as output:
                checker.check(root)
            self.assertIn("Checked 4 local links across 2 HTML pages", output.getvalue())

    def test_missing_page_and_anchor_are_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "index.html").write_text('<a href="missing.html">bad</a><a href="#absent">bad</a>')
            with self.assertRaises(SystemExit) as error:
                checker.check(root)
            self.assertIn("missing target missing.html", str(error.exception))
            self.assertIn("missing anchor #absent", str(error.exception))

    @unittest.skipUnless(os.environ.get("HX"), "set HX to verify Steel examples with a built Neohelix")
    def test_steel_guide_examples_load_in_real_renderer(self):
        repo = Path(__file__).resolve().parents[1]
        examples = re.findall(r"```scheme\n(.*?)```", (repo / "docs/steel.md").read_text(), re.S)
        self.assertEqual(len(examples), 4)
        binary = str(Path(os.environ["HX"]).resolve())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            user = root / "config/helix"
            user.mkdir(parents=True)
            # Additional examples belong inside configure!, with that module's imports.
            installer = examples[0].rstrip()
            (user / "helix.scm").write_text(installer[:-1] + "\n" + "\n".join(examples[2:]) + "\n)\n")
            (user / "init.scm").write_text(examples[1] + '\n(displayln "NEOHELIX_DOC_EXAMPLES_OK")\n')
            before, after = root / "before.rs", root / "after.rs"
            before.write_text("fn old() {}\n")
            after.write_text("fn new() {}\n")
            env = dict(os.environ, HELIX_STEEL_CONFIG=str(user), HELIX_RUNTIME=str(repo / "runtime"),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"),
                       XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
                       STEEL_HOME=str(root / "steel"), HELIX_DIFF_CACHE="0", HELIX_DIFF_WORKER="0")
            result = subprocess.run([binary, "--render-diff", "--color", "never", str(before), str(after)],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("NEOHELIX_DOC_EXAMPLES_OK", result.stderr)


if __name__ == "__main__":
    unittest.main()
