use super::*;
use router::{Action, Router};
use serde_json::json;

fn open(uri: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"rust","version":1,"text":text}}})
}
fn hover(id: u64, uri: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"textDocument/hover","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":0}}})
}
fn only_server(mut actions: Vec<Action>) -> Value {
    assert_eq!(actions.len(), 1);
    match actions.remove(0) {
        Action::Server(value) => value,
        _ => panic!("expected server action"),
    }
}
fn only_client(mut actions: Vec<Action>, expected: u64) -> Value {
    assert_eq!(actions.len(), 1);
    match actions.remove(0) {
        Action::Client(client, value) => {
            assert_eq!(client, expected);
            value
        }
        _ => panic!("expected client action"),
    }
}

#[test]
fn routing_separates_colliding_ids_cancellation_and_diagnostics() {
    let mut router = Router::default();
    router.attach(1);
    router.attach(2);
    only_server(router.client(1, open("file:///one.rs", "one")));
    only_server(router.client(2, open("file:///two.rs", "two")));
    let first = only_server(router.client(1, hover(1, "file:///one.rs")));
    let second = only_server(router.client(2, hover(1, "file:///two.rs")));
    assert_ne!(first["id"], second["id"]);
    let cancel =
        only_server(router.client(2, json!({"method":"$/cancelRequest","params":{"id":1}})));
    assert_eq!(cancel["params"]["id"], second["id"]);
    let result = only_client(
        router.server(json!({"jsonrpc":"2.0","id":second["id"],"result":"two"})),
        2,
    );
    assert_eq!(result["id"], 1);
    assert_eq!(result["result"], "two");
    let result = only_client(
        router.server(json!({"jsonrpc":"2.0","id":first["id"],"result":"one"})),
        1,
    );
    assert_eq!(result["id"], 1);
    only_client(router.server(json!({"method":"textDocument/publishDiagnostics","params":{"uri":"file:///one.rs","version":1,"diagnostics":[]}})), 1);
}

#[test]
fn overlapping_documents_and_mutating_requests_isolate_before_forwarding() {
    let mut router = Router::default();
    router.attach(1);
    router.attach(2);
    only_server(router.client(1, open("file:///same.rs", "unsaved one")));
    assert!(matches!(
        router
            .client(2, open("file:///same.rs", "unsaved two"))
            .as_slice(),
        [Action::Private(2, _)]
    ));
    assert!(matches!(
        router.client(2, hover(7, "file:///same.rs")).as_slice(),
        [Action::Private(2, _)]
    ));
    assert!(matches!(
        router
            .client(
                1,
                json!({"method":"workspace/executeCommand","id":3,"params":{"command":"dangerous"}})
            )
            .as_slice(),
        [Action::Private(1, _)]
    ));
    let closes = router.detach(1);
    assert_eq!(only_server(closes)["method"], "textDocument/didClose");
    only_server(router.client(2, open("file:///same.rs", "unsaved two")));
}

#[test]
fn settings_versions_and_server_edits_fail_closed() {
    let mut router = Router::default();
    router.attach(1);
    router.attach(2);
    let settings =
        json!({"method":"workspace/didChangeConfiguration","params":{"settings":{"feature":true}}});
    only_server(router.client(1, settings.clone()));
    assert!(router.client(2, settings).is_empty());
    assert!(matches!(router.client(2, json!({"method":"workspace/didChangeConfiguration","params":{"settings":{"feature":false}}})).as_slice(), [Action::Private(2, _)]));
    only_server(router.client(1, open("file:///one.rs", "one")));
    assert!(matches!(router.client(1, json!({"method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///one.rs","version":1},"contentChanges":[]}})).as_slice(), [Action::Private(1, _)]));
    let rejected = only_server(router.server(
        json!({"id":"edit","method":"workspace/applyEdit","params":{"edit":{"changes":{}}}}),
    ));
    assert_eq!(rejected["result"]["applied"], false);
}

