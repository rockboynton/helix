#!/usr/bin/env python3
"""Measure actual jjui launch/navigation to painted diff, not just formatter time.

Uses a private shared-object clone, normal Helix config, isolated jj/jjui settings
and caches. Marker instrumentation is identical for Delta and Neohelix. Workers
are stopped after each trial. No OS cache flush; no repeated revision per trial.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import statistics
import struct
import subprocess
import tempfile
import termios
import time

try:
    import pyte
except ImportError:
    raise SystemExit("This PTY benchmark requires pyte (e.g. a Nix Python with python3Packages.pyte).")

HELPER = r'''#!/usr/bin/env python3
import json, os, pathlib, shlex, subprocess, sys, time
started = time.monotonic_ns()
args = sys.argv[1:]
try:
    parent = [part.decode() for part in pathlib.Path(f"/proc/{os.getppid()}/cmdline").read_bytes().split(b"\0") if part]
except OSError:
    parent = shlex.split(subprocess.check_output(["ps", "-p", str(os.getppid()), "-o", "command="], text=True))
revision = next((parent[i+1] for i,arg in enumerate(parent[:-1]) if arg in ("-r", "--revision")), None)
mode = os.environ["HX_BENCH_MODE"]
command = [os.environ["HX_BENCH_DELTA"] if mode == "delta" else os.environ["HX_BENCH_HX"]] + args
result = subprocess.run(command, capture_output=True)
marker = "HXDIFF_" + str(os.getpid())
record = dict(started=started, finished=time.monotonic_ns(), revision=revision,
              marker=marker, status=result.returncode, args=args,
              elapsed_ms=(time.monotonic_ns()-started)/1e6,
              worker="retained renderer" in result.stderr.decode(errors="replace"),
              native_profile_hit="native profile hit" in result.stderr.decode(errors="replace"),
              fallback="fallback" in result.stderr.decode(errors="replace"))
with open(os.environ["HX_BENCH_TRACE"], "a") as trace:
    trace.write(json.dumps(record) + "\n")
if result.returncode in (0,1):
    sys.stdout.buffer.write(marker.encode() + b"\n" + result.stdout)
else:
    sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
'''


def run(command, **kwargs):
    return subprocess.run(command, check=True, capture_output=True, **kwargs)


def records(path):
    if not path.exists():
        return []
    lines = path.read_text().splitlines()
    # A writer may still be appending its last record.
    return [json.loads(line) for line in lines if line.endswith("}")]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hx", required=True)
    parser.add_argument("--jjui", required=True)
    parser.add_argument("--launcher", required=True)
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--count", type=int, default=4)
    parser.add_argument("--anchor", default="930c788a")
    parser.add_argument("--modes", nargs="+", default=["delta", "one-shot", "auto-worker"])
    parser.add_argument("--cold-resources", action="store_true")
    parser.add_argument("--full-view", action="store_true", help="also measure d from the final preview")
    args = parser.parse_args()
    if args.iterations < 1 or args.count < 1 or not set(args.modes) <= {"delta", "one-shot", "auto-worker"}:
        parser.error("positive counts and supported modes are required")
    hx, jjui, launcher = [str(Path(value).resolve()) for value in (args.hx, args.jjui, args.launcher)]
    delta = shutil.which("delta")
    if not delta:
        parser.error("delta is required")
    revisions = run(["git", "log", "--first-parent", "--format=%H", f"-n{args.count}", args.anchor], text=True).stdout.splitlines()
    results = {}
    with tempfile.TemporaryDirectory(prefix="hxui-") as directory:
        root = Path(directory)
        work = root / "repo"
        run(["git", "clone", "--shared", "--no-checkout", str(Path.cwd()), str(work)])
        run(["git", "-C", str(work), "checkout", "--detach", revisions[0]])
        run(["jj", "git", "init", "--colocate", str(work)])
        change_ids = {revision: run(["jj", "log", "--no-graph", "-r", revision, "-T", "change_id"], cwd=work, text=True).stdout.strip() for revision in revisions}
        helper = root / "formatter"
        helper.write_text(HELPER)
        helper.chmod(0o700)
        for trial in range(args.iterations):
            order = args.modes if trial % 2 == 0 else list(reversed(args.modes))
            for mode in order:
                case = root / f"{trial}-{mode}"
                case.mkdir(mode=0o700)
                sockets, config = case / "s", case / "config"
                sockets.mkdir(mode=0o700)
                config.mkdir()
                trace = case / "trace.jsonl"
                jjconfig = case / "jj.toml"
                jjconfig.write_text('[ui]\ndiff-formatter="bench"\n' + "\n".join(
                    f'[merge-tools.{name}]\nprogram={json.dumps(str(helper))}\n'
                    'diff-do-chdir=false\ndiff-invocation-mode="dir"\ndiff-expected-exit-codes=[0,1]\n'
                    'diff-args=' + json.dumps(
                        (["--paging", "never", "--width", "$width"] + ([] if inline else ["--side-by-side"])
                         if mode == "delta" else ["--render-diff", "--color", "always", "--width", "$width", "--diff-layout", "inline" if inline else "side-by-side"])
                        + ["$left", "$right"]) + '\n'
                    for name, inline in [("bench", False), ("bench-inline", True)]))
                (config / "config.toml").write_text('[preview]\nshow_at_start=true\nposition="right"\nwidth_percentage=50\nrevision_command=' + json.dumps(
                    ["show", "--color", "always", "-r", "$change_id", "--config", "ui.diff-formatter=bench-inline"]) + '\n')
                env = dict(os.environ, JJUI="1", JJ_CONFIG=str(jjconfig), JJUI_CONFIG_DIR=str(config),
                           HELIX_DIFF_WORKER="1" if mode == "auto-worker" else "0",
                           HELIX_DIFF_WORKER_SHARED_VIEWPORT="1", HELIX_DIFF_WORKER_TRACE="1", HELIX_DIFF_CACHE_TRACE="1",
                           HELIX_DIFF_WORKER_DIR=str(sockets), XDG_CACHE_HOME=str(case / "cache"),
                           # Warmup must execute the actual native binary: a
                           # Python timing shim here would delay it artificially.
                           NEOHELIX_HX_BIN=hx, NEOHELIX_JJUI_BIN=jjui,
                           HX_BENCH_HX=hx, HX_BENCH_DELTA=delta, HX_BENCH_MODE=mode, HX_BENCH_TRACE=str(trace),
                           TERM="xterm-256color", COLUMNS="160", LINES="40", PWD=str(work))
                if not args.cold_resources and mode != "delta":
                    # Shared resources only: unrelated tiny inputs, no daemon or
                    # measured revision preloaded into an in-memory cache.
                    for side in ["a", "b"]:
                        target = case / side
                        target.mkdir()
                        (target / "warm.rs").write_text(f'/// ```rust\n/// let x = "{side}";\n/// ```\nfn main() {{}}\n')
                        (target / "warm.md").write_text(f'# {side}\n```rust\nfn main() {{}}\n```\n')
                    for width in ["100", "80", "160"]:
                        for _ in range(3):
                            primed = run([hx, "--render-diff", "--width", width, str(case / "a"), str(case / "b")],
                                         env=dict(env, HELIX_DIFF_WORKER="0"), cwd=work)
                            if b"native profile hit" in primed.stderr:
                                break
                        else:
                            raise RuntimeError("shared-resource priming did not reach a validated native profile hit")
                    for path in (case / "cache/helix/diff-highlights-v1").glob("*.styles"):
                        path.unlink()
                master, slave = pty.openpty()
                profiles_before = sorted(p.name for p in (case / "cache/helix/diff-profile-v1").glob("*.profile"))
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
                started = time.monotonic_ns()
                process = subprocess.Popen(["bash", launcher, "-r", " | ".join(revisions), str(work)],
                                           cwd=work, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
                os.close(slave)
                screen, seen = b"", set()
                retained_pid = None
                terminal = pyte.Screen(160, 40)
                terminal_stream = pyte.ByteStream(terminal)
                synchronized = False

                def paint(since, view, revision):
                    nonlocal screen, synchronized, retained_pid
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        if select.select([master], [], [], 0.01)[0]:
                            try:
                                data = os.read(master, 65536)
                            except OSError:
                                break
                            screen = (screen + data)[-262144:]
                            terminal_stream.feed(data)
                            synchronization = list(re.finditer(rb"\x1b\[\?2026([hl])", screen[-len(data)-20:]))
                            if synchronization:
                                synchronized = synchronization[-1][1] == b"h"
                            for query, reply in [(b"\x1b[6n", b"\x1b[1;1R"), (b"\x1b[c", b"\x1b[?1;2c"),
                                                 (b"\x1b[>c", b"\x1b[>0;0;0c"), (b"\x1b[?u", b"\x1b[?0u"),
                                                 (b"\x1b]11;?", b"\x1b]11;rgb:2828/2828/2828\x1b\\")]:
                                if query in data:
                                    os.write(master, reply)
                        now = time.monotonic_ns()
                        if synchronized:
                            continue
                        for record in records(trace):
                            if record["marker"] in seen:
                                continue
                            observed = record["revision"]
                            if not observed or not any(candidate.startswith(observed) for candidate in (revision, change_ids[revision])):
                                continue
                            if record["marker"] in "\n".join(terminal.display):
                                seen.add(record["marker"])
                                pids = [p.read_text().strip() for p in sockets.glob("*.pid")]
                                if mode == "auto-worker":
                                    if not record["worker"] or record["fallback"] or len(pids) != 1:
                                        raise RuntimeError(f"worker reuse failed: {record}, pids={pids}")
                                    if retained_pid is not None and pids[0] != retained_pid:
                                        raise RuntimeError("worker rotated between preview/full-view requests")
                                    retained_pid = pids[0]
                                value = dict(trial=trial, mode=mode, view=view, revision=revision[:9],
                                             shared_disk_resources=not args.cold_resources,
                                             paint_ms=round((now-since)/1e6, 2), formatter_ms=round(record["elapsed_ms"], 2),
                                             worker=record["worker"], fallback=record["fallback"], worker_pids=pids,
                                             native_profile_hit=record["native_profile_hit"],
                                             width=record["args"][record["args"].index("--width")+1])
                                results.setdefault((mode, view, revision), []).append(value["paint_ms"])
                                print(json.dumps(value), flush=True)
                                return
                        if process.poll() is not None:
                            break
                    diagnostic = records(trace)
                    raise RuntimeError(f"no painted diff: {mode}/{view}, exit={process.poll()}, trace={diagnostic}, screen={terminal.display}")

                try:
                    for index, revision in enumerate(revisions):
                        if index:
                            started = time.monotonic_ns()
                            os.write(master, b"j")
                        paint(started, "startup" if index == 0 else "next-preview", revision)
                        if args.full_view and index == len(revisions) - 1:
                            started = time.monotonic_ns()
                            os.write(master, b"d")
                            paint(started, "full-view", revision)
                    print(json.dumps(dict(mode=mode, trial=trial,
                                          new_profiles=sorted(p.name for p in (case / "cache/helix/diff-profile-v1").glob("*.profile") if p.name not in profiles_before))), flush=True)
                finally:
                    if process.poll() is None:
                        os.write(master, b"q")
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGTERM)
                            process.wait(timeout=3)
                    os.close(master)
                    run([hx, "--diff-worker-stop"], env=env, cwd=work)
                    time.sleep(0.05)
                    if list(sockets.glob("*.pid")):
                        raise RuntimeError("benchmark worker cleanup failed")
    for (mode, view, revision), samples in results.items():
        print(json.dumps(dict(summary=mode, view=view, revision=revision[:9], median_paint_ms=round(statistics.median(samples), 2))), flush=True)


if __name__ == "__main__":
    main()
