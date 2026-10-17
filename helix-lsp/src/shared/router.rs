//! Shared document and request routing policy, independent of sockets.
use super::documents::{Documents, View};
use super::edits::{self, Edit, Snapshot};
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
    snapshot: Option<Snapshot>,
}

struct Reply {
    id: Value,
    registration: Option<Value>,
    request: Value,
}

struct Sync {
    client: u64,
    uri: String,
    expected: i64,
    target: View,
    tail: helix_core::ChangeSet,
}

#[derive(Default)]
pub(super) struct Router {
    clients: BTreeSet<u64>,
    sequence: u64,
    pending: HashMap<u64, Pending>,
    documents: Documents,
    syncs: HashMap<String, Sync>,
    replies: HashMap<String, Reply>,
    registrations: BTreeMap<String, Value>,
    tokens: HashMap<String, (u64, Value)>,
    global_tokens: HashMap<String, (Value, BTreeSet<u64>)>,
    settings: Option<Value>,
    edits: BTreeMap<String, Edit>,
    renamed: BTreeMap<(u64, String), String>,
    rebinding: BTreeSet<(u64, String)>,
}

fn key(value: &Value) -> String {
    value.to_string()
}
fn method(value: &Value) -> &str {
    value["method"].as_str().unwrap_or("")
}

