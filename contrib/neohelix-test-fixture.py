#!/usr/bin/env python3
"""Create disposable local repositories for docs/test-plan.md; print their root."""

import argparse
import json
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import zlib


BASE = '''pub mod other;

pub fn greet(name: &str) -> String {
    let my_http_value = format!("hello {name}: NEOHELIX_SAMPLE");
    if name.is_empty() {
        return "anonymous".to_string();
    }
    my_http_value
}

// trailing whitespace fixture:\x20\x20\x20
''' + ''.join(f'// Unchanged context {number:02d}\n' for number in range(16)) + '''
pub fn footer() -> &'static str {
    "before"
}
'''


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def run(root, *args):
    return subprocess.run(args, cwd=root, check=True, capture_output=True, text=True).stdout


def png(path):
    def chunk(kind, payload):
        return struct.pack("!I", len(payload)) + kind + payload + struct.pack("!I", zlib.crc32(kind + payload))

    rows = b''.join(b'\0' + b''.join(bytes((x * 4, y * 8, 128)) for x in range(64)) for y in range(32))
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack("!2I5B", 64, 32, 8, 2, 0, 0, 0))
                     + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def create(with_jj=True):
    root = Path(tempfile.mkdtemp(prefix="neohelix-test-"))
    before, after = root / "before", root / "after"
    write(before / "Cargo.toml", '[package]\nname="neohelix-fixture"\nversion="0.1.0"\nedition="2021"\n')
    write(before / ".gitignore", "/target/\n")
    write(before / "src/lib.rs", BASE)
    write(before / "src/other.rs", 'pub fn message() -> String {\n    crate::greet("NEOHELIX_SAMPLE")\n}\n')
    write(before / "obsolete.md", "This file will be removed.\n")
    write(before / "src/old_name.rs", "// A rename fixture.\n")
    write(before / "data.csv", 'name,description,value\nalpha,"quoted, comma",1\nbeta,"two ""quotes""",2\n')
    bindings = ('Ctrl+Right { focus-column-or-monitor-right; }\n'
                'Ctrl+Left { focus-column-or-monitor-left; }\n'
                'Mod+Ctrl+Right { focus-window-or-workspace-down; }\n'
                'Mod+Ctrl+Left { focus-window-or-workspace-up; }\n')
    write(before / "bindings.kdl", bindings)
    write(before / "README.md", '# Fixture\n\nMixedCase snake_case myHTTPValue ((nested))\n\n```rust\nfn injected() {}\n```\n\n' + "long wrapping line " * 30 + "\n")
    write(before / ".helix/helix.scm", '(provide fixture-marker) (define fixture-marker #t)\n')
    write(before / ".helix/init.scm", '(require "helix/configuration.scm") (set-option! "text-width" 88)\n')
    shutil.copytree(before, after)
    swapped = bindings.replace("column-or-monitor-right", "window-or-workspace-down").replace(
        "column-or-monitor-left", "window-or-workspace-up")
    swapped = swapped.replace("Mod+Ctrl+Right { focus-window-or-workspace-down", "Mod+Ctrl+Right { focus-column-or-monitor-right").replace(
        "Mod+Ctrl+Left { focus-window-or-workspace-up", "Mod+Ctrl+Left { focus-column-or-monitor-left")
    write(after / "bindings.kdl", swapped)
    write(after / "src/lib.rs", BASE.replace("hello {name}", "welcome {name}").replace('"before"', '"after"'))
    write(after / "src/added.rs", "pub fn added() -> u32 {\n    42\n}\n")
    (after / "obsolete.md").unlink()
    (after / "src/old_name.rs").rename(after / "src/renamed.rs")

    for name in ["git", "jj"] if with_jj else ["git"]:
        repo = root / name
        shutil.copytree(before, repo)
        run(repo, "git", "init", "-q")
        run(repo, "git", "config", "user.name", "Neohelix Test")
        run(repo, "git", "config", "user.email", "neohelix-test@example.invalid")
        run(repo, "git", "config", "core.hooksPath", "/dev/null")
        run(repo, "git", "config", "commit.gpgsign", "false")
        run(repo, "git", "add", ".")
        run(repo, "git", "commit", "-qm", "Fixture baseline")
        if name == "jj":
            run(repo, "jj", "--config", 'signing.behavior="drop"', "git", "init", "--colocate")
        (repo / "obsolete.md").unlink()
        (repo / "src/old_name.rs").unlink()
        shutil.copytree(after, repo, dirs_exist_ok=True)
    patches = root / "patches"
    patches.mkdir()
    write(patches / "git.patch", run(root / "git", "git", "diff", "--no-ext-diff", "--color=never"))
    if with_jj:
        write(patches / "jj.patch", run(root / "jj", "jj", "--config", 'signing.behavior="drop"',
                                       "diff", "--color-words", "--color", "never"))
    (root / "three").mkdir()
    for name, greeting in [("left", "left"), ("base", "hello"), ("right", "right")]:
        write(root / "three" / (name + ".rs"), BASE.replace("hello {name}", greeting + " {name}"))
    (root / "media").mkdir()
    png(root / "media/sample.png")
    write(root / "manifest.json", json.dumps({"root": str(root), "jj": with_jj}, indent=2) + "\n")
    return root


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--without-jj", action="store_true", help="create Git/directory fixtures only")
    parser.add_argument("--large-mib", type=int, default=0, help="also create an optional large.txt (0–64 MiB)")
    args = parser.parse_args()
    if not 0 <= args.large_mib <= 64:
        parser.error("--large-mib must be between 0 and 64")
    if not shutil.which("git") or (not args.without_jj and not shutil.which("jj")):
        parser.error("git and jj must be on PATH; use --without-jj if jj is unavailable")
    root = create(with_jj=not args.without_jj)
    if args.large_mib:
        line = "Large-file search target: NEOHELIX_LARGE\n"
        size = args.large_mib * 1024 * 1024
        write(root / "large.txt", (line * (size // len(line) + 1))[:size])
    print(root)
