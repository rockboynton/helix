#!/usr/bin/env python3
"""Fresh processes, warm shared resources, unseen source styles, no daemon.

Run from the repository containing the requested Git revisions. The user's
normal configuration/theme stays active. Only cache files in a private temporary
directory are removed between trials. No OS filesystem cache is flushed.
"""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile

from diff_bench_support import execute, snapshots


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hx", required=True, type=lambda value: str(Path(value).resolve()))
    parser.add_argument("--revisions", nargs="+", default=["930c788a", "e25ef77fa", "f36d57681", "1891050b8"])
    parser.add_argument("--threads", nargs="+", default=["1", "2", "4"])
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--layouts", nargs="+", default=["inline", "side-by-side"])
    parser.add_argument("--patch", action="store_true", help="feed identical unified patches to both tools instead of comparing complete files")
    args = parser.parse_args()
    assert args.iterations > 0
    with tempfile.TemporaryDirectory(prefix="neohelix-first-view-") as scratch:
        root = Path(scratch)
        environment = dict(os.environ, XDG_CACHE_HOME=str(root / "cache"),
                           HELIX_DIFF_WORKER="0", HELIX_DIFF_CACHE="1",
                           HELIX_DIFF_CACHE_TRACE="1", COLUMNS="160")
        environment.pop("HELIX_DIFF_HIGHLIGHT_CACHE", None)
        pairs = [(revision, left, right)
                 for revision, left, right, _ in snapshots(root, args.revisions)]
        patches = {}
        if args.patch:
            for revision in args.revisions:
                patches[revision] = subprocess.check_output([
                    "git", "show", "--format=", "--no-ext-diff", "--no-textconv",
                    "--no-color", revision, "--"])

        def command(left, right, layout):
            return [args.hx, "--render-diff", "--color", "always", "--width", "160",
                    "--diff-layout", layout] + (["--patch"] if args.patch else [str(left), str(right)])

        # Explicitly prime the union of configuration/grammar/query resources.
        # Source styles are then removed before EVERY measured invocation.
        for revision, left, right in pairs:
            elapsed, _ = execute(command(left, right, args.layouts[0]),
                                 dict(environment, HELIX_DIFF_RENDER_THREADS="1"), input_bytes=patches.get(revision))
            print(json.dumps(dict(prime=revision, elapsed_ms=round(elapsed, 2))), flush=True)

        styles = root / "cache" / "helix" / "diff-highlights-v1"
        for revision, left, right in pairs:
            for layout in args.layouts:
                expected = execute(command(left, right, layout),
                                   dict(environment, HELIX_DIFF_CACHE="0", HELIX_DIFF_RENDER_THREADS="1",
                                        HELIX_DIFF_INCREMENTAL_PARSE="0", HELIX_DIFF_SYNTAX_RANGES="0"), input_bytes=patches.get(revision))[1].stdout
                delta = ["delta", "--paging", "never", "--width", "160"]
                if layout == "side-by-side":
                    delta += ["--side-by-side"]
                if not args.patch:
                    delta += [str(left), str(right)]
                delta_times = [execute(delta, environment, (0, 1), input_bytes=patches.get(revision))[0] for _ in range(args.iterations)]
                measurements = {threads: [] for threads in args.threads}
                source_hits = {threads: [] for threads in args.threads}
                # Interleave concurrency settings instead of timing one entire
                # group before the next, reducing order/thermal bias.
                for iteration in range(args.iterations):
                    order = args.threads if iteration % 2 == 0 else list(reversed(args.threads))
                    for threads in order:
                        for path in styles.glob("*.styles"):
                            path.unlink()
                        assert not any(styles.glob("*.styles")), "source cache was not cleared"
                        elapsed, result = execute(command(left, right, layout),
                                                  dict(environment, HELIX_DIFF_RENDER_THREADS=threads), input_bytes=patches.get(revision))
                        # Identical sources occurring twice within ONE request
                        # can legitimately reuse a just-produced style snapshot.
                        source_hits[threads].append(result.stderr.count(b"highlight hit"))
                        assert result.stdout == expected, f"output mismatch: {revision}/{layout}/{threads}"
                        measurements[threads].append(elapsed)
                print(json.dumps(dict(revision=revision, layout=layout,
                                      input="patch" if args.patch else "files",
                                      incremental=environment.get("HELIX_DIFF_INCREMENTAL_PARSE", "1"),
                                      syntax_ranges=environment.get("HELIX_DIFF_SYNTAX_RANGES", "1"),
                                      neohelix_ms={key: round(statistics.median(value), 2) for key, value in measurements.items()},
                                      delta_ms=round(statistics.median(delta_times), 2),
                                      within_request_source_hits=source_hits,
                                      prior_request_source_hits=0, parity="exact")), flush=True)


if __name__ == "__main__":
    main()
