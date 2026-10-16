"""Shared process timing and complete Git snapshot fixtures for diff benchmarks."""
import hashlib
import os
import subprocess
import time


def execute(command, environment, accepted=(0,), input_bytes=None):
    started = time.perf_counter()
    result = subprocess.run(command, env=environment, input=input_bytes, capture_output=True, timeout=120)
    elapsed = (time.perf_counter() - started) * 1000
    if result.returncode not in accepted:
        raise RuntimeError(result.stderr.decode(errors="replace"))
    return elapsed, result


def snapshots(root, revisions):
    pairs = []
    seen_sources = set()
    for revision in revisions:
        left, right = root / revision / "left", root / revision / "right"
        left.mkdir(parents=True)
        right.mkdir(parents=True)
        names = subprocess.check_output(
            ["git", "diff", "--name-only", "-z", revision + "^", revision]
        ).split(b"\0")
        current_sources = set()
        count = 0
        for name in filter(None, names):
            name = os.fsdecode(name)
            count += 1
            for version, directory in [(revision + "^", left), (revision, right)]:
                content = subprocess.run(
                    ["git", "show", version + ":" + name], capture_output=True, check=False
                )
                if content.returncode == 0:
                    path = directory / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(content.stdout)
                    current_sources.add(hashlib.sha256(content.stdout).digest())
        pairs.append((revision, left, right, dict(
            files=count, unique_sources=len(current_sources),
            sources_seen_earlier=len(current_sources & seen_sources))))
        seen_sources.update(current_sources)
    return pairs
