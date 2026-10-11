use std::{collections::HashMap, mem, time::Duration};

use helix_event::register_hook;
use helix_vcs::FileBlame;
use helix_view::{
    events::{DocumentDidOpen, EditorConfigDidChange},
    handlers::{BlameEvent, Handlers},
    DocumentId,
};
use tokio::time::Instant;

use crate::job;

/// Coalesces requests per document without dropping requests for other files.
#[derive(Default)]
pub struct BlameHandler {
    pending: HashMap<DocumentId, BlameEvent>,
}

impl helix_event::AsyncHook for BlameHandler {
    type Event = BlameEvent;

    fn handle_event(
        &mut self,
        event: Self::Event,
        _timeout: Option<tokio::time::Instant>,
    ) -> Option<tokio::time::Instant> {
        self.pending.insert(event.doc_id, event);
        Some(Instant::now() + Duration::from_millis(50))
    }

    fn finish_debounce(&mut self) {
        for (doc_id, event) in mem::take(&mut self.pending) {
            tokio::spawn(async move {
                let path = event.path;
                let expected_path = path.clone();
                let result =
                    match tokio::task::spawn_blocking(move || FileBlame::try_new(path)).await {
                        Ok(result) => result,
                        Err(err) => Err(err.into()),
                    };
                job::dispatch(move |editor, _| {
                    let Some(doc) = editor.document_mut(doc_id) else {
                        return;
                    };
                    if doc.path() != Some(&expected_path) {
                        return;
                    }
                    doc.file_blame = Some(result);
                    if !editor.config().inline_blame.auto_fetch {
                        if let Some(line) = event.line {
                            crate::commands::blame_line_impl(editor, doc_id, line);
                        } else {
                            editor.set_status("Blame for this file is now available")
                        }
                    }
                })
                .await;
            });
        }
    }
}

pub(super) fn register_hooks(handlers: &Handlers) {
    let tx = handlers.blame.clone();
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        if event.editor.config().inline_blame.auto_fetch {
            helix_event::send_blocking(
                &tx,
                BlameEvent {
                    path: event.path.to_path_buf(),
                    doc_id: event.doc,
                    line: None,
                },
            );
        }
        Ok(())
    });
    let tx = handlers.blame.clone();
    register_hook!(move |event: &mut EditorConfigDidChange<'_>| {
        let has_enabled_inline_blame = !event.old_config.inline_blame.auto_fetch
            && event.editor.config().inline_blame.auto_fetch;

        if has_enabled_inline_blame {
            // request blame for all documents, since any of them could have
            // outdated blame
            for doc in event.editor.documents() {
                if let Some(path) = doc.path() {
                    helix_event::send_blocking(
                        &tx,
                        BlameEvent {
                            path: path.to_path_buf(),
                            doc_id: doc.id(),
                            line: None,
                        },
                    );
                }
            }
        }
        Ok(())
    });
}
