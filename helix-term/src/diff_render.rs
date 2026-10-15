//! Read-only, flow-text frontend for standalone use and external diff hosts.
//! It emits text/SGR only: no terminal acquisition, cursor motion or alternate screen.
use std::{
    collections::BTreeSet,
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use helix_core::{syntax, text_annotations::TextAnnotations, Rope};
use helix_view::{
    diff_session::{DiffChange, DiffLayout},
    document::{from_reader, DocumentAccess},
    graphics::{Color, Modifier, Rect},
    theme,
    view::ViewPosition,
    Document, Theme,
};
use tui::buffer::Buffer;

use crate::{
    args::Args,
    config::Config,
    ui::{
        document::{render_document, LinePos, TextRenderer},
        editor::EditorView,
        text_decorations::{self, DecorationManager},
    },
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    #[default]
    SideBySide,
    Inline,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

struct Renderer {
    theme: Theme,
    loader: Arc<ArcSwap<syntax::Loader>>,
    config: Arc<ArcSwap<helix_view::editor::Config>>,
    icons: helix_view::icons::Icons,
    width: u16,
    context: usize,
    layout: Layout,
    color: bool,
}

struct SourceLine<'a> {
    doc: &'a Document,
    index: usize,
    numbers: [Option<usize>; 2],
    removed: Option<bool>,
    change: Option<DiffChange>,
    words: &'a [Range<usize>],
}

pub fn run(args: &Args, output: &mut impl Write, stdout_is_terminal: bool) -> Result<()> {
    let config = Config::load_default().map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut dirs = vec![helix_loader::config_dir()];
    dirs.extend(helix_loader::runtime_dirs().iter().cloned());
    let themes = theme::Loader::new(&dirs);
    let name = args
        .diff_theme
        .as_deref()
        .or_else(|| config.theme.as_ref().map(|theme| theme.choose(None)));
    let theme = match name {
        Some(name) => themes.load(name)?,
        None => themes.default_theme(true),
    };
    let trust =
        helix_loader::workspace_trust::WorkspaceTrust::new((&config.editor.workspace_trust).into());
    let loader = helix_core::config::user_lang_loader(&trust)?;
    loader.set_scopes(theme.scopes().to_vec());
    let icons_loader = helix_view::icons::Loader::new(&dirs);
    let icons = match config.icons.as_deref() {
        Some(name) => icons_loader.load(name, &theme, true)?,
        None => icons_loader.default(&theme),
    };
    let renderer = Renderer {
        theme,
        icons,
        loader: Arc::new(ArcSwap::from_pointee(loader)),
        config: Arc::new(ArcSwap::from_pointee(config.editor)),
        width: args.diff_width.unwrap_or(100),
        context: args.diff_context.unwrap_or(3),
        layout: args.diff_layout.unwrap_or_default(),
        color: match args.diff_color.unwrap_or_default() {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => stdout_is_terminal,
        },
    };
    if args.patch {
        return renderer.patch(&crate::diff_patch::read(args)?, output);
    }
    let paths = args.files.keys().collect::<Vec<_>>();
    let [left, right] = paths.as_slice() else {
        anyhow::bail!("--render-diff requires two file or directory paths")
    };
    if left.is_dir() && right.is_dir() {
        let mut files = BTreeSet::new();
        collect_files(left, left, &mut files)?;
        collect_files(right, right, &mut files)?;
        for file in files {
            renderer.pair(&left.join(&file), &right.join(&file), &file, output)?;
        }
    } else {
        anyhow::ensure!(
            !left.is_dir() && !right.is_dir(),
            "compare two files or two directories, not a file and directory"
        );
        renderer.pair(left, right, right, output)?;
    }
    Ok(())
}

fn collect_files(root: &Path, directory: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_files(root, &entry.path(), files)?;
        } else {
            files.insert(entry.path().strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

#[derive(Default, PartialEq, Eq)]
struct Input {
    bytes: Vec<u8>,
    symlink: bool,
    exists: bool,
    executable: bool,
}

fn read_input(path: &Path) -> Result<Input> {
    if path == Path::new("/dev/null") {
        return Ok(Input::default());
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Input::default()),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_symlink() {
        return Ok(Input {
            bytes: std::fs::read_link(path)?
                .to_string_lossy()
                .as_bytes()
                .to_vec(),
            symlink: true,
            exists: true,
            executable: false,
        });
    }
    if metadata.is_dir() {
        return Ok(Input::default());
    }
    anyhow::ensure!(metadata.is_file(), "not a regular file: {}", path.display());
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    Ok(Input {
        bytes: std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        symlink: false,
        exists: true,
        executable,
    })
}

impl Renderer {
    fn patch(&self, patch: &crate::diff_patch::Patch, output: &mut impl Write) -> Result<()> {
        let doc =
            crate::diff_patch::document(patch, None, self.config.clone(), self.loader.clone());
        let model = doc.inline_diff.as_ref().unwrap();
        let mut index = 0;
        while index < model.lines.len() {
            let line = model.lines[index];
            if line.numbers == [None; 2] {
                writeln!(
                    output,
                    "{}",
                    visible(doc.text().line(index).to_string().trim_end_matches('\n'))
                )?;
                index += 1;
                continue;
            }
            let mut groups = [Vec::new(), Vec::new()];
            if self.layout == Layout::Inline {
                groups[0].push(line);
                index += 1;
            } else if line.removed.is_none() {
                groups[0].push(line);
                groups[1].push(line);
                index += 1;
            } else {
                while index < model.lines.len() && model.lines[index].removed.is_some() {
                    let line = model.lines[index];
                    groups[usize::from(line.removed != Some(true))].push(line);
                    index += 1;
                }
            }
            let widths = if self.layout == Layout::Inline {
                [self.width, 0]
            } else {
                [(self.width - 3) / 2, self.width - 3 - (self.width - 3) / 2]
            };
            for row in 0..groups[0].len().max(groups[1].len()) {
                let mut buffer = Buffer::empty(Rect::new(0, 0, self.width, 1));
                for side in 0..2 {
                    if let Some(line) = groups[side].get(row) {
                        let x = if side == 0 { 0 } else { widths[0] + 3 };
                        self.line(
                            &mut buffer,
                            Rect::new(x, 0, widths[side], 1),
                            SourceLine {
                                doc: &model.sources[line.source],
                                words: &model.source_words[line.source],
                                index: line.line,
                                numbers: if self.layout == Layout::Inline {
                                    line.numbers
                                } else {
                                    [line.numbers[side], None]
                                },
                                removed: line.removed,
                                change: line.removed.map(|removed| {
                                    if removed {
                                        DiffChange::Removed
                                    } else {
                                        DiffChange::Added
                                    }
                                }),
                            },
                        );
                    }
                }
                if self.layout == Layout::SideBySide {
                    buffer.set_string(widths[0] + 1, 0, "│", self.theme.get("ui.window"));
                }
                emit(&buffer, self.color, output)?;
            }
        }
        Ok(())
    }
    fn document(&self, bytes: &[u8], path: &Path) -> Result<Document> {
        let (text, encoding, bom) = from_reader(&mut &*bytes, None)?;
        let mut doc = Document::from(
            text,
            Some((encoding, bom)),
            self.config.clone(),
            self.loader.clone(),
        );
        doc.access = DocumentAccess::ReadOnly;
        // Path only selects syntax; no Editor::open, providers, LSPs or hooks.
        doc.set_path(Some(path));
        doc.detect_language(&self.loader.load());
        Ok(doc)
    }

    fn pair(&self, left: &Path, right: &Path, name: &Path, output: &mut impl Write) -> Result<()> {
        let before = read_input(left)?;
        let after = read_input(right)?;
        anyhow::ensure!(before.exists || after.exists, "neither diff input exists");
        if before == after {
            return Ok(());
        }
        writeln!(
            output,
            "diff --neohelix {}",
            visible(&name.to_string_lossy())
        )?;
        if before.symlink || after.symlink {
            let describe = |input: &Input| {
                if input.symlink {
                    visible(&String::from_utf8_lossy(&input.bytes))
                } else if input.exists {
                    "(regular file)".into()
                } else {
                    "(absent)".into()
                }
            };
            writeln!(
                output,
                "Symlink/type change: {} -> {}",
                describe(&before),
                describe(&after)
            )?;
            return Ok(());
        }
        let binary = |bytes: &[u8]| {
            bytes.contains(&0)
                && !bytes.starts_with(&[0xff, 0xfe])
                && !bytes.starts_with(&[0xfe, 0xff])
        };
        if before.executable != after.executable {
            writeln!(
                output,
                "Executable bit: {} -> {}",
                before.executable, after.executable
            )?;
        }
        if binary(&before.bytes) || binary(&after.bytes) {
            writeln!(output, "Binary files differ")?;
            return Ok(());
        }
        let docs = [
            self.document(&before.bytes, left)?,
            self.document(&after.bytes, right)?,
        ];
        let texts = docs.each_ref().map(|doc| doc.text().clone());
        let layout = DiffLayout::compute(&texts, [0, 0]);
        if layout.hunks.is_empty() {
            writeln!(
                output,
                "{}",
                if !before.exists {
                    "Added empty file"
                } else if !after.exists {
                    "Removed empty file"
                } else if before.bytes == after.bytes {
                    "Text unchanged"
                } else {
                    "File bytes/encoding changed; decoded text is identical"
                }
            )?;
            return Ok(());
        }
        for range in context_ranges(&layout, &texts, self.context) {
            writeln!(
                output,
                "@@ aligned rows {}..{} @@",
                range.start + 1,
                range.end
            )?;
            match self.layout {
                Layout::SideBySide => self.side_by_side(&docs, &layout, range, output)?,
                Layout::Inline => self.inline(&docs, &layout, range, output)?,
            }
        }
        for (side, doc) in docs.iter().enumerate() {
            if doc.text().len_chars() != 0 && doc.text().char(doc.text().len_chars() - 1) != '\n' {
                writeln!(
                    output,
                    "\\ No newline at end of {} file",
                    if side == 0 { "left" } else { "right" }
                )?;
            }
        }
        Ok(())
    }

    fn side_by_side(
        &self,
        docs: &[Document; 2],
        layout: &DiffLayout,
        rows: Range<usize>,
        output: &mut impl Write,
    ) -> Result<()> {
        let widths = [(self.width - 3) / 2, self.width - 3 - (self.width - 3) / 2];
        for row in rows {
            let mut buffer = Buffer::empty(Rect::new(0, 0, self.width, 1));
            for side in 0..2 {
                let line = line_at(&layout.sides[side].rows, row)
                    .filter(|&line| line < real_lines(docs[side].text()));
                let x = if side == 0 { 0 } else { widths[0] + 3 };
                if let Some(line) = line {
                    self.line(
                        &mut buffer,
                        Rect::new(x, 0, widths[side], 1),
                        SourceLine {
                            doc: &docs[side],
                            words: &layout.sides[side].words,
                            index: line,
                            numbers: [Some(line + 1), None],
                            removed: layout.sides[side].is_changed(line).then_some(side == 0),
                            change: layout.sides[side].gutter_at(line, true),
                        },
                    );
                }
            }
            buffer.set_string(widths[0] + 1, 0, "│", self.theme.get("ui.window"));
            emit(&buffer, self.color, output)?;
        }
        Ok(())
    }

    fn inline(
        &self,
        docs: &[Document; 2],
        layout: &DiffLayout,
        rows: Range<usize>,
        output: &mut impl Write,
    ) -> Result<()> {
        let mut emitted = BTreeSet::new();
        for row in rows {
            let lines = [0, 1].map(|side| {
                line_at(&layout.sides[side].rows, row)
                    .filter(|&line| line < real_lines(docs[side].text()))
            });
            let changed = [0, 1]
                .map(|side| lines[side].is_some_and(|line| layout.sides[side].is_changed(line)));
            if changed.into_iter().any(|changed| changed) {
                // Emit every removed line before every added line in its hunk.
                let index = layout
                    .hunks
                    .iter()
                    .position(|hunk| {
                        lines[0].is_some_and(|line| hunk.before.contains(&(line as u32)))
                            || lines[1].is_some_and(|line| hunk.after.contains(&(line as u32)))
                    })
                    .unwrap();
                if !emitted.insert(index) {
                    continue;
                }
                let hunk = &layout.hunks[index];
                for (side, range) in [hunk.before.clone(), hunk.after.clone()]
                    .into_iter()
                    .enumerate()
                {
                    for line in range.start as usize
                        ..(range.end as usize).min(real_lines(docs[side].text()))
                    {
                        let mut buffer = Buffer::empty(Rect::new(0, 0, self.width, 1));
                        let area = buffer.area;
                        let mut numbers = [None; 2];
                        numbers[side] = Some(line + 1);
                        self.line(
                            &mut buffer,
                            area,
                            SourceLine {
                                doc: &docs[side],
                                words: &layout.sides[side].words,
                                index: line,
                                numbers,
                                removed: Some(side == 0),
                                change: Some(if hunk.is_pure_insertion() {
                                    DiffChange::Added
                                } else if hunk.is_pure_removal() {
                                    DiffChange::Removed
                                } else {
                                    DiffChange::Modified
                                }),
                            },
                        );
                        emit(&buffer, self.color, output)?;
                    }
                }
            } else if let Some(line) = lines[1] {
                let mut buffer = Buffer::empty(Rect::new(0, 0, self.width, 1));
                let area = buffer.area;
                self.line(
                    &mut buffer,
                    area,
                    SourceLine {
                        doc: &docs[1],
                        words: &[],
                        index: line,
                        numbers: lines.map(|line| line.map(|line| line + 1)),
                        removed: None,
                        change: None,
                    },
                );
                emit(&buffer, self.color, output)?;
            }
        }
        Ok(())
    }

    fn line(&self, buffer: &mut Buffer, area: Rect, source: SourceLine<'_>) {
        let SourceLine {
            doc,
            index: line,
            numbers,
            removed,
            change,
            words,
        } = source;
        let numbers =
            numbers.map(|number| number.map(|number| number.to_string()).unwrap_or_default());
        let number = match self.layout {
            Layout::SideBySide => format!("{:>4} ", numbers[0]),
            Layout::Inline => format!("{:>4} {:>4} ", numbers[0], numbers[1]),
        };
        let prefix = (number.len() as u16 + 2).min(area.width);
        buffer.set_stringn(
            area.x,
            0,
            &number,
            prefix as usize,
            self.theme.get("ui.linenr"),
        );
        if let Some(change) = change {
            let (icon, scope) = match change {
                DiffChange::Added => (&self.icons.diff.added, "diff.plus.gutter"),
                DiffChange::Removed => (&self.icons.diff.deleted, "diff.minus.gutter"),
                DiffChange::Modified => (&self.icons.diff.modified, "diff.delta.gutter"),
            };
            buffer.set_stringn(
                area.x + prefix - 2,
                0,
                &icon.icon_char.to_string(),
                1,
                icon.style
                    .map(Into::into)
                    .unwrap_or_else(|| self.theme.get(scope)),
            );
        }
        let viewport = area.clip_left(prefix);
        if viewport.width == 0 {
            return;
        }
        let loader = self.loader.load();
        let anchor = doc.text().line_to_char(line);
        let highlighter = EditorView::doc_syntax_highlighter(doc, anchor, 1_u16, &loader);
        let mut decorations = DecorationManager::default();
        if let Some(removed) = removed {
            let bg = text_decorations::diff::background(&self.theme, removed);
            let word_bg = text_decorations::diff::word_background(&self.theme, removed);
            decorations.add_decoration(text_decorations::diff::Words(Some(
                words.iter().map(|range| (range.clone(), word_bg)).collect(),
            )));
            decorations.add_decoration(move |renderer: &mut TextRenderer, pos: LinePos| {
                renderer.set_line_background(Some(bg), pos.visual_line)
            });
        }
        let mut format = doc.text_format(viewport.width, Some(&self.theme));
        format.soft_wrap = false;
        render_document(
            buffer,
            viewport,
            doc,
            ViewPosition {
                anchor,
                ..ViewPosition::default()
            },
            &TextAnnotations::default(),
            highlighter,
            Vec::new(),
            &self.theme,
            decorations,
            Some(format),
        );
        // Flow text has no horizontal viewport to scroll. Mark clipped source
        // explicitly rather than silently presenting a truncated line as complete.
        let end = helix_core::line_ending::line_end_char_index(&doc.text().slice(..), line);
        let mut format = doc.text_format(viewport.width, None);
        format.soft_wrap = false;
        let column = helix_core::visual_offset_from_block(
            doc.text().slice(..),
            anchor,
            end,
            &format,
            &TextAnnotations::default(),
        )
        .0
        .col;
        if column > viewport.width as usize {
            let last = viewport.right() - 1;
            if last > viewport.x && buffer[(last - 1, 0)].width() > 1 {
                buffer[(last - 1, 0)].set_symbol(" ");
            }
            buffer.set_stringn(
                viewport.right() - 1,
                0,
                "…",
                1,
                self.theme.get("ui.virtual.wrap"),
            );
        }
    }
}

fn real_lines(text: &Rope) -> usize {
    text.len_lines() - usize::from(text.line(text.len_lines() - 1).len_chars() == 0)
}
fn line_at(rows: &[usize], row: usize) -> Option<usize> {
    rows.binary_search(&row).ok()
}

fn context_ranges(layout: &DiffLayout, texts: &[Rope; 2], context: usize) -> Vec<Range<usize>> {
    let height = layout
        .sides
        .iter()
        .map(|side| side.rows.last().copied().unwrap_or(0) + 1)
        .max()
        .unwrap_or(0);
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for hunk in &layout.hunks {
        let starts = [hunk.before.start as usize, hunk.after.start as usize];
        let start = [0, 1]
            .map(|side| layout.sides[side].row(starts[side].min(texts[side].len_lines() - 1)))
            .into_iter()
            .min()
            .unwrap();
        let length = hunk.before.len().max(hunk.after.len()).max(1);
        let end = start + length;
        let range = start.saturating_sub(context)..end.saturating_add(context).min(height);
        if let Some(last) = ranges.last_mut().filter(|last| last.end >= range.start) {
            last.end = last.end.max(range.end);
        } else {
            ranges.push(range);
        }
    }
    ranges
}

fn visible(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                char::from_u32(0x2400 + c as u32)
                    .filter(|_| c as u32 <= 31)
                    .unwrap_or('�')
            } else {
                c
            }
        })
        .collect()
}

