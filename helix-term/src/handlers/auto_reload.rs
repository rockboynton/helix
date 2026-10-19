use std::borrow::Cow;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use std::time::Duration;

use helix_core::file_watcher::{events_from_paths, EventType, FileSystemDidChange};
use helix_event::{dispatch, register_hook, send_blocking};
use helix_view::editor::Config;
use helix_view::events::ConfigDidChange;
use helix_view::handlers::{AutoReloadEvent, Handlers};
use helix_view::{DocumentId, Editor};
use tokio::time::Instant;

use crate::compositor::Compositor;
use crate::ui::{Prompt, PromptEvent};
use crate::{job, ui};

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use arc_swap::ArcSwap;
    use helix_core::{syntax, Rope, Tendril, Transaction};
    use helix_view::{editor::Action, graphics::Rect, theme};
    use std::sync::Arc;

    pub(crate) fn editor(auto_reload: bool) -> Editor {
        let mut config = crate::config::Config::default();
        config.editor.auto_reload.enable = auto_reload;
        config.editor.auto_reload.prompt_if_modified = false;
        config.editor.file_watcher.enable = false;
        static EVENTS: std::sync::Once = std::sync::Once::new();
        EVENTS.call_once(helix_event::register_event::<FileSystemDidChange>);
        config.editor.enable_steel = false;
        config.editor.lsp.enable = false;
        config.editor.word_completion.enable = false;
        config.editor.inline_blame.auto_fetch = false;
        let editor_config = Arc::new(ArcSwap::from_pointee(config.editor.clone()));
        // No global hooks: each unit test owns an independent editor/runtime.
        let handlers = helix_view::handlers::Handlers {
            completions: helix_view::handlers::completion::CompletionHandler::new(
                tokio::sync::mpsc::channel(8).0,
            ),
            signature_hints: tokio::sync::mpsc::channel(8).0,
            auto_save: tokio::sync::mpsc::channel(8).0,
            auto_reload: tokio::sync::mpsc::channel(8).0,
            document_colors: tokio::sync::mpsc::channel(8).0,
            blame: tokio::sync::mpsc::channel(8).0,
            document_links: tokio::sync::mpsc::channel(8).0,
            word_index: helix_view::handlers::word_index::Handler::spawn(),
            pull_diagnostics: tokio::sync::mpsc::channel(8).0,
            pull_all_documents_diagnostics: tokio::sync::mpsc::channel(8).0,
            code_action_hint: tokio::sync::mpsc::channel(8).0,
        };
        let mut editor = Editor::new(
            Rect::new(0, 0, 80, 24),
            Arc::new(theme::Loader::new(&[])),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
            editor_config,
            handlers,
            helix_loader::workspace_trust::WorkspaceTrust::fully_trusted(),
        );
        editor.new_file(Action::VerticalSplit);
        editor
    }

    fn check(editor: &mut Editor) -> bool {
        if !editor.config().auto_reload.enable {
            return false;
        }
        let mut compositor = Compositor::new(Rect::new(0, 0, 80, 24));
        let paths = changed_unwatched_paths(editor);
        let mut handled = false;
        for path in paths {
            let id = editor.document_id_by_path(&path).unwrap();
            let stamp = path
                .metadata()
                .ok()
                .map(|meta| (meta.modified().unwrap(), meta.len()));
            let doc = editor.document(id).unwrap();
            handled |= stamp != doc.auto_reload_seen_stamp;
            handle_document_change(editor, &mut compositor, id, false);
        }
        handled
    }

    #[tokio::test]
    async fn reloads_clean_buffer_and_preserves_undo() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut editor = editor(true);
        let id = editor.open(&path, Action::Replace).unwrap();
        std::fs::write(&path, "new and longer\n").unwrap();
        assert!(check(&mut editor));
        let mut view = editor.tree.get(editor.tree.focus).clone();
        let doc = editor.document_mut(id).unwrap();
        assert_eq!(doc.text(), &Rope::from_str("new and longer\n"));
        doc.undo(&mut view);
        assert_eq!(doc.text(), &Rope::from_str("old\n"));
    }

    #[tokio::test]
    async fn dirty_buffer_is_not_overwritten_and_warning_is_deduplicated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut editor = editor(true);
        let id = editor.open(&path, Action::Replace).unwrap();
        let view = editor.tree.focus;
        let doc = editor.document_mut(id).unwrap();
        let transaction = Transaction::change(
            doc.text(),
            [(0, 3, Some(Tendril::from("unsaved")))].into_iter(),
        );
        assert!(doc.apply(&transaction, view));
        std::fs::write(&path, "external\n").unwrap();

        assert!(check(&mut editor));
        assert_eq!(
            editor.document(id).unwrap().text(),
            &Rope::from_str("unsaved\n")
        );
        assert!(!check(&mut editor));
    }

    #[tokio::test]
    async fn detects_atomic_replacement_of_a_hidden_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut editor = editor(true);
        let id = editor.open(&path, Action::Replace).unwrap();
        editor.new_file(Action::Replace);
        let replacement = directory.path().join("replacement.txt");
        std::fs::write(&replacement, "replacement\n").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(check(&mut editor));
        assert_eq!(
            editor.document(id).unwrap().text(),
            &Rope::from_str("replacement\n")
        );
    }

    #[tokio::test]
    async fn disabled_reload_leaves_the_buffer_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        std::fs::write(&path, "old\n").unwrap();
        let mut editor = editor(false);
        let id = editor.open(&path, Action::Replace).unwrap();
        std::fs::write(&path, "external\n").unwrap();
        assert!(!check(&mut editor));
        assert_eq!(
            editor.document(id).unwrap().text(),
            &Rope::from_str("old\n")
        );
    }

    #[tokio::test]
    async fn deleted_buffer_is_preserved_and_recreation_reloads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        std::fs::write(&path, "original\n").unwrap();
        let mut editor = editor(true);
        let id = editor.open(&path, Action::Replace).unwrap();
        std::fs::remove_file(&path).unwrap();
        check(&mut editor);
        assert!(editor.document(id).unwrap().auto_reload_deleted);
        assert_eq!(
            editor.document(id).unwrap().text().to_string(),
            "original\n"
        );
        assert!(changed_unwatched_paths(&editor).is_empty());
        std::fs::write(&path, "recreated\n").unwrap();
        check(&mut editor);
        assert_eq!(
            editor.document(id).unwrap().text().to_string(),
            "recreated\n"
        );
    }

    #[test]
    fn accepts_boolean_and_detailed_auto_reload_configuration() {
        let old: Config = toml::from_str("auto-reload = false").unwrap();
        assert!(!old.auto_reload.enable);
        let detailed: Config = toml::from_str("[auto-reload]\nenable = true\nprompt-if-modified = false\n[auto-reload.poll]\ninterval = 1000").unwrap();
        assert!(detailed.auto_reload.enable);
        assert!(!detailed.auto_reload.prompt_if_modified);
        assert_eq!(detailed.auto_reload.poll.interval, 1000);
    }
}

