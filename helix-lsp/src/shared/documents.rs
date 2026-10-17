//! One authoritative document, with independently versioned frontend views.
use anyhow::{bail, ensure, Context, Result};
use helix_core::{ChangeSet, Rope, Tendril};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone)]
pub(super) struct View {
    pub version: i64,
    pub text: Arc<str>,
    pub pending: ChangeSet,
}

pub(super) struct Document {
    pub views: BTreeMap<u64, View>,
    pub server: View,
    pub saved: Option<Arc<str>>,
    pub language_id: String,
}

#[derive(Default)]
pub(super) struct Documents {
    pub entries: BTreeMap<String, Document>,
    pub encoding: String,
    pub incremental: bool,
}

impl Documents {
    pub fn open(&mut self, client: u64, mut message: Value) -> Result<Vec<Value>> {
        let item = &message["params"]["textDocument"];
        let uri = item["uri"].as_str().context("open lacks URI")?.to_owned();
        let text: Arc<str> = item["text"].as_str().context("open lacks text")?.into();
        let version = item["version"].as_i64().context("open lacks version")?;
        let language_id = item["languageId"].as_str().unwrap_or("").to_owned();
        ensure!(
            self.entries.len() < 4096 || self.entries.contains_key(&uri),
            "shared document limit reached"
        );
        if let Some(doc) = self.entries.get_mut(&uri) {
            ensure!(
                text == doc.server.text
                    || version == 0
                    || doc.views.get(&client).is_some_and(|view| view.text == text),
                "joining buffer already contains divergent edits"
            );
            let pending = doc
                .views
                .get(&client)
                .filter(|view| view.text == text)
                .map(|view| view.pending.clone())
                .unwrap_or_else(|| {
                    super::text::transaction(&text, &doc.server.text)
                        .changes()
                        .clone()
                });
            doc.views.insert(
                client,
                View {
                    version,
                    text,
                    pending,
                },
            );
            // The joining window receives the authoritative unsaved contents
            // through a version-checked synchronization request.
            return Ok(Vec::new());
        }
        message["params"]["textDocument"]["version"] = json!(1);
        let mut views = BTreeMap::new();
        let pending = ChangeSet::new(Rope::from_str(&text).slice(..));
        views.insert(
            client,
            View {
                version,
                text: text.clone(),
                pending: pending.clone(),
            },
        );
        self.entries.insert(
            uri,
            Document {
                views,
                server: View {
                    version: 1,
                    text,
                    pending,
                },
                saved: None,
                language_id,
            },
        );
        Ok(vec![message])
    }

    pub fn close(&mut self, client: u64, uri: &str) -> Vec<Value> {
        if let Some(doc) = self.entries.get_mut(uri) {
            doc.views.remove(&client);
            if doc.views.is_empty() {
                self.entries.remove(uri);
                return vec![
                    json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}}),
                ];
            }
        }
        Vec::new()
    }

    pub fn detach(&mut self, client: u64) -> Vec<Value> {
        let uris = self.entries.keys().cloned().collect::<Vec<_>>();
        uris.iter()
            .flat_map(|uri| self.close(client, uri))
            .collect()
    }

    pub fn owns(&self, client: u64, uri: &str) -> bool {
        self.entries
            .get(uri)
            .is_some_and(|doc| doc.views.contains_key(&client))
    }

    pub fn change(&mut self, client: u64, message: &Value) -> Result<(Vec<Value>, ChangeSet)> {
        let uri = message["params"]["textDocument"]["uri"]
            .as_str()
            .context("change lacks URI")?;
        let version = message["params"]["textDocument"]["version"]
            .as_i64()
            .context("change lacks version")?;
        let doc = self.entries.get_mut(uri).context("document is not open")?;
        let view = doc
            .views
            .get_mut(&client)
            .context("frontend does not own document")?;
        ensure!(version > view.version, "non-increasing document version");
        let synchronized = view.text == doc.server.text;
        let (text, local) = transaction_from_changes(
            &view.text,
            &message["params"]["contentChanges"],
            &self.encoding,
        )?;
        let canonical = local.clone().map(view.pending.clone());
        let pending = view.pending.clone().map_with_assoc(local, true);
        let before = Rope::from_str(&doc.server.text);
        let mut after = before.clone();
        ensure!(
            canonical.apply(&mut after),
            "canonical change length mismatch"
        );
        *view = View {
            version,
            text: text.into(),
            pending,
        };
        for (&id, view) in &mut doc.views {
            if id != client {
                view.pending = view.pending.clone().compose(canonical.clone());
            }
        }
        let merged = after.to_string();
        if merged == doc.server.text.as_ref() {
            return Ok((Vec::new(), canonical));
        }
        let changes = if synchronized {
            message["params"]["contentChanges"].clone()
        } else if self.incremental {
            let encoding = match self.encoding.as_str() {
                "utf-8" => crate::OffsetEncoding::Utf8,
                "utf-32" => crate::OffsetEncoding::Utf32,
                _ => crate::OffsetEncoding::Utf16,
            };
            json!(crate::Client::changeset_to_changes(
                &before, &after, &canonical, encoding
            ))
        } else {
            json!([{"text":merged}])
        };
        doc.server.text = merged.into();
        doc.server.version += 1;
        Ok((
            vec![
                json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":doc.server.version},"contentChanges":changes}}),
            ],
            canonical,
        ))
    }

    pub fn acknowledge(
        &mut self,
        client: u64,
        uri: &str,
        expected: i64,
        target: View,
        pending: ChangeSet,
        result: &Value,
    ) {
        if result["applied"] != true {
            return;
        }
        if let Some(view) = self
            .entries
            .get_mut(uri)
            .and_then(|doc| doc.views.get_mut(&client))
        {
            if view.version == expected {
                if let Some(version) = result["version"].as_i64() {
                    *view = View {
                        version,
                        text: target.text,
                        pending,
                    };
                }
            }
        }
    }
}

