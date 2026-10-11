//! Editable workspace search results, based on PR #4381.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{bail, ensure, Context as _, Result};
use helix_core::{encoding::Encoding, regex::RegexBuilder, Rope, Tendril, Transaction};
use helix_view::{
    document::{from_reader, RefactorFile, RefactorState},
    editor::Action,
    Document, Editor,
};

use crate::job::{Callback, Jobs};

struct OpenBuffer {
    text: Rope,
    encoding: &'static Encoding,
    modified: bool,
}

pub(super) fn start(editor: &mut Editor, jobs: &mut Jobs, pattern: &str) -> Result<()> {
    ensure!(
        !pattern.is_empty(),
        "Refactoring requires a nonempty search pattern"
    );
    let config = editor.config();
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(config.search.smart_case && !pattern.chars().any(char::is_uppercase))
        .build()?;
    let picker_config = config.file_picker.clone();
    let source = editor
        .document(editor.tree.get(editor.tree.focus).doc)
        .unwrap();
    let extension = source
        .path()
        .and_then(|p| p.extension())
        .map(|s| s.to_owned());
    let language = source.language_id().map(str::to_owned);
    let root = helix_loader::find_workspace().0;
    let mut open = BTreeMap::new();
    for doc in editor.documents.values_mut() {
        if let Some(path) = doc.path().map(PathBuf::from) {
            open.insert(
                path,
                OpenBuffer {
                    text: doc.text().clone(),
                    encoding: doc.encoding(),
                    modified: doc.is_modified(),
                },
            );
        }
    }
    let pattern = pattern.to_owned();
    jobs.callback(async move {
        let (state, text) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut walk = ignore::WalkBuilder::new(root);
            walk.hidden(picker_config.hidden)
                .parents(picker_config.parents)
                .ignore(picker_config.ignore)
                .git_ignore(picker_config.git_ignore)
                .git_global(picker_config.git_global)
                .git_exclude(picker_config.git_exclude)
                .follow_links(picker_config.follow_symlinks)
                .max_depth(picker_config.max_depth);
            let mut paths: std::collections::BTreeSet<_> = open.keys().cloned().collect();
            for entry in walk.build().filter_map(Result::ok) {
                if entry.file_type().is_some_and(|t| t.is_file()) {
                    paths.insert(helix_stdx::path::canonicalize(entry.path()));
                }
            }
            let mut state = RefactorState {
                files: Vec::new(),
                line_count: 0,
            };
            let mut text = String::new();
            for path in paths {
                if extension
                    .as_ref()
                    .is_some_and(|ext| path.extension() != Some(ext.as_os_str()))
                {
                    continue;
                }
                let bytes = match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        log::debug!("Refactor search skipped {}: {err}", path.display());
                        continue;
                    }
                };
                // Unopened binary files cannot be edited as line-based search results.
                if !open.contains_key(&path) && bytes.contains(&0) {
                    continue;
                }
                let disk =
                    from_reader(&mut bytes.as_slice(), open.get(&path).map(|b| b.encoding))?.0;
                let original = if let Some(buffer) = open.get(&path) {
                    ensure!(
                        buffer.modified || buffer.text == disk,
                        "{} changed externally; reload it and search again",
                        path.display()
                    );
                    buffer.text.clone()
                } else {
                    disk
                };
                let mut lines = Vec::new();
                for (line_no, line) in original.lines().enumerate() {
                    let line = line.to_string();
                    let line = strip_ending(&line);
                    if regex.is_match(line) {
                        lines.push((line_no, state.line_count));
                        text.push_str(line);
                        text.push('\n');
                        state.line_count += 1;
                    }
                }
                if !lines.is_empty() {
                    state.files.push(RefactorFile {
                        path,
                        original,
                        disk_contents: bytes,
                        lines,
                    });
                }
            }
            Ok((state, Rope::from_str(&text)))
        })
        .await??;
        Ok(Callback::EditorCompositor(Box::new(move |editor, _| {
            if state.line_count == 0 {
                editor.set_status("No matches found");
                return;
            }
            let mut doc =
                Document::from(text, None, editor.config.clone(), editor.syn_loader.clone());
            doc.name = Some(format!("[refactor: {pattern}]"));
            if let Some(language) = language {
                let _ = doc.set_language_by_language_id(&language, &editor.syn_loader.load());
            }
            let count = state.line_count;
            doc.refactor = Some(state);
            editor.new_file_from_document(Action::VerticalSplit, doc);
            editor.set_status(format!(
                "Edit {count} matching lines, then run :refactor-apply (or global_refactor)"
            ));
        })))
    });
    Ok(())
}

