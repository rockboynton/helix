use super::{
    discovery,
    router::{Action, Router},
    wire, Input, Output, Spec, VERSION,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};
use tokio::{
    io::BufReader,
    net::{UnixListener, UnixStream},
    sync::mpsc,
};

enum Event {
    Attach(u64, Spec, Value, mpsc::Sender<Output>),
    Client(u64, Value),
    Gone(u64),
    Server(Value),
    ServerGone,
}

struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn connection(stream: UnixStream, id: u64, events: mpsc::Sender<Event>) -> Result<()> {
    discovery::authenticate(&stream)?;
    let (mut read, mut write) = stream.into_split();
    let input =
        tokio::time::timeout(Duration::from_secs(5), wire::read::<Input>(&mut read)).await??;
    let Input::Attach {
        version,
        spec,
        initialize,
    } = input
    else {
        anyhow::bail!("expected attachment");
    };
    ensure!(version == VERSION, "incompatible LSP broker protocol");
    let (tx, mut rx) = mpsc::channel(4);
    events.send(Event::Attach(id, spec, initialize, tx)).await?;
    let mut writer = tokio::spawn(async move {
        while let Some(output) = rx.recv().await {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(5), wire::write(&mut write, &output))
                    .await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });
    loop {
        let input = tokio::select! {
            input = wire::read::<Input>(&mut read) => input,
            _ = &mut writer => break,
        };
        match input {
            Ok(Input::Message(value)) => {
                if events.send(Event::Client(id, value)).await.is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    writer.abort();
    let _ = events.send(Event::Gone(id)).await;
    Ok(())
}

async fn dispatch(
    actions: Vec<Action>,
    clients: &mut HashMap<u64, mpsc::Sender<Output>>,
    server: &mut tokio::process::ChildStdin,
    router: &mut Router,
) -> Result<()> {
    let mut actions = std::collections::VecDeque::from(actions);
    while let Some(action) = actions.pop_front() {
        match action {
            Action::Server(value) => {
                tokio::time::timeout(Duration::from_secs(5), wire::write_lsp(server, &value))
                    .await
                    .context("language server stopped reading")??;
            }
            Action::Client(id, value) => {
                if let Some(tx) = clients.get(&id) {
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_millis(100),
                            tx.send(Output::Message(value))
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        clients.remove(&id);
                        actions.extend(router.detach(id));
                    }
                }
            }
            Action::Private(id, reason) => {
                if let Some(tx) = clients.remove(&id) {
                    // The frontend cannot submit another message until Ack.
                    // A full output queue is a disconnect, never unbounded RAM.
                    let _ = tokio::time::timeout(
                        Duration::from_millis(100),
                        tx.send(Output::Private(reason.into())),
                    )
                    .await;
                }
                actions.extend(router.detach(id));
            }
        }
    }
    Ok(())
}

pub(super) async fn serve(socket: PathBuf) -> Result<()> {
    discovery::validate_socket(&socket)?;
    let lock = discovery::lock(&socket, "serve")?;
    if !discovery::try_lock(&lock)? {
        return Ok(());
    }
    if let Ok(meta) = fs::symlink_metadata(&socket) {
        ensure!(
            meta.file_type().is_socket() && meta.uid() == discovery::uid(),
            "unsafe stale LSP socket"
        );
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    let _guard = SocketGuard(socket.clone());
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let (events, mut rx) = mpsc::channel(16);
    let mut counter = 0u64;
    let mut clients = HashMap::new();
    let mut router = Router::default();
    let mut process: Option<tokio::process::Child> = None;
    let mut stdin = None;
    let mut initialized: Option<Value> = None;
    let mut waiting = Vec::new();
    let idle = std::env::var("HELIX_SHARED_LSP_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(120)
        .clamp(1, 3600);
    let mut last_active = tokio::time::Instant::now();
    let started = tokio::time::Instant::now();
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    let expected = socket.file_stem().unwrap().to_str().unwrap().to_owned();
    let mut tasks = super::proxy::Tasks::default();
    let result = async { loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                counter = counter.checked_add(1).context("exhausted connection IDs")?;
                let tx = events.clone();
                let id = counter;
                tasks.0.retain(|task| !task.is_finished());
                if tasks.0.len() < 64 {
                    tasks.0.push(tokio::spawn(async move { let _ = connection(stream, id, tx).await; }));
                }
            }
            event = rx.recv() => {
                let mut actions = Vec::new();
                match event.context("broker event stream ended")? {
                    Event::Attach(id, spec, initialize, tx) => {
                        if clients.len() >= 8 || spec.key(&initialize).ok().as_deref() != Some(&expected) {
                            let _ = tx.try_send(Output::Private("incompatible or full broker".into()));
                            continue;
                        }
                        if process.is_none() {
                            let mut child = spec.spawn()?;
                            let mut input = child.stdin.take().unwrap();
                            let stdout = child.stdout.take().unwrap();
                            let mut params = initialize;
                            params["processId"] = json!(std::process::id());
                            let request = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":params});
                            tokio::time::timeout(Duration::from_secs(5), wire::write_lsp(&mut input, &request)).await??;
                            let tx = events.clone();
                            tasks.0.push(tokio::spawn(async move {
                                let mut reader = BufReader::new(stdout);
                                while let Ok(Some(value)) = wire::read_lsp(&mut reader).await {
                                    if tx.send(Event::Server(value)).await.is_err() { return; }
                                }
                                let _ = tx.send(Event::ServerGone).await;
                            }));
                            stdin = Some(input);
                            process = Some(child);
                        }
                        clients.insert(id, tx);
                        if let Some(result) = &initialized {
                            clients[&id].try_send(Output::Attached(result.clone()))?;
                            actions.extend(router.attach(id));
                        } else { waiting.push(id); }
                    }
                    Event::Client(id, value) => {
                        actions.extend(router.client(id, value));
                        let Some(input) = stdin.as_mut() else { continue; };
                        dispatch(actions, &mut clients, input, &mut router).await?;
                        if let Some(tx) = clients.get(&id) {
                            if !matches!(tokio::time::timeout(Duration::from_millis(100), tx.send(Output::Ack)).await, Ok(Ok(()))) {
                                clients.remove(&id);
                                dispatch(router.detach(id), &mut clients, input, &mut router).await?;
                            }
                        }
                        continue;
                    }
                    Event::Gone(id) => {
                        clients.remove(&id); waiting.retain(|waiting| *waiting != id); actions.extend(router.detach(id));
                        // No frontend remains to own filesystem watchers. Never
                        // retain a potentially stale index after the last exit.
                        if clients.is_empty() && initialized.is_some() { return Ok(()); }
                    }
                    Event::Server(value) => {
                        if initialized.is_none() && value.get("method").is_none() && value["id"] == 0 {
                            ensure!(value.get("error").is_none(), "language server initialization failed");
                            let result = value["result"].clone();
                            initialized = Some(result.clone());
                            let message = json!({"jsonrpc":"2.0","method":"initialized","params":{}});
                            tokio::time::timeout(Duration::from_secs(5), wire::write_lsp(stdin.as_mut().unwrap(), &message)).await??;
                            for id in waiting.drain(..) {
                                if let Some(tx) = clients.get(&id) {
                                    tx.try_send(Output::Attached(result.clone()))?;
                                    actions.extend(router.attach(id));
                                }
                            }
                        } else { actions.extend(router.server(value)); }
                    }
                    Event::ServerGone => return Ok(()),
                }
                if let Some(input) = stdin.as_mut() { dispatch(actions, &mut clients, input, &mut router).await?; }
                if !clients.is_empty() { last_active = tokio::time::Instant::now(); }
            }
            _ = timer.tick() => {
                ensure!(initialized.is_some() || started.elapsed() < Duration::from_secs(60), "language server initialization timed out");
                if clients.is_empty() && last_active.elapsed() >= Duration::from_secs(idle) { return Ok(()); }
            }
        }
    }}.await;
    if let Some(mut child) = process {
        if result.is_ok() && initialized.is_some() && child.try_wait().ok().flatten().is_none() {
            // Ordinary last-client exit gets the real server's shutdown path,
            // allowing it to reap cargo/proc-macro children before we resort to
            // killing it. Disconnect/crash handling must still be bounded.
            if let Some(input) = stdin.as_mut() {
                let graceful = async {
                    wire::write_lsp(input, &json!({"jsonrpc":"2.0","id":"neohelix/shutdown","method":"shutdown","params":null})).await?;
                    loop {
                        match rx.recv().await {
                            Some(Event::Server(value))
                                if value.get("method").is_none()
                                    && value["id"] == "neohelix/shutdown" =>
                            {
                                break
                            }
                            Some(Event::Server(value))
                                if value.get("method").is_some() && value.get("id").is_some() =>
                            {
                                wire::write_lsp(input, &json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32800,"message":"LSP broker shutting down"}})).await?;
                            }
                            Some(Event::ServerGone) | None => return Ok::<_, anyhow::Error>(()),
                            _ => (),
                        }
                    }
                    wire::write_lsp(input, &json!({"jsonrpc":"2.0","method":"exit"})).await?;
                    child.wait().await?;
                    Ok(())
                };
                let _ = tokio::time::timeout(Duration::from_secs(2), graceful).await;
            }
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}
