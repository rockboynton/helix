//! Tree-local live path filtering. Filtering never mutates browsing expansion.
use super::*;
use helix_core::unicode::segmentation::UnicodeSegmentation;

#[derive(Clone, Default)]
pub(super) struct Filter {
    pub query: String,
    pub paths: BTreeSet<PathBuf>,
    expanded: BTreeSet<PathBuf>,
    matches: Vec<usize>,
    editing: Option<Edit>,
    original: Option<Position>,
}

#[derive(Clone)]
struct Position {
    path: Option<PathBuf>,
    scroll: usize,
}

#[derive(Clone)]
struct Edit {
    query: String,
    expanded: BTreeSet<PathBuf>,
    position: Position,
    original: Option<Position>,
}

impl FileTree {
    fn position(&self) -> Position {
        Position {
            path: self.nodes.get(self.selected).map(|node| node.path.clone()),
            scroll: self.scroll,
        }
    }

    fn restore_position(&mut self, position: Position) {
        if let Some(index) = self
            .nodes
            .iter()
            .position(|node| Some(&node.path) == position.path.as_ref())
        {
            self.selected = index;
        }
        self.scroll = position.scroll;
    }

    pub(super) fn expansion(&self) -> &BTreeSet<PathBuf> {
        if self.filter.query.is_empty() {
            &self.expanded
        } else {
            &self.filter.expanded
        }
    }

    pub(super) fn expansion_mut(&mut self) -> &mut BTreeSet<PathBuf> {
        if self.filter.query.is_empty() {
            &mut self.expanded
        } else {
            &mut self.filter.expanded
        }
    }

    pub fn filter_editing(&self) -> bool {
        self.filter.editing.is_some()
    }

    fn find_matches(&mut self) {
        self.filter.matches.clear();
        self.filter.paths.clear();
        let query = self.filter.query.to_lowercase();
        if query.is_empty() {
            return;
        }
        for (index, node) in self.nodes.iter().enumerate() {
            if node.path.to_string_lossy().to_lowercase().contains(&query) {
                self.filter.matches.push(index);
                self.filter.paths.insert(node.path.clone());
                for parent in node
                    .path
                    .ancestors()
                    .skip(1)
                    .filter(|p| !p.as_os_str().is_empty())
                {
                    self.filter.paths.insert(parent.to_path_buf());
                    self.filter.expanded.insert(parent.to_path_buf());
                }
            }
        }
    }

    fn set_filter(&mut self, query: String) {
        if !query.is_empty() && self.filter.original.is_none() {
            self.filter.original = Some(self.position());
        }
        self.filter.query = query;
        self.filter.expanded.clear();
        self.find_matches();
        self.scroll = 0;
        if self.filter.query.is_empty() {
            if let Some(position) = self.filter.original.clone() {
                self.restore_position(position);
            }
        } else if !self.filter.matches.contains(&self.selected) {
            if let Some(&index) = self.filter.matches.first() {
                self.selected = index;
            }
        }
        self.rebuild_visibility();
    }

    pub(super) fn begin_filter(&mut self) {
        self.filter.editing = Some(Edit {
            query: self.filter.query.clone(),
            expanded: self.filter.expanded.clone(),
            position: self.position(),
            original: self.filter.original.clone(),
        });
    }

    pub(super) fn clear_filter(&mut self) {
        let original = self.filter.original.take();
        self.filter = Filter::default();
        self.rebuild_visibility();
        if let Some(position) = original {
            self.restore_position(position);
        }
        self.ensure_filter_selection();
    }

    pub(super) fn next_match(&mut self, backwards: bool) {
        let matches = &self.filter.matches;
        if matches.is_empty() {
            return;
        }
        let position = matches.iter().position(|&index| index == self.selected);
        let next = if backwards {
            position
                .unwrap_or(0)
                .checked_sub(1)
                .unwrap_or(matches.len() - 1)
        } else {
            position.map_or(0, |position| (position + 1) % matches.len())
        };
        let path = self.nodes[matches[next]].path.clone();
        self.reveal(&path);
    }

    pub(super) fn filter_status(&self) -> String {
        if self.filter.editing.is_some() {
            format!("/{}▏ ({})", self.filter.query, self.filter.matches.len())
        } else if !self.filter.query.is_empty() {
            let position = self
                .filter
                .matches
                .iter()
                .position(|&index| index == self.selected)
                .map_or(0, |index| index + 1);
            format!(
                "/{} {}/{} · Esc clear",
                self.filter.query,
                position,
                self.filter.matches.len()
            )
        } else {
            "/ filter · ? help".to_owned()
        }
    }

