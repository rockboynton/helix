//! A changeset owns immutable baselines and references live working documents.
//! No repository access happens while painting the review.
use crate::{diff_render::Layout, ui::file_tree::FileTree};
use anyhow::{Context, Result};
use helix_view::{
    document::{from_reader, DocumentAccess},
    editor::Action,
    Document, DocumentId, Editor, ViewId,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Entry {
    pub name: PathBuf,
    before: Arc<Vec<u8>>,
    after: Option<PathBuf>,
    snapshot: Arc<Vec<u8>>,
    status: &'static str,
    documents: [Option<DocumentId>; 2],
    inline: Option<(DocumentId, [DocumentId; 2], [i32; 2])>,
    reload_after: bool,
    pub counts: Option<(usize, usize)>,
    count_versions: Option<[i32; 2]>,
    message: Option<String>,
    patch: Option<Arc<crate::diff_patch::Patch>>,
}

impl Entry {
    pub fn label(&self) -> String {
        match self.counts {
            Some((added, removed)) => format!("{} +{added} -{removed}", self.status),
            None => self.status.into(),
        }
    }
}

pub struct Review {
    pub entries: Vec<Entry>,
    pub active: Option<usize>,
    pub layout: Layout,
    pub views: Vec<ViewId>,
    pub origin: Origin,
    watch_roots: Vec<PathBuf>,
    positions: HashMap<DocumentId, (i32, helix_core::Selection, helix_view::view::ViewPosition)>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Origin {
    Repository(PathBuf),
    JjRevision { root: PathBuf, commit: String },
    Directories(PathBuf, PathBuf),
    Patch(PathBuf),
}

#[derive(Default)]
pub struct RefreshState {
    running: bool,
    pending: bool,
}

impl Origin {
    fn affected(&self, path: &Path) -> bool {
        match self {
            Self::Repository(root) => path.starts_with(root),
            Self::JjRevision { .. } => false, // Immutable commit snapshots do not watch the checkout.
            Self::Directories(left, right) => path.starts_with(left) || path.starts_with(right),
            Self::Patch(file) => !file.as_os_str().is_empty() && path == file,
        }
    }
}

pub(crate) fn refresh_changed(
    editor: &mut Editor,
    compositor: &mut crate::compositor::Compositor,
    jobs: &mut crate::job::Jobs,
    paths: &BTreeSet<PathBuf>,
) {
    let Some(view) = compositor.find::<crate::ui::EditorView>() else {
        return;
    };
    let Some(review) = view
        .file_tree
        .as_ref()
        .and_then(|tree| tree.review.as_ref())
    else {
        return;
    };
    if !paths.iter().any(|path| {
        review.origin.affected(path)
            || matches!(review.origin, Origin::Repository(_))
                && review.watch_roots.iter().any(|root| path.starts_with(root))
    }) {
        return;
    }
    let origin = review.origin.clone();
    let state = view.review_refresh.clone();
    {
        let mut state = state.lock();
        if state.running {
            state.pending = true;
            return;
        }
        state.running = true;
    }
    load_impl(editor, jobs, origin, Some(state));
}

pub fn load(editor: &mut Editor, jobs: &mut crate::job::Jobs, origin: Origin) {
    load_impl(editor, jobs, origin, None);
}

fn load_impl(
    editor: &mut Editor,
    jobs: &mut crate::job::Jobs,
    origin: Origin,
    automatic: Option<Arc<parking_lot::Mutex<RefreshState>>>,
) {
    if matches!(&origin, Origin::Patch(path) if path.as_os_str().is_empty()) {
        editor
            .set_error("A piped patch is an immutable snapshot; reopen with new input to refresh");
        return;
    }
    editor.navigation_generation += 1;
    let generation = editor.navigation_generation;
    let extra = editor
        .documents()
        .filter(|doc| doc.is_modified())
        .filter_map(|doc| doc.path().map(Path::to_path_buf))
        .collect::<Vec<_>>();
    let root = match &origin {
        Origin::Repository(root) | Origin::Patch(root) | Origin::JjRevision { root, .. } => root,
        Origin::Directories(_, root) => root,
    };
    let trust = editor
        .workspace_trust
        .query(
            &helix_loader::find_workspace_in(root).0,
            helix_loader::workspace_trust::TrustQuery::Git,
        )
        .is_trusted();
    if automatic.is_none() {
        editor.set_status("Loading review…");
    }
    jobs.callback(async move {
        let requested_origin = origin.clone();
        let result = tokio::task::spawn_blocking(move || match origin {
            Origin::Repository(root) => Review::repository_with_edits(root, trust, &extra),
            Origin::JjRevision { root, commit } => Review::jj_revision(root, commit, trust),
            Origin::Directories(left, right) => Review::directories_with_edits(left, right, &extra),
            Origin::Patch(path) => {
                let patch = crate::diff_patch::parse(&std::fs::read_to_string(&path)?)?;
                Ok((PathBuf::new(), Review::patch(&patch, Origin::Patch(path))))
            }
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result);
        Ok(crate::job::Callback::EditorCompositorJobs(Box::new(
            move |editor, compositor, jobs| {
                let pending = automatic.as_ref().is_some_and(|state| {
                    let mut state = state.lock();
                    state.running = false;
                    std::mem::take(&mut state.pending)
                });
                // Even stale and failed workers release the gate; a later event can retry.
                if generation != editor.navigation_generation {
                    return;
                }
                let (root, review) = match result {
                    Ok(value) => value,
                    Err(error) => {
                        if automatic.is_none() {
                            editor.set_error(format!("{error:#}"));
                        } else {
                            log::debug!("review refresh failed: {error:#}");
                        }
                        return;
                    }
                };
                editor
                    .file_watcher
                    .set_review_roots(review.watch_roots.clone());
                let focus = editor.tree.focus;
                let status = automatic.is_some().then(|| editor.status_msg.clone());
                let mut tree = Review::panel(root, review);
                let mut tree_selection = None;
                let mut tree_focused = false;
                let mut expansion = None;
                if let Some(view) = compositor.find::<crate::ui::EditorView>() {
                    if let Some(layout) = view.review_layout.take() {
                        tree.review.as_mut().unwrap().layout = layout;
                    }
                    if let Some(old_tree) = &view.file_tree {
                        let same_review = old_tree
                            .review
                            .as_ref()
                            .is_some_and(|old| old.origin == tree.review.as_ref().unwrap().origin);
                        // Selecting another revision changes snapshot identity,
                        // not the user's chosen split/inline review layout.
                        if !same_review && old_tree.root == tree.root {
                            if let Some(old) = &old_tree.review {
                                let new = tree.review.as_mut().unwrap();
                                new.layout = old.layout;
                                new.views = old.views.clone();
                            }
                        }
                        if same_review {
                            tree.visible = old_tree.visible;
                        }
                        // A new review starts expanded, independent of a filesystem
                        // sidebar's collapsed folders. Only refreshes retain the
                        // review's own expansion choices; diff-off restores browsing.
                        if same_review && old_tree.root == tree.root {
                            tree.expanded = old_tree.expanded.clone();
                            let old_directories = old_tree
                                .nodes
                                .iter()
                                .filter(|node| node.directory)
                                .map(|node| &node.path)
                                .collect::<BTreeSet<_>>();
                            tree.expanded.extend(
                                tree.nodes
                                    .iter()
                                    .filter(|node| {
                                        node.directory && !old_directories.contains(&node.path)
                                    })
                                    .map(|node| node.path.clone()),
                            );
                            expansion = Some(tree.expanded.clone());
                            tree.scroll = old_tree.scroll;
                            tree_selection = old_tree
                                .nodes
                                .get(old_tree.selected)
                                .map(|node| node.path.clone());
                            tree_focused = old_tree.focused;
                        }
                    }
                    if let Some(old) = view
                        .file_tree
                        .as_mut()
                        .and_then(|tree| tree.review.as_mut())
                        .filter(|old| old.origin == tree.review.as_ref().unwrap().origin)
                    {
                        let new = tree.review.as_mut().unwrap();
                        new.layout = old.layout;
                        new.views = old.views.clone();
                        new.positions = std::mem::take(&mut old.positions);
                        let active = old.active.map(|i| old.entries[i].name.clone());
                        for entry in &mut new.entries {
                            if let Some(previous) =
                                old.entries.iter().find(|old| old.name == entry.name)
                            {
                                if previous.before == entry.before
                                    && previous.message == entry.message
                                    && previous.patch == entry.patch
                                {
                                    entry.documents[0] = previous.documents[0];
                                    entry.inline = previous.inline;
                                }
                                if previous.after == entry.after {
                                    if let Some(id) = previous.documents[1]
                                        .filter(|&id| editor.document(id).is_some())
                                    {
                                        let doc = editor.document(id).unwrap();
                                        let changed = previous.snapshot != entry.snapshot
                                            || previous.patch != entry.patch;
                                        if doc.is_modified()
                                            || !changed
                                            || (!doc.is_read_only() && entry.after.is_some())
                                        {
                                            entry.documents[1] = Some(id);
                                            entry.reload_after = changed
                                                && !doc.is_modified()
                                                && !doc.is_read_only();
                                        }
                                    }
                                }
                            }
                        }
                        new.active = active.and_then(|name| {
                            new.entries.iter().position(|entry| entry.name == name)
                        });
                    }
                }
                let review = tree.review.as_mut().unwrap();
                if !review.entries.is_empty() {
                    let index = review.active.unwrap_or(0);
                    if let Err(error) = review.open(index, editor) {
                        editor.set_error(format!("{error:#}"));
                    }
                    let name = review.entries[index].name.clone();
                    tree.reveal(&name);
                    tree.focused = false;
                } else {
                    editor.stop_diff();
                    editor.set_status("No changes to review");
                }
                if automatic.is_some() {
                    if let Some(expanded) = expansion {
                        tree.expanded = expanded;
                    }
                    if let Some(path) = tree_selection {
                        tree.reveal(&path);
                    }
                    tree.focused = tree_focused;
                    if editor.tree.contains(focus) {
                        editor.focus(focus);
                    }
                    // Background refresh must not replace a command's status message.
                    if let Some(status) = status {
                        editor.status_msg = status;
                    }
                }
                if let Some(view) = compositor.find::<crate::ui::EditorView>() {
                    if view
                        .file_tree
                        .as_ref()
                        .is_some_and(|tree| tree.review.is_none())
                    {
                        view.review_previous_tree = view.file_tree.take();
                    }
                    view.file_tree_pending_visible = None;
                    view.file_tree = Some(tree);
                }
                if pending {
                    let paths = match requested_origin {
                        Origin::Repository(root) | Origin::JjRevision { root, .. } => {
                            BTreeSet::from([root])
                        }
                        Origin::Directories(left, right) => BTreeSet::from([left, right]),
                        Origin::Patch(path) => BTreeSet::from([path]),
                    };
                    refresh_changed(editor, compositor, jobs, &paths);
                }
            },
        )))
    });
}

/// Hiding a panel preserves its review, navigation state and watchers.
pub fn toggle_tree(
    editor: &mut Editor,
    compositor: &mut crate::compositor::Compositor,
    jobs: &mut crate::job::Jobs,
) {
    let Some(view) = compositor.find::<crate::ui::EditorView>() else {
        return;
    };
    if let Some(tree) = &mut view.file_tree {
        tree.visible = !tree.visible;
        tree.focused = tree.visible;
        if let Some(visible) = &mut view.file_tree_pending_visible {
            *visible = tree.visible;
        }
    } else if let Some(visible) = &mut view.file_tree_pending_visible {
        *visible = !*visible;
    } else {
        view.file_tree_pending_visible = Some(true);
        load_tree(editor, jobs, helix_loader::find_workspace().0);
    }
}

pub fn load_tree(editor: &mut Editor, jobs: &mut crate::job::Jobs, root: PathBuf) {
    editor.file_watcher.set_review_roots(Vec::new());
    editor.navigation_generation += 1;
    let generation = editor.navigation_generation;
    let config = editor.config().file_picker.clone();
    jobs.callback(async move {
        let result = tokio::task::spawn_blocking(move || FileTree::scan(root, config))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|tree| tree);
        Ok(crate::job::Callback::EditorCompositor(Box::new(
            move |editor, compositor| {
                if editor.navigation_generation != generation {
                    return;
                }
                let mut tree = match result {
                    Ok(tree) => tree,
                    Err(error) => {
                        if let Some(view) = compositor.find::<crate::ui::EditorView>() {
                            view.file_tree_pending_visible = None;
                        }
                        editor.set_error(format!("{error:#}"));
                        return;
                    }
                };
                if let Some(view) = compositor.find::<crate::ui::EditorView>() {
                    if let Some(old) = &view.file_tree {
                        if old.root == tree.root {
                            tree.expanded = old.expanded.clone();
                            tree.scroll = old.scroll;
                            tree.focused = old.focused;
                            tree.visible = old.visible;
                            if let Some(node) = old.nodes.get(old.selected) {
                                tree.reveal(&node.path);
                            }
                        }
                    }
                    if let Some(visible) = view.file_tree_pending_visible.take() {
                        tree.visible = visible;
                    }
                    tree.focused &= tree.visible;
                    view.file_tree = Some(tree);
                    view.review_previous_tree = None;
                }
            },
        )))
    });
}

/// End the comparison without discarding any buffers or writing files.
pub fn finish(editor: &mut Editor, compositor: &mut crate::compositor::Compositor) {
    let review_target = compositor.find::<crate::ui::EditorView>().and_then(|view| {
        let review = view.file_tree.as_ref()?.review.as_ref()?;
        let entry = review.entries.get(review.active?)?;
        Some((entry.documents[1]?, review.views.clone()))
    });
    let target = review_target.or_else(|| {
        if let Some(session) = editor
            .diff_session
            .as_ref()
            .or(editor.inline_diff_session.as_ref())
        {
            Some((session.documents[1], session.views.to_vec()))
        } else {
            editor.three_diff_session.as_ref().map(|session| {
                let side = if editor.tree.focus == session.views[0] {
                    0
                } else {
                    2
                };
                (session.documents[side], session.views.to_vec())
            })
        }
    });
    editor.navigation_generation += 1;
    editor.file_watcher.set_review_roots(Vec::new());
    editor.stop_diff();
    if let Some((document, views)) = target {
        if let Some(&left) = views.iter().find(|&&id| editor.tree.contains(id)) {
            // Capture the working pane's cursor/scroll before closing its view.
            let position = views.iter().find_map(|&id| {
                let view = editor.tree.try_get(id)?;
                let doc = editor.document(document)?;
                (view.doc == document).then(|| (doc.selection(id).clone(), doc.view_offset(id)))
            });
            if editor.document(document).is_some() {
                editor.focus(left);
                editor.switch(document, Action::Replace);
                if let Some((selection, offset)) = position {
                    let doc = editor.document_mut(document).unwrap();
                    doc.set_selection(left, selection);
                    doc.set_view_offset(left, offset);
                }
                for id in views {
                    if id != left && editor.tree.contains(id) {
                        editor.close(id);
                    }
                }
                editor.focus(left);
            }
        }
    }
    if let Some(view) = compositor.find::<crate::ui::EditorView>() {
        let was_review = view
            .file_tree
            .as_ref()
            .is_some_and(|tree| tree.review.is_some());
        if was_review || view.review_previous_tree.is_some() {
            view.file_tree = view.review_previous_tree.take();
        }
    }
}

impl Review {
    pub fn jj_revision(root: PathBuf, commit: String, trust: bool) -> Result<(PathBuf, Self)> {
        anyhow::ensure!(
            trust,
            "JJ review requires workspace trust; run :workspace-trust"
        );
        let mut registry = helix_vcs::DiffProviderRegistry::default();
        registry.add(&root, trust);
        let root = registry
            .review_root(&root)
            .context("no JJ workspace found")?;
        let files = registry.jj_revision_files(&root, &commit)?;
        let entries = files
            .into_iter()
            .map(|file| Entry {
                name: file.path,
                before: Arc::new(file.before),
                // History browsing must never write a snapshot into the live checkout.
                after: None,
                snapshot: Arc::new(file.after),
                status: file.status,
                documents: [None; 2],
                inline: None,
                reload_after: false,
                counts: None,
                count_versions: None,
                message: None,
                patch: None,
            })
            .collect();
        Ok((
            root.clone(),
            Self {
                entries,
                active: None,
                layout: Layout::SideBySide,
                views: Vec::new(),
                origin: Origin::JjRevision { root, commit },
                watch_roots: Vec::new(),
                positions: HashMap::new(),
            },
        ))
    }

    pub fn repository(root: PathBuf, trust: bool) -> Result<(PathBuf, Self)> {
        Self::repository_with_edits(root, trust, &[])
    }

    fn repository_with_edits(
        root: PathBuf,
        trust: bool,
        extra: &[PathBuf],
    ) -> Result<(PathBuf, Self)> {
        anyhow::ensure!(root.is_dir(), "review requires a repository directory");
        let root = std::fs::canonicalize(root)?;
        anyhow::ensure!(
            trust || !root.ancestors().any(|path| path.join(".jj").is_dir()),
            "JJ review requires workspace trust; run :workspace-trust, then :diff-review"
        );
        let mut registry = helix_vcs::DiffProviderRegistry::default();
        registry.add(&root, trust);
        let root = registry
            .review_root(&root)
            .context("no Git/JJ repository found")?;
        let mut files = registry.review_files(&root)?;
        for path in extra.iter().filter(|path| path.starts_with(&root)) {
            if !files.iter().any(|file| file.path == *path) {
                files.push(helix_vcs::ReviewFile {
                    path: path.clone(),
                    before_path: path.clone(),
                    status: "M",
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut entries = Vec::new();
        for file in files {
            let before = registry
                .review_base(&file.before_path)
                .with_context(|| format!("reading baseline {}", file.before_path.display()))?;
            let (mut after, snapshot, mut message) = input(&file.path)?;
            if extra.contains(&file.path) && message.is_none() {
                after = Some(file.path.clone());
            }
            if before == snapshot
                && file.status != "R"
                && message.is_none()
                && !extra.contains(&file.path)
            {
                message = Some("Text unchanged; metadata or index state differs".into());
                after = None;
            }
            let status = file.status;
            entries.push(Entry {
                name: file.path.strip_prefix(&root)?.to_path_buf(),
                before: Arc::new(before),
                after,
                snapshot: Arc::new(snapshot),
                status,
                documents: [None; 2],
                inline: None,
                reload_after: false,
                counts: None,
                count_versions: None,
                message,
                patch: None,
            });
        }
        let mut watch_roots = vec![root.clone()];
        watch_roots.extend(
            registry
                .get_watched_paths(&root)
                .into_iter()
                .filter_map(|path| path.parent().map(Path::to_path_buf)),
        );
        for path in watch_roots.clone() {
            if let Ok(common) = std::fs::read_to_string(path.join("commondir")) {
                if let Ok(common) = path.join(common.trim()).canonicalize() {
                    watch_roots.push(common);
                }
            }
        }
        Ok((
            root.clone(),
            Self {
                entries,
                active: None,
                layout: Layout::SideBySide,
                views: Vec::new(),
                origin: Origin::Repository(root),
                watch_roots,
                positions: HashMap::new(),
            },
        ))
    }

    pub fn directories(left: PathBuf, right: PathBuf) -> Result<(PathBuf, Self)> {
        Self::directories_with_edits(left, right, &[])
    }

    fn directories_with_edits(
        left: PathBuf,
        right: PathBuf,
        extra: &[PathBuf],
    ) -> Result<(PathBuf, Self)> {
        anyhow::ensure!(
            left.is_dir() && right.is_dir(),
            "review needs two directories"
        );
        let left = std::fs::canonicalize(left)?;
        let right = std::fs::canonicalize(right)?;
        let mut files = BTreeSet::new();
        collect(&left, &left, &mut files)?;
        collect(&right, &right, &mut files)?;
        for path in extra {
            if let Ok(path) = path.strip_prefix(&right) {
                files.insert(path.to_path_buf());
            }
        }
        let mut entries = Vec::new();
        for name in files {
            let left_exists = std::fs::symlink_metadata(left.join(&name)).is_ok();
            let right_exists = std::fs::symlink_metadata(right.join(&name)).is_ok();
            let (_, before, old_message) = input(&left.join(&name))?;
            let (mut after, snapshot, new_message) = input(&right.join(&name))?;
            if extra.contains(&right.join(&name)) && new_message.is_none() {
                after = Some(right.join(&name));
            }
            if before == snapshot
                && old_message == new_message
                && left_exists == right_exists
                && !extra.contains(&right.join(&name))
            {
                continue;
            }
            let status = if !left_exists {
                "A"
            } else if !right_exists {
                "D"
            } else {
                "M"
            };
            let message = if old_message.is_some() || new_message.is_some() {
                Some(format!(
                    "Type/symlink change: {:?} → {:?}",
                    old_message, new_message
                ))
            } else {
                None
            };
            entries.push(Entry {
                name,
                before: Arc::new(before),
                after,
                snapshot: Arc::new(snapshot),
                status,
                documents: [None; 2],
                inline: None,
                reload_after: false,
                counts: None,
                count_versions: None,
                message,
                patch: None,
            });
        }
        Ok((
            right.clone(),
            Self {
                entries,
                active: None,
                layout: Layout::SideBySide,
                views: Vec::new(),
                origin: Origin::Directories(left.clone(), right.clone()),
                watch_roots: vec![left, right],
                positions: HashMap::new(),
            },
        ))
    }

    pub fn panel(root: PathBuf, review: Self) -> FileTree {
        let mut tree = FileTree::from_paths(
            root,
            review
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry.name.clone(), false, Some(index))),
        );
        tree.expand_all();
        if let Some(entry) = review.entries.first() {
            tree.reveal(&entry.name);
        }
        tree.review = Some(review);
        tree
    }

    pub fn patch(patch: &crate::diff_patch::Patch, origin: Origin) -> Self {
        let entries = patch
            .files()
            .into_iter()
            .map(|(name, patch)| {
                let removed = patch
                    .sections
                    .iter()
                    .flat_map(|section| &section.rows)
                    .filter(|row| row.removed == Some(true))
                    .count();
                let added = patch
                    .sections
                    .iter()
                    .flat_map(|section| &section.rows)
                    .filter(|row| row.removed == Some(false))
                    .count();
                Entry {
                    name,
                    before: Arc::new(Vec::new()),
                    after: None,
                    snapshot: Arc::new(Vec::new()),
                    status: "M",
                    documents: [None; 2],
                    inline: None,
                    reload_after: false,
                    counts: Some((added, removed)),
                    count_versions: None,
                    message: None,
                    patch: Some(Arc::new(patch)),
                }
            })
            .collect();
        Self {
            entries,
            active: None,
            layout: Layout::SideBySide,
            views: Vec::new(),
            watch_roots: match &origin {
                Origin::Patch(path) => path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            },
            origin,
            positions: HashMap::new(),
        }
    }

    pub fn open(&mut self, index: usize, editor: &mut Editor) -> Result<()> {
        editor
            .file_watcher
            .set_review_roots(self.watch_roots.clone());
        anyhow::ensure!(index < self.entries.len(), "no review file selected");
        for &id in &self.views {
            if let Some(view) = editor.tree.try_get(id) {
                let doc = editor.document(view.doc).unwrap();
                self.positions.insert(
                    doc.id(),
                    (
                        doc.version(),
                        doc.selection(id).clone(),
                        doc.view_offset(id),
                    ),
                );
            }
        }
        editor.stop_diff();
        // Reuse the review's designated panes even after editing changed focus.
        self.views.retain(|&view| editor.tree.contains(view));
        if let Some(&view) = self.views.first() {
            editor.focus(view);
        }
        let count = if self.layout == Layout::Inline { 1 } else { 2 };
        while self.views.len() > count {
            editor.close(self.views.pop().unwrap());
        }
        if self.views.len() < count {
            self.views = editor.comparison_views(count);
        }
        let entry = &mut self.entries[index];
        for side in 0..2 {
            if entry.documents[side].is_some_and(|id| editor.document(id).is_some()) {
                continue;
            }
            let editable = entry
                .after
                .as_ref()
                .filter(|_| side == 1 && !editor.pager && entry.message.is_none());
            let id = if let Some(patch) = &entry.patch {
                let mut doc = crate::diff_patch::document(
                    patch,
                    Some(side),
                    editor.config.clone(),
                    editor.syn_loader.clone(),
                );
                doc.name = Some(format!(
                    "Patch {}: {}",
                    if side == 0 { "before" } else { "after" },
                    entry.name.display()
                ));
                editor.new_file_from_document(Action::Load, doc)
            } else if let Some(path) = editable {
                // Editor::open reuses unsaved working buffers rather than re-reading them.
                editor.open(path, Action::Load)?
            } else {
                let bytes = if side == 0 {
                    &entry.before
                } else {
                    &entry.snapshot
                };
                let mut doc = snapshot(bytes, &entry.name, editor)?;
                doc.name = Some(format!(
                    "{}: {}",
                    if side == 0 { "Before" } else { "After" },
                    entry.name.display()
                ));
                if let Some(message) = &entry.message {
                    doc = snapshot(message.as_bytes(), Path::new("metadata.txt"), editor)?;
                    doc.name = Some(format!("Metadata: {}", entry.name.display()));
                }
                editor.new_file_from_document(Action::Load, doc)
            };
            entry.documents[side] = Some(id);
        }
        let ids = entry.documents.map(Option::unwrap);
        if entry.reload_after {
            let id = ids[1];
            if !editor.document(id).unwrap().is_modified()
                && !editor.document(id).unwrap().is_read_only()
            {
                let view_id = *self.views.last().unwrap();
                editor.focus(view_id);
                editor.switch(id, Action::Replace);
                restore(editor, id, view_id, self.positions.get(&id));
                let path = editor.document(id).unwrap().path().unwrap().to_path_buf();
                let trust = editor
                    .workspace_trust
                    .query(
                        &helix_loader::find_workspace_in(&path).0,
                        helix_loader::workspace_trust::TrustQuery::Git,
                    )
                    .is_trusted();
                editor.documents.get_mut(&id).unwrap().reload(
                    editor.tree.get_mut(view_id),
                    &mut editor.diff_providers,
                    trust,
                )?;
            }
            entry.reload_after = false;
        }
        let texts = ids.map(|id| editor.document(id).unwrap().text().clone());
        let versions = ids.map(|id| editor.document(id).unwrap().version());
        if self.layout == Layout::Inline
            && entry.patch.is_none()
            && entry.message.is_none()
            && !editor.document(ids[1]).unwrap().is_read_only()
        {
            editor.focus(self.views[0]);
            editor.switch(ids[1], Action::Replace);
            restore(editor, ids[1], self.views[0], self.positions.get(&ids[1]));
            editor.start_inline_diff(ids, self.views[0])?;
        } else if self.layout == Layout::Inline {
            let id = if let Some((id, _, _)) = entry.inline.filter(|(id, old_ids, old)| {
                *old_ids == ids && *old == versions && editor.document(*id).is_some()
            }) {
                id
            } else {
                let mut doc = if let Some(patch) = &entry.patch {
                    crate::diff_patch::document(
                        patch,
                        None,
                        editor.config.clone(),
                        editor.syn_loader.clone(),
                    )
                } else {
                    let sources = [0, 1].map(|side| {
                        let mut doc = Document::from(
                            texts[side].clone(),
                            None,
                            editor.config.clone(),
                            editor.syn_loader.clone(),
                        );
                        doc.access = DocumentAccess::ReadOnly;
                        let source = editor.document(ids[side]).unwrap();
                        doc.set_language(
                            source.language_config().cloned().map(Arc::new),
                            &editor.syn_loader.load(),
                        );
                        doc
                    });
                    let (text, model) = helix_view::inline_diff::InlineDiff::pair(sources);
                    let mut doc = Document::from(
                        text,
                        None,
                        editor.config.clone(),
                        editor.syn_loader.clone(),
                    );
                    doc.access = DocumentAccess::ReadOnly;
                    doc.inline_diff = Some(Box::new(model));
                    doc
                };
                doc.name = Some(format!("Review: {}", entry.name.display()));
                let id = editor.new_file_from_document(Action::Load, doc);
                entry.inline = Some((id, ids, versions));
                id
            };
            editor.focus(self.views[0]);
            editor.switch(id, Action::Replace);
            restore(editor, id, self.views[0], self.positions.get(&id));
        } else {
            for (side, &id) in ids.iter().enumerate() {
                editor.focus(self.views[side]);
                editor.switch(id, Action::Replace);
                restore(editor, id, self.views[side], self.positions.get(&id));
            }
            editor.start_diff([self.views[0], self.views[1]])?;
        }
        self.active = Some(index);
        editor.set_status(format!(
            "Review {}/{}: {} ({})",
            index + 1,
            self.entries.len(),
            self.entries[index].name.display(),
            if editor.inline_diff_session.is_some() {
                "inline, editable"
            } else if self.layout == Layout::Inline {
                "inline, read-only"
            } else {
                "split"
            }
        ));
        Ok(())
    }

    pub fn update_counts(&mut self, editor: &Editor) {
        let Some(index) = self.active else {
            return;
        };
        let entry = &mut self.entries[index];
        if entry.patch.is_some() || entry.message.is_some() {
            return;
        }
        let Some(session) = editor
            .diff_session
            .as_ref()
            .or(editor.inline_diff_session.as_ref())
        else {
            return;
        };
        if entry.documents != session.documents.map(Some) {
            return;
        }
        let versions = session
            .documents
            .map(|id| editor.document(id).unwrap().version());
        if entry.count_versions == Some(versions) {
            return;
        }
        if let Some(layout) = session
            .result
            .load_full()
            .filter(|layout| layout.versions == versions)
        {
            entry.counts = Some((
                layout.hunks.iter().map(|h| h.after.len()).sum(),
                layout.hunks.iter().map(|h| h.before.len()).sum(),
            ));
            entry.count_versions = Some(versions);
        }
    }
}

fn restore(
    editor: &mut Editor,
    id: DocumentId,
    view: ViewId,
    saved: Option<&(i32, helix_core::Selection, helix_view::view::ViewPosition)>,
) {
    let Some((version, selection, offset)) = saved else {
        return;
    };
    let doc = editor.document_mut(id).unwrap();
    if *version == doc.version() {
        doc.set_selection(
            view,
            selection.clone().ensure_invariants(doc.text().slice(..)),
        );
        doc.set_view_offset(view, *offset);
    }
}

fn snapshot(bytes: &[u8], path: &Path, editor: &Editor) -> Result<Document> {
    let binary = bytes.contains(&0)
        && !bytes.starts_with(&[0xff, 0xfe])
        && !bytes.starts_with(&[0xfe, 0xff]);
    let (text, encoding, bom) = if binary {
        from_reader(&mut &b"Binary file (read-only metadata)\n"[..], None)?
    } else {
        from_reader(&mut &*bytes, None)?
    };
    let mut doc = Document::from(
        text,
        Some((encoding, bom)),
        editor.config.clone(),
        editor.syn_loader.clone(),
    );
    doc.access = DocumentAccess::ReadOnly;
    let loader = editor.syn_loader.load();
    let language = loader
        .language_for_filename(path)
        .map(|id| loader.language(id).config().clone());
    doc.set_language(language, &loader);
    Ok(doc)
}

fn input(path: &Path) -> Result<(Option<PathBuf>, Vec<u8>, Option<String>)> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, Vec::new(), None))
        }
        Err(err) => return Err(err.into()),
    };
    if metadata.is_symlink() {
        return Ok((
            None,
            std::fs::read_link(path)?
                .to_string_lossy()
                .as_bytes()
                .to_vec(),
            Some("symlink (not followed)".into()),
        ));
    }
    if metadata.is_dir() {
        return Ok((None, Vec::new(), Some("directory".into())));
    }
    if !metadata.is_file() {
        return Ok((None, Vec::new(), Some("special file (not read)".into())));
    }
    let bytes = std::fs::read(path)?;
    let binary = bytes.contains(&0)
        && !bytes.starts_with(&[0xff, 0xfe])
        && !bytes.starts_with(&[0xfe, 0xff]);
    Ok((
        (!binary).then(|| path.to_path_buf()),
        bytes,
        binary.then(|| "Binary files differ (read-only metadata)".into()),
    ))
}

fn collect(root: &Path, dir: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    let mut walk = ignore::WalkBuilder::new(dir);
    walk.hidden(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git" && entry.file_name() != ".jj");
    for item in walk.build() {
        let item = item?;
        if item.depth() != 0 && !item.file_type().is_some_and(|kind| kind.is_dir()) {
            paths.insert(item.path().strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}