impl Router {
    fn rename_files(&mut self, params: &Value) -> anyhow::Result<Vec<Action>> {
        use anyhow::Context;
        let mut moves = BTreeMap::new();
        for file in params["files"]
            .as_array()
            .context("missing renamed files")?
        {
            let old = file["oldUri"]
                .as_str()
                .context("missing old URI")?
                .parse::<crate::Url>()?
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("invalid old file URI"))?;
            let new = file["newUri"]
                .as_str()
                .context("missing new URI")?
                .parse::<crate::Url>()?
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("invalid new file URI"))?;
            for uri in self.documents.entries.keys() {
                let path = uri
                    .parse::<crate::Url>()?
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("invalid file URI"))?;
                if let Ok(suffix) = path.strip_prefix(&old) {
                    let destination = if suffix.as_os_str().is_empty() {
                        new.clone()
                    } else {
                        new.join(suffix)
                    };
                    let new_uri = crate::Url::from_file_path(destination)
                        .map_err(|_| anyhow::anyhow!("invalid destination"))?
                        .to_string();
                    if *uri != new_uri {
                        anyhow::ensure!(
                            !self.documents.entries.contains_key(&new_uri),
                            "rename destination has another open buffer; preserving both buffers"
                        );
                        moves.insert(uri.clone(), new_uri);
                    }
                }
            }
        }
        let mut actions = Vec::new();
        for (old, new) in moves {
            let doc = self.documents.entries.remove(&old).unwrap();
            actions.push(Action::Server(json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":old}}})));
            actions.push(Action::Server(json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":new,"version":doc.server.version,"languageId":doc.language_id,"text":doc.server.text.as_ref()}}})));
            for &client in doc.views.keys() {
                for ((owner, _), destination) in &mut self.renamed {
                    if *owner == client && *destination == old {
                        *destination = new.clone();
                    }
                }
                self.rebinding.remove(&(client, old.clone()));
                self.renamed.insert((client, old.clone()), new.clone());
                self.rebinding.insert((client, new.clone()));
                actions.push(Action::Client(client, json!({"jsonrpc":"2.0","method":"neohelix/filesRenamed","params":{"files":[{"oldUri":old,"newUri":new}]}})));
            }
            for sync in self.syncs.values_mut().filter(|sync| sync.uri == old) {
                sync.uri = new.clone();
            }
            self.documents.entries.insert(new, doc);
        }
        Ok(actions)
    }

    fn annotate_edit(&mut self, client: u64, value: &mut Value, snapshot: &Snapshot) {
        if !value.is_object()
            || (value.get("changes").is_none() && value.get("documentChanges").is_none())
        {
            return;
        }
        if self.edits.len() >= 128 {
            if let Some(oldest) = self
                .edits
                .iter()
                .min_by_key(|(_, edit)| edit.created)
                .map(|(key, _)| key.clone())
            {
                self.edits.remove(&oldest);
            }
        }
        let token = format!("neohelix/shared/{}/{}", std::process::id(), self.next());
        let original = value.clone();
        if !value["changeAnnotations"].is_object() {
            value["changeAnnotations"] = json!({});
        }
        value["changeAnnotations"][&token] = json!({"label":"Shared LSP version check"});
        self.edits.insert(
            token,
            Edit {
                client,
                original,
                snapshot: snapshot.clone(),
                created: std::time::Instant::now(),
                prepared: None,
            },
        );
    }

    fn edit_request(&mut self, client: u64, value: &Value) -> Vec<Action> {
        let result = (|| -> anyhow::Result<Vec<Action>> {
            let token = value["params"]["token"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing workspace edit token"))?;
            if value["method"] == "neohelix/prepareEdit" {
                let edit = self.edits.get(token).ok_or_else(|| {
                    anyhow::anyhow!("workspace edit expired or broker restarted; request it again")
                })?;
                anyhow::ensure!(
                    edit.client == client && edit.prepared.is_none(),
                    "workspace edit is not available to this client"
                );
                let plan = edit.prepare(&self.documents)?;
                let reply = json!({"edit":plan.local,"localUris":plan.changes.iter().filter(|change| change.local).map(|change| &change.uri).collect::<Vec<_>>()});
                self.edits.get_mut(token).unwrap().prepared = Some(plan);
                return Ok(vec![Action::Client(
                    client,
                    json!({"jsonrpc":"2.0","id":value["id"],"result":reply}),
                )]);
            }
            let edit = self
                .edits
                .get(token)
                .ok_or_else(|| anyhow::anyhow!("workspace edit expired"))?;
            anyhow::ensure!(
                edit.client == client,
                "workspace edit belongs to another client"
            );
            let edit = self.edits.remove(token).unwrap();
            let plan = edit
                .prepared
                .ok_or_else(|| anyhow::anyhow!("workspace edit was not prepared"))?;
            if value["params"]["applied"] != true {
                return Ok(vec![Action::Client(
                    client,
                    json!({"jsonrpc":"2.0","id":value["id"],"result":null}),
                )]);
            }
            anyhow::ensure!(
                edit.created.elapsed().as_secs() < 60,
                "workspace edit expired during application"
            );
            for change in &plan.changes {
                let doc = self
                    .documents
                    .entries
                    .get(&change.uri)
                    .ok_or_else(|| anyhow::anyhow!("document closed during workspace edit"))?;
                anyhow::ensure!(if change.local { doc.server.text.as_ref() == change.target || doc.server.version == change.version } else { doc.server.version == change.version }, "document changed during workspace edit; some local edits may already be applied; review or undo before retrying");
                if change.local && doc.server.text.as_ref() != change.target {
                    let report = value["params"]["documents"]
                        .as_array()
                        .and_then(|items| items.iter().find(|item| item["uri"] == change.uri))
                        .ok_or_else(|| anyhow::anyhow!("missing applied document state"))?;
                    anyhow::ensure!(
                        report["text"].as_str() == Some(change.target.as_str())
                            && report["version"].as_i64().is_some_and(|version| doc
                                .views
                                .get(&client)
                                .is_some_and(|view| version >= view.version)),
                        "applied document does not match prepared edit"
                    );
                }
            }
            let mut actions = Vec::new();
            for change in plan.changes {
                let doc = self.documents.entries.get_mut(&change.uri).unwrap();
                if doc.server.text.as_ref() == change.target {
                    continue;
                }
                let before = helix_core::Rope::from_str(&doc.server.text);
                let after = helix_core::Rope::from_str(&change.target);
                let encoding = match self.documents.encoding.as_str() {
                    "utf-8" => crate::OffsetEncoding::Utf8,
                    "utf-32" => crate::OffsetEncoding::Utf32,
                    _ => crate::OffsetEncoding::Utf16,
                };
                let changes = if self.documents.incremental {
                    json!(crate::Client::changeset_to_changes(
                        &before,
                        &after,
                        &change.changes,
                        encoding
                    ))
                } else {
                    json!([{"text":change.target}])
                };
                for view in doc.views.values_mut() {
                    view.pending = view.pending.clone().compose(change.changes.clone());
                }
                if change.local {
                    let report = value["params"]["documents"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["uri"] == change.uri)
                        .unwrap();
                    let view = doc.views.get_mut(&client).unwrap();
                    view.text = change.target.clone().into();
                    view.version = report["version"].as_i64().unwrap();
                    view.pending = helix_core::ChangeSet::new(after.slice(..));
                }
                for sync in self
                    .syncs
                    .values_mut()
                    .filter(|sync| sync.uri == change.uri)
                {
                    sync.tail = sync.tail.clone().compose(change.changes.clone());
                }
                doc.server.text = change.target.into();
                doc.server.version += 1;
                actions.push(Action::Server(json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":change.uri,"version":doc.server.version},"contentChanges":changes}})));
            }
            actions.extend(self.synchronize());
            actions.push(Action::Client(
                client,
                json!({"jsonrpc":"2.0","id":value["id"],"result":null}),
            ));
            Ok(actions)
        })();
        result.unwrap_or_else(|error| vec![Action::Client(client, json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32801,"message":error.to_string()}}))])
    }

    pub fn initialize(&mut self, result: &Value) {
        self.documents.encoding = result["capabilities"]["positionEncoding"]
            .as_str()
            .unwrap_or("utf-16")
            .into();
        let sync = &result["capabilities"]["textDocumentSync"];
        self.documents.incremental = *sync == 2 || sync["change"] == 2;
    }

    fn synchronize(&mut self) -> Vec<Action> {
        let mut tasks = Vec::new();
        for (uri, doc) in &self.documents.entries {
            for (&client, view) in &doc.views {
                if view.text != doc.server.text
                    && !self.rebinding.contains(&(client, uri.clone()))
                    && !self
                        .syncs
                        .values()
                        .any(|sync| sync.client == client && sync.uri == *uri)
                {
                    tasks.push((client, uri.clone(), view.clone(), doc.server.clone()));
                }
            }
        }
        let mut actions = Vec::new();
        for (client, uri, view, target) in tasks {
            let id = format!("neohelix/sync/{}", self.next());
            let changes = view
                .pending
                .changes_iter()
                .map(|(start, end, text)| {
                    json!([
                        start,
                        end,
                        text.map(|text| text.to_string()).unwrap_or_default()
                    ])
                })
                .collect::<Vec<_>>();
            actions.push(Action::Client(client, json!({"jsonrpc":"2.0","id":id,"method":"neohelix/syncDocument","params":{"uri":uri,"expectedVersion":view.version,"changes":changes}})));
            let tail =
                helix_core::ChangeSet::new(helix_core::Rope::from_str(&target.text).slice(..));
            self.syncs.insert(
                id,
                Sync {
                    client,
                    uri,
                    expected: view.version,
                    target,
                    tail,
                },
            );
        }
        actions
    }

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
        self.edits.retain(|_, edit| edit.client != client);
        self.renamed.retain(|(owner, _), _| *owner != client);
        self.rebinding.retain(|(owner, _)| *owner != client);
        self.tokens.retain(|_, (owner, _)| *owner != client);
        let mut actions = self
            .documents
            .detach(client)
            .into_iter()
            .map(Action::Server)
            .collect::<Vec<_>>();
        self.syncs.retain(|_, sync| sync.client != client);
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
                if method(&original.request) == "workspace/applyEdit" {
                    actions.push(Action::Server(json!({"jsonrpc":"2.0","id":original.id,"result":{"applied":false,"failureReason":"frontend disconnected during workspace edit; inspect changes before retrying"}})));
                } else if self.clients.is_empty() {
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
            if let Some(id) = value["id"].as_str() {
                if self.syncs.get(id).is_some_and(|sync| sync.client == client) {
                    let sync = self.syncs.remove(id).unwrap();
                    self.documents.acknowledge(
                        client,
                        &sync.uri,
                        sync.expected,
                        sync.target,
                        sync.tail,
                        &value["result"],
                    );
                    // A rejected stale request is retried after the queued local
                    // change; errors must not spin indefinitely.
                    if value.get("error").is_some() {
                        return vec![Action::Private(
                            client,
                            "frontend could not synchronize shared document",
                        )];
                    }
                    let mut actions = self.synchronize();
                    if let Some(doc) = self.documents.entries.get(&sync.uri) {
                        if let Some(view) = doc.views.get(&client) {
                            if doc.saved.as_ref() == Some(&view.text) {
                                actions.push(Action::Client(client, json!({"jsonrpc":"2.0","method":"neohelix/documentSaved","params":{"uri":sync.uri,"expectedVersion":view.version}})));
                            }
                        }
                    }
                    return actions;
                }
            }
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
        if name == "neohelix/filesRenamed" {
            return self.rename_files(&value["params"]).unwrap_or_else(|error| vec![Action::Client(client, json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"type":1,"message":format!("Shared file rename: {error}")}}))]);
        }
        if name == "neohelix/documentRebound" {
            let old = value["params"]["oldUri"].as_str().unwrap_or("").to_owned();
            if value["params"]["closed"] == true {
                if let Some(new) = self.renamed.remove(&(client, old)) {
                    self.rebinding.remove(&(client, new.clone()));
                    return self
                        .documents
                        .close(client, &new)
                        .into_iter()
                        .map(Action::Server)
                        .collect();
                }
            }
            return Vec::new();
        }
        if name == "neohelix/prepareEdit" || name == "neohelix/finishEdit" {
            return self.edit_request(client, &value);
        }
        if name == "neohelix/documentSaved" {
            let Some(doc) = value["params"]["uri"]
                .as_str()
                .and_then(|uri| self.documents.entries.get_mut(uri))
            else {
                return Vec::new();
            };
            if !doc.views.contains_key(&client) {
                return Vec::new();
            }
            doc.saved = value["params"]["text"].as_str().map(Into::into);
            return doc.views.iter().filter(|(id, view)| **id != client && value["params"]["text"].as_str() == Some(view.text.as_ref())).map(|(&client, view)| Action::Client(client, json!({"jsonrpc":"2.0","method":"neohelix/documentSaved","params":{"uri":value["params"]["uri"],"expectedVersion":view.version}}))).collect();
        }
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
        if matches!(
            name.as_str(),
            "workspace/didRenameFiles" | "workspace/didCreateFiles" | "workspace/didDeleteFiles"
        ) {
            return vec![Action::Server(value)];
        }
        if let Some(uri) = value["params"]["textDocument"]["uri"].as_str() {
            if let Some(new) = self.renamed.get(&(client, uri.to_owned())) {
                if name == "textDocument/didClose" {
                    return Vec::new();
                }
                value["params"]["textDocument"]["uri"] = json!(new);
            }
        }
        let uri = value["params"]["textDocument"]["uri"]
            .as_str()
            .map(str::to_owned);
        if name == "textDocument/didOpen" {
            if let Some(uri) = &uri {
                self.rebinding.remove(&(client, uri.clone()));
                self.renamed
                    .retain(|(owner, _), new| *owner != client || new != uri);
            }
            return match self.documents.open(client, value) {
                Ok(messages) => {
                    let mut actions = messages.into_iter().map(Action::Server).collect::<Vec<_>>();
                    actions.extend(self.synchronize());
                    actions
                }
                Err(error) => {
                    log::warn!("shared document open: {error}");
                    vec![Action::Private(client, "invalid document open")]
                }
            };
        }
        if let Some(uri) = &uri {
            if !self.documents.owns(client, uri) {
                return vec![Action::Private(
                    client,
                    "document is not owned by this client",
                )];
            }
            if value.get("id").is_some()
                && self.documents.entries.get(uri).is_some_and(|doc| {
                    doc.views
                        .get(&client)
                        .is_some_and(|view| view.text != doc.server.text)
                })
            {
                // Do not interpret positions from an older frontend against
                // newer canonical text. Waiting for its sync reply could
                // deadlock a frontend blocked on this very request.
                return vec![Action::Client(
                    client,
                    json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32801,"message":"shared document has pending peer changes; retry after synchronization"}}),
                )];
            }
        }
        if name == "textDocument/didClose" {
            if let Some(uri) = uri {
                self.syncs
                    .retain(|_, sync| sync.client != client || sync.uri != uri);
                return self
                    .documents
                    .close(client, &uri)
                    .into_iter()
                    .map(Action::Server)
                    .collect();
            }
            return Vec::new();
        }
        if name == "textDocument/didChange" {
            return match self.documents.change(client, &value) {
                Ok((messages, canonical)) => {
                    for sync in self
                        .syncs
                        .values_mut()
                        .filter(|sync| Some(&sync.uri) == uri.as_ref())
                    {
                        sync.tail = sync.tail.clone().compose(canonical.clone());
                    }
                    let mut actions = messages.into_iter().map(Action::Server).collect::<Vec<_>>();
                    actions.extend(self.synchronize());
                    actions
                }
                Err(error) => {
                    log::warn!("shared document change: {error}");
                    vec![Action::Private(client, "invalid document change")]
                }
            };
        }
        if matches!(
            name.as_str(),
            "textDocument/didSave" | "textDocument/willSave"
        ) {
            return vec![Action::Server(value)];
        }
        // Known mutations execute once; workspace edits are coordinated on
        // application. Unknown extensions still isolate before execution.
        let safe = matches!(
            name.as_str(),
            "textDocument/hover"
                | "textDocument/prepareRename"
                | "textDocument/rename"
                | "textDocument/codeAction"
                | "codeAction/resolve"
                | "workspace/executeCommand"
                | "workspace/willRenameFiles"
                | "workspace/willCreateFiles"
                | "workspace/willDeleteFiles"
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
                snapshot: matches!(
                    name.as_str(),
                    "textDocument/rename"
                        | "textDocument/codeAction"
                        | "codeAction/resolve"
                        | "workspace/willRenameFiles"
                        | "workspace/willCreateFiles"
                        | "workspace/willDeleteFiles"
                )
                .then(|| edits::snapshot(&self.documents)),
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
                if let Some(snapshot) = pending.snapshot.filter(|_| value.get("error").is_none()) {
                    let result = &mut value["result"];
                    self.annotate_edit(pending.client, result, &snapshot);
                    if let Some(items) = result.as_array_mut() {
                        for item in items {
                            self.annotate_edit(pending.client, &mut item["edit"], &snapshot);
                        }
                    } else if result.get("edit").is_some() {
                        self.annotate_edit(pending.client, &mut result["edit"], &snapshot);
                    }
                }
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
                if let Some(&client) = self.clients.first().filter(|_| self.replies.len() < 128) {
                    self.annotate_edit(
                        client,
                        &mut value["params"]["edit"],
                        &edits::snapshot(&self.documents),
                    );
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
            if let Some(doc) = value["params"]["uri"]
                .as_str()
                .and_then(|uri| self.documents.entries.get(uri))
            {
                if value["params"]["version"]
                    .as_i64()
                    .is_some_and(|version| version != doc.server.version)
                {
                    return Vec::new();
                }
                return doc
                    .views
                    .iter()
                    .filter(|(_, view)| view.text == doc.server.text)
                    .map(|(&client, view)| {
                        let mut value = value.clone();
                        value["params"]["version"] = json!(view.version);
                        Action::Client(client, value)
                    })
                    .collect();
            }
        }
        self.clients
            .iter()
            .map(|client| Action::Client(*client, value.clone()))
            .collect()
    }
}
