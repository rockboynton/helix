use anyhow::{ensure, Context};
use helix_core::{Tendril, Transaction, Uri};
use serde_json::{json, Value};

impl crate::Editor {
    pub fn rebind_shared_documents(
        &mut self,
        origin: helix_lsp::LanguageServerId,
        params: &Value,
    ) -> anyhow::Result<()> {
        for file in params["files"]
            .as_array()
            .context("missing renamed files")?
        {
            let old = file["oldUri"]
                .as_str()
                .context("missing old URI")?
                .parse::<helix_lsp::Url>()?;
            let new = file["newUri"]
                .as_str()
                .context("missing new URI")?
                .parse::<helix_lsp::Url>()?;
            let old = Uri::try_from(&old)?;
            let new = Uri::try_from(&new)?;
            let old = old.as_path().context("not a file URI")?;
            let new = new.as_path().context("not a file URI")?;
            let docs = self
                .documents
                .values()
                .filter(|doc| doc.language_servers().any(|server| server.id() == origin))
                .filter_map(|doc| {
                    doc.path()
                        .and_then(|path| path.strip_prefix(old).ok())
                        .map(|suffix| {
                            (
                                doc.id(),
                                if suffix.as_os_str().is_empty() {
                                    new.to_owned()
                                } else {
                                    new.join(suffix)
                                },
                            )
                        })
                })
                .collect::<Vec<_>>();
            if docs.is_empty() && self.document_by_path(new).is_none() {
                if let Some(server) = self.language_server_by_id(origin) {
                    let _ = server.send_custom_notification(
                        "neohelix/documentRebound".into(),
                        Some(
                            json!({"oldUri":file["oldUri"],"newUri":file["newUri"],"closed":true}),
                        ),
                    );
                }
            }
            for (id, path) in docs {
                self.set_doc_path(id, &path);
            }
        }
        self.needs_redraw = true;
        Ok(())
    }

    pub fn saved_shared_document(
        &mut self,
        origin: helix_lsp::LanguageServerId,
        params: &Value,
    ) -> anyhow::Result<()> {
        let url = params["uri"]
            .as_str()
            .context("missing URI")?
            .parse::<helix_lsp::Url>()?;
        let uri = Uri::try_from(&url)?;
        let Some(doc) = uri.as_path().and_then(|path| self.document_by_path(path)) else {
            return Ok(());
        };
        if !doc.language_servers().any(|server| server.id() == origin)
            || params["expectedVersion"].as_i64() != Some(doc.version() as i64)
        {
            return Ok(());
        }
        let id = doc.id();
        let view = self.get_synced_view_id(id);
        let doc = self.documents.get_mut(&id).unwrap();
        doc.append_changes_to_history(self.tree.get_mut(view));
        doc.reset_modified();
        let revision = doc.get_last_saved_revision();
        doc.set_last_saved_revision(revision, std::time::SystemTime::now());
        self.needs_redraw = true;
        Ok(())
    }

    pub fn sync_shared_document(
        &mut self,
        origin: helix_lsp::LanguageServerId,
        params: &Value,
    ) -> anyhow::Result<Value> {
        let url = params["uri"]
            .as_str()
            .context("missing URI")?
            .parse::<helix_lsp::Url>()?;
        let uri = Uri::try_from(&url)?;
        let path = uri.as_path().context("not a file URI")?;
        let Some(doc) = self.document_by_path(path) else {
            return Ok(json!({"applied":false,"closed":true}));
        };
        let id = doc.id();
        ensure!(
            doc.language_servers().any(|server| server.id() == origin),
            "document is not attached to this language server"
        );
        if params["expectedVersion"].as_i64() != Some(doc.version() as i64) {
            return Ok(json!({"applied":false,"version":doc.version()}));
        }
        let mut changes = Vec::new();
        let mut previous = 0;
        for change in params["changes"].as_array().context("missing changes")? {
            let start = change[0].as_u64().context("invalid start")? as usize;
            let end = change[1].as_u64().context("invalid end")? as usize;
            ensure!(
                previous <= start && start <= end && end <= doc.text().len_chars(),
                "invalid change range"
            );
            let text = change[2].as_str().context("invalid replacement")?;
            changes.push((start, end, (!text.is_empty()).then(|| Tendril::from(text))));
            previous = end;
        }
        let transaction = Transaction::change(doc.text(), changes.into_iter());
        let view_id = self.get_synced_view_id(id);
        let doc = self.documents.get_mut(&id).unwrap();
        doc.append_changes_to_history(self.tree.get_mut(view_id));
        ensure!(
            doc.apply_shared(&transaction, view_id, origin),
            "shared buffer is read-only"
        );
        doc.append_changes_to_history(self.tree.get_mut(view_id));
        self.needs_redraw = true;
        Ok(json!({"applied":true,"version":doc.version()}))
    }
}
