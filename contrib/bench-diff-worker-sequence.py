#!/usr/bin/env python3
"""Compare fresh clients across unseen adjacent revisions, not repeat views.

Run in the Helix repository. Uses private temporary caches/socket directories,
normal user configuration, full-source inputs and exact full-parse references.
Shared disk resources are primed explicitly; normally the worker starts on
request zero. --warm-worker initializes it separately using unrelated inputs.
Each trial/mode visits every revision once. Source styles start empty for each
sequence, then remain available just as in normal use. No OS caches are flushed.
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
    parser.add_argument("--anchor", default="930c788a")
    parser.add_argument("--count", type=int, default=8)
    parser.add_argument("--revisions", nargs="+")
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--layouts", nargs="+", choices=["inline", "side-by-side"],
                        default=["inline", "side-by-side"])
    parser.add_argument("--no-source-disk-cache", action="store_true",
                        help="disable source style disk caches, retaining worker document trees")
    parser.add_argument("--serial-control", action="store_true",
                        help="also measure serial one-shot rendering to isolate parallelism")
    parser.add_argument("--memory", action="store_true",
                        help="sample worker RSS using ps, outside timed requests")
    parser.add_argument("--session-profiler", type=lambda value: str(Path(value).resolve()),
                        help="also profile the sequence with the diff-render-bench example")
    parser.add_argument("--profile-only", action="store_true",
                        help="only run the session profiler, not timed CLI comparisons")
    parser.add_argument("--warm-worker", action="store_true",
                        help="initialize the worker with unrelated tiny Rust/Markdown files; report startup separately")
    args = parser.parse_args()
    if args.profile_only and not args.session_profiler:
        parser.error("--profile-only requires --session-profiler")
    if args.iterations < 1 or args.count < 1:
        parser.error("iterations and count must be positive")
    revisions = args.revisions or subprocess.check_output(
        ["git", "log", "--first-parent", "--format=%H", f"-n{args.count}", args.anchor],
        text=True).splitlines()
    revisions = [subprocess.check_output(["git", "rev-parse", rev], text=True).strip()
                 for rev in revisions]
    if len(set(revisions)) != len(revisions):
        parser.error("each revision must be visited only once per sequence")

    # Short paths also fit macOS's Unix socket address limit.
    with tempfile.TemporaryDirectory(prefix="hxseq-") as scratch:
        root = Path(scratch)
        pairs = snapshots(root / "inputs", revisions)

        def command(left, right, layout):
            return [args.hx, "--render-diff", "--color", "always", "--width", "160",
                    "--diff-layout", layout, str(left), str(right)]

        reference_env = dict(os.environ, XDG_CACHE_HOME=str(root / "reference"),
                             HELIX_DIFF_WORKER_DIR=str(root / "rw"),
                             HELIX_DIFF_WORKER="0", HELIX_DIFF_CACHE="0",
                             HELIX_DIFF_HIGHLIGHT_CACHE="0", HELIX_DIFF_RENDER_THREADS="1",
                             HELIX_DIFF_INCREMENTAL_PARSE="0", HELIX_DIFF_SYNTAX_RANGES="0")
        expected = {}
        for revision, left, right, _ in pairs:
            for layout in args.layouts:
                expected[revision, layout] = execute(command(left, right, layout), reference_env)[1].stdout

        if args.session_profiler:
            for layout in args.layouts:
                requests = []
                for revision, left, right, _ in pairs:
                    reference = root / f"{revision}-{layout}.ansi"
                    reference.write_bytes(expected[revision, layout])
                    requests.append([str(left), str(right), str(reference)])
                manifest = root / f"{layout}.json"
                manifest.write_text(json.dumps(requests))
                profile_env = dict(os.environ, XDG_CACHE_HOME=str(root / f"profile-{layout}"),
                                   HELIX_DIFF_WORKER_DIR=str(root / "pw"),
                                   HELIX_DIFF_WORKER="0", HELIX_DIFF_CACHE="1",
                                   HELIX_DIFF_RENDER_THREADS="4", DIFF_BENCH_PROFILE="1",
                                   HELIX_DIFF_HIGHLIGHT_CACHE="0" if args.no_source_disk_cache else "1",
                                   DIFF_BENCH_SEQUENCE=str(manifest))
                result = execute([args.session_profiler, str(pairs[0][1]), str(pairs[0][2]),
                                  layout, "1"], profile_env)[1]
                print(json.dumps(dict(session_profile=layout,
                                      lines=result.stdout.decode().splitlines())), flush=True)
            if args.profile_only:
                return

        modes = ["one-shot", "worker"]
        if args.serial_control:
            modes.insert(1, "one-shot-serial")
        values = {(mode, layout, rev): [] for mode in modes + ["delta"]
                  for layout in args.layouts for rev in revisions}
        for iteration in range(args.iterations):
            for layout in args.layouts:
                cache = root / f"c{iteration}-{layout}"
                worker_root = root / f"w{iteration}-{layout}"
                worker_root.mkdir(mode=0o700)
                environment = dict(os.environ, XDG_CACHE_HOME=str(cache),
                                   HELIX_DIFF_WORKER_DIR=str(worker_root),
                                   HELIX_DIFF_WORKER="0", HELIX_DIFF_CACHE="1",
                                   HELIX_DIFF_HIGHLIGHT_CACHE="0" if args.no_source_disk_cache else "1",
                                   HELIX_DIFF_WORKER_TRACE="1", HELIX_DIFF_CACHE_TRACE="1",
                                   HELIX_DIFF_RENDER_THREADS="4", HELIX_DIFF_INCREMENTAL_PARSE="1",
                                   HELIX_DIFF_SYNTAX_RANGES="1", HELIX_DIFF_WORKER_IDLE_SECS="120",
                                   COLUMNS="160")
                # Prime shared disk resources only, not the measured worker.
                for _, left, right, _ in pairs:
                    execute(command(left, right, layout), environment)
                order = modes if iteration % 2 == 0 else list(reversed(modes))
                for mode in order:
                    for path in (cache / "helix" / "diff-highlights-v1").glob("*.styles"):
                        path.unlink()
                    mode_env = dict(environment,
                                    HELIX_DIFF_WORKER="1" if mode == "worker" else "0",
                                    HELIX_DIFF_RENDER_THREADS="1" if mode == "one-shot-serial" else "4")
                    pid = None
                    try:
                        if mode == "worker" and args.warm_worker:
                            elapsed, result = execute([args.hx, "--diff-worker-warm", "--width", "160"], mode_env)
                            if b"retained renderer" not in result.stderr or b"fallback" in result.stderr:
                                raise RuntimeError("worker warmup fell back")
                            pids = [path.read_text().strip() for path in worker_root.glob("*.pid")]
                            if len(pids) != 1:
                                raise RuntimeError(f"worker warmup did not retain one process: {pids}")
                            pid = pids[0]
                            print(json.dumps(dict(warmup_ms=round(elapsed, 2), trial=iteration,
                                                  layout=layout, pid=pid, source="synthetic Rust/Markdown")), flush=True)
                        for index, (revision, left, right, info) in enumerate(pairs):
                            elapsed, result = execute(command(left, right, layout), mode_env)
                            if result.stdout != expected[revision, layout]:
                                raise RuntimeError(f"output mismatch: {mode}/{layout}/{revision}")
                            if mode == "worker":
                                if b"retained renderer" not in result.stderr or b"fallback" in result.stderr:
                                    raise RuntimeError(f"worker fallback: {result.stderr.decode(errors='replace')}")
                                pids = [path.read_text().strip() for path in worker_root.glob("*.pid")]
                                if len(pids) != 1 or (pid is not None and pids[0] != pid):
                                    raise RuntimeError(f"worker was not retained: {pids}, previous={pid}")
                                pid = pids[0]
                            values[mode, layout, revision].append(elapsed)
                            memory = {}
                            if pid is not None and args.memory:
                                memory["worker_rss_kib"] = int(subprocess.check_output(
                                    ["ps", "-p", pid, "-o", "rss="], text=True).strip())
                            print(json.dumps(dict(trial=iteration, mode=mode, layout=layout,
                                                  revision=revision[:9], request=index,
                                                  elapsed_ms=round(elapsed, 2), pid=pid,
                                                  parity="exact", **info, **memory)), flush=True)
                    finally:
                        if mode == "worker":
                            execute([args.hx, "--diff-worker-stop"], mode_env)
                for revision, left, right, _ in pairs:
                    delta = ["delta", "--paging", "never", "--width", "160"]
                    if layout == "side-by-side":
                        delta += ["--side-by-side"]
                    elapsed, _ = execute(delta + [str(left), str(right)], environment, (0, 1))
                    values["delta", layout, revision].append(elapsed)

        for layout in args.layouts:
            for index, revision in enumerate(revisions):
                print(json.dumps(dict(summary=revision[:9], layout=layout, request=index,
                                      worker_preinitialized=args.warm_worker,
                                      source_disk_cache=not args.no_source_disk_cache,
                                      median_ms={mode: round(statistics.median(values[mode, layout, revision]), 2)
                                                 for mode in modes + ["delta"]})), flush=True)


if __name__ == "__main__":
    main()
