//! Single-owner document and request routing policy, independent of sockets.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) enum Action {
    Server(Value),
    Client(u64, Value),
    Private(u64, &'static str),
}

struct Pending {
    client: u64,
    id: Value,
    tokens: Vec<String>,
}

struct Reply {
    id: Value,
    registration: Option<Value>,
    request: Value,
}

#[derive(Default)]
pub(super) struct Router {
    clients: BTreeSet<u64>,
    sequence: u64,
    pending: HashMap<u64, Pending>,
    owners: HashMap<String, (u64, i64)>,
    replies: HashMap<String, Reply>,
    registrations: BTreeMap<String, Value>,
    tokens: HashMap<String, (u64, Value)>,
    global_tokens: HashMap<String, (Value, BTreeSet<u64>)>,
    settings: Option<Value>,
}

fn key(value: &Value) -> String {
    value.to_string()
}
fn method(value: &Value) -> &str {
    value["method"].as_str().unwrap_or("")
}

impl Router {
    fn next(&mut self) -> u64 {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("exhausted LSP request IDs");
        self.sequence
    }

    pub(super) fn attach(&mut self, client: u64) -> Vec<Action> {
        self.clients.insert(client);
        if self.registrations.is_empty() {
            return Vec::new();
        }
        let registrations = self.registrations.values().cloned().collect::<Vec<_>>();
        let id = format!("neohelix/replay/{}", self.next());
        vec![Action::Client(
            client,
            json!({"jsonrpc":"2.0", "id":id,
            "method":"client/registerCapability", "params":{"registrations":registrations}}),
        )]
    }

    pub(super) fn detach(&mut self, client: u64) -> Vec<Action> {
        self.clients.remove(&client);
        self.tokens.retain(|_, (owner, _)| *owner != client);
        let mut actions = Vec::new();
        self.owners.retain(|uri, (owner, _)| {
            if *owner == client {
                actions.push(Action::Server(json!({"jsonrpc":"2.0", "method":"textDocument/didClose", "params":{"textDocument":{"uri":uri}}})));
                false
            } else { true }
        });
        self.pending.retain(|id, pending| {
            if pending.client == client {
                actions.push(Action::Server(
                    json!({"jsonrpc":"2.0", "method":"$/cancelRequest", "params":{"id":id}}),
                ));
                false
            } else {
                true
            }
        });
        // Transfer pending configuration/registration requests to a surviving
        // frontend. Losing the old leader must not lose filesystem watchers.
        let prefix = format!("{client}:");
        let mut retry = Vec::new();
        self.replies.retain(|id, original| {
            if id.starts_with(&prefix) {
                if self.clients.is_empty() {
                    actions.push(Action::Server(json!({"jsonrpc":"2.0", "id":original.id, "error":{"code":-32800,"message":"LSP frontend disconnected"}})));
                } else {
                    retry.push(original.request.clone());
                }
                false
            } else { true }
        });
        for request in retry {
            actions.extend(self.server(request));
        }
        actions
    }