    pub(super) fn handle_filter(&mut self, event: &Event) -> bool {
        if !self.filter_editing() {
            return false;
        }
        let mut query = self.filter.query.clone();
        match event {
            Event::Paste(text) => query.extend(text.chars().filter(|c| !c.is_control())),
            Event::Key(key) => match key.code {
                KeyCode::Enter => {
                    self.filter.editing = None;
                    if self.filter.query.is_empty() {
                        self.clear_filter();
                    }
                    return true;
                }
                KeyCode::Esc => {
                    let edit = self.filter.editing.take().unwrap();
                    self.filter.query = edit.query;
                    self.filter.original = edit.original;
                    self.find_matches();
                    self.filter.expanded = edit.expanded;
                    self.rebuild_visibility();
                    self.restore_position(edit.position);
                    return true;
                }
                KeyCode::Backspace => {
                    if let Some((start, _)) = query.grapheme_indices(true).next_back() {
                        query.truncate(start);
                    }
                }
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => query.clear(),
                KeyCode::Char('w') if key.modifiers == KeyModifiers::CONTROL => {
                    query = query.trim_end().to_owned();
                    while query.chars().last().is_some_and(|c| !c.is_whitespace()) {
                        query.pop();
                    }
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(c)
                }
                _ => return true,
            },
            _ => return false,
        }
        self.set_filter(query);
        true
    }

    /// Rebuild indices against refreshed nodes without losing the query or edit.
    pub fn inherit_filter(&mut self, old: &Self) {
        self.filter = old.filter.clone();
        self.find_matches();
        let old_paths = old
            .nodes
            .iter()
            .map(|node| &node.path)
            .collect::<BTreeSet<_>>();
        self.filter
            .expanded
            .retain(|path| old.filter.expanded.contains(path) || !old_paths.contains(path));
        self.restore_position(old.position());
        self.ensure_filter_selection();
    }

    pub fn ensure_filter_selection(&mut self) {
        self.rebuild_visibility();
        if !self.visible_cache.contains(&self.selected) {
            if let Some(&index) = self.visible_cache.first() {
                self.selected = index;
            }
        }
    }

    pub fn restore_filter_selection(&mut self, old: &Self) {
        if !self.filter.query.is_empty() && self.filter.query == old.filter.query {
            self.restore_position(old.position());
            self.focused = old.focused;
            self.visible = old.visible;
            self.ensure_filter_selection();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(helix_view::input::KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
        })
    }

    fn tree() -> FileTree {
        FileTree::from_paths(
            PathBuf::from("/virtual"),
            ["src/a.rs", "tests/b.rs", "notes/doc.md", "résumé.rs"]
                .map(|path| (PathBuf::from(path), false, None)),
        )
    }

    #[test]
    fn filtering_reveals_ancestors_wraps_and_restores_browsing_state() {
        let mut tree = tree();
        tree.reveal(Path::new("src/a.rs"));
        let original = tree.position();
        let expanded = tree.expanded.clone();
        tree.begin_filter();
        for c in "RS".chars() {
            assert!(tree.handle_filter(&key(KeyCode::Char(c))));
        }
        assert_eq!(tree.filter.matches.len(), 3);
        assert!(tree
            .visible()
            .iter()
            .all(|&i| !tree.nodes[i].path.starts_with("notes")));
        assert!(tree
            .visible()
            .iter()
            .any(|&i| tree.nodes[i].path == Path::new("tests/b.rs")));
        assert_eq!(tree.expanded, expanded);
        tree.handle_filter(&key(KeyCode::Enter));
        let mut visited = BTreeSet::new();
        for _ in 0..3 {
            tree.next_match(false);
            visited.insert(tree.selected);
        }
        assert_eq!(visited, tree.filter.matches.iter().copied().collect());
        let selected = tree.selected;
        tree.next_match(false);
        tree.next_match(true);
        assert_eq!(tree.selected, selected);
        tree.clear_filter();
        assert_eq!(tree.expanded, expanded);
        assert_eq!(tree.position().path, original.path);
    }

    #[test]
    fn cancelled_edits_restore_query_and_unicode_backspace_is_grapheme_aware() {
        let mut tree = tree();
        tree.set_filter("rs".into());
        tree.begin_filter();
        tree.set_filter("nothing-matches".into());
        assert!(tree.visible().is_empty());
        tree.handle_filter(&key(KeyCode::Esc));
        assert_eq!(tree.filter.query, "rs");
        assert_eq!(tree.filter.matches.len(), 3);
        tree.clear_filter();
        tree.begin_filter();
        tree.handle_filter(&Event::Paste("e\u{301}".into()));
        tree.handle_filter(&key(KeyCode::Backspace));
        assert!(tree.filter.query.is_empty());
        assert!(tree.filter_editing());
        tree.handle_filter(&key(KeyCode::Esc));
        assert!(!tree.filter_editing());
    }

    #[test]
    fn refreshing_keeps_filter_and_recomputes_matches_for_changed_catalog() {
        let mut old = tree();
        old.set_filter("rs".into());
        let mut new = FileTree::from_paths(
            PathBuf::from("/virtual"),
            ["new/deep/c.rs", "notes/doc.md"].map(|path| (PathBuf::from(path), false, None)),
        );
        new.expanded = old.expanded.clone();
        new.inherit_filter(&old);
        assert_eq!(new.filter.query, "rs");
        assert_eq!(new.filter.matches.len(), 1);
        assert!(new
            .visible()
            .iter()
            .any(|&i| new.nodes[i].path == Path::new("new/deep/c.rs")));
        assert!(new
            .visible()
            .iter()
            .all(|&i| !new.nodes[i].path.starts_with("notes")));
    }
}