pub(super) fn apply_changes(before: &str, changes: &Value, encoding: &str) -> Result<String> {
    transaction_from_changes(before, changes, encoding).map(|(text, _)| text)
}

fn transaction_from_changes(
    before: &str,
    changes: &Value,
    encoding: &str,
) -> Result<(String, ChangeSet)> {
    let mut text = before.to_owned();
    let mut combined = ChangeSet::new(Rope::from_str(before).slice(..));
    for change in changes.as_array().context("missing changes")? {
        let replacement = change["text"].as_str().context("change lacks text")?;
        let transaction = if let Some(range) = change.get("range") {
            let start = byte_offset(&text, &range["start"], encoding)?;
            let end = byte_offset(&text, &range["end"], encoding)?;
            ensure!(start <= end, "reversed document range");
            let rope = Rope::from_str(&text);
            let transaction = ChangeSet::from_changes(
                &rope,
                [(
                    text[..start].chars().count(),
                    text[..end].chars().count(),
                    Some(Tendril::from(replacement)),
                )]
                .into_iter(),
            );
            text.replace_range(start..end, replacement);
            transaction
        } else {
            let transaction = super::text::transaction(&text, replacement)
                .changes()
                .clone();
            text = replacement.into();
            transaction
        };
        combined = combined.compose(transaction);
    }
    Ok((text, combined))
}

pub(super) fn byte_offset(text: &str, position: &Value, encoding: &str) -> Result<usize> {
    let line = position["line"].as_u64().context("invalid line")? as usize;
    let character = position["character"]
        .as_u64()
        .context("invalid character")? as usize;
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n');
    for _ in 0..line {
        offset += lines.next().context("line out of bounds")?.len();
    }
    ensure!(
        line == 0 || offset < text.len() || text.ends_with('\n'),
        "line out of bounds"
    );
    let row = lines.next().unwrap_or("").trim_end_matches(['\r', '\n']);
    let mut units = 0;
    for (byte, ch) in row.char_indices() {
        if units == character {
            return Ok(offset + byte);
        }
        units += match encoding {
            "utf-8" => ch.len_utf8(),
            "utf-32" => 1,
            _ => ch.len_utf16(),
        };
        ensure!(units <= character, "position splits encoded character");
    }
    if units == character {
        Ok(offset + row.len())
    } else {
        bail!("character out of bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_typing_preserves_operation_order_and_pending_invariants() {
        let mut documents = Documents::default();
        for client in [1, 2] {
            documents.open(client, json!({"params":{"textDocument":{"uri":"file:///same.rs","text":"abc","version":0}}})).unwrap();
        }
        for index in 0..20 {
            for client in [1, 2] {
                let letter = if client == 1 { "🦀" } else { "é" };
                let position = if client == 1 { index * 2 } else { index };
                documents.change(client, &json!({"params":{"textDocument":{"uri":"file:///same.rs","version":index+1},"contentChanges":[{"range":{"start":{"line":0,"character":position},"end":{"line":0,"character":position}},"text":letter}]}})).unwrap();
                let doc = &documents.entries["file:///same.rs"];
                for view in doc.views.values() {
                    let mut actual = Rope::from_str(&view.text);
                    assert!(view.pending.apply(&mut actual));
                    assert_eq!(actual.to_string(), doc.server.text.as_ref());
                }
            }
        }
        let text = &documents.entries["file:///same.rs"].server.text;
        assert_eq!(text.chars().filter(|ch| *ch == '🦀').count(), 20);
        assert_eq!(text.chars().filter(|ch| *ch == 'é').count(), 20);
        assert!(text.ends_with("abc"));
    }

    #[test]
    fn encoded_positions_reject_surrogate_splits_and_invalid_lines() {
        assert_eq!(
            byte_offset("λ🦀abc", &json!({"line":0,"character":3}), "utf-16").unwrap(),
            6
        );
        assert!(byte_offset("λ🦀abc", &json!({"line":0,"character":2}), "utf-16").is_err());
        assert!(byte_offset("abc", &json!({"line":1,"character":0}), "utf-16").is_err());
        assert!(byte_offset("abc\n", &json!({"line":0,"character":4}), "utf-16").is_err());
        assert_eq!(
            byte_offset("abc\n", &json!({"line":1,"character":0}), "utf-16").unwrap(),
            4
        );
    }
}
