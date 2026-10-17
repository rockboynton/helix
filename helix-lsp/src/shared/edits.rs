//! Version-checked workspace edits. Files are never saved by the broker.
use super::documents::Documents;
use anyhow::{ensure, Context, Result};
use helix_core::{ChangeSet, Rope, Tendril};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

pub(super) type Snapshot = BTreeMap<String, (i64, Arc<str>)>;

pub(super) fn snapshot(documents: &Documents) -> Snapshot {
    documents
        .entries
        .iter()
        .map(|(uri, doc)| (uri.clone(), (doc.server.version, doc.server.text.clone())))
        .collect()
}

pub(super) struct Edit {
    pub client: u64,
    pub original: Value,
    pub snapshot: Snapshot,
    pub created: Instant,
    pub prepared: Option<Plan>,
}

pub(super) struct Change {
    pub uri: String,
    pub version: i64,
    pub target: String,
    pub changes: ChangeSet,
    pub local: bool,
}

pub(super) struct Plan {
    pub changes: Vec<Change>,
    pub local: Value,
}

fn remap(uri: &str, renames: &[(String, String)], reverse: bool) -> Result<String> {
    let mut path = uri
        .parse::<crate::Url>()?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("not a file URI"))?;
    let items: Box<dyn Iterator<Item = &(String, String)>> = if reverse {
        Box::new(renames.iter().rev())
    } else {
        Box::new(renames.iter())
    };
    for (old, new) in items {
        let (from, to) = if reverse { (new, old) } else { (old, new) };
        let from = from
            .parse::<crate::Url>()?
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("invalid rename URI"))?;
        let to = to
            .parse::<crate::Url>()?
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("invalid rename URI"))?;
        if let Ok(suffix) = path.strip_prefix(from) {
            path = if suffix.as_os_str().is_empty() {
                to
            } else {
                to.join(suffix)
            };
        }
    }
    Ok(crate::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("invalid remapped path"))?
        .to_string())
}

