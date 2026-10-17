use super::*;
use serde_json::json;
use std::time::Duration;
use tokio::io::{self, BufReader, BufWriter};
use tokio::sync::mpsc::{channel, Receiver};
use tokio::time::timeout;

async fn read_wire(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn write_fragmented(writer: &mut (impl AsyncWrite + Unpin), value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    bytes.extend(body);
    for chunk in bytes.chunks(3) {
        writer.write_all(chunk).await.unwrap();
    }
    writer.flush().await.unwrap();
}

fn request(tx: &UnboundedSender<Payload>, method: &str, id: u64) -> Receiver<Result<Value>> {
    let (chan, rx) = channel(1);
    tx.send(Payload::Request {
        chan,
        value: jsonrpc::MethodCall {
            jsonrpc: Some(jsonrpc::Version::V2),
            method: method.into(),
            params: jsonrpc::Params::None,
            id: jsonrpc::Id::Num(id),
        },
    })
    .unwrap();
    rx
}

fn notification(method: &str) -> Payload {
    Payload::Notification(jsonrpc::Notification {
        jsonrpc: Some(jsonrpc::Version::V2),
        method: method.into(),
        params: jsonrpc::Params::None,
    })
}

#[tokio::test]
async fn buffered_streams_preserve_initialization_and_bidirectional_routing() {
    timeout(Duration::from_secs(5), async {
        // Small capacity forces partial reads/writes, including within UTF-8.
        let (client, peer) = io::duplex(32);
        let (read, write) = io::split(client);
        let (read_peer, mut write_peer) = io::split(peer);
        let mut read_peer = BufReader::new(read_peer);
        let (mut incoming, tx, initialized, _) = Transport::start(
            BufReader::new(read),
            BufWriter::new(write),
            io::empty(),
            LanguageServerId::default(),
            "duplex-test".into(),
        );
        let mut init = request(&tx, "initialize", 1);
        let mut queued = request(&tx, "test/queued", 2);
        assert_eq!(read_wire(&mut read_peer).await["method"], "initialize");
        write_fragmented(&mut write_peer, json!({"jsonrpc":"2.0", "id":1, "result":{}})).await;
        assert_eq!(init.recv().await.unwrap().unwrap(), json!({}));
        initialized.notify_one();
        let (_, call) = incoming.recv().await.unwrap();
        assert!(matches!(call, jsonrpc::Call::Notification(n) if n.method == "initialized"));
        assert_eq!(read_wire(&mut read_peer).await["id"], 2);
        write_fragmented(&mut write_peer, json!({"jsonrpc":"2.0", "id":2, "result":"λ🦀"})).await;
        assert_eq!(queued.recv().await.unwrap().unwrap(), json!("λ🦀"));

        write_fragmented(&mut write_peer, json!({"jsonrpc":"2.0", "id":"server-1", "method":"workspace/configuration"})).await;
        let (_, call) = incoming.recv().await.unwrap();
        assert!(matches!(call, jsonrpc::Call::MethodCall(m) if m.id == jsonrpc::Id::Str("server-1".into())));
        tx.send(Payload::Response(jsonrpc::Output::Success(jsonrpc::Success {
            jsonrpc: Some(jsonrpc::Version::V2),
            id: jsonrpc::Id::Str("server-1".into()),
            result: json!([{"setting":true}]),
        }))).unwrap();
        assert_eq!(read_wire(&mut read_peer).await["result"], json!([{"setting":true}]));

        write_fragmented(&mut write_peer, json!({"jsonrpc":"2.0", "method":"test/notice", "params":{"text":"🦀"}})).await;
        let (_, call) = incoming.recv().await.unwrap();
        assert!(matches!(call, jsonrpc::Call::Notification(n) if n.method == "test/notice"));
        drop(tx);
    }).await.expect("transport stalled");
}

#[tokio::test]
async fn stream_disconnect_fails_pending_requests_and_reports_exit() {
    timeout(Duration::from_secs(5), async {
        let (client, peer) = io::duplex(256);
        let (read, write) = io::split(client);
        let mut peer = BufReader::new(peer);
        let (mut incoming, tx, initialized, _) = Transport::start(
            BufReader::new(read),
            write,
            io::empty(),
            LanguageServerId::default(),
            "eof-test".into(),
        );
        initialized.notify_one();
        incoming.recv().await.unwrap();
        let mut response = request(&tx, "test/pending", 7);
        assert_eq!(read_wire(&mut peer).await["id"], 7);
        drop(peer);
        assert!(matches!(
            response.recv().await.unwrap(),
            Err(Error::StreamClosed)
        ));
        let (_, call) = incoming.recv().await.unwrap();
        assert!(matches!(call, jsonrpc::Call::Notification(n) if n.method == "exit"));
        drop(tx);
    })
    .await
    .expect("disconnect stalled");
}

#[tokio::test]
async fn shutdown_acknowledges_server_requests_and_flushes_exit() {
    timeout(Duration::from_secs(5), async {
        let (client, peer) = io::duplex(256);
        let (read, write) = io::split(client);
        let (read_peer, mut write_peer) = io::split(peer);
        let mut read_peer = BufReader::new(read_peer);
        let (mut incoming, tx, initialized, flushed) = Transport::start(
            BufReader::new(read), BufWriter::new(write), io::empty(), LanguageServerId::default(), "shutdown-test".into(),
        );
        initialized.notify_one();
        incoming.recv().await.unwrap();
        let mut response = request(&tx, "shutdown", 3);
        assert_eq!(read_wire(&mut read_peer).await["method"], "shutdown");
        write_fragmented(&mut write_peer, json!({"jsonrpc":"2.0", "id":"late-registration", "method":"client/registerCapability"})).await;
        let ack = read_wire(&mut read_peer).await;
        assert_eq!(ack["id"], "late-registration");
        assert_eq!(ack["result"], Value::Null);
        write_fragmented(&mut write_peer, json!({"jsonrpc":"2.0", "id":3, "result":null})).await;
        response.recv().await.unwrap().unwrap();
        tx.send(notification("exit")).unwrap();
        flushed.notified().await;
        assert_eq!(read_wire(&mut read_peer).await["method"], "exit");
        drop(tx);
    }).await.expect("shutdown stalled");
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_transports_requests_and_disconnects() {
    timeout(Duration::from_secs(5), async {
        let (client, peer) = tokio::net::UnixStream::pair().unwrap();
        let (read, write) = io::split(client);
        let mut peer = BufReader::new(peer);
        let (mut incoming, tx, initialized, _) = Transport::start(
            BufReader::new(read),
            BufWriter::new(write),
            io::empty(),
            LanguageServerId::default(),
            "socket-test".into(),
        );
        initialized.notify_one();
        incoming.recv().await.unwrap();
        let mut response = request(&tx, "test/socket", 17);
        assert_eq!(read_wire(&mut peer).await["id"], 17);
        write_fragmented(
            peer.get_mut(),
            json!({"jsonrpc":"2.0", "id":17, "result":"socket reply 🦀"}),
        )
        .await;
        assert_eq!(
            response.recv().await.unwrap().unwrap(),
            json!("socket reply 🦀")
        );
        drop(peer);
        let (_, call) = incoming.recv().await.unwrap();
        assert!(matches!(call, jsonrpc::Call::Notification(n) if n.method == "exit"));
        drop(tx);
    })
    .await
    .expect("socket transport stalled");
}

#[cfg(unix)]
#[tokio::test]
async fn child_pipes_still_transport_notifications() {
    timeout(Duration::from_secs(5), async {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (mut incoming, tx, initialized, _) = Transport::start(
            BufReader::new(child.stdout.take().unwrap()),
            BufWriter::new(child.stdin.take().unwrap()),
            BufReader::new(child.stderr.take().unwrap()),
            LanguageServerId::default(),
            "pipe-test".into(),
        );
        initialized.notify_one();
        incoming.recv().await.unwrap();
        tx.send(notification("test/echo")).unwrap();
        let (_, call) = incoming.recv().await.unwrap();
        assert!(matches!(call, jsonrpc::Call::Notification(n) if n.method == "test/echo"));
        drop(tx);
        child.start_kill().unwrap();
        child.wait().await.unwrap();
    })
    .await
    .expect("child-pipe transport stalled");
}
