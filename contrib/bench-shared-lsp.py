#!/usr/bin/env python3
"""Matched direct/shared protocol benchmark with two disjoint real source files.

Linux /proc memory includes proxies, broker, server and their live descendants.
Not an editor-startup benchmark. Never writes source files. Run without builds.
"""
import argparse
import importlib.util
import json
import os
import math
import statistics
from pathlib import Path
import tempfile
import time

module = importlib.util.spec_from_file_location("baseline", Path(__file__).with_name("bench-lsp-instances.py"))
baseline = importlib.util.module_from_spec(module)
module.loader.exec_module(baseline)


def processes(roots):
    found = set(roots)
    pending = list(roots)
    while pending:
        pid = pending.pop()
        try:
            children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
        except OSError:
            continue
        for child in map(int, children):
            if child not in found:
                found.add(child)
                pending.append(child)
    result = []
    for pid in sorted(found):
        try:
            pss = next(int(line.split()[1]) for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines() if line.startswith("Pss:"))
            command = Path(f"/proc/{pid}/comm").read_text().strip()
            result.append(dict(pid=pid, command=command, pss_kib=pss))
        except (OSError, StopIteration):
            continue
    return result


def measure(args, shared):
    clients = []
    with tempfile.TemporaryDirectory(prefix="hxlsp-bench-",dir="/tmp") as directory:
        os.chmod(directory, 0o700)
        environment = dict(os.environ, HELIX_SHARED_LSP_DIR=directory, HELIX_SHARED_LSP_IDLE_SECS="1", PWD=str(args.root))
        for key in ["_", "SHLVL", "OLDPWD", "COLUMNS", "LINES", "ZELLIJ_PANE_ID", "HELIX_INTERNAL_LSP_SPEC", "HELIX_INTERNAL_LSP_SOCKET"]:
            environment.pop(key, None)
        spec = dict(command=str(args.server.resolve()), args=[], root=str(args.root), environment=environment)
        command = [str(args.hx), "--internal-lsp-proxy"] if shared else [str(args.server)]
        launch_environment = dict(environment, HELIX_INTERNAL_LSP_SPEC=json.dumps(spec)) if shared else environment
        try:
            timings = []
            queries = []
            for file in args.files:
                text = file.read_text()
                # Choose a declared function with a nonempty real-server hover.
                candidates = [(n, line.find("fn ")+3) for n,line in enumerate(text.splitlines()) if "fn " in line and not line.lstrip().startswith("//")]
                if not candidates:
                    raise ValueError(f"no function declaration in {file}")
                line, character = candidates[0]
                query = dict(textDocument=dict(uri=file.as_uri()),position=dict(line=line,character=character))
                started = time.monotonic()
                client = baseline.Server(command, args.root, args.options, args.timeout, launch_environment)
                clients.append(client)
                client.request("initialize", dict(processId=os.getpid(), rootUri=args.root.as_uri(),
                    workspaceFolders=[dict(uri=args.root.as_uri(),name=args.root.name)], initializationOptions=args.options,
                    capabilities=dict(workspace=dict(configuration=True),textDocument=dict(hover=dict(contentFormat=["plaintext"])))))
                initialized = time.monotonic()
                client.notify("initialized")
                client.notify("textDocument/didOpen",dict(textDocument=dict(uri=file.as_uri(),languageId="rust",version=1,text=text)))
                deadline = time.monotonic()+args.timeout
                while True:
                    hover = client.request("textDocument/hover",query)
                    if hover and hover.get("contents"):
                        break
                    if time.monotonic()>deadline:
                        raise TimeoutError(f"no useful hover for {file}")
                    time.sleep(.1)
                timings.append(dict(file=str(file.relative_to(args.root)), initialize_ms=round((initialized-started)*1000,2), useful_hover_ms=round((time.monotonic()-started)*1000,2)))
                queries.append(query)
            time.sleep(args.settle)
            latencies = []
            for client,query in zip(clients,queries):
                for _ in range(20):
                    client.request("textDocument/hover",query)
                samples = []
                for _ in range(args.requests):
                    started = time.perf_counter_ns()
                    hover = client.request("textDocument/hover",query)
                    samples.append((time.perf_counter_ns()-started)/1_000_000)
                    if not hover or not hover.get("contents"):
                        raise RuntimeError("warm request lost its useful hover")
                samples.sort()
                latencies.append(dict(requests=len(samples),p50_ms=round(statistics.median(samples),3),
                    p95_ms=round(samples[math.ceil(len(samples)*.95)-1],3),
                    p99_ms=round(samples[math.ceil(len(samples)*.99)-1],3),max_ms=round(max(samples),3)))
            edit_latencies = []
            for client,query,file in zip(clients,queries,args.files):
                text=file.read_text()
                line=text.count("\n")
                character=len(text.rsplit("\n",1)[-1].encode("utf-16-le"))//2
                samples=[]
                for index in range(args.edit_requests):
                    started=time.perf_counter_ns()
                    client.notify("textDocument/didChange",dict(textDocument=dict(uri=file.as_uri(),version=index+2),contentChanges=[dict(range=dict(start=dict(line=line,character=character),end=dict(line=line,character=character)),text="\n")]))
                    hover=client.request("textDocument/hover",query)
                    samples.append((time.perf_counter_ns()-started)/1_000_000)
                    if not hover or not hover.get("contents"):
                        raise RuntimeError("edit barrier lost its useful hover")
                    line+=1; character=0
                samples.sort()
                edit_latencies.append(dict(requests=len(samples),p50_ms=round(statistics.median(samples),3),p95_ms=round(samples[math.ceil(len(samples)*.95)-1],3),p99_ms=round(samples[math.ceil(len(samples)*.99)-1],3),max_ms=round(max(samples),3)) if samples else None)
            sample = processes([client.process.pid for client in clients])
            result = dict(mode="shared" if shared else "direct", timings=timings, warm_hover_latency=latencies, edit_hover_latency=edit_latencies, processes=sample,
                total_pss_kib=sum(p["pss_kib"] for p in sample) if sample else None)
            print(json.dumps(result), flush=True)
        finally:
            for client in reversed(clients):
                client.close()
            deadline = time.monotonic()+5
            while list(Path(directory).glob("*.sock")) and time.monotonic()<deadline:
                time.sleep(.05)
            if list(Path(directory).glob("*.sock")):
                raise RuntimeError("broker did not clean up after benchmark")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root",type=lambda p: Path(p).resolve(),required=True)
    parser.add_argument("--files",nargs=2,type=lambda p: Path(p).resolve(),required=True)
    parser.add_argument("--server",type=Path,required=True)
    parser.add_argument("--hx",type=lambda p: Path(p).resolve(),default=Path("target/debug/hx").resolve())
    parser.add_argument("--timeout",type=float,default=90)
    parser.add_argument("--settle",type=float,default=2)
    parser.add_argument("--requests",type=int,default=200)
    parser.add_argument("--edit-requests",type=int,default=100,help="in-memory newline edits followed by a useful hover barrier per file")
    parser.add_argument("--options",type=json.loads,default={"cargo":{"buildScripts":{"enable":False}},"procMacro":{"enable":False},"checkOnSave":False})
    parser.add_argument("--mode",choices=["direct","shared","both"],default="both")
    parser.add_argument("--reverse",action="store_true",help="measure shared before direct to check ordering effects")
    args = parser.parse_args()
    if args.requests < 1:
        parser.error("--requests must be positive")
    if args.edit_requests < 0:
        parser.error("--edit-requests must be nonnegative")
    order=[True,False] if args.reverse else [False,True]
    for shared in (order if args.mode=="both" else [args.mode=="shared"]):
        measure(args,shared)