/// Handler for FileSystemDidChange events (from filesentry or polling)
struct ReloadHandler {
    enable: AtomicBool,
    prompt_if_modified: AtomicBool,
}

impl ReloadHandler {
    pub fn refresh_config(&self, config: &Config) {
        self.enable
            .store(config.auto_reload.enable, atomic::Ordering::Relaxed);
        self.prompt_if_modified.store(
            config.auto_reload.prompt_if_modified,
            atomic::Ordering::Relaxed,
        );
    }

    fn on_file_did_change(&self, event: &mut FileSystemDidChange) {
        if !self.enable.load(atomic::Ordering::Relaxed) {
            return;
        }
        let fs_events = event.fs_events.clone();
        if !fs_events
            .iter()
            .any(|event| event.ty != EventType::Tempfile)
        {
            return;
        }
        let prompt_if_modified = self.prompt_if_modified.load(atomic::Ordering::Relaxed);
        job::dispatch_blocking(move |editor, compositor| {
            let mut vcs_reload = false;

            for fs_event in &*fs_events {
                if fs_event.ty == EventType::Tempfile {
                    continue;
                }
                vcs_reload |= editor.diff_providers.needs_reload(fs_event);

                let Some(doc_id) = document_id_for_event(editor, fs_event.path.as_std_path())
                else {
                    continue;
                };

                handle_document_change(editor, compositor, doc_id, prompt_if_modified);
            }

            if vcs_reload {
                reload_vcs_diffs(editor);
            }
        });
    }
}

/// Handler for polling-based change detection for unwatched files.
/// Polls documents not covered by the file watcher (e.g., outside workspace or in ignored dirs).
/// Co-Authored-By: Anthony Rubick <68485672+AnthonyMichaelTDM@users.noreply.github.com>
#[derive(Debug)]
pub(super) struct PollHandler;

impl PollHandler {
    pub fn new() -> Self {
        PollHandler
    }
}

impl helix_event::AsyncHook for PollHandler {
    type Event = AutoReloadEvent;

    fn handle_event(
        &mut self,
        event: Self::Event,
        _existing_debounce: Option<Instant>,
    ) -> Option<Instant> {
        match event {
            AutoReloadEvent::PollAfter { interval } => {
                Some(Instant::now() + Duration::from_millis(interval.max(1)))
            }
        }
    }

