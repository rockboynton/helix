"""Exercise effective Steel health checks with an isolated real Neohelix process."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


@unittest.skipUnless(os.environ.get("HX"), "set HX to a built Steel-enabled Neohelix")
class HealthTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="neohelix-health-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.user = self.root / "config/helix"
        self.user.mkdir(parents=True)
        self.project = self.root / "project"
        (self.project / ".helix").mkdir(parents=True)
        (self.project / ".helix/helix.scm").write_text(";; Project configuration module\n")
        (self.project / ".git").mkdir()
        repo = Path(__file__).resolve().parents[1]
        self.binary = str(Path(os.environ["HX"]).resolve())
        self.env = dict(os.environ, HELIX_RUNTIME=str(repo / "runtime"),
                        HELIX_STEEL_CONFIG=str(self.user), STEEL_HOME=str(self.root / "steel"),
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_DATA_HOME=str(self.root / "data"),
                        XDG_CACHE_HOME=str(self.root / "cache"),
                        XDG_STATE_HOME=str(self.root / "state"), TOKIO_WORKER_THREADS="2")
        # Empty servers suppress built-in nil/nixd without needing any installed tools.
        (self.user / "init.scm").write_text(self.override("neohelix-health-user"))

    @staticmethod
    def override(formatter):
        return ('(require "helix/configuration.scm")\n'
                '(define-language "nix" (language-servers \'()) '
                f'(formatter (command "{formatter}")))\n')

    def health(self, category="nix", *flags, cwd=None):
        return subprocess.run([self.binary, "--health", category, *flags],
                              cwd=cwd or self.project, env=self.env, capture_output=True,
                              text=True, timeout=30)

    def test_user_overrides_and_untrusted_project(self):
        (self.project / ".helix/init.scm").write_text('(error "UNTRUSTED_PROJECT_RAN")')
        result = self.health()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("neohelix-health-user", result.stdout)
        self.assertIn("Configured language servers: None", result.stdout)
        self.assertNotIn("UNTRUSTED_PROJECT_RAN", result.stderr)
        self.assertNotIn("'nixfmt'", result.stdout)

    def test_language_table_uses_overrides(self):
        # Keep the fake command short enough for the table's narrow columns.
        (self.user / "init.scm").write_text(self.override("nh"))
        result = self.health("all-languages")
        self.assertEqual(result.returncode, 0, result.stderr)
        row = next(line for line in result.stdout.splitlines() if line.startswith("nix "))
        self.assertIn("nh", row)
        self.assertNotIn("nil", row)

    def test_steel_server_command_and_stdout_remain_separate(self):
        (self.user / "init.scm").write_text(
            '(require "helix/configuration.scm")\n'
            '(define-lsp "health-lsp" (command "neohelix-health-server"))\n'
            '(define-language "nix" (language-servers \'("health-lsp")))\n'
            '(displayln "HEALTH_CONFIG_OUTPUT")\n')
        result = self.health()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("health-lsp", result.stdout)
        self.assertIn("neohelix-health-server", result.stdout)
        self.assertNotIn("HEALTH_CONFIG_OUTPUT", result.stdout)
        self.assertIn("HEALTH_CONFIG_OUTPUT", result.stderr)

    def test_clipboard_uses_steel_settings(self):
        (self.user / "init.scm").write_text(
            '(require "helix/configuration.scm")\n'
            '(set-option! "clipboard-provider" "none")\n')
        result = self.health("clipboard")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("System clipboard provider: Not installed", result.stdout)

    def test_trusted_project_overrides_user_and_working_directory(self):
        (self.user / "config.toml").write_text('[editor.workspace-trust]\nlevel = "insecure"\n')
        (self.project / ".helix/init.scm").write_text(self.override("neohelix-health-project"))
        result = self.health("nix", "--working-dir", str(self.project), cwd=self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("neohelix-health-project", result.stdout)
        self.assertNotIn("neohelix-health-user", result.stdout)

    def test_broken_steel_is_not_silently_replaced_with_defaults(self):
        (self.user / "init.scm").write_text('(error "HEALTH_CONFIG_BROKEN")')
        result = self.health()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HEALTH_CONFIG_BROKEN", result.stderr)
        self.assertNotIn("Configured formatter", result.stdout)

    def test_disabled_steel_preserves_toml_configuration(self):
        (self.user / "config.toml").write_text('[editor]\nenable-steel = false\n')
        (self.user / "init.scm").write_text('(error "DISABLED_STEEL_RAN")')
        (self.user / "languages.toml").write_text(
            '[[language]]\nname = "nix"\nlanguage-servers = []\n'
            'formatter = { command = "neohelix-health-toml" }\n')
        result = self.health()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("neohelix-health-toml", result.stdout)
        self.assertNotIn("DISABLED_STEEL_RAN", result.stderr)


if __name__ == "__main__":
    unittest.main()
