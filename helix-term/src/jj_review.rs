//! Native revision selection driving the existing in-process review workspace.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use helix_vcs::{DiffProviderRegistry, JjRevision};
use helix_view::Editor;

use crate::{
    diff_review::{self, Origin},
    job::{Callback, Jobs},
    ui::{overlay::overlaid, Picker, PickerColumn},
};

pub const DEFAULT_REVSET: &str = "@ | ancestors(@, 50) | bookmarks()";

fn registry(root: &Path, trusted: bool) -> Result<(PathBuf, DiffProviderRegistry)> {
    anyhow::ensure!(
        trusted,
        "JJ history requires workspace trust; run :workspace-trust"
    );
    let mut registry = DiffProviderRegistry::default();
    registry.add(root, trusted);
    let root = registry
        .review_root(root)
        .context("no JJ workspace found")?;
    Ok((root, registry))
}

pub fn open(editor: &mut Editor, jobs: &mut Jobs, root: PathBuf, revset: String) {
    let trusted = editor
        .workspace_trust
        .query(
            &helix_loader::find_workspace_in(&root).0,
            helix_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted();
    editor.navigation_generation += 1;
    let generation = editor.navigation_generation;
    editor.set_status("Loading JJ revisions…");
    jobs.callback(async move {
        let result = tokio::task::spawn_blocking(move || {
            let (root, registry) = registry(&root, trusted)?;
            let revisions = registry.jj_revisions(&root, &revset, 200)?;
            Ok::<_, anyhow::Error>((root, revisions))
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result);
        Ok(Callback::EditorCompositorJobs(Box::new(
            move |editor, compositor, _jobs| {
                if generation != editor.navigation_generation {
                    return;
                }
                let (root, revisions) = match result {
                    Ok(value) => value,
                    Err(error) => {
                        editor.set_error(format!("{error:#}"));
                        return;
                    }
                };
                if revisions.is_empty() {
                    editor.set_status("No JJ revisions match this revset");
                    return;
                }
                let columns = [
                    PickerColumn::new("change", |revision: &JjRevision, _: &PathBuf| {
                        format!(
                            "{} {}",
                            if revision.working_copy { "@" } else { "○" },
                            revision.change_id
                        )
                        .into()
                    }),
                    PickerColumn::new("commit", |revision: &JjRevision, _: &PathBuf| {
                        revision.commit_id[..12.min(revision.commit_id.len())]
                            .to_owned()
                            .into()
                    }),
                    PickerColumn::new("bookmarks", |revision: &JjRevision, _: &PathBuf| {
                        revision.bookmarks.join(" ").into()
                    }),
                    PickerColumn::new("author", |revision: &JjRevision, _: &PathBuf| {
                        revision.author.clone().into()
                    }),
                    PickerColumn::new("description", |revision: &JjRevision, _: &PathBuf| {
                        if revision.description.is_empty() {
                            "(no description)".into()
                        } else {
                            revision.description.clone().into()
                        }
                    }),
                ];
                let selection_root = root.clone();
                let picker =
                    Picker::new(columns, 4, revisions, root, move |cx, revision, _action| {
                        let origin = if revision.working_copy {
                            Origin::Repository(selection_root.clone())
                        } else {
                            Origin::JjRevision {
                                root: selection_root.clone(),
                                commit: revision.commit_id.clone(),
                            }
                        };
                        diff_review::load(cx.editor, cx.jobs, origin);
                    })
                    .with_title("JJ revisions (first 200)".into());
                editor.set_status(
                    "Select a JJ revision; @ is editable, historical snapshots are read-only",
                );
                compositor.push(Box::new(overlaid(picker)));
            },
        )))
    });
}