    fn finish_debounce(&mut self) {
        job::dispatch_blocking(move |editor, _compositor| {
            let config = editor.config();
            if !config.auto_reload.enable || !config.auto_reload.poll.enable {
                return;
            }

            let poll_interval = config.auto_reload.poll.interval;

            // Check unwatched documents for external modifications
            let modified_paths = changed_unwatched_paths(editor);

            // Poll extra watched paths (e.g., VCS HEAD files outside workspace)
            let extra_path_changes = editor.file_watcher.poll_extra_paths();

            // Dispatch changes through the FileSystemDidChange hook
            let all_changed: Vec<PathBuf> = modified_paths
                .into_iter()
                .chain(extra_path_changes)
                .collect();
            if !all_changed.is_empty() {
                let events = events_from_paths(all_changed);
                dispatch(FileSystemDidChange { fs_events: events });
            }

            // Schedule next poll
            send_blocking(
                &editor.handlers.auto_reload,
                AutoReloadEvent::PollAfter {
                    interval: poll_interval,
                },
            );
        });
    }
}

/// Low-frequency backstop for missed/deferred native events and unwatched files.
fn changed_unwatched_paths(editor: &Editor) -> Vec<PathBuf> {
    editor
        .documents()
        .filter_map(|doc| {
            let path = doc.path()?;
            // Native notifications can arrive during a save or insert session.
            // Retain a low-frequency consistency check even for covered files.
            let changed = match path.metadata() {
                Ok(metadata) => metadata.modified().ok().is_some_and(|mtime| {
                    mtime != doc.last_saved_time()
                        || doc
                            .last_saved_file_size()
                            .is_some_and(|len| len != metadata.len())
                        || doc.auto_reload_deleted
                }),
                Err(err) => err.kind() == io::ErrorKind::NotFound && !doc.auto_reload_deleted,
            };
            changed.then(|| path.to_path_buf())
        })
        .collect()
}

/// Re-check unwatched open files when the terminal regains focus -- they are only
/// polled otherwise, and regaining focus is when a user is most likely to have
/// just edited them elsewhere. Routes through the same `FileSystemDidChange` path
/// as the watcher and poll (so prompt de-duplication still applies).
pub(crate) fn on_focus_gained(editor: &Editor) {
    if !editor.config().auto_reload.enable {
        return;
    }
    let changed = changed_unwatched_paths(editor);
    if !changed.is_empty() {
        dispatch(FileSystemDidChange {
            fs_events: events_from_paths(changed),
        });
    }
}

// Native backends report canonical paths, while open buffers can retain a
// symlink alias (notably /var versus /private/var on macOS).
fn document_id_for_event(editor: &Editor, path: &std::path::Path) -> Option<DocumentId> {
    editor.document_id_by_path(path).or_else(|| {
        editor.documents().find_map(|doc| {
            let doc_path = doc.path()?;
            let canonical = doc_path.canonicalize().ok().or_else(|| {
                Some(
                    doc_path
                        .parent()?
                        .canonicalize()
                        .ok()?
                        .join(doc_path.file_name()?),
                )
            })?;
            (canonical == path).then_some(doc.id())
        })
    })
}

/// Handler for document changes detected by filesentry or polling.
fn handle_document_change(
    editor: &mut Editor,
    compositor: &mut Compositor,
    doc_id: DocumentId,
    prompt_if_modified: bool,
) {
    if !editor.config().auto_reload.enable || editor.write_count != 0 {
        return;
    }
    let Some(doc) = editor.document(doc_id) else {
        return;
    };
    let Some(path) = doc.path().map(ToOwned::to_owned) else {
        return;
    };
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if !doc.auto_reload_deleted {
                editor.document_mut(doc_id).unwrap().auto_reload_deleted = true;
                editor.set_warning(format!(
                    "{} was deleted externally; the buffer was preserved",
                    path.display()
                ));
            }
            return;
        }
        Err(err) => {
            log::warn!("could not stat {} for auto-reload: {err}", path.display());
            return;
        }
    };
    let Ok(mtime) = metadata.modified() else {
        return;
    };
    let stamp = (mtime, metadata.len());
    if mtime == doc.last_saved_time()
        && doc
            .last_saved_file_size()
            .is_none_or(|len| len == metadata.len())
    {
        let doc = editor.document_mut(doc_id).unwrap();
        doc.auto_reload_seen_stamp = None;
        doc.auto_reload_deleted = false;
        return;
    }
    if doc.is_modified() {
        if doc.auto_reload_seen_stamp == Some(stamp) {
            return;
        }
        editor.document_mut(doc_id).unwrap().auto_reload_seen_stamp = Some(stamp);
        if prompt_if_modified {
            prompt_reload_modified(compositor, doc_id, path.display().to_string());
        } else {
            editor.set_warning(format!(
                "{} changed externally but has unsaved changes, use :reload to refresh",
                path.display()
            ));
        }
        return;
    }
    // Keep active insert sessions stable. Polling remains a backstop for changes
    // deferred here, including files otherwise covered by the native watcher.
    if editor.mode == helix_view::document::Mode::Insert
        && editor.tree.get(editor.tree.focus).doc == doc_id
    {
        return;
    }
    match reload_document(editor, doc_id) {
        Ok(()) => editor.set_status(format!("{} reloaded (external changes)", path.display())),
        Err(err) => editor.set_error(format!("{} auto-reload failed: {err}", path.display())),
    }
}