    pub(super) fn client(&mut self, client: u64, mut value: Value) -> Vec<Action> {
        if !self.clients.contains(&client) {
            return Vec::new();
        }
        if value.get("method").is_none() {
            if let Some(reply) = self
                .replies
                .remove(&format!("{client}:{}", key(&value["id"])))
            {
                let mut actions = Vec::new();
                if value.get("error").is_none() {
                    if let Some(mut registration) = reply.registration {
                        if method(&registration) == "client/registerCapability" {
                            if let Some(items) = registration["params"]["registrations"].as_array()
                            {
                                for item in items {
                                    self.registrations.insert(key(&item["id"]), item.clone());
                                }
                            }
                        } else if let Some(items) = registration["params"]["unregisterations"]
                            .as_array()
                            .or_else(|| registration["params"]["unregistrations"].as_array())
                        {
                            for item in items {
                                self.registrations.remove(&key(&item["id"]));
                            }
                        }
                        registration["id"] = json!(format!("neohelix/replay/{}", self.next()));
                        actions.extend(
                            self.clients
                                .iter()
                                .filter(|id| **id != client)
                                .map(|id| Action::Client(*id, registration.clone())),
                        );
                    }
                }
                value["id"] = reply.id;
                actions.push(Action::Server(value));
                return actions;
            }
            if value.get("error").is_some()
                && value["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("neohelix/replay/"))
            {
                // A peer that rejected a copied watcher registration cannot
                // safely become the notification leader after another exits.
                return vec![Action::Private(
                    client,
                    "frontend rejected replayed registration",
                )];
            }
            return Vec::new(); // Synthetic registration/progress acknowledgements.
        }
        let name = method(&value).to_owned();
        if name == "initialized" || name == "exit" {
            return Vec::new();
        }
        if name == "shutdown" {
            return vec![Action::Client(
                client,
                json!({"jsonrpc":"2.0", "id":value["id"], "result":null}),
            )];
        }
        if name == "$/cancelRequest" {
            let id = &value["params"]["id"];
            if let Some((&upstream, _)) = self
                .pending
                .iter()
                .find(|(_, p)| p.client == client && p.id == *id)
            {
                value["params"]["id"] = json!(upstream);
                return vec![Action::Server(value)];
            }
            return Vec::new();
        }
        if name == "workspace/didChangeConfiguration" {
            let settings = value["params"]["settings"].clone();
            if let Some(current) = &self.settings {
                return if *current == settings {
                    Vec::new()
                } else {
                    vec![Action::Private(client, "language-server settings changed")]
                };
            }
            self.settings = Some(settings);
            return vec![Action::Server(value)];
        }
        if name == "workspace/didChangeWatchedFiles" || name == "$/setTrace" {
            return if self.clients.first() == Some(&client) {
                vec![Action::Server(value)]
            } else {
                Vec::new()
            };
        }
        let uri = value["params"]["textDocument"]["uri"]
            .as_str()
            .map(str::to_owned);
        if name == "textDocument/didOpen" {
            let Some(uri) = uri else {
                return vec![Action::Private(client, "invalid document open")];
            };
            if self.owners.contains_key(&uri) {
                return vec![Action::Private(
                    client,
                    "another client already has this document open",
                )];
            }
            self.owners.insert(
                uri,
                (
                    client,
                    value["params"]["textDocument"]["version"]
                        .as_i64()
                        .unwrap_or(0),
                ),
            );
            return vec![Action::Server(value)];
        }
        if let Some(uri) = &uri {
            if !self
                .owners
                .get(uri)
                .is_some_and(|(owner, _)| *owner == client)
            {
                return vec![Action::Private(
                    client,
                    "document is not owned by this client",
                )];
            }
        }
        if name == "textDocument/didClose" {
            if let Some(uri) = uri {
                self.owners.remove(&uri);
            }
            return vec![Action::Server(value)];
        }
        if name == "textDocument/didChange" {
            let Some(uri) = uri else {
                return vec![Action::Private(client, "change lacks a document")];
            };
            let version = value["params"]["textDocument"]["version"]
                .as_i64()
                .unwrap_or(-1);
            let (_, previous) = self.owners.get_mut(&uri).unwrap();
            if version <= *previous {
                return vec![Action::Private(client, "non-increasing document version")];
            }
            *previous = version;
            return vec![Action::Server(value)];
        }
        if matches!(
            name.as_str(),
            "textDocument/didSave" | "textDocument/willSave"
        ) {
            return vec![Action::Server(value)];
        }
        // Mutating/extension requests isolate before execution. No opaque
        // server data or state-changing command is replayed automatically.
        let safe = matches!(
            name.as_str(),
            "textDocument/hover"
                | "textDocument/completion"
                | "completionItem/resolve"
                | "textDocument/signatureHelp"
                | "textDocument/definition"
                | "textDocument/declaration"
                | "textDocument/typeDefinition"
                | "textDocument/implementation"
                | "textDocument/references"
                | "textDocument/documentHighlight"
                | "textDocument/documentSymbol"
                | "workspace/symbol"
                | "textDocument/semanticTokens/full"
                | "textDocument/semanticTokens/full/delta"
                | "textDocument/semanticTokens/range"
                | "textDocument/inlayHint"
                | "inlayHint/resolve"
                | "textDocument/foldingRange"
                | "textDocument/selectionRange"
                | "textDocument/documentLink"
                | "documentLink/resolve"
                | "textDocument/documentColor"
                | "textDocument/colorPresentation"
                | "textDocument/formatting"
                | "textDocument/rangeFormatting"
                | "textDocument/onTypeFormatting"
                | "textDocument/willSaveWaitUntil"
                | "textDocument/diagnostic"
                | "workspace/diagnostic"
        );
        if !safe || value.get("id").is_none() {
            return vec![Action::Private(
                client,
                "operation requires an independent language server; retry it",
            )];
        }
        if self.pending.len() >= 1024 || self.tokens.len() >= 1024 {
            return vec![Action::Private(client, "shared request capacity reached")];
        }
        let id = value["id"].clone();
        if self
            .pending
            .values()
            .any(|pending| pending.client == client && pending.id == id)
        {
            return vec![Action::Private(client, "duplicate request ID")];
        }
        let upstream = self.next();
        self.pending.insert(
            upstream,
            Pending {
                client,
                id,
                tokens: Vec::new(),
            },
        );
        value["id"] = json!(upstream);
        for field in ["workDoneToken", "partialResultToken"] {
            if let Some(token) = value["params"].get(field).cloned() {
                let token_id = format!("neohelix/{client}/{upstream}/{field}");
                self.tokens.insert(key(&json!(token_id)), (client, token));
                self.pending
                    .get_mut(&upstream)
                    .unwrap()
                    .tokens
                    .push(key(&json!(token_id)));
                value["params"][field] = json!(token_id);
            }
        }
        vec![Action::Server(value)]
    }

    pub(super) fn server(&mut self, mut value: Value) -> Vec<Action> {
        if value.get("method").is_none() {
            if let Some(pending) = value["id"].as_u64().and_then(|id| self.pending.remove(&id)) {
                for token in pending.tokens {
                    self.tokens.remove(&token);
                }
                value["id"] = pending.id;
                return vec![Action::Client(pending.client, value)];
            }
            return Vec::new();
        }
        let name = method(&value).to_owned();
        if let Some(id) = value.get("id").cloned() {
            if matches!(
                name.as_str(),
                "workspace/diagnostic/refresh"
                    | "workspace/semanticTokens/refresh"
                    | "workspace/inlayHint/refresh"
            ) {
                // Read-only invalidations affect every frontend. Acknowledge
                // once upstream; frontend acknowledgements use synthetic IDs.
                value["id"] = json!(format!("neohelix/refresh/{}", self.next()));
                let mut actions = self
                    .clients
                    .iter()
                    .map(|client| Action::Client(*client, value.clone()))
                    .collect::<Vec<_>>();
                actions.push(Action::Server(
                    json!({"jsonrpc":"2.0","id":id,"result":null}),
                ));
                return actions;
            }
            if name == "workspace/applyEdit" {
                return vec![Action::Server(
                    json!({"jsonrpc":"2.0", "id":id, "result":{"applied":false,"failureReason":"shared LSP does not apply cross-editor workspace edits"}}),
                )];
            }
            if name == "window/showDocument" || name == "window/showMessageRequest" {
                return vec![Action::Server(json!({"jsonrpc":"2.0", "id":id,
                    "result":if name == "window/showDocument" { json!({"success":false}) } else { Value::Null }}))];
            }
            if matches!(
                name.as_str(),
                "client/registerCapability" | "client/unregisterCapability"
            ) {
                let bytes = self
                    .registrations
                    .values()
                    .map(|v| v.to_string().len())
                    .sum::<usize>()
                    + self
                        .replies
                        .values()
                        .filter_map(|reply| reply.registration.as_ref())
                        .map(|v| v.to_string().len())
                        .sum::<usize>()
                    + value.to_string().len();
                if bytes > 8 * 1024 * 1024
                    || self.registrations.len()
                        + self
                            .replies
                            .values()
                            .filter_map(|reply| reply.registration.as_ref())
                            .map(|request| {
                                request["params"]["registrations"]
                                    .as_array()
                                    .map_or(0, Vec::len)
                            })
                            .sum::<usize>()
                        + value["params"]["registrations"]
                            .as_array()
                            .map_or(0, Vec::len)
                        > 256
                    || self.replies.len() >= 128
                {
                    return vec![Action::Server(
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"registration capacity reached"}}),
                    )];
                }
                if let Some(&client) = self.clients.first() {
                    let routed = json!(format!("neohelix/server/{}", self.next()));
                    self.replies.insert(
                        format!("{client}:{}", key(&routed)),
                        Reply {
                            id,
                            registration: Some(value.clone()),
                            request: value.clone(),
                        },
                    );
                    value["id"] = routed;
                    return vec![Action::Client(client, value)];
                }
            }
            if name == "window/workDoneProgress/create" {
                if self.global_tokens.len() >= 256 {
                    return vec![Action::Server(
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"progress capacity reached"}}),
                    )];
                }
                let original = key(&value["params"]["token"]);
                let routed = json!(format!("neohelix/global/{}", self.next()));
                self.global_tokens
                    .insert(original, (routed.clone(), self.clients.clone()));
                value["params"]["token"] = routed;
                value["id"] = json!(format!("neohelix/progress/{}", self.next()));
                let mut actions = self
                    .clients
                    .iter()
                    .map(|client| Action::Client(*client, value.clone()))
                    .collect::<Vec<_>>();
                actions.push(Action::Server(
                    json!({"jsonrpc":"2.0","id":id,"result":null}),
                ));
                return actions;
            }
            if name == "workspace/configuration" || name == "workspace/workspaceFolders" {
                if let Some(&client) = self.clients.first().filter(|_| self.replies.len() < 128) {
                    let routed = json!(format!("neohelix/server/{}", self.next()));
                    self.replies.insert(
                        format!("{client}:{}", key(&routed)),
                        Reply {
                            id,
                            registration: None,
                            request: value.clone(),
                        },
                    );
                    value["id"] = routed;
                    return vec![Action::Client(client, value)];
                }
            }
            return vec![Action::Server(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported shared LSP server request"}}),
            )];
        }
        if name == "$/progress" {
            if let Some((client, original)) =
                self.tokens.get(&key(&value["params"]["token"])).cloned()
            {
                if value["params"]["value"]["kind"] == "end" {
                    self.tokens.remove(&key(&value["params"]["token"]));
                }
                value["params"]["token"] = original;
                return vec![Action::Client(client, value)];
            }
            if let Some((routed, clients)) = self
                .global_tokens
                .get(&key(&value["params"]["token"]))
                .cloned()
            {
                if value["params"]["value"]["kind"] == "end" {
                    self.global_tokens.remove(&key(&value["params"]["token"]));
                }
                value["params"]["token"] = routed;
                return clients
                    .iter()
                    .filter(|id| self.clients.contains(id))
                    .map(|id| Action::Client(*id, value.clone()))
                    .collect();
            }
            return Vec::new();
        }
        if name == "textDocument/publishDiagnostics" {
            if let Some((client, _)) = value["params"]["uri"]
                .as_str()
                .and_then(|uri| self.owners.get(uri))
            {
                return vec![Action::Client(*client, value)];
            }
        }
        self.clients
            .iter()
            .map(|client| Action::Client(*client, value.clone()))
            .collect()
    }
}
