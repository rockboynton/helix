//! Shared, persistent navigation panel for filesystem browsing and changesets.
use crate::compositor::EventResult;
use anyhow::Result;
use helix_view::{
    editor::Action,
    graphics::Rect,
    input::{Event, MouseButton, MouseEventKind},
    keyboard::KeyCode,
    Editor,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use tui::buffer::Buffer;

#[derive(Debug)]
pub struct Node {
    pub path: PathBuf,
    pub directory: bool,
    pub depth: usize,
    pub entry: Option<usize>,
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub expanded: BTreeSet<PathBuf>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    pub visible: bool,
    pub area: Rect,
    pub review: Option<crate::diff_review::Review>,
    visible_cache: Vec<usize>,
}

impl FileTree {
    pub fn from_paths(
        root: PathBuf,
        paths: impl IntoIterator<Item = (PathBuf, bool, Option<usize>)>,
    ) -> Self {
        let mut map: BTreeMap<PathBuf, (bool, Option<usize>)> = BTreeMap::new();
        for (path, directory, entry) in paths {
            let mut parent = path.parent();
            while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
                map.entry(path.to_path_buf()).or_insert((true, None)).0 = true;
                parent = path.parent();
            }
            let node = map.entry(path).or_insert((directory, entry));
            node.0 |= directory;
            if entry.is_some() {
                node.1 = entry;
            }
        }
        let mut nodes = map
            .into_iter()
            .map(|(path, (directory, entry))| Node {
                depth: path.components().count().saturating_sub(1),
                path,
                directory,
                entry,
            })
            .collect::<Vec<_>>();
        // Lexical component ordering keeps descendants contiguous, with folders first.
        nodes.sort_by(|a, b| {
            let mut ac = a.path.components();
            let mut bc = b.path.components();
            loop {
                match (ac.next(), bc.next()) {
                    (Some(x), Some(y)) if x == y => continue,
                    (Some(x), Some(y)) => {
                        let depth = a.path.components().count() - ac.clone().count();
                        let adir = a.directory || a.path.components().count() > depth;
                        let bdir = b.directory || b.path.components().count() > depth;
                        return bdir.cmp(&adir).then_with(|| x.cmp(&y));
                    }
                    (None, None) => return std::cmp::Ordering::Equal,
                    (None, _) => return std::cmp::Ordering::Less,
                    (_, None) => return std::cmp::Ordering::Greater,
                }
            }
        });
        let mut tree = Self {
            root,
            nodes,
            expanded: BTreeSet::new(),
            selected: 0,
            scroll: 0,
            focused: true,
            visible: true,
            area: Rect::default(),
            review: None,
            visible_cache: Vec::new(),
        };
        tree.rebuild_visibility();
        tree
    }

    pub fn scan(root: PathBuf, config: helix_view::editor::FilePickerConfig) -> Result<Self> {
        anyhow::ensure!(root.is_dir(), "file tree requires an existing directory");
        let mut walk = ignore::WalkBuilder::new(&root);
        walk.hidden(config.hidden)
            .parents(config.parents)
            .ignore(config.ignore)
            .git_ignore(config.git_ignore)
            .git_global(config.git_global)
            .git_exclude(config.git_exclude)
            .follow_links(false)
            .max_depth(config.max_depth)
            .filter_entry(|entry| entry.file_name() != ".git" && entry.file_name() != ".jj")
            .add_custom_ignore_filename(helix_loader::config_dir().join("ignore"))
            .add_custom_ignore_filename(".helix/ignore");
        let mut paths = Vec::new();
        for item in walk.build() {
            let item = item?;
            if item.depth() == 0 {
                continue;
            }
            let path = item.path().strip_prefix(&root)?.to_path_buf();
            // Metadata directories are never navigation targets.
            if path
                .components()
                .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".jj")
            {
                continue;
            }
            paths.push((
                path,
                item.file_type().is_some_and(|kind| kind.is_dir()),
                None,
            ));
        }
        Ok(Self::from_paths(root, paths))
    }

    pub fn visible(&self) -> Vec<usize> {
        self.visible_cache.clone()
    }

    fn rebuild_visibility(&mut self) {
        self.visible_cache = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.path
                    .ancestors()
                    .skip(1)
                    .filter(|path| !path.as_os_str().is_empty())
                    .all(|path| self.expanded.contains(path))
            })
            .map(|(index, _)| index)
            .collect();
    }

    pub fn reveal(&mut self, path: &Path) {
        for parent in path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            self.expanded.insert(parent.to_path_buf());
        }
        if let Some(index) = self.nodes.iter().position(|node| node.path == path) {
            self.selected = index;
        }
        self.rebuild_visibility();
    }

    pub fn render(&mut self, area: Rect, surface: &mut Buffer, editor: &Editor) {
        self.area = area;
        if let Some(review) = &mut self.review {
            review.update_counts(editor);
        }
        surface.clear_with(area, editor.theme.get("ui.background"));
        let title = if self.review.is_some() {
            "Changes"
        } else {
            "Files"
        };
        surface.set_stringn(
            area.x,
            area.y,
            title,
            area.width.saturating_sub(1) as usize,
            editor.theme.get("ui.text.focus"),
        );
        let visible = self.visible();
        let selected = visible
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(0);
        let height = area.height.saturating_sub(2) as usize;
        if selected < self.scroll {
            self.scroll = selected;
        }
        if selected >= self.scroll + height {
            self.scroll = (selected + 1).saturating_sub(height);
        }
        for (row, &index) in visible.iter().skip(self.scroll).take(height).enumerate() {
            let node = &self.nodes[index];
            let marker = if node.directory {
                if self.expanded.contains(&node.path) {
                    "▾"
                } else {
                    "▸"
                }
            } else {
                " "
            };
            let name = node
                .path
                .file_name()
                .unwrap_or(node.path.as_os_str())
                .to_string_lossy();
            let name = safe(&name);
            let suffix = node
                .entry
                .and_then(|index| self.review.as_ref()?.entries.get(index))
                .map(|entry| format!(" {}", entry.label()))
                .unwrap_or_default();
            let text = format!(
                "{}{} {}{}",
                "  ".repeat(node.depth.min(32)),
                marker,
                name,
                suffix
            );
            let style = editor.theme.get(if index == self.selected && self.focused {
                "ui.selection"
            } else if node.directory {
                "ui.text.directory"
            } else {
                "ui.text"
            });
            let line = Rect::new(
                area.x,
                area.y + row as u16 + 1,
                area.width.saturating_sub(1),
                1,
            );
            surface.set_style(line, style);
            surface.set_stringn(line.x, line.y, &text, line.width as usize, style);
        }
        if area.width > 0 {
            for y in area.y..area.y + area.height {
                surface.set_string(
                    area.x + area.width - 1,
                    y,
                    "│",
                    editor.theme.get("ui.window"),
                );
            }
        }
    }

    pub fn handle(&mut self, event: &Event, editor: &mut Editor) -> Result<EventResult> {
        if !self.focused && !matches!(event, Event::Mouse(_)) {
            return Ok(EventResult::Ignored(None));
        }
        let visible = self.visible();
        if let Event::Mouse(mouse) = event {
            let inside = mouse.column >= self.area.x
                && mouse.column < self.area.right()
                && mouse.row >= self.area.y
                && mouse.row < self.area.bottom();
            if inside
                && matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                )
            {
                let position = visible
                    .iter()
                    .position(|&index| index == self.selected)
                    .unwrap_or(0);
                let position = if mouse.kind == MouseEventKind::ScrollUp {
                    position.saturating_sub(3)
                } else {
                    (position + 3).min(visible.len().saturating_sub(1))
                };
                if let Some(&index) = visible.get(position) {
                    self.selected = index;
                }
                return Ok(EventResult::Consumed(None));
            }
            if mouse.column >= self.area.x
                && mouse.column < self.area.right()
                && mouse.row >= self.area.y
                && mouse.row < self.area.bottom()
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            {
                self.focused = true;
                if let Some(&index) =
                    visible.get(self.scroll + mouse.row.saturating_sub(self.area.y + 1) as usize)
                {
                    self.selected = index;
                    self.activate(editor)?;
                }
                return Ok(EventResult::Consumed(None));
            }
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.focused = false;
            }
        }
        if !self.focused {
            return Ok(EventResult::Ignored(None));
        }
        let Event::Key(key) = event else {
            return Ok(EventResult::Consumed(None));
        };
        let position = visible
            .iter()
            .position(|&index| index == self.selected)
            .unwrap_or(0);
        match key.code {
            KeyCode::Char(':') | KeyCode::Char(' ') => {
                self.focused = false;
                return Ok(EventResult::Ignored(None));
            }
            KeyCode::Char('q') => {
                self.focused = false;
                if editor.pager {
                    return Ok(EventResult::Ignored(None));
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(&index) =
                    visible.get((position + 1).min(visible.len().saturating_sub(1)))
                {
                    self.selected = index;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(&index) = visible.get(position.saturating_sub(1)) {
                    self.selected = index;
                }
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if let Some(&index) = visible.first() {
                    self.selected = index;
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if let Some(&index) = visible.last() {
                    self.selected = index;
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(node) = self.nodes.get(self.selected) {
                    if !self.expanded.remove(&node.path) {
                        if let Some(parent) = node.path.parent().map(Path::to_path_buf) {
                            self.reveal(&parent);
                        }
                    }
                }
                self.rebuild_visibility();
            }
            KeyCode::Char('v') | KeyCode::Char('n') | KeyCode::Char('p')
                if self.review.is_some() =>
            {
                let review = self.review.as_mut().unwrap();
                let index = review.active.unwrap_or(0);
                let index = match key.code {
                    KeyCode::Char('n') => (index + 1).min(review.entries.len().saturating_sub(1)),
                    KeyCode::Char('p') => index.saturating_sub(1),
                    _ => {
                        review.layout = if review.layout == crate::diff_render::Layout::Inline {
                            crate::diff_render::Layout::SideBySide
                        } else {
                            crate::diff_render::Layout::Inline
                        };
                        index
                    }
                };
                if !review.entries.is_empty() {
                    review.open(index, editor)?;
                    let name = review.entries[index].name.clone();
                    self.reveal(&name);
                }
            }
            KeyCode::Enter
                if self
                    .nodes
                    .get(self.selected)
                    .is_some_and(|node| node.entry.is_some()) =>
            {
                let index = self.nodes[self.selected].entry.unwrap();
                if let Some(review) = &mut self.review {
                    review.open(index, editor)?;
                    self.focused = false;
                }
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.activate(editor)?,
            KeyCode::Tab | KeyCode::Esc => self.focused = false,
            _ => {}
        }
        Ok(EventResult::Consumed(None))
    }

    pub fn activate(&mut self, editor: &mut Editor) -> Result<()> {
        let Some(node) = self.nodes.get(self.selected) else {
            return Ok(());
        };
        if node.directory {
            if !self.expanded.remove(&node.path) {
                self.expanded.insert(node.path.clone());
            }
            self.rebuild_visibility();
        } else if let Some(index) = node.entry {
            if let Some(review) = &mut self.review {
                review.open(index, editor)?;
                self.focused = false;
            }
        } else {
            editor.open(&self.root.join(&node.path), Action::Replace)?;
            self.focused = false;
        }
        Ok(())
    }
}

fn safe(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '�' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_expansion_reveal_and_empty_tree() {
        let mut tree = FileTree::from_paths(
            PathBuf::from("/tmp"),
            [
                ("a/b.rs".into(), false, Some(0)),
                ("a/c.rs".into(), false, Some(1)),
                ("root.rs".into(), false, Some(2)),
            ],
        );
        assert_eq!(tree.visible().len(), 2);
        tree.reveal(Path::new("a/c.rs"));
        assert_eq!(tree.visible().len(), 4);
        assert_eq!(tree.nodes[tree.selected].path, Path::new("a/c.rs"));
        assert!(FileTree::from_paths(PathBuf::new(), [])
            .visible()
            .is_empty());
        let mut collision = FileTree::from_paths(
            PathBuf::new(),
            [("a".into(), false, Some(0)), ("a/b".into(), false, Some(1))],
        );
        collision.reveal(Path::new("a/b"));
        assert!(collision.nodes[0].directory);
        assert_eq!(collision.nodes[0].entry, Some(0));
        assert_eq!(collision.visible().len(), 2);
    }
}