/// Reload a document without disturbing the focused view of a hidden buffer,
/// and retain the fork's trust boundary for VCS/filter execution.
fn reload_document(editor: &mut Editor, doc_id: DocumentId) -> anyhow::Result<()> {
    let doc = editor
        .document(doc_id)
        .ok_or_else(|| anyhow::anyhow!("document was closed"))?;
    let trust = editor
        .workspace_trust
        .query(
            doc.workspace_root(),
            helix_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted();
    let mut view = editor
        .tree
        .views()
        .find(|(view, _)| view.doc == doc_id)
        .map(|(view, _)| view.clone())
        .unwrap_or_else(|| editor.tree.get(editor.tree.focus).clone());
    view.doc = doc_id;
    let doc = editor.documents.get_mut(&doc_id).unwrap();
    doc.ensure_view_init(view.id);
    doc.reload(&mut view, &mut editor.diff_providers, trust)?;
    doc.file_blame = None;
    doc.is_blame_potentially_out_of_date = true;
    doc.auto_reload_seen_stamp = None;
    doc.auto_reload_deleted = false;
    let scrolloff = editor.config().scrolloff;
    for (view, _) in editor
        .tree
        .views_mut()
        .filter(|(view, _)| view.doc == doc_id)
    {
        view.sync_changes(editor.documents.get_mut(&doc_id).unwrap());
        view.ensure_cursor_in_view(editor.documents.get_mut(&doc_id).unwrap(), scrolloff);
    }
    super::blame::request_visible_annotations(editor);
    Ok(())
}

/// Reload VCS diffs for all documents
fn reload_vcs_diffs(editor: &mut Editor) {
    for doc in editor.documents.values_mut() {
        let Some(path) = doc.path().map(std::path::Path::to_path_buf) else {
            continue;
        };
        doc.set_version_control_head(editor.diff_providers.get_current_head_name(&path));
        match editor.diff_providers.get_diff_base(&path) {
            Some(diff_base) => doc.set_diff_base(diff_base),
            None => doc.diff_handle = None,
        }
    }
}

/// Shows a prompt asking the user whether to reload a modified document.
/// Co-Authored-By: Anthony Rubick <68485672+AnthonyMichaelTDM@users.noreply.github.com>
fn prompt_reload_modified(compositor: &mut Compositor, doc_id: DocumentId, path_str: String) {
    let prompt = Prompt::new(
        Cow::Owned(format!(
            "{path_str} changed externally (unsaved changes exist). Press Enter to reload, Esc to ignore: "
        )),
        None,
        ui::completers::none,
        move |cx, _input, event| {
            match event {
                PromptEvent::Validate => {
                    match reload_document(cx.editor, doc_id) {
                        Ok(_) => {
                            cx.editor.set_status(format!("{path_str} reloaded"));
                        }
                        Err(err) => {
                            cx.editor
                                .set_error(format!("{path_str} reload failed: {err}"));
                        }
                    }
                }
                PromptEvent::Abort => {
                    cx.editor
                        .set_status(format!("{path_str} external changes ignored"));
                }
                PromptEvent::Update => {}
            }
        },
    );
    compositor.push(Box::new(prompt));
}

pub(super) fn register_hooks(handlers: &Handlers, config: &Config) {
    // Register handler for FileSystemDidChange events (from filesentry)
    let handler = Arc::new(ReloadHandler {
        enable: config.auto_reload.enable.into(),
        prompt_if_modified: config.auto_reload.prompt_if_modified.into(),
    });
    let handler_ = handler.clone();
    let poll = handlers.auto_reload.clone();
    register_hook!(move |event: &mut ConfigDidChange<'_>| {
        handler_.refresh_config(event.new);
        if event.new.auto_reload.enable && event.new.auto_reload.poll.enable {
            send_blocking(
                &poll,
                AutoReloadEvent::PollAfter {
                    interval: event.new.auto_reload.poll.interval.max(1),
                },
            );
        }
        Ok(())
    });
    register_hook!(move |event: &mut FileSystemDidChange| {
        handler.on_file_did_change(event);
        Ok(())
    });

    // Start polling if enabled
    if config.auto_reload.enable && config.auto_reload.poll.enable {
        send_blocking(
            &handlers.auto_reload,
            AutoReloadEvent::PollAfter {
                interval: config.auto_reload.poll.interval,
            },
        );
    }
}
