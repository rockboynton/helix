use super::{discovery, wire, Input, Output, Spec, VERSION};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    os::fd::{AsFd, AsRawFd},
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader, ReadBuf},
    sync::mpsc,
    task::JoinHandle,
};

#[derive(Default)]
pub(super) struct Tasks(pub Vec<JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

// Tokio stdin uses a blocking thread which cannot be cancelled on server exit.
// A proxy always has pipes; use readiness-based descriptors instead.
struct Pipe(tokio::io::unix::AsyncFd<std::fs::File>);
impl Pipe {
    fn new(fd: impl AsFd) -> Result<Self> {
        let file = std::fs::File::from(fd.as_fd().try_clone_to_owned()?);
        // SAFETY: fcntl operates on our owned, live descriptor.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        ensure!(flags >= 0, "could not inspect proxy pipe");
        // SAFETY: sets only the nonblocking flag on the live pipe descriptor.
        ensure!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "could not configure proxy pipe"
        );
        Ok(Self(tokio::io::unix::AsyncFd::new(file)?))
    }
}
impl AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut ready = std::task::ready!(self.0.poll_read_ready(cx))?;
            match ready
                .try_io(|fd| std::io::Read::read(&mut fd.get_ref(), buf.initialize_unfilled()))
            {
                Ok(Ok(size)) => {
                    buf.advance(size);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_) => (),
            }
        }
    }
}
impl AsyncWrite for Pipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut ready = std::task::ready!(self.0.poll_write_ready(cx))?;
            if let Ok(result) = ready.try_io(|fd| std::io::Write::write(&mut fd.get_ref(), buf)) {
                return Poll::Ready(result);
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub(super) fn reader(
    reader: impl AsyncBufRead + Unpin + Send + 'static,
    tasks: &mut Tasks,
) -> mpsc::Receiver<Result<Value>> {
    let (tx, rx) = mpsc::channel(1);
    tasks.0.push(tokio::spawn(async move {
        let mut reader = reader;
        loop {
            match wire::read_lsp(&mut reader).await {
                Ok(Some(value)) => {
                    if tx.send(Ok(value)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            }
        }
    }));
    rx
}

#[derive(Default)]
pub(super) struct History {
    documents: BTreeMap<String, Value>,
    settings: Option<Value>,
    bytes: usize,
    encoding: String,
    syncs: BTreeMap<String, Value>,
}
impl History {
    pub(super) fn record(&mut self, value: &Value) -> bool {
        if value["method"] == "neohelix/finishEdit" {
            for item in value["params"]["documents"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if !self.record(&json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":item}})) { return false; }
            }
        }
        if value.get("method").is_none() {
            if let Some(params) = value["id"].as_str().and_then(|id| self.syncs.remove(id)) {
                return self.synchronized(&params, &value["result"]).is_ok();
            }
        }
        let method = value["method"].as_str().unwrap_or("");
        if method == "workspace/didChangeConfiguration" {
            let size = value.to_string().len();
            let old = self.settings.as_ref().map_or(0, |v| v.to_string().len());
            if self.bytes - old + size > 8 * 1024 * 1024 {
                return false;
            }
            self.bytes = self.bytes - old + size;
            self.settings = Some(value.clone());
            return true;
        }
        let Some(uri) = value["params"]["textDocument"]["uri"].as_str() else {
            return true;
        };
        if method == "textDocument/didClose" {
            if let Some(history) = self.documents.remove(uri) {
                self.bytes -= history.to_string().len();
            }
            return true;
        }
        if !matches!(method, "textDocument/didOpen" | "textDocument/didChange") {
            return true;
        }
        let snapshot = if method == "textDocument/didChange" {
            let Some(mut snapshot) = self.documents.get(uri).cloned() else {
                return false;
            };
            let Ok(text) = super::documents::apply_changes(
                snapshot["params"]["textDocument"]["text"].as_str().unwrap(),
                &value["params"]["contentChanges"],
                &self.encoding,
            ) else {
                return false;
            };
            snapshot["params"]["textDocument"]["text"] = json!(text);
            snapshot["params"]["textDocument"]["version"] =
                value["params"]["textDocument"]["version"].clone();
            snapshot
        } else {
            value.clone()
        };
        let old = self
            .documents
            .get(uri)
            .map_or(0, |value| value.to_string().len());
        let size = snapshot.to_string().len();
        if self.bytes - old + size > 8 * 1024 * 1024 || (self.documents.len() >= 512 && old == 0) {
            return false;
        }
        self.bytes = self.bytes - old + size;
        self.documents.insert(uri.into(), snapshot);
        true
    }
    fn synchronized(&mut self, params: &Value, result: &Value) -> Result<()> {
        if result["applied"] != true {
            return Ok(());
        }
        let uri = params["uri"].as_str().context("sync lacks URI")?;
        let snapshot = self
            .documents
            .get(uri)
            .context("sync for closed document")?;
        ensure!(
            snapshot["params"]["textDocument"]["version"] == params["expectedVersion"],
            "sync replay version mismatch"
        );
        let before = helix_core::Rope::from_str(
            snapshot["params"]["textDocument"]["text"].as_str().unwrap(),
        );
        let changes = params["changes"]
            .as_array()
            .context("sync lacks changes")?
            .iter()
            .map(|change| {
                Ok((
                    change[0].as_u64().context("invalid start")? as usize,
                    change[1].as_u64().context("invalid end")? as usize,
                    Some(helix_core::Tendril::from(
                        change[2].as_str().context("invalid replacement")?,
                    )),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let transaction = helix_core::Transaction::change(&before, changes.into_iter());
        let mut text = before;
        ensure!(transaction.apply(&mut text), "invalid sync transaction");
        ensure!(self.record(&json!({"method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":result["version"]},"contentChanges":[{"text":text.to_string()}]}})), "shared replay capacity reached");
        Ok(())
    }
    pub(super) fn replay(&self) -> Vec<Value> {
        self.settings
            .iter()
            .cloned()
            .chain(self.documents.values().cloned())
            .collect()
    }
    fn retains(value: &Value) -> bool {
        matches!(
            value["method"].as_str(),
            Some(
                "textDocument/didOpen"
                    | "textDocument/didChange"
                    | "textDocument/didClose"
                    | "workspace/didChangeConfiguration"
            )
        )
    }
}

fn cancel_pending(pending: &mut BTreeMap<String, Value>, reason: &str) -> Vec<Value> {
    std::mem::take(pending).into_values().map(|id| json!({"jsonrpc":"2.0", "id":id,
        "error":{"code":-32800,"message":format!("Shared LSP switched to a private server: {reason}; retry the request")}})).collect()
}

fn backend_capabilities(mut capabilities: Value) -> Value {
    if let Some(experimental) = capabilities
        .get_mut("experimental")
        .and_then(Value::as_object_mut)
    {
        if experimental.remove("neohelixShared").is_some() {
            experimental.remove("neohelixSharedSession");
            let original = experimental.remove("neohelixBackendExperimental");
            if let Some(original) = original {
                capabilities["experimental"] = original;
            } else if experimental.is_empty() {
                capabilities.as_object_mut().unwrap().remove("experimental");
            }
        }
    }
    capabilities
}

async fn private(
    spec: Spec,
    initialize: Value,
    original_result: Option<Value>,
    mut history: History,
    mut input: mpsc::Receiver<Result<Value>>,
    mut output: Pipe,
    trigger: Option<Value>,
) -> Result<()> {
    let mut child = spec.spawn()?;
    let result = async {
    let mut writer = child.stdin.take().unwrap();
    let mut tasks = Tasks::default();
    let mut server = reader(BufReader::new(child.stdout.take().unwrap()), &mut tasks);
    let original_id = initialize["id"].clone();
    let mut initialize = initialize;
    initialize["id"] = json!("neohelix/private-initialize");
    wire::write_lsp(&mut writer, &initialize).await?;
    let mut deferred = Vec::new();
    let mut deferred_bytes = 0;
    let mut server_calls = BTreeSet::new();
    loop {
        tokio::select! {
            value = server.recv() => {
                let value = value.context("private server exited during initialization")??;
                if value.get("method").is_none() && value["id"] == "neohelix/private-initialize" {
                    ensure!(value.get("error").is_none(), "private server initialization failed");
                    if let Some(old) = &original_result {
                        ensure!(backend_capabilities(old["capabilities"].clone()) == backend_capabilities(value["result"]["capabilities"].clone()), "private server capabilities changed; restart the language server");
                        wire::write_lsp(&mut writer, &json!({"jsonrpc":"2.0","method":"initialized","params":{}})).await?;
                        for message in history.replay() { wire::write_lsp(&mut writer, &message).await?; }
                    } else {
                        let mut response = value;
                        response["id"] = original_id;
                        wire::write_lsp(&mut output, &response).await?;
                    }
                    break;
                }
                if value.get("method").is_some() && value.get("id").is_some() {
                    server_calls.insert(value["id"].to_string());
                }
                wire::write_lsp(&mut output, &value).await?;
            }
            value = input.recv() => {
                let Some(value) = value else { return Ok(()); };
                let value = value?;
                if value["id"].as_str().is_some_and(|id| id.starts_with("neohelix/sync/")) {
                    ensure!(history.record(&value), "failed to retain late shared edit");
                    continue;
                }
                if private_control(&value, &mut output).await? { continue; }
                if value.get("method").is_none() && server_calls.remove(&value["id"].to_string()) {
                    wire::write_lsp(&mut writer, &value).await?;
                } else {
                    deferred_bytes += value.to_string().len();
                    ensure!(deferred_bytes <= wire::LIMIT, "too much input during private initialization");
                    deferred.push(value);
                }
            }
        }
    }
    if let Some(value) = trigger {
        if !private_control(&value, &mut output).await? && value.get("id").is_none() {
            wire::write_lsp(&mut writer, &value).await?;
        }
    }
    for value in deferred {
        if private_control(&value, &mut output).await? { continue; }
        wire::write_lsp(&mut writer, &value).await?;
    }
    loop {
        tokio::select! {
            value = server.recv() => {
                let Some(value) = value else { return Ok(()); };
                wire::write_lsp(&mut output, &value?).await?;
            }
            value = input.recv() => {
                let Some(value) = value else { return Ok(()); };
                let value = value?;
                if value["id"].as_str().is_some_and(|id| id.starts_with("neohelix/sync/")) {
                    let uri = value["id"].as_str().and_then(|id| history.syncs.get(id)).and_then(|params| params["uri"].as_str()).map(str::to_owned);
                    ensure!(history.record(&value), "failed to retain late shared edit");
                    if value["result"]["applied"] == true {
                        if let Some(snapshot) = uri.and_then(|uri| history.documents.get(&uri)) {
                            let item = &snapshot["params"]["textDocument"];
                            wire::write_lsp(&mut writer, &json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":item["uri"],"version":item["version"]},"contentChanges":[{"text":item["text"]}]}})).await?;
                        }
                    }
                    continue;
                }
                if private_control(&value, &mut output).await? { continue; }
                let exit = value["method"] == "exit";
                wire::write_lsp(&mut writer, &value).await?;
                if exit { let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await; return Ok(()); }
            }
        }
    }
    }.await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

async fn private_control(value: &Value, output: &mut Pipe) -> Result<bool> {
    if !value["method"]
        .as_str()
        .is_some_and(|method| method.starts_with("neohelix/"))
    {
        return Ok(false);
    }
    if let Some(id) = value.get("id") {
        wire::write_lsp(output, &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32801,"message":"shared broker is no longer available; inspect any applied edits before requesting the action again"}})).await?;
    }
    Ok(true)
}

pub(super) async fn run(spec: Spec) -> Result<()> {
    let mut tasks = Tasks::default();
    let mut input = reader(BufReader::new(Pipe::new(std::io::stdin())?), &mut tasks);
    let mut output = Pipe::new(std::io::stdout())?;
    let initialize = input.recv().await.context("no initialize request")??;
    ensure!(
        initialize["method"] == "initialize" && initialize.get("id").is_some(),
        "expected initialize request"
    );
    let key = spec.key(&initialize["params"])?;
    let attach = async {
        let mut stream = discovery::connect(&key).await?;
        wire::write(
            &mut stream,
            &Input::Attach {
                version: VERSION,
                spec: spec.clone(),
                initialize: initialize["params"].clone(),
            },
        )
        .await?;
        let Output::Attached(result) = wire::read(&mut stream).await? else {
            anyhow::bail!("broker rejected attachment");
        };
        Ok::<_, anyhow::Error>((stream, result))
    };
    let attached = tokio::time::timeout(Duration::from_secs(60), attach).await;
    let (stream, result) = match attached {
        Ok(Ok(value)) => value,
        _ => {
            return private(
                spec,
                initialize,
                None,
                History::default(),
                input,
                output,
                None,
            )
            .await
        }
    };
    wire::write_lsp(
        &mut output,
        &json!({"jsonrpc":"2.0", "id":initialize["id"], "result":result}),
    )
    .await?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let (tx, mut broker) = mpsc::channel(1);
    tasks.0.push(tokio::spawn(async move {
        loop {
            let frame = wire::read::<Output>(&mut socket_read).await;
            let failed = frame.is_err();
            if tx.send(frame).await.is_err() || failed {
                break;
            }
        }
    }));
    let mut history = History {
        encoding: result["capabilities"]["positionEncoding"]
            .as_str()
            .unwrap_or("utf-16")
            .into(),
        ..History::default()
    };
    let mut pending = BTreeMap::new();
    let mut inflight: Option<(Value, bool)> = None;
    let reason;
    loop {
        tokio::select! {
            value = input.recv(), if inflight.is_none() => {
                let Some(value) = value else { return Ok(()); };
                let value = value?;
                if value.get("method").is_some() && value.get("id").is_some() {
                    pending.insert(value["id"].to_string(), value["id"].clone());
                }
                let recorded = history.record(&value);
                if !recorded { inflight = Some((value, false)); reason = "unsaved replay capacity reached".into(); break; }
                let frame = Input::Message(value.clone());
                inflight = Some((value, History::retains(match &frame { Input::Message(value) => value, _ => unreachable!() })));
                if wire::write(&mut socket_write, &frame).await.is_err() { reason = "broker disconnected".into(); break; }
            }
            frame = broker.recv() => {
                match frame {
                    Some(Ok(Output::Ack)) => {
                        if inflight.as_ref().is_some_and(|(value, _)| value["method"] == "exit") { return Ok(()); }
                        inflight = None;
                    }
                    Some(Ok(Output::Message(value))) => {
                        if value["method"] == "neohelix/syncDocument" {
                            history.syncs.insert(value["id"].as_str().context("sync lacks ID")?.to_owned(), value["params"].clone());
                        }
                        if value.get("method").is_none() { pending.remove(&value["id"].to_string()); }
                        wire::write_lsp(&mut output, &value).await?;
                    }
                    Some(Ok(Output::Private(message))) => { reason = message; break; }
                    _ => { reason = "broker disconnected".into(); break; }
                }
            }
        }
    }
    drop(socket_write);
    for response in cancel_pending(&mut pending, &reason) {
        wire::write_lsp(&mut output, &response).await?;
    }
    wire::write_lsp(&mut output, &json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"type":2,"message":format!("Shared LSP: using an independent server ({reason})")}})).await?;
    let trigger = inflight.and_then(|(value, retained)| (!retained).then_some(value));
    private(
        spec,
        initialize,
        Some(result),
        history,
        input,
        output,
        trigger,
    )
    .await
}