fn color_code(color: Color, bg: bool) -> String {
    let code = if bg { 48 } else { 38 };
    match color {
        Color::Reset => (if bg { 49 } else { 39 }).to_string(),
        Color::Rgb(r, g, b) => format!("{code};2;{r};{g};{b}"),
        Color::Indexed(index) => format!("{code};5;{index}"),
        color => {
            let index = match color {
                Color::Black => 0,
                Color::Red => 1,
                Color::Green => 2,
                Color::Yellow => 3,
                Color::Blue => 4,
                Color::Magenta => 5,
                Color::Cyan => 6,
                Color::Gray => 7,
                Color::LightGray => 8,
                Color::LightRed => 9,
                Color::LightGreen => 10,
                Color::LightYellow => 11,
                Color::LightBlue => 12,
                Color::LightMagenta => 13,
                Color::LightCyan => 14,
                Color::White => 15,
                _ => unreachable!(),
            };
            format!("{code};5;{index}")
        }
    }
}

fn emit(buffer: &Buffer, color: bool, output: &mut impl Write) -> Result<()> {
    let mut previous = None;
    let mut x = 0;
    while x < buffer.area.width {
        let cell = &buffer[(x, 0)];
        let style = (cell.fg, cell.bg, cell.modifier);
        if color && previous != Some(style) {
            write!(
                output,
                "\x1b[0;{};{}",
                color_code(cell.fg, false),
                color_code(cell.bg, true)
            )?;
            for (flag, code) in [
                (Modifier::BOLD, 1),
                (Modifier::DIM, 2),
                (Modifier::ITALIC, 3),
                (Modifier::REVERSED, 7),
                (Modifier::CROSSED_OUT, 9),
            ] {
                if cell.modifier.contains(flag) {
                    write!(output, ";{code}")?;
                }
            }
            write!(output, "m")?;
            previous = Some(style);
        }
        write!(output, "{}", visible(cell.symbol.as_str()))?;
        x += cell.width().max(1) as u16;
    }
    if color {
        write!(output, "\x1b[0m")?;
    }
    writeln!(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_core::unicode::width::UnicodeWidthStr;

    fn renderer(layout: Layout, color: bool) -> Renderer {
        let theme = theme::Loader::new(&[]).default_theme(true);
        let loader = helix_core::config::default_lang_loader();
        loader.set_scopes(theme.scopes().to_vec());
        let icons = helix_view::icons::Loader::new(&[]).default(&theme);
        Renderer {
            theme,
            icons,
            loader: Arc::new(ArcSwap::from_pointee(loader)),
            config: Arc::new(ArcSwap::from_pointee(helix_view::editor::Config::default())),
            width: 100,
            context: 3,
            layout,
            color,
        }
    }

    fn render_pair(before: &str, after: &str, layout: Layout, color: bool) -> String {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.rs");
        let right = directory.path().join("right.rs");
        std::fs::write(&left, before).unwrap();
        std::fs::write(&right, after).unwrap();
        let mut output = Vec::new();
        renderer(layout, color)
            .pair(&left, &right, Path::new("file.rs"), &mut output)
            .unwrap();
        assert_eq!(std::fs::read_to_string(left).unwrap(), before);
        assert_eq!(std::fs::read_to_string(right).unwrap(), after);
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn replacement_words_have_distinct_backgrounds_in_both_layouts() {
        for layout in [Layout::Inline, Layout::SideBySide] {
            let output = render_pair("fn old() {}\n", "fn new() {}\n", layout, true);
            for color in [
                "48;2;48;0;0",
                "48;2;0;40;0",
                "48;2;80;24;24",
                "48;2;24;72;32",
            ] {
                assert!(output.contains(color), "missing {color}: {output:?}");
            }
            let patch = crate::diff_patch::parse(
                "--- a/x.rs\n+++ b/x.rs\n@@ -50 +60 @@\n-fn old() {}\n+fn new() {}\n",
            )
            .unwrap();
            let mut output = Vec::new();
            renderer(layout, true).patch(&patch, &mut output).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("48;2;80;24;24"), "{output:?}");
            assert!(output.contains("48;2;24;72;32"), "{output:?}");
        }
    }

    #[test]
    fn patch_layouts_use_source_coordinates_syntax_and_safe_output() {
        for layout in [Layout::Inline, Layout::SideBySide] {
            let patch = crate::diff_patch::parse(
                "--- a/x.rs\n+++ b/x.rs\n@@ -50 +60 @@\n-fn old() {}\n+fn new() {}\n",
            )
            .unwrap();
            let mut output = Vec::new();
            renderer(layout, false).patch(&patch, &mut output).unwrap();
            let output = String::from_utf8(output).unwrap();
            for part in ["old()", "new()", "50", "60"] {
                assert!(output.contains(part), "{output}");
            }
            assert!(!output.contains('\x1b'));
            let patch = crate::diff_patch::parse(
                "--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+\x1b]52;c;evil\x07\n",
            )
            .unwrap();
            let mut output = Vec::new();
            renderer(layout, false).patch(&patch, &mut output).unwrap();
            assert!(!output.contains(&0x1b));
            assert!(!output.contains(&7));
        }
    }

    #[test]
    fn both_layouts_preserve_lines_and_inline_orders_removed_before_added() {
        for layout in [Layout::Inline, Layout::SideBySide] {
            let output = render_pair(
                "fn old() {}\nfn gone() {}\n",
                "fn new() {}\nfn extra() {}\n",
                layout,
                false,
            );
            for text in ["fn old", "fn gone", "fn new", "fn extra"] {
                assert!(output.contains(text), "{output}");
            }
            assert!(!output.contains('\x1b'));
            if layout == Layout::Inline {
                assert!(output.find("fn gone").unwrap() < output.find("fn new").unwrap());
            } else {
                assert!(output.contains('│'));
            }
        }
    }

    #[test]
    fn syntax_uses_original_documents_and_output_only_contains_sgr_controls() {
        let renderer = renderer(Layout::SideBySide, true);
        let doc = renderer
            .document(b"fn main() {}\n", Path::new("main.rs"))
            .unwrap();
        assert_eq!(doc.language_name(), Some("rust"));
        assert!(doc.syntax().is_some());
        assert!(doc.is_read_only());
        let output = render_pair(
            "fn old() {}\n",
            "fn new() {} // \x1b]52;c;payload\x07\n",
            Layout::SideBySide,
            true,
        );
        assert!(!output.contains("\x1b]52"));
        assert!(!output.contains('\x07'));
        assert!(output.contains("48;2;48;0;0"));
        assert!(output.contains("48;2;0;40;0"));
        for control in output.split('\x1b').skip(1) {
            assert!(control.starts_with('['));
            let end = control.find('m').unwrap();
            assert!(control[1..end]
                .chars()
                .all(|c| c.is_ascii_digit() || c == ';'));
        }
    }

    #[test]
    fn context_is_bounded_and_wide_or_long_lines_have_explicit_clipping() {
        let before = (0..50)
            .map(|line| format!("line{line:02}\n"))
            .collect::<String>();
        let after = before.replace("line25", "changed25");
        let output = render_pair(&before, &after, Layout::Inline, false);
        assert!(output.contains("line22") && output.contains("line28"));
        assert!(!output.contains("line00") && !output.contains("line49"));
        let output = render_pair(
            "old\n",
            &format!("{}\n", "界".repeat(100)),
            Layout::SideBySide,
            false,
        );
        assert!(output.contains('…'));
        for line in output
            .lines()
            .filter(|line| !line.starts_with("diff ") && !line.starts_with("@@"))
        {
            assert!(line.width() <= 100, "{line}");
        }
    }

    #[test]
    fn directory_inputs_include_empty_additions_binary_and_symlinks_without_following() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        std::fs::create_dir_all(left.join("nested")).unwrap();
        std::fs::create_dir_all(right.join("nested")).unwrap();
        std::fs::write(right.join("nested/empty"), b"").unwrap();
        std::fs::write(left.join("binary"), b"\0old").unwrap();
        std::fs::write(right.join("binary"), b"\0new").unwrap();
        let mut files = BTreeSet::new();
        collect_files(&left, &left, &mut files).unwrap();
        collect_files(&right, &right, &mut files).unwrap();
        assert_eq!(files.len(), 2);
        let renderer = renderer(Layout::Inline, false);
        let mut output = Vec::new();
        for file in files {
            renderer
                .pair(&left.join(&file), &right.join(&file), &file, &mut output)
                .unwrap();
        }
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Binary files differ"));
        assert!(output.contains("Added empty file"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/must-not-be-read", right.join("link")).unwrap();
            let mut output = Vec::new();
            renderer
                .pair(
                    &left.join("link"),
                    &right.join("link"),
                    Path::new("link"),
                    &mut output,
                )
                .unwrap();
            assert!(String::from_utf8(output)
                .unwrap()
                .contains("Symlink/type change"));
        }
    }

    #[test]
    fn no_newline_and_missing_input_are_explicit_and_unchanged_files_are_silent() {
        assert!(render_pair("old", "new", Layout::Inline, false).contains("No newline"));
        assert!(render_pair("same\n", "same\n", Layout::Inline, false).is_empty());
        let directory = tempfile::tempdir().unwrap();
        assert!(renderer(Layout::Inline, false)
            .pair(
                &directory.path().join("missing1"),
                &directory.path().join("missing2"),
                Path::new("missing"),
                &mut Vec::new()
            )
            .is_err());
    }
}
