#!/usr/bin/env python3
"""Black-box tests of actual hx proxy/broker processes and unsaved-buffer replay.

HX defaults to target/debug/hx. Includes deterministic Python LSP fixtures and
PTY-backed editors. Set RUST_ANALYZER for the optional native real-server check.
"""
import importlib.util
from concurrent.futures import ThreadPoolExecutor
import json
import os
import fcntl
from pathlib import Path
import signal
import socket
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest

spec = importlib.util.spec_from_file_location("baseline", Path(__file__).with_name("bench-lsp-instances.py"))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
HX = str(Path(os.environ.get("HX", Path(__file__).resolve().parents[1] / "target/debug/hx")).resolve())

FAKE = r'''
import json,os,sys,re
documents = {}
def record(method, **extra):
    line = json.dumps(dict(pid=os.getpid(), parent=os.getppid(), method=method, **extra)) + "\n"
    fd = os.open(os.environ["FAKE_LSP_LOG"], os.O_WRONLY|os.O_CREAT|os.O_APPEND, 0o600)
    os.write(fd, line.encode()); os.close(fd)
def send(value):
    body = json.dumps(dict(jsonrpc="2.0", **value)).encode()
    wire = f"Content-Length: {len(body)}\r\n\r\n".encode() + body
    sys.stdout.buffer.write(wire); sys.stdout.buffer.flush()
def offset(text, position):
    lines = text.splitlines(keepends=True)
    before = "".join(lines[:position["line"]])
    line = lines[position["line"]] if position["line"] < len(lines) else ""
    prefix = line.encode("utf-16-le")[:position["character"]*2].decode("utf-16-le")
    return len(before) + len(prefix)
record("spawn")
while True:
    headers = {}
    while (line := sys.stdin.buffer.readline()) != b"\r\n":
        if not line: sys.exit(0)
        key,value = line.decode().split(":",1); headers[key.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    method, params = message.get("method"), message.get("params") or {}
    if method is None:
        record("reply",id=message.get("id"),result=message.get("result"))
        continue
    record(method, uri=params.get("textDocument", {}).get("uri"))
    if method == "initialize":
        send(dict(id=message["id"], result=dict(capabilities=dict(hoverProvider=True,renameProvider=True,textDocumentSync=2,executeCommandProvider=dict(commands=["fixture.applyEdit"])))))
    elif method == "initialized":
        send(dict(id="register",method="client/registerCapability",params=dict(registrations=[dict(id="watch",method="workspace/didChangeWatchedFiles",registerOptions=dict(watchers=[]))])))
        send(dict(id="config",method="workspace/configuration",params=dict(items=[dict(section="")])))
        send(dict(id="progress",method="window/workDoneProgress/create",params=dict(token="index")))
        send(dict(method="$/progress",params=dict(token="index",value=dict(kind="begin",title="test"))))
        send(dict(method="$/progress",params=dict(token="index",value=dict(kind="end"))))
    elif method == "textDocument/didOpen":
        item=params["textDocument"]; documents[item["uri"]]=[item["text"],item["version"]]
        record("state",uri=item["uri"],text=item["text"],version=item["version"])
    elif method == "textDocument/didChange":
        item=params["textDocument"]; text,old=documents[item["uri"]]
        assert item["version"] > old
        for change in params["contentChanges"]:
            if "range" in change:
                start=offset(text,change["range"]["start"]); end=offset(text,change["range"]["end"])
                text=text[:start]+change["text"]+text[end:]
            else: text=change["text"]
        documents[item["uri"]]=[text,item["version"]]
        record("state",uri=item["uri"],text=text,version=item["version"])
    elif method == "textDocument/didClose": documents.pop(params["textDocument"]["uri"],None)
    elif method == "textDocument/hover":
        text,version=documents[params["textDocument"]["uri"]]
        send(dict(id=message["id"],result=dict(contents=text,range=None)))
    elif method == "textDocument/rename":
        changes=[]
        renamed=params["newName"]=="moved"
        old_uri=params["textDocument"]["uri"]
        new_uri=old_uri.rsplit('/',1)[0]+'/moved.rs'
        for uri,(text,version) in documents.items():
            edits=[]
            for line,row in enumerate(text.splitlines()):
                for match in re.finditer(r'\bold\b',row):
                    edits.append(dict(range=dict(start=dict(line=line,character=match.start()),end=dict(line=line,character=match.end())),newText=params["newName"]))
            if edits: changes.append(dict(textDocument=dict(uri=new_uri if renamed and uri==old_uri else uri,version=version),edits=edits))
        resources=[dict(kind="rename",oldUri=old_uri,newUri=new_uri)] if renamed else []
        send(dict(id=message["id"],result=dict(documentChanges=resources+changes)))
    elif method == "workspace/symbol":
        if params.get("query") == "crash": os._exit(7)
        send(dict(id=message["id"],result=[]))
    elif method == "workspace/executeCommand":
        if params.get("command")=="fixture.applyEdit":
            changes={}
            for uri,(text,version) in documents.items():
                row=text.splitlines()[0]
                changes[uri]=[dict(range=dict(start=dict(line=0,character=0),end=dict(line=0,character=len(row))),newText=row.replace("old","new"))]
            send(dict(id="apply-edit",method="workspace/applyEdit",params=dict(edit=dict(changes=changes))))
        send(dict(id=message["id"],result=None))
    elif method == "shutdown": send(dict(id=message["id"],result=None))
    elif method == "exit": sys.exit(0)
    elif "id" in message: send(dict(id=message["id"],result=None))
'''


class SharedTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hxls-",dir="/tmp")
        self.root = Path(self.temp.name).resolve()
        self.sockets = self.root / "s"
        self.sockets.mkdir(mode=0o700)
        self.log = self.root / "events"
        self.fake = self.root / "server.py"
        self.fake.write_text(FAKE)
        self.clients = []
        self.editors = []

    def pump(self):
        for process, fd, log in self.editors:
            if select.select([fd],[],[],0)[0]:
                try: data=os.read(fd,65536)
                except OSError: continue
                for query,reply in [(b"\x1b[6n",b"\x1b[1;1R"),(b"\x1b[c",b"\x1b[?1;2c"),
                                    (b"\x1b[?u",b"\x1b[?0u"),(b"\x1b]11;?",b"\x1b]11;rgb:2020/2020/2020\x1b\\")]:
                    if query in data: os.write(fd,reply)

    def wait(self, predicate):
        deadline=time.monotonic()+20
        while time.monotonic()<deadline:
            self.pump()
            try:
                if predicate(): return
            except FileNotFoundError:
                # Atomic saves can briefly move the old pathname away.
                pass
            time.sleep(.02)
        logs=[p.read_text()[:1500]+"\n...\n"+p.read_text()[-4000:] for _,_,p in self.editors if p.exists()]
        self.fail(f"timed out; LSP events={self.entries()}, editor logs={logs}")

    def editor(self, file, pane, sharing="1", server=None):
        config=self.root/"config/helix"
        config.mkdir(parents=True,exist_ok=True)
        (config/"helix.scm").write_text("(provide configure!) (define (configure!) #t)")
        (config/"init.scm").write_text('(require "helix.scm") (configure!)')
        command=str(Path(server or sys.executable).resolve())
        arguments=[] if server else ["-u",str(self.fake)]
        options='\nconfig={cargo={buildScripts={enable=false}},procMacro={enable=false},checkOnSave=false}\n' if server else '\n'
        (config/"languages.toml").write_text('[language-server.fixture]\ncommand='+json.dumps(command)+
            '\nargs='+json.dumps(arguments)+options+'[[language]]\nname="rust"\nroots=["Cargo.toml"]\nlanguage-servers=["fixture"]\n')
        (self.root/"Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\nedition="2021"\n')
        env=dict(os.environ,HELIX_SHARED_LSP=sharing,HELIX_SHARED_LSP_DIR=str(self.sockets),
            HELIX_SHARED_LSP_IDLE_SECS="1",FAKE_LSP_LOG=str(self.log),ZELLIJ_PANE_ID=str(pane),
            HELIX_RUNTIME=str(Path(__file__).resolve().parents[1]/"runtime"),
            HELIX_STEEL_CONFIG=str(config),XDG_CONFIG_HOME=str(config.parent),
            XDG_CACHE_HOME=str(self.root/"cache"),XDG_DATA_HOME=str(self.root/"data"),
            XDG_STATE_HOME=str(self.root/"state"),TERM="xterm-256color",PWD=str(self.root))
        master,slave=pty.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",30,120,0,0))
        log=self.root/f"editor-{pane}.log"
        process=subprocess.Popen([HX,"-v","--log",str(log),str(file)],cwd=self.root,env=env,
            stdin=slave,stdout=slave,stderr=slave)
        os.close(slave)
        self.editors.append((process,master,log))
        return master

    def entries(self):
        if not self.log.exists(): return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def buffer_text(self, pane, uri):
        """Replay logged local edits and acknowledged peer transactions.

        Release builds coalesce updates; acknowledgment counts are not text
        versions shared between editor processes.
        """
        log=self.editors[pane-1][2]
        if not log.exists(): return None
        documents={}; pending={}
        def offset(text, position):
            rows=text.splitlines(keepends=True)
            before="".join(rows[:position["line"]])
            row=rows[position["line"]] if position["line"]<len(rows) else ""
            return len(before)+len(row.encode("utf-16-le")[:position["character"]*2].decode("utf-16-le"))
        for line in log.read_text().splitlines():
            sent='fixture -> ' in line
            marker='fixture -> ' if sent else 'fixture <- '
            if marker not in line: continue
            try: value=json.loads(line.split(marker,1)[1])
            except json.JSONDecodeError: continue
            method=value.get('method'); params=value.get('params') or {}
            if not sent and method=='neohelix/syncDocument': pending[value['id']]=params
            elif sent and method=='textDocument/didOpen':
                item=params['textDocument']; documents[item['uri']]=item['text']
            elif sent and method=='textDocument/didClose': documents.pop(params['textDocument']['uri'],None)
            elif sent and method=='textDocument/didChange':
                name=params['textDocument']['uri']
                if name not in documents: continue
                text=documents[name]
                for change in params['contentChanges']:
                    if 'range' in change:
                        start=offset(text,change['range']['start']); end=offset(text,change['range']['end'])
                        text=text[:start]+change['text']+text[end:]
                    else: text=change['text']
                documents[name]=text
            elif sent and value.get('id') in pending:
                update=pending.pop(value['id'])
                if (value.get('result') or {}).get('applied') and update['uri'] in documents:
                    text=documents[update['uri']]
                    for start,end,replacement in reversed(update['changes']): text=text[:start]+replacement+text[end:]
                    documents[update['uri']]=text
        return documents.get(uri)

    def starts(self):
        return [entry for entry in self.entries() if entry["method"] == "spawn"]

    def start(self, options=None, pane="1"):
        options = options or {}
        environment = dict(os.environ, HELIX_SHARED_LSP_DIR=str(self.sockets),
                           HELIX_SHARED_LSP_IDLE_SECS="1", FAKE_LSP_LOG=str(self.log),
                           PWD=str(self.root))
        environment.pop("HELIX_INTERNAL_LSP_SPEC", None)
        for name in ["_", "SHLVL", "OLDPWD", "COLUMNS", "LINES", "ZELLIJ_PANE_ID"]:
            environment.pop(name, None)
        server_spec = dict(command=str(Path(sys.executable).resolve()), args=["-u", str(self.fake)],
                           root=str(self.root), environment=environment)
        environment = dict(environment, HELIX_INTERNAL_LSP_SPEC=json.dumps(server_spec), ZELLIJ_PANE_ID=pane)
        client = baseline.Server([HX, "--internal-lsp-proxy"], self.root, options, 5, environment)
        self.clients.append(client)
        client.request("initialize", dict(processId=os.getpid(), rootUri=self.root.as_uri(),
            initializationOptions=options, capabilities=dict(workspace=dict(configuration=True))))
        client.notify("initialized")
        return client

    def open(self, client, name, text):
        uri = (self.root / name).as_uri()
        client.notify("textDocument/didOpen", dict(textDocument=dict(uri=uri,languageId="rust",version=1,text=text)))
        return uri

    def hover(self, client, uri):
        return client.request("textDocument/hover", dict(textDocument=dict(uri=uri),position=dict(line=0,character=0)))["contents"]

    def test_disjoint_clients_share_one_process_and_survive_other_exit(self):
        first, second = self.start(pane="1"), self.start(pane="2")
        a = self.open(first, "one.rs", "unsaved first")
        b = self.open(second, "two.rs", "unsaved second")
        self.assertEqual(self.hover(first, a), "unsaved first")
        self.assertEqual(self.hover(second, b), "unsaved second")
        self.assertEqual(len(self.starts()), 1, self.entries())
        first.close(); self.clients.remove(first)
        self.assertEqual(self.hover(second, b), "unsaved second")

    def test_divergent_preexisting_unsaved_buffer_is_not_overwritten(self):
        first, second = self.start(), self.start()
        a = self.open(first, "same.rs", "first client's version")
        b = self.open(second, "other.rs", "λ🦀abc")
        second.notify("textDocument/didChange", dict(textDocument=dict(uri=b,version=2),contentChanges=[dict(
            range=dict(start=dict(line=0,character=3),end=dict(line=0,character=6)),text="xyz")]))
        self.assertEqual(self.hover(second, b), "λ🦀xyz")
        shared = self.open(second, "same.rs", "second client's version")
        self.assertEqual(self.hover(second, shared), "second client's version")
        self.assertEqual(self.hover(second, b), "λ🦀xyz")
        self.assertEqual(self.hover(first, a), "first client's version")
        self.assertEqual(len(self.starts()), 2)
        self.assertFalse((self.root / "same.rs").exists(), "fallback wrote unsaved text to disk")

    def test_execute_command_runs_once_without_starting_a_private_server(self):
        first, second = self.start(), self.start()
        a = self.open(first, "one.rs", "one")
        b = self.open(second, "two.rs", "two")
        self.hover(first, a); self.hover(second, b)
        second.request("workspace/executeCommand", dict(command="once"))
        self.assertEqual(self.hover(second, b), "two")
        self.assertEqual(sum(e["method"] == "workspace/executeCommand" for e in self.entries()), 1)
        self.assertEqual(self.hover(first, a), "one")
        self.assertEqual(len(self.starts()),1)

    def test_different_settings_do_not_share(self):
        first = self.start(dict(feature=True))
        second = self.start(dict(feature=False))
        a = self.open(first,"one.rs","one"); b = self.open(second,"two.rs","two")
        self.hover(first,a); self.hover(second,b)
        self.assertEqual(len(self.starts()),2)

    def test_simultaneous_startup_uses_one_backend(self):
        with ThreadPoolExecutor(max_workers=4) as executor:
            clients=list(executor.map(lambda n: self.start(pane=str(n)),range(4)))
        for n,client in enumerate(clients):
            uri=self.open(client,f"{n}.rs",f"buffer {n}")
            self.assertEqual(self.hover(client,uri),f"buffer {n}")
        self.assertEqual(len(self.starts()),1,self.entries())

    def test_crashed_proxy_does_not_kill_surviving_frontend(self):
        first,second=self.start(),self.start()
        a=self.open(first,"one.rs","one"); b=self.open(second,"two.rs","two")
        self.hover(first,a); self.hover(second,b)
        first.process.kill(); first.process.wait(); first.close(); self.clients.remove(first)
        self.assertEqual(self.hover(second,b),"two")
        self.assertEqual(len(self.starts()),1)

    def test_broker_limit_uses_private_fallback(self):
        for n in range(9):
            client=self.start(dict(configuration=n))
            uri=self.open(client,f"{n}.rs",str(n))
            self.assertEqual(self.hover(client,uri),str(n))
        self.assertEqual(len(list(self.sockets.glob("*.sock"))),8)
        self.assertEqual(len(self.starts()),9)

    def test_last_frontend_gracefully_shuts_down_and_reaps_backend(self):
        client=self.start(); uri=self.open(client,"one.rs","one")
        self.hover(client,uri)
        pid=self.starts()[0]["pid"]
        client.close(); self.clients.remove(client)
        self.wait(lambda: not list(self.sockets.glob("*.sock")))
        methods=[entry["method"] for entry in self.entries() if entry["pid"]==pid]
        self.assertEqual(methods.count("shutdown"),1,methods)
        self.assertEqual(methods.count("exit"),1,methods)
        with self.assertRaises(ProcessLookupError): os.kill(pid,0)

    def test_malformed_local_connections_do_not_kill_backend(self):
        client=self.start(); uri=self.open(client,"one.rs","unchanged")
        self.assertEqual(self.hover(client,uri),"unchanged")
        path=next(self.sockets.glob("*.sock"))
        for body in [b"not json",json.dumps(dict(Attach=dict(version=1,spec=dict(
                command="/missing/server",args=[],root=str(self.root),environment={}),initialize={}))).encode()]:
            with socket.socket(socket.AF_UNIX) as peer:
                peer.connect(str(path)); peer.sendall(struct.pack("<I",len(body))+body)
        self.assertEqual(self.hover(client,uri),"unchanged")
        self.assertEqual(len(self.starts()),1)

    def test_replay_capacity_falls_back_without_losing_large_unsaved_buffer(self):
        client=self.start()
        text="x"*(8*1024*1024)
        uri=self.open(client,"large.rs",text)
        self.assertEqual(self.hover(client,uri),text)
        self.assertFalse((self.root/"large.rs").exists())

    def test_server_crash_cancels_requests_and_preserves_both_buffers(self):
        first, second = self.start(), self.start()
        a = self.open(first,"one.rs","unsaved one"); b = self.open(second,"two.rs","unsaved two")
        self.hover(first,a); self.hover(second,b)
        with self.assertRaisesRegex(RuntimeError,"retry"):
            first.request("workspace/symbol",dict(query="crash"))
        self.assertEqual(self.hover(first,a),"unsaved one")
        self.assertEqual(self.hover(second,b),"unsaved two")
        self.assertEqual(len(self.starts()),3)

    def test_unsafe_socket_directory_uses_independent_servers(self):
        self.sockets.chmod(0o755)
        first, second = self.start(), self.start()
        a = self.open(first,"one.rs","one"); b = self.open(second,"two.rs","two")
        self.hover(first,a); self.hover(second,b)
        self.assertEqual(len(self.starts()),2)
        self.assertFalse(list(self.sockets.glob("*.sock")))

    def test_broker_refuses_to_replace_regular_file(self):
        target = self.sockets / ("0"*32 + ".sock")
        target.write_text("preserve me")
        env = dict(os.environ, HELIX_SHARED_LSP_DIR=str(self.sockets),
                   HELIX_INTERNAL_LSP_SOCKET=str(target))
        result = subprocess.run([HX,"--internal-lsp-broker"],env=env,capture_output=True,timeout=5)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(target.read_text(),"preserve me")
        target.unlink()

    def test_two_real_editors_share_same_unsaved_document_bidirectionally(self):
        one,two=self.root/"one.rs",self.root/"two.rs"
        one.write_text("fn first() {}\n"); two.write_text("fn second() {}\n")
        first=self.editor(one,1); second=self.editor(two,2)
        self.wait(lambda: sum(e["method"]=="textDocument/didOpen" for e in self.entries())>=2)
        self.assertEqual(len(self.starts()),1,self.entries())
        os.write(second,b"iUNSAVED \x1b")
        self.wait(lambda: any(e["method"]=="state" and e.get("text","").startswith("UNSAVED ") for e in self.entries()))
        os.write(second,b":open "+str(one).encode()+b"\r")
        self.wait(lambda: one.as_uri() in self.editors[1][2].read_text())
        os.write(first,b"iFIRST \x1b")
        self.wait(lambda: any(e["method"]=="state" and e.get("uri")==one.as_uri() and e.get("text","").startswith("FIRST ") for e in self.entries()))
        self.wait(lambda: 'neohelix/syncDocument' in self.editors[1][2].read_text())
        # A subsequent command is processed after the peer update in the UI.
        os.write(second,b":write\r")
        self.wait(lambda: one.read_text().startswith("FIRST "))
        os.write(second,b"iSECOND \x1b")
        self.wait(lambda: any(e["method"]=="state" and e.get("uri")==one.as_uri() and "SECOND " in e.get("text","") for e in self.entries()))
        self.wait(lambda: 'neohelix/syncDocument' in self.editors[0][2].read_text())
        os.write(first,b":write\r")
        self.wait(lambda: "SECOND " in one.read_text())
        self.assertIn("FIRST ",one.read_text())
        self.assertEqual(len(self.starts()),1,self.entries())
        self.assertEqual(two.read_text(),"fn second() {}\n")

    def test_two_real_editors_join_unsaved_text_and_concurrent_insertions_converge(self):
        file=self.root/"same.rs"
        file.write_text("fn same() {}\n")
        first=self.editor(file,1)
        self.wait(lambda: any(e["method"]=="state" for e in self.entries()))
        os.write(first,b"iUNSAVED \x1b")
        self.wait(lambda: any(e["method"]=="state" and e.get("text","").startswith("UNSAVED ") for e in self.entries()))
        second=self.editor(file,2)
        self.wait(lambda: self.editors[1][2].exists() and 'neohelix/syncDocument' in self.editors[1][2].read_text())
        os.write(second,b":write\r")
        self.wait(lambda: file.read_text().startswith("UNSAVED "))
        os.write(first,b"i12345\x1b")
        os.write(second,b"i67890\x1b")
        def complete(text):
            return all(text.count(char)==1 for char in "1234567890")
        self.wait(lambda: any(e["method"]=="state" and complete(e.get("text","")) for e in self.entries()))
        expected=[e["text"] for e in self.entries() if e["method"]=="state"][-1]
        for sequence in ["12345","67890"]:
            self.assertEqual("".join(char for char in expected if char in sequence),sequence)
        os.write(first,b":write\r")
        self.wait(lambda: file.read_text()==expected)
        os.write(second,b":write\r")
        self.wait(lambda: 'written' in self.editors[1][2].read_text() or file.read_text()==expected)
        self.assertEqual(file.read_text(),expected)
        self.assertEqual(len(self.starts()),1,self.entries())

    def test_rename_coordinates_two_real_editors_without_saving_or_new_backend(self):
        one,two=self.root/"one.rs",self.root/"two.rs"
        one.write_text("fn old() {}\n"); two.write_text("fn use_it() { old(); }\n")
        first=self.editor(one,1); second=self.editor(two,2)
        self.wait(lambda: sum(e["method"]=="state" for e in self.entries())==2)
        os.write(first,b"3l r\x15new\r")
        self.wait(lambda: len({e.get("uri") for e in self.entries() if e["method"]=="state" and "new" in e.get("text","")})==2)
        self.wait(lambda: '"applied":true' in self.editors[1][2].read_text())
        self.assertEqual(one.read_text(),"fn old() {}\n")
        self.assertEqual(two.read_text(),"fn use_it() { old(); }\n")
        self.assertEqual(len(self.starts()),1,self.entries())
        os.write(first,b":write\r"); os.write(second,b":write\r")
        self.wait(lambda: "new" in one.read_text() and "new" in two.read_text())

    def test_real_editor_restart_is_private_and_preserves_unsaved_text(self):
        one,two=self.root/"one.rs",self.root/"two.rs"
        one.write_text("fn first() {}\n"); two.write_text("fn second() {}\n")
        self.editor(one,1); second=self.editor(two,2)
        self.wait(lambda: sum(e["method"]=="textDocument/didOpen" for e in self.entries())>=2)
        self.assertEqual(len(self.starts()),1,self.entries())
        shared=self.starts()[0]["pid"]
        os.write(second,b"iUNSAVED \x1b")
        self.wait(lambda: any(e["method"]=="state" and e.get("text","").startswith("UNSAVED ") for e in self.entries()))
        os.write(second,b":lsp-restart\r")
        self.wait(lambda: len(self.starts())==2)
        private=self.starts()[-1]["pid"]
        self.wait(lambda: any(e["method"]=="state" and e["pid"]==private and e.get("text","").startswith("UNSAVED ") for e in self.entries()))
        os.kill(shared,0)
        self.assertFalse(any(e["method"]=="shutdown" and e["pid"]==shared for e in self.entries()))
        self.assertEqual(two.read_text(),"fn second() {}\n")

    def test_server_initiated_workspace_edit_has_one_driver_and_updates_peer(self):
        one,two=self.root/"one.rs",self.root/"two.rs"
        one.write_text("fn old() {}\n"); two.write_text("fn old() {}\n")
        first=self.editor(one,1); second=self.editor(two,2)
        self.wait(lambda: sum(e["method"]=="state" for e in self.entries())==2)
        os.write(second,b":lsp-workspace-command fixture.applyEdit\r")
        self.wait(lambda: any(e["method"]=="reply" and e.get("id")=="apply-edit" and e.get("result",{}).get("applied") for e in self.entries()))
        self.wait(lambda: '"applied":true' in self.editors[1][2].read_text())
        self.assertEqual(sum(e["method"]=="reply" and e.get("id")=="apply-edit" for e in self.entries()),1)
        self.assertEqual(len(self.starts()),1)
        self.assertEqual(one.read_text(),"fn old() {}\n")
        self.assertEqual(two.read_text(),"fn old() {}\n")
        os.write(first,b":write\r"); os.write(second,b":write\r")
        self.wait(lambda: "new" in one.read_text() and "new" in two.read_text())

    def test_server_workspace_rename_then_text_edit_rebinds_shared_buffer(self):
        old,new=self.root/"old.rs",self.root/"moved.rs"
        old.write_text("fn old() {}\n")
        first=self.editor(old,1); second=self.editor(old,2)
        self.wait(lambda: all(log.exists() and 'textDocument/didOpen' in log.read_text() for _,_,log in self.editors))
        os.write(first,b"3l r\x15moved\r")
        self.wait(lambda: new.exists() and not old.exists())
        self.wait(lambda: any(e["method"]=="state" and e.get("uri")==new.as_uri() and "fn moved" in e.get("text","") for e in self.entries()))
        self.wait(lambda: '"applied":true' in self.editors[1][2].read_text())
        self.assertEqual(new.read_text(),"fn old() {}\n")
        os.write(second,b":write\r")
        self.wait(lambda: new.read_text()=="fn moved() {}\n")
        self.assertEqual(len(self.starts()),1,self.entries())

    def test_moving_shared_unsaved_buffer_rebinds_peer_without_reloading(self):
        old,new=self.root/"old.rs",self.root/"new.rs"
        old.write_text("fn same() {}\n")
        first=self.editor(old,1); second=self.editor(old,2)
        self.wait(lambda: all(log.exists() and 'textDocument/didOpen' in log.read_text() for _,_,log in self.editors))
        os.write(first,b"iUNSAVED \x1b")
        self.wait(lambda: self.buffer_text(2,old.as_uri())=="UNSAVED fn same() {}\n")
        os.write(first,b":move "+str(new).encode()+b"\r")
        self.wait(lambda: new.exists() and not old.exists())
        self.wait(lambda: all(new.as_uri() in log.read_text() and 'neohelix/filesRenamed' in log.read_text() for _,_,log in self.editors))
        self.assertEqual(new.read_text(),"fn same() {}\n")
        os.write(second,b":write\r")
        self.wait(lambda: new.read_text().startswith("UNSAVED "))
        self.assertFalse(old.exists(),"peer saved to the old filename")
        self.assertEqual(len(self.starts()),1,self.entries())

    def test_peer_save_clears_modified_state_without_forcing_other_editor_to_save(self):
        file=self.root/"same.rs"
        file.write_text("fn same() {}\n")
        first=self.editor(file,1); second=self.editor(file,2)
        self.wait(lambda: all(log.exists() and 'textDocument/didOpen' in log.read_text() for _,_,log in self.editors))
        os.write(first,b"iSHARED \x1b")
        self.wait(lambda: self.buffer_text(2,file.as_uri())=="SHARED fn same() {}\n")
        os.write(second,b":write\r")
        self.wait(lambda: file.read_text().startswith("SHARED "))
        self.wait(lambda: 'neohelix/documentSaved' in self.editors[0][2].read_text())
        os.write(first,b":quit-all\r")
        self.wait(lambda: self.editors[0][0].poll() is not None)
        self.assertIsNone(self.editors[1][0].poll())
        self.assertEqual(len(self.starts()),1)

    def test_backend_crash_replays_peer_edits_without_forcing_save(self):
        file=self.root/"same.rs"
        file.write_text("fn same() {}\n")
        first=self.editor(file,1); self.editor(file,2)
        self.wait(lambda: all(log.exists() and 'textDocument/didOpen' in log.read_text() for _,_,log in self.editors))
        os.write(first,b"iREMOTE \x1b")
        self.wait(lambda: self.buffer_text(2,file.as_uri())=="REMOTE fn same() {}\n")
        os.kill(self.starts()[0]["pid"],signal.SIGKILL)
        self.wait(lambda: len(self.starts())==3)
        private={entry["pid"] for entry in self.starts()[1:]}
        self.wait(lambda: {entry["pid"] for entry in self.entries() if entry["method"]=="state" and entry.get("text","").startswith("REMOTE ")} >= private)
        self.assertEqual(file.read_text(),"fn same() {}\n")

    def test_real_editors_without_opt_in_remain_independent(self):
        one,two=self.root/"one.rs",self.root/"two.rs"
        one.write_text("fn first() {}\n"); two.write_text("fn second() {}\n")
        self.editor(one,1,sharing="0"); self.editor(two,2,sharing="0")
        self.wait(lambda: sum(e["method"]=="textDocument/didOpen" for e in self.entries())>=2)
        self.assertEqual(len(self.starts()),2)
        self.assertFalse(list(self.sockets.glob("*.sock")))

    @unittest.skipUnless(os.environ.get("RUST_ANALYZER"),"set RUST_ANALYZER for native real-server editor test")
    def test_two_real_editors_with_rust_analyzer(self):
        source=self.root/"src"; source.mkdir()
        (source/"lib.rs").write_text("mod one; mod two;\n")
        one,two=source/"one.rs",source/"two.rs"
        one.write_text("pub fn first() -> u32 { 1 }\n")
        two.write_text("pub fn second() -> u32 { super::one::first() }\n")
        first=self.editor(one,1,server=os.environ["RUST_ANALYZER"])
        second=self.editor(two,2,server=os.environ["RUST_ANALYZER"])
        def received(log):
            result=[]
            if not log.exists(): return result
            for line in log.read_text().splitlines():
                if "fixture <- " in line:
                    try: result.append(json.loads(line.split("fixture <- ",1)[1]))
                    except json.JSONDecodeError: pass
            return result
        self.wait(lambda: all(any("capabilities" in message.get("result",{}) for message in received(log) if isinstance(message.get("result"),dict)) for _,_,log in self.editors))
        self.wait(lambda: all(log.exists() and 'textDocument/didOpen' in log.read_text() for _,_,log in self.editors))
        for fd in [first,second]: os.write(fd,b"7l")
        last_request=0
        def useful_hovers():
            nonlocal last_request
            if all(any(isinstance(message.get("result"),dict) and message["result"].get("contents") for message in received(log)) for _,_,log in self.editors): return True
            # A server may correctly return null until indexing finishes.
            if time.monotonic()-last_request>.5:
                for fd in [first,second]: os.write(fd,b" k")
                last_request=time.monotonic()
            return False
        self.wait(useful_hovers)
        # Exercise the real analyzer's cross-file WorkspaceEdit, not merely a
        # fixture that happens to accept our messages.
        os.write(first,b"\x1b")
        time.sleep(.03)
        os.write(first,b" r")
        def rename_prepared():
            requests=[]
            for line in self.editors[0][2].read_text().splitlines():
                if 'fixture -> ' in line:
                    try: message=json.loads(line.split('fixture -> ',1)[1])
                    except json.JSONDecodeError: continue
                    if message.get('method')=='textDocument/prepareRename': requests.append(message['id'])
            return any(message.get('id') in requests and message.get('result') is not None for message in received(self.editors[0][2]))
        self.wait(rename_prepared)
        os.write(first,b"\x15renamed\r")
        self.wait(lambda: 'neohelix/syncDocument' in self.editors[1][2].read_text())
        self.assertIn("first",one.read_text())
        self.assertIn("first",two.read_text())
        for fd in [first,second]:
            os.write(fd,b"\x1b")
        time.sleep(.03)
        for fd in [first,second]: os.write(fd,b":write\r")
        self.wait(lambda: "renamed" in one.read_text() and "renamed" in two.read_text())
        # Every upstream child belongs to the broker, not to both editor proxies.
        rows=[row.split(None,2) for row in subprocess.check_output(["ps","-axo","pid=,ppid=,comm="],text=True).splitlines()]
        descendants={p.pid for p,_,_ in self.editors}
        while True:
            children={int(pid) for pid,parent,_ in rows if int(parent) in descendants}
            if children <= descendants: break
            descendants.update(children)
        servers=[int(pid) for pid,_,command in rows if int(pid) in descendants and Path(command).name=="rust-analyzer"]
        self.assertEqual(len(servers),1,servers)

    def tearDown(self):
        for process,fd,_ in self.editors:
            if process.poll() is None:
                try: os.write(fd,b"\x1b:quit-all!\r")
                except OSError: pass
        deadline=time.monotonic()+3
        while any(p.poll() is None for p,_,_ in self.editors) and time.monotonic()<deadline:
            self.pump(); time.sleep(.02)
        for process,fd,_ in self.editors:
            if process.poll() is None: process.kill()
            process.wait(); os.close(fd)
        for client in reversed(self.clients): client.close()
        deadline = time.monotonic() + 5
        while list(self.sockets.glob("*.sock")) and time.monotonic() < deadline:
            time.sleep(.05)
        leftovers = list(self.sockets.glob("*.sock"))
        # Test-created children only; never leave a failed fixture's daemon alive.
        if leftovers:
            for entry in self.starts():
                for pid in [entry["pid"], entry["parent"]]:
                    try: os.kill(pid, signal.SIGTERM)
                    except ProcessLookupError: pass
        self.temp.cleanup()
        self.assertFalse(leftovers, "broker did not expire/remove its socket")


if __name__ == "__main__":
    unittest.main()