#[test]
fn configuration_requests_route_back_and_registrations_replay_on_attachment() {
    let mut router = Router::default();
    router.attach(1);
    let configuration = only_client(
        router.server(
            json!({"method":"workspace/configuration","id":"upstream","params":{"items":[]}}),
        ),
        1,
    );
    let reply = only_server(router.client(1, json!({"id":configuration["id"],"result":[{}]})));
    assert_eq!(reply["id"], "upstream");
    let actions = router.server(json!({"method":"client/registerCapability","id":"registration","params":{"registrations":[{"id":"watch","method":"workspace/didChangeWatchedFiles","registerOptions":{"watchers":[]}}]}}));
    let registration = only_client(actions, 1);
    let acknowledged =
        only_server(router.client(1, json!({"id":registration["id"],"result":null})));
    assert_eq!(acknowledged["id"], "registration");
    let replay = only_client(router.attach(2), 2);
    assert_eq!(replay["params"]["registrations"][0]["id"], "watch");
    assert!(router
        .client(2, json!({"id":replay["id"],"result":null}))
        .is_empty());
    let event = json!({"method":"workspace/didChangeWatchedFiles","params":{"changes":[]}});
    only_server(router.client(1, event.clone()));
    assert!(router.client(2, event.clone()).is_empty());
    router.detach(1);
    only_server(router.client(2, event));
}

#[test]
fn progress_tokens_do_not_collide_between_clients() {
    let mut router = Router::default();
    router.attach(1);
    router.attach(2);
    let mut query = json!({"method":"workspace/symbol","id":1,"params":{"query":"q","partialResultToken":"same"}});
    let first = only_server(router.client(1, query.clone()));
    query["id"] = json!(2);
    let second = only_server(router.client(2, query));
    assert_ne!(
        first["params"]["partialResultToken"],
        second["params"]["partialResultToken"]
    );
    let progress = only_client(router.server(json!({"method":"$/progress","params":{"token":second["params"]["partialResultToken"],"value":[]}})), 2);
    assert_eq!(progress["params"]["token"], "same");
    let global = router.server(
        json!({"id":"create","method":"window/workDoneProgress/create","params":{"token":"same"}}),
    );
    let global_token = global
        .iter()
        .find_map(|action| match action {
            Action::Client(_, value) => Some(value["params"]["token"].clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(global_token, "same");
    let global_progress = router
        .server(json!({"method":"$/progress","params":{"token":"same","value":{"kind":"end"}}}));
    assert_eq!(global_progress.len(), 2);
    for action in global_progress {
        match action {
            Action::Client(_, value) => assert_eq!(value["params"]["token"], global_token),
            _ => panic!("expected progress"),
        }
    }
}

#[test]
fn pending_registration_moves_to_surviving_frontend() {
    let mut router = Router::default();
    router.attach(1);
    router.attach(2);
    let request = json!({"id":"watch","method":"client/registerCapability","params":{"registrations":[{"id":"w","method":"workspace/didChangeWatchedFiles"}]}});
    let old = only_client(router.server(request), 1);
    let reassigned = only_client(router.detach(1), 2);
    assert_ne!(old["id"], reassigned["id"]);
    let reply = only_server(router.client(2, json!({"id":reassigned["id"],"result":null})));
    assert_eq!(reply["id"], "watch");
    assert_eq!(
        only_client(router.attach(3), 3)["params"]["registrations"][0]["id"],
        "w"
    );
}

#[test]
fn registration_limit_includes_unacknowledged_requests() {
    let mut router = Router::default();
    router.attach(1);
    let registrations = (0..200)
        .map(|i| json!({"id":i,"method":"workspace/didChangeWatchedFiles"}))
        .collect::<Vec<_>>();
    only_client(router.server(json!({"id":1,"method":"client/registerCapability","params":{"registrations":registrations}})), 1);
    let rejected = only_server(router.server(json!({"id":2,"method":"client/registerCapability","params":{"registrations":registrations}})));
    assert!(rejected.get("error").is_some());
}

#[test]
fn diagnostic_refresh_reaches_all_frontends_but_acknowledges_once() {
    let mut router = Router::default();
    router.attach(1);
    router.attach(2);
    let actions = router.server(json!({"id":"refresh","method":"workspace/diagnostic/refresh"}));
    assert_eq!(actions.len(), 3);
    let mut acknowledgements = 0;
    for action in actions {
        match action {
            Action::Client(client, value) => assert!(router
                .client(client, json!({"id":value["id"],"result":null}))
                .is_empty()),
            Action::Server(value) => {
                assert_eq!(value["id"], "refresh");
                acknowledgements += 1;
            }
            _ => panic!("unexpected isolation"),
        }
    }
    assert_eq!(acknowledgements, 1);
}

#[test]
fn rejected_registration_is_not_replayed_to_other_clients() {
    let mut router = Router::default();
    router.attach(1);
    let request = only_client(router.server(json!({"id":"bad","method":"client/registerCapability","params":{"registrations":[{"id":"bad-watch","method":"workspace/didChangeWatchedFiles"}]}})), 1);
    let reply = only_server(router.client(
        1,
        json!({"id":request["id"],"error":{"code":-32602,"message":"invalid glob"}}),
    ));
    assert_eq!(reply["id"], "bad");
    assert!(router.attach(2).is_empty());
}

#[test]
fn peer_rejecting_replayed_watchers_is_isolated() {
    let mut router = Router::default();
    router.attach(1);
    let registration = only_client(router.server(json!({"id":1,"method":"client/registerCapability","params":{"registrations":[{"id":"watch","method":"workspace/didChangeWatchedFiles"}]}})), 1);
    only_server(router.client(1, json!({"id":registration["id"],"result":null})));
    let replay = only_client(router.attach(2), 2);
    assert!(matches!(
        router
            .client(
                2,
                json!({"id":replay["id"],"error":{"code":-32603,"message":"watch limit"}})
            )
            .as_slice(),
        [Action::Private(2, _)]
    ));
}

#[test]
fn replay_retains_exact_unsaved_change_sequence_and_closes() {
    let mut history = proxy::History::default();
    let opened = open("file:///one.rs", "λ🦀abc");
    let changed = json!({"method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///one.rs","version":2},"contentChanges":[{"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":6}},"text":"xyz"}]}});
    assert!(history.record(&opened));
    assert!(history.record(&changed));
    assert_eq!(history.replay(), [opened, changed]);
    assert!(history.record(&json!({"method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///one.rs"}}})));
    assert!(history.replay().is_empty());
    assert!(!history.record(&open("file:///huge.rs", &"x".repeat(8 * 1024 * 1024))));
    assert!(history.replay().is_empty());
}