impl Edit {
    pub fn prepare(&self, documents: &Documents) -> Result<Plan> {
        ensure!(
            self.created.elapsed().as_secs() < 60,
            "workspace edit expired; request it again"
        );
        let mut local = self.original.clone();
        let mut renames = Vec::new();
        for operation in local["documentChanges"].as_array().into_iter().flatten() {
            if let Some(kind) = operation["kind"].as_str() {
                match kind {
                    "rename" => {
                        let old = operation["oldUri"]
                            .as_str()
                            .context("rename lacks old URI")?
                            .to_owned();
                        let new = operation["newUri"]
                            .as_str()
                            .context("rename lacks new URI")?
                            .to_owned();
                        // Detect destination collisions before the driver touches
                        // the filesystem; never replace another unsaved buffer.
                        let mapping = [(old.clone(), new.clone())];
                        for uri in documents.entries.keys() {
                            let destination = remap(uri, &mapping, false)?;
                            ensure!(
                                destination == *uri
                                    || !documents.entries.contains_key(&destination),
                                "rename destination has another open buffer"
                            );
                        }
                        renames.push((old, new));
                    }
                    "create" | "delete" => {
                        let uri = operation["uri"]
                            .as_str()
                            .context("resource operation lacks URI")?;
                        let prefix = uri.trim_end_matches('/').to_owned() + "/";
                        ensure!(
                            !documents
                                .entries
                                .keys()
                                .any(|open| open == uri || open.starts_with(&prefix)),
                            "refusing to create/delete a file with an open shared buffer"
                        );
                    }
                    _ => anyhow::bail!("unsupported resource operation"),
                }
            }
        }
        if let Some(annotations) = local["changeAnnotations"].as_object_mut() {
            annotations.retain(|key, _| !key.starts_with("neohelix/shared/"));
        }
        let mut changes = Vec::new();
        let mut process = |uri: &str, version: Option<i64>, edits: &Value| -> Result<bool> {
            let source = if documents.entries.contains_key(uri) {
                uri.to_owned()
            } else {
                remap(uri, &renames, true)?
            };
            let uri = source.as_str();
            let Some(doc) = documents.entries.get(uri) else {
                return Ok(true);
            };
            let (expected, before) = self
                .snapshot
                .get(uri)
                .context("file opened after workspace edit was requested; retry")?;
            ensure!(
                doc.server.version == *expected && &doc.server.text == before,
                "document changed since workspace edit was requested; retry"
            );
            ensure!(
                version.is_none_or(|version| version == *expected),
                "outdated workspace edit version"
            );
            ensure!(
                !changes.iter().any(|change: &Change| change.uri == uri),
                "multiple edits for the same document are not supported"
            );
            let own = doc.views.get(&self.client);
            ensure!(
                own.is_none_or(|view| view.text == *before),
                "requesting buffer has pending peer changes; retry"
            );
            let rope = Rope::from_str(before);
            let mut edits = edits
                .as_array()
                .context("invalid text edits")?
                .iter()
                .map(|edit| {
                    let start = super::documents::byte_offset(
                        before,
                        &edit["range"]["start"],
                        &documents.encoding,
                    )?;
                    let end = super::documents::byte_offset(
                        before,
                        &edit["range"]["end"],
                        &documents.encoding,
                    )?;
                    ensure!(start <= end, "reversed workspace edit");
                    Ok((
                        before[..start].chars().count(),
                        before[..end].chars().count(),
                        Some(Tendril::from(
                            edit["newText"].as_str().context("missing replacement")?,
                        )),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            edits.sort_by_key(|edit| (edit.0, edit.1));
            let mut end = 0;
            for edit in &edits {
                ensure!(edit.0 >= end, "overlapping workspace edits");
                end = edit.1;
            }
            let transaction = ChangeSet::from_changes(&rope, edits.into_iter());
            let mut after = rope;
            ensure!(transaction.apply(&mut after), "invalid workspace edit");
            changes.push(Change {
                uri: uri.into(),
                version: *expected,
                target: after.to_string(),
                changes: transaction,
                local: own.is_some(),
            });
            Ok(own.is_some())
        };
        if let Some(items) = local["documentChanges"].as_array_mut() {
            let mut retained = Vec::new();
            for mut item in std::mem::take(items) {
                if item.get("kind").is_some() {
                    retained.push(item);
                    continue;
                }
                let uri = item["textDocument"]["uri"]
                    .as_str()
                    .context("edit lacks URI")?
                    .to_owned();
                if process(
                    &uri,
                    item["textDocument"]["version"].as_i64(),
                    &item["edits"],
                )? {
                    let source = if documents.entries.contains_key(&uri) {
                        uri.clone()
                    } else {
                        remap(&uri, &renames, true)?
                    };
                    if let Some(view) = documents
                        .entries
                        .get(&source)
                        .and_then(|doc| doc.views.get(&self.client))
                    {
                        item["textDocument"]["version"] = json!(view.version);
                    }
                    retained.push(item);
                }
            }
            *items = retained;
        } else if let Some(items) = local["changes"].as_object_mut() {
            let mut retained = serde_json::Map::new();
            for (uri, edits) in std::mem::take(items) {
                if process(&uri, None, &edits)? {
                    retained.insert(uri, edits);
                }
            }
            *items = retained;
        }
        for change in &mut changes {
            change.uri = remap(&change.uri, &renames, false)?;
        }
        Ok(Plan { changes, local })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Documents {
        let mut documents = Documents::default();
        documents.open(1, json!({"params":{"textDocument":{"uri":"file:///old.rs","version":11,"languageId":"rust","text":"old"}}})).unwrap();
        documents
    }

    fn edit(documents: &Documents, original: Value) -> Edit {
        Edit {
            client: 1,
            original,
            snapshot: snapshot(documents),
            created: Instant::now(),
            prepared: None,
        }
    }

    fn replacement() -> Value {
        json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"newText":"new"})
    }

    #[test]
    fn translates_canonical_version_to_independent_frontend_version() {
        let documents = fixture();
        let plan = edit(&documents, json!({"documentChanges":[{"textDocument":{"uri":"file:///old.rs","version":1},"edits":[replacement()]}]})).prepare(&documents).unwrap();
        assert_eq!(
            plan.local["documentChanges"][0]["textDocument"]["version"],
            11
        );
        assert_eq!(plan.changes[0].target, "new");
    }

    #[test]
    fn rejects_stale_overlapping_and_open_buffer_deletion_before_mutation() {
        let mut documents = fixture();
        let stale = edit(
            &documents,
            json!({"changes":{"file:///old.rs":[replacement()]}}),
        );
        documents.change(1, &json!({"params":{"textDocument":{"uri":"file:///old.rs","version":12},"contentChanges":[{"text":"typing"}]}})).unwrap();
        assert!(stale.prepare(&documents).is_err());
        assert!(edit(
            &documents,
            json!({"changes":{"file:///old.rs":[replacement(),replacement()]}})
        )
        .prepare(&documents)
        .is_err());
        assert!(edit(
            &documents,
            json!({"documentChanges":[{"kind":"delete","uri":"file:///old.rs"}]})
        )
        .prepare(&documents)
        .is_err());
        assert_eq!(
            documents.entries["file:///old.rs"].server.text.as_ref(),
            "typing"
        );
    }

    #[test]
    fn rename_followed_by_text_edit_uses_original_snapshot_and_final_path() {
        let documents = fixture();
        let plan = edit(&documents, json!({"documentChanges":[{"kind":"rename","oldUri":"file:///old.rs","newUri":"file:///new.rs"},{"textDocument":{"uri":"file:///new.rs","version":1},"edits":[replacement()]}]})).prepare(&documents).unwrap();
        assert_eq!(plan.changes[0].uri, "file:///new.rs");
        assert_eq!(
            plan.local["documentChanges"][1]["textDocument"]["version"],
            11
        );
    }
}
