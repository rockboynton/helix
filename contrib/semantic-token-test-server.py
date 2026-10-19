#!/usr/bin/env python3
"""Deterministic stdio LSP fixture for semantic-token integration tests."""
import json
from pathlib import Path
import sys

mode, log_path = sys.argv[1:]
documents = {}
refreshed = False


def send(**message):
    body = json.dumps(dict(jsonrpc="2.0", **message)).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


while True:
    headers = {}
    while (line := sys.stdin.buffer.readline()) != b"\r\n":
        if not line:
            sys.exit(0)
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    method, params = message.get("method"), message.get("params") or {}
    with Path(log_path).open("a") as log:
        log.write(json.dumps(message) + "\n")
    if method == "initialize":
        send(id=message["id"], result=dict(capabilities=dict(
            textDocumentSync=1,
            # Deliberately override the syntactic function classification so
            # the editor test can prove that server colors reach the surface.
            semanticTokensProvider=dict(legend=dict(tokenTypes=["type"], tokenModifiers=[]),
                                        full=mode == "full", range=mode == "range"))))
    elif method == "textDocument/didOpen":
        document = params["textDocument"]
        documents[document["uri"]] = document["text"]
    elif method == "textDocument/didChange":
        documents[params["textDocument"]["uri"]] = params["contentChanges"][-1]["text"]
    elif method in ("textDocument/semanticTokens/full", "textDocument/semanticTokens/range"):
        text = documents[params["textDocument"]["uri"]]
        lines = text.splitlines()
        row = next((i for i, line in enumerate(lines) if "fn " in line), None)
        data = [] if row is None else [row, lines[row].index("fn ") + 3, 3, 0, 0]
        send(id=message["id"], result=dict(data=data))
        if not refreshed:
            refreshed = True
            send(id="semantic-refresh", method="workspace/semanticTokens/refresh")
    elif method == "shutdown":
        send(id=message["id"], result=None)
    elif method == "exit":
        break
    elif "id" in message and method:
        send(id=message["id"], result=None)