#[tokio::test]
async fn wire_rejects_oversized_truncated_and_duplicate_headers() {
    use tokio::io::BufReader;
    let too_big = (wire::LIMIT as u32 + 1).to_le_bytes();
    assert!(wire::read::<Value>(&mut too_big.as_slice()).await.is_err());
    let truncated = [10u8, 0, 0, 0, b'{'];
    assert!(wire::read::<Value>(&mut truncated.as_slice())
        .await
        .is_err());
    let malformed = b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
    assert!(wire::read_lsp(&mut BufReader::new(&malformed[..]))
        .await
        .is_err());
    let huge_header = vec![b'x'; 9000];
    assert!(wire::read_lsp(&mut BufReader::new(huge_header.as_slice()))
        .await
        .is_err());
}

#[test]
fn compatibility_ignores_only_frontend_pid() {
    let root = tempfile::tempdir().unwrap();
    let spec = Spec::capture(std::env::current_exe().unwrap(), &[], root.path(), &[]).unwrap();
    let first =
        serde_json::json!({"processId":1,"capabilities":{"workspace":{"configuration":true}}});
    let mut second = first.clone();
    second["processId"] = serde_json::json!(2);
    assert_eq!(spec.key(&first).unwrap(), spec.key(&second).unwrap());
    second["capabilities"]["workspace"]["configuration"] = serde_json::json!(false);
    assert_ne!(spec.key(&first).unwrap(), spec.key(&second).unwrap());
    for field in ["rootUri", "workspaceFolders", "initializationOptions"] {
        let mut different = first.clone();
        different[field] = json!("different");
        assert_ne!(spec.key(&first).unwrap(), spec.key(&different).unwrap());
    }
    let mut different = spec.clone();
    different.args.push("--different".into());
    assert_ne!(spec.key(&first).unwrap(), different.key(&first).unwrap());
    different = spec.clone();
    different
        .environment
        .insert("LANG".into(), "different".into());
    assert_ne!(spec.key(&first).unwrap(), different.key(&first).unwrap());
    different = spec.clone();
    different.root = root.path().join("different");
    assert_ne!(spec.key(&first).unwrap(), different.key(&first).unwrap());
}
