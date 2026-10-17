"""Protocol/cleanup checks for the measurement harness, not a shared-LSP broker."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "bench_lsp_instances", Path(__file__).with_name("bench-lsp-instances.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)

FAKE = r'''
import json, sys
def send(value):
    body = json.dumps(dict(jsonrpc="2.0", **value)).encode()
    wire = f"Content-Length: {len(body)}\r\n\r\n".encode() + body
    for part in (wire[:7], wire[7:19], wire[19:]):
        sys.stdout.buffer.write(part)
        sys.stdout.buffer.flush()
pending = None
while True:
    headers = {}
    while (line := sys.stdin.buffer.readline()) != b"\r\n":
        if not line: sys.exit(0)
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    msg = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    if msg.get("method") == "initialize":
        pending = msg["id"]
        send(dict(id=999, method="workspace/configuration",
                  params=dict(items=[dict(section="rust-analyzer")])) )
    elif msg.get("id") == 999:
        assert msg["result"] == [dict(example=True)], msg
        send(dict(id=998, method="workspace/applyEdit", params=dict(edit=dict(changes={}))))
    elif msg.get("id") == 998:
        assert msg["result"]["applied"] is False
        send(dict(id=pending, result=dict(capabilities=dict(hoverProvider=True))))
    elif msg.get("method") == "textDocument/hover":
        send(dict(id=msg["id"], result=dict(contents="test hover")))
    elif msg.get("method") == "shutdown":
        send(dict(id=msg["id"], result=None))
    elif msg.get("method") == "exit":
        break
'''


class BaselineTests(unittest.TestCase):
    def test_two_independent_servers_route_configuration_and_fragmented_replies(self):
        with tempfile.TemporaryDirectory(prefix="hx-lsp-test-") as path:
            root = Path(path)
            servers = []
            try:
                for _ in range(2):
                    server = bench.Server([sys.executable, "-u", "-c", FAKE], root,
                                          dict(example=True), 2)
                    servers.append(server)
                    result = server.request("initialize", {})
                    self.assertTrue(result["capabilities"]["hoverProvider"])
                    self.assertEqual(server.request("textDocument/hover")["contents"], "test hover")
                self.assertNotEqual(servers[0].process.pid, servers[1].process.pid)
            finally:
                for server in servers:
                    server.close()
            for server in servers:
                self.assertEqual(server.process.returncode, 0)
                self.assertFalse(server.reader.is_alive())

    def test_dead_server_is_reaped(self):
        with tempfile.TemporaryDirectory(prefix="hx-lsp-test-") as path:
            server = bench.Server([sys.executable, "-c", "pass"], Path(path), {}, 1)
            with self.assertRaises((EOFError, BrokenPipeError)):
                server.request("initialize")
            server.close()
            self.assertIsNotNone(server.process.returncode)

    def test_unresponsive_server_is_terminated_and_reaped(self):
        with tempfile.TemporaryDirectory(prefix="hx-lsp-test-") as path:
            server = bench.Server([sys.executable, "-c", "import time; time.sleep(60)"],
                                  Path(path), {}, 0.05)
            server.close()
            self.assertIsNotNone(server.process.returncode)
            self.assertFalse(server.reader.is_alive())


if __name__ == "__main__":
    unittest.main()