fn strip_ending(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line)
}

fn replacements(state: &RefactorState, text: &Rope) -> Result<Vec<(PathBuf, Transaction)>> {
    let mut lines: Vec<_> = text
        .lines()
        .map(|l| strip_ending(&l.to_string()).to_owned())
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    ensure!(lines.len() == state.line_count,
        "Refactoring must keep the number of result lines unchanged; undo added/deleted lines first");
    let mut replacements = Vec::new();
    for file in &state.files {
        let mut changes = Vec::new();
        for &(source_line, result_line) in &file.lines {
            let original = file.original.line(source_line).to_string();
            let original = strip_ending(&original);
            if original != lines[result_line] {
                let from = file.original.line_to_char(source_line);
                changes.push((
                    from,
                    from + original.chars().count(),
                    Some(Tendril::from(lines[result_line].as_str())),
                ));
            }
        }
        if !changes.is_empty() {
            replacements.push((
                file.path.clone(),
                Transaction::change(&file.original, changes.into_iter()),
            ));
        }
    }
    Ok(replacements)
}

pub(super) fn apply(editor: &mut Editor) -> Result<()> {
    let refactor_id = editor.tree.get(editor.tree.focus).doc;
    let doc = editor.document(refactor_id).unwrap();
    let state = doc
        .refactor
        .as_ref()
        .context("Current buffer is not a refactoring view")?
        .clone();
    let replacements = replacements(&state, doc.text())?;
    // Validate every source before applying any transaction.
    let mut pending = Vec::new();
    for (path, transaction) in replacements {
        let source = state.files.iter().find(|file| file.path == path).unwrap();
        ensure!(
            std::fs::read(&path)? == source.disk_contents,
            "{} changed on disk; repeat the refactoring search",
            path.display()
        );
        let id = editor.open(&path, Action::Load)?;
        ensure!(
            editor.document(id).unwrap().text() == &source.original,
            "{} changed in the editor; repeat the refactoring search",
            path.display()
        );
        pending.push((id, transaction));
    }
    let count = pending.len();
    for (id, transaction) in pending {
        let mut view = editor
            .tree
            .views()
            .find(|(view, _)| view.doc == id)
            .map(|(view, _)| view.clone())
            .unwrap_or_else(|| editor.tree.get(editor.tree.focus).clone());
        view.doc = id;
        let doc = editor.documents.get_mut(&id).unwrap();
        doc.ensure_view_init(view.id);
        doc.append_changes_to_history(&mut view);
        if !doc.apply(&transaction, view.id) {
            bail!("Failed to apply refactoring transaction");
        }
        doc.append_changes_to_history(&mut view);
    }
    editor
        .close_document(refactor_id, true)
        .map_err(|_| anyhow::anyhow!("Could not close refactoring view"))?;
    editor.set_status(format!(
        "Refactored {count} buffers; use :write-all to save. Undo in each source buffer."
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(original: &str, lines: Vec<(usize, usize)>) -> RefactorState {
        RefactorState {
            line_count: lines.len(),
            files: vec![RefactorFile {
                path: PathBuf::from("source.rs"),
                original: Rope::from_str(original),
                disk_contents: original.as_bytes().to_vec(),
                lines,
            }],
        }
    }

    #[test]
    fn preserves_unicode_crlf_and_unmatched_lines() {
        let state = state("é old\r\nuntouched\r\nold 🦀\r\n", vec![(0, 0), (2, 1)]);
        let changes = replacements(&state, &Rope::from_str("é new\nnew 🦀\n")).unwrap();
        let mut text = state.files[0].original.clone();
        assert!(changes[0].1.apply(&mut text));
        assert_eq!(text, "é new\r\nuntouched\r\nnew 🦀\r\n");
    }

    #[test]
    fn unchanged_results_produce_no_transactions() {
        let state = state("old\n", vec![(0, 0)]);
        assert!(replacements(&state, &Rope::from_str("old\n"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rejects_added_or_deleted_result_lines() {
        let state = state("old\nother\n", vec![(0, 0), (1, 1)]);
        assert!(replacements(&state, &Rope::from_str("new\n")).is_err());
        assert!(replacements(&state, &Rope::from_str("new\nother\nextra\n")).is_err());
    }

    #[test]
    fn can_replace_a_result_with_an_empty_line() {
        let state = state("old\n", vec![(0, 0)]);
        let changes = replacements(&state, &Rope::from_str("\n")).unwrap();
        let mut text = state.files[0].original.clone();
        assert!(changes[0].1.apply(&mut text));
        assert_eq!(text, "\n");
    }

    #[test]
    fn keeps_a_source_without_a_final_newline() {
        let state = state("old", vec![(0, 0)]);
        let changes = replacements(&state, &Rope::from_str("new\n")).unwrap();
        let mut text = state.files[0].original.clone();
        assert!(changes[0].1.apply(&mut text));
        assert_eq!(text, "new");
    }

    fn refactor_editor(
        directory: &std::path::Path,
    ) -> (Editor, helix_view::DocumentId, Vec<helix_view::DocumentId>) {
        let mut editor = crate::handlers::auto_reload::tests::editor(false);
        let mut state = RefactorState {
            files: Vec::new(),
            line_count: 2,
        };
        let mut ids = Vec::new();
        for (line, name) in ["first.txt", "second.txt"].into_iter().enumerate() {
            let path = directory.join(name);
            std::fs::write(&path, "old\n").unwrap();
            let id = editor.open(&path, Action::Load).unwrap();
            ids.push(id);
            state.files.push(RefactorFile {
                path,
                original: Rope::from_str("old\n"),
                disk_contents: b"old\n".to_vec(),
                lines: vec![(0, line)],
            });
        }
        let mut doc = Document::from(
            Rope::from_str("new\nnew\n"),
            None,
            editor.config.clone(),
            editor.syn_loader.clone(),
        );
        doc.refactor = Some(state);
        editor.new_file_from_document(Action::VerticalSplit, doc);
        let refactor = editor.tree.get(editor.tree.focus).doc;
        (editor, refactor, ids)
    }

    #[tokio::test]
    async fn stale_disk_aborts_before_changing_any_source() {
        let directory = tempfile::tempdir().unwrap();
        let (mut editor, refactor, ids) = refactor_editor(directory.path());
        std::fs::write(directory.path().join("second.txt"), "external\n").unwrap();
        assert!(apply(&mut editor).is_err());
        assert!(editor.document(refactor).is_some());
        for id in ids {
            assert_eq!(
                editor.document(id).unwrap().text(),
                &Rope::from_str("old\n")
            );
        }
    }

    #[tokio::test]
    async fn stale_buffer_aborts_before_changing_any_source() {
        let directory = tempfile::tempdir().unwrap();
        let (mut editor, refactor, ids) = refactor_editor(directory.path());
        let view = editor.tree.focus;
        let doc = editor.document_mut(ids[1]).unwrap();
        doc.ensure_view_init(view);
        let edit = Transaction::change(
            doc.text(),
            [(0, 3, Some(Tendril::from("edited")))].into_iter(),
        );
        doc.apply(&edit, view);
        assert!(apply(&mut editor).is_err());
        assert!(editor.document(refactor).is_some());
        assert_eq!(
            editor.document(ids[0]).unwrap().text(),
            &Rope::from_str("old\n")
        );
        assert_eq!(
            editor.document(ids[1]).unwrap().text(),
            &Rope::from_str("edited\n")
        );
    }

    #[tokio::test]
    async fn applies_undoable_edits_without_writing_to_disk() {
        let directory = tempfile::tempdir().unwrap();
        let (mut editor, refactor, ids) = refactor_editor(directory.path());
        apply(&mut editor).unwrap();
        assert!(editor.document(refactor).is_none());
        for id in ids {
            let mut view = editor.tree.get(editor.tree.focus).clone();
            view.doc = id;
            let doc = editor.document_mut(id).unwrap();
            doc.ensure_view_init(view.id);
            assert_eq!(doc.text(), &Rope::from_str("new\n"));
            assert_eq!(std::fs::read(doc.path().unwrap()).unwrap(), b"old\n");
            assert!(doc.undo(&mut view));
            assert_eq!(doc.text(), &Rope::from_str("old\n"));
        }
    }
}
