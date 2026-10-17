#!/usr/bin/env python3
"""Measure independent LSP instances before implementing cross-editor sharing.

Requires an existing workspace/file and explicit hover position. Never edits
workspace files. Measures protocol initialization and a nonempty hover, not a
fully indexed workspace or an end-to-end Helix session. RSS/PSS are main-server
process measurements, not descendant-process totals or predicted broker savings.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time


class Server:
    def __init__(self, command, root, options, timeout):
        self.process = subprocess.Popen(command, cwd=root, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.options = options
        self.root = root
        self.timeout = timeout
        self.responses = queue.Queue()
        self.lock = threading.Lock()
        self.counter = 0
        self.reader = threading.Thread(target=self.receive, daemon=True)
        self.reader.start()

    def send(self, value):
        body = json.dumps(dict(jsonrpc="2.0", **value)).encode()
        with self.lock:
            self.process.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
            self.process.stdin.flush()

    def notify(self, method, params=None):
        self.send(dict(method=method, params=params or {}))

    def receive(self):
        try:
            stream = self.process.stdout
            while True:
                headers = {}
                while (line := stream.readline()) not in (b"\r\n", b"\n"):
                    if not line:
                        raise EOFError("server closed stdout")
                    name, value = line.decode("ascii").split(":", 1)
                    headers[name.lower()] = value.strip()
                length = int(headers["content-length"])
                if not 0 < length <= 32 * 1024 * 1024:
                    raise ValueError("oversized/invalid LSP message")
                body = stream.read(length)
                if len(body) != length:
                    raise EOFError("truncated LSP message")
                message = json.loads(body)
                if "method" in message and "id" in message:
                    self.answer(message)
                elif "id" in message:
                    self.responses.put(message)
                # Notifications are deliberately discarded, not accumulated.
        except Exception as error:
            self.responses.put(error)

    def answer(self, message):
        method, params = message["method"], message.get("params", {})
        result = None
        if method == "workspace/configuration":
            result = []
            for item in params.get("items", []):
                value = self.options
                for part in item.get("section", "").split("."):
                    if part and part != "rust-analyzer":
                        value = value.get(part) if isinstance(value, dict) else None
                result.append(value)
        elif method == "workspace/workspaceFolders":
            result = [dict(uri=self.root.as_uri(), name=self.root.name)]
        elif method == "workspace/applyEdit":
            result = dict(applied=False, failureReason="read-only benchmark")
        elif method not in ("window/workDoneProgress/create", "client/registerCapability",
                            "client/unregisterCapability"):
            self.send(dict(id=message["id"], error=dict(code=-32601, message="unsupported by benchmark")))
            return
        self.send(dict(id=message["id"], result=result))

    def request(self, method, params=None):
        self.counter += 1
        ident = self.counter
        self.send(dict(id=ident, method=method, params=params or {}))
        response = self.responses.get(timeout=self.timeout)
        if isinstance(response, Exception):
            raise response
        if response.get("id") != ident:
            raise RuntimeError("unexpected response ID")
        if "error" in response:
            raise RuntimeError(f"{method}: {response['error']}")
        return response.get("result")

    def memory(self):
        # RSS includes shared pages; PSS apportions them between processes.
        values = {}
        try:
            for line in Path(f"/proc/{self.process.pid}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    values["rss_kib"] = int(line.split()[1])
            for line in Path(f"/proc/{self.process.pid}/smaps_rollup").read_text().splitlines():
                if line.startswith("Pss:"):
                    values["pss_kib"] = int(line.split()[1])
        except OSError:
            if "rss_kib" not in values:
                try:
                    values["rss_kib"] = int(subprocess.check_output(
                        ["ps", "-p", str(self.process.pid), "-o", "rss="], text=True).strip())
                except (ValueError, OSError, subprocess.CalledProcessError):
                    pass
        return values

    def close(self):
        try:
            if self.process.poll() is None:
                self.timeout = min(self.timeout, 3)
                self.request("shutdown")
                self.notify("exit")
                self.process.wait(timeout=3)
        except Exception:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait()
        finally:
            try:
                self.process.stdin.close()
            except BrokenPipeError:
                pass
            self.reader.join(timeout=1)
            self.process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=lambda p: Path(p).resolve())
    parser.add_argument("--file", required=True, type=lambda p: Path(p).resolve())
    parser.add_argument("--language-id", required=True)
    parser.add_argument("--line", required=True, type=int, help="zero-based")
    parser.add_argument("--character", required=True, type=int, help="zero-based UTF-16 offset")
    parser.add_argument("--instances", type=int, default=2)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--settle", type=float, default=1)
    parser.add_argument("--init-options", type=json.loads, default={})
    parser.add_argument("--server", nargs=argparse.REMAINDER, required=True)
    args = parser.parse_args()
    if not args.server or not 1 <= args.instances <= 8 or args.timeout <= 0 or args.settle < 0:
        parser.error("server, 1-8 instances, positive timeout and nonnegative settle are required")
    if min(args.line, args.character) < 0 or not args.root.is_dir() or not args.file.is_file():
        parser.error("existing root/file and nonnegative position are required")
    text, servers = args.file.read_text(), []
    try:
        for index in range(args.instances):
            started = time.monotonic()
            server = Server(args.server, args.root, args.init_options, args.timeout)
            servers.append(server)
            params = dict(processId=os.getpid(), rootUri=args.root.as_uri(),
                workspaceFolders=[dict(uri=args.root.as_uri(), name=args.root.name)],
                initializationOptions=args.init_options,
                capabilities=dict(workspace=dict(configuration=True),
                    textDocument=dict(hover=dict(contentFormat=["plaintext"]))))
            server.request("initialize", params)
            initialized = time.monotonic()
            server.notify("initialized")
            server.notify("textDocument/didOpen", dict(textDocument=dict(
                uri=args.file.as_uri(), languageId=args.language_id, version=1, text=text)))
            deadline = time.monotonic() + args.timeout
            while True:
                hover = server.request("textDocument/hover", dict(
                    textDocument=dict(uri=args.file.as_uri()),
                    position=dict(line=args.line, character=args.character)))
                if hover and hover.get("contents"):
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError("no nonempty hover: check the selected position")
                time.sleep(0.05)
            ready = time.monotonic()
            time.sleep(args.settle)
            print(json.dumps(dict(instance=index + 1,
                initialize_ms=round((initialized-started)*1000, 2),
                launch_to_hover_ms=round((ready-started)*1000, 2),
                servers=[dict(pid=s.process.pid, **s.memory()) for s in servers])), flush=True)
    finally:
        for server in reversed(servers):
            server.close()


if __name__ == "__main__":
    main()
