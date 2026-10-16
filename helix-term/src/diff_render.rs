//! Read-only, flow-text frontend for standalone use and external diff hosts.
//! It emits text/SGR only: no terminal acquisition, cursor motion or alternate screen.
use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, BTreeSet, VecDeque},
    hash::{Hash, Hasher},
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
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
    profile: Option<RefCell<Vec<FileRenderProfile>>>,
    documents: Rc<RefCell<DocumentCache>>,
}

struct CachedDocument {
    digest: u64,
    language: Option<String>,
    bytes: Vec<u8>,
    document: Rc<Document>,
}

/// Bounds retained source bytes and entry count; syntax trees add memory on top.
/// Documents are read-only and their old snapshot paths are never used to read
/// inputs or label output. A content/language key survives jj's temporary paths.
struct DocumentCache {
    entries: VecDeque<CachedDocument>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for DocumentCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_entries: 64,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

impl DocumentCache {
    fn get(&mut self, digest: u64, language: Option<&str>, bytes: &[u8]) -> Option<Rc<Document>> {
        let index = self.entries.iter().position(|entry| {
            entry.digest == digest && entry.language.as_deref() == language && entry.bytes == bytes
        })?;
        let entry = self.entries.remove(index).unwrap();
        let document = entry.document.clone();
        self.entries.push_back(entry);
        Some(document)
    }

    fn insert(&mut self, digest: u64, bytes: &[u8], document: Rc<Document>) {
        if self.max_entries == 0
            || bytes.len() > self.max_bytes
            || (document.language_config().is_some() && document.syntax().is_none())
        {
            return;
        }
        while self.entries.len() >= self.max_entries || self.bytes + bytes.len() > self.max_bytes {
            let oldest = self.entries.pop_front().unwrap();
            self.bytes -= oldest.bytes.len();
        }
        self.bytes += bytes.len();
        self.entries.push_back(CachedDocument {
            digest,
            language: document.language_name().map(str::to_owned),
            bytes: bytes.to_vec(),
            document,
        });
    }
}

/// Timings for one changed, non-binary file pair. Document time includes decoding
/// and Tree-sitter parsing; render time includes row formatting and output writes.
#[derive(Debug, Default)]
pub struct FileRenderProfile {
    pub path: PathBuf,
    pub read: Duration,
    pub document: Duration,
    pub prepare: Duration,
    pub syntax: Duration,
    pub cache_hits: usize,
    pub diff: Duration,
    pub render: Duration,
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    doc: &'a Document,
    index: usize,
    numbers: [Option<usize>; 2],
    removed: Option<bool>,
    change: Option<DiffChange>,
    words: &'a [Range<usize>],
}

pub fn run(args: &Args, output: &mut impl Write, stdout_is_terminal: bool) -> Result<()> {
    RenderSession::new(args)?.render(args, output, stdout_is_terminal)
}

/// Configuration and syntax resources shared by sequential flow-render requests.
///
/// A session is a snapshot of configuration in the current workspace. Callers must
/// recreate it when configuration/trust changes, and must not reuse it across
/// workspaces. Source files are deliberately reread on every request; unchanged
/// content with the same language reuses a bounded cache of read-only documents. This is not
/// a concurrent renderer: requests share the language loader's theme scopes.
pub struct RenderSession {
    themes: Arc<theme::Loader>,
    theme: Theme,
    icons: helix_view::icons::Icons,
    loader: Arc<ArcSwap<syntax::Loader>>,
    config: Arc<ArcSwap<helix_view::editor::Config>>,
    documents: Rc<RefCell<DocumentCache>>,
}

impl RenderSession {
    /// Reset retained documents and set bounds on entry count and source bytes.
    /// Trees and document allocations consume memory beyond the source budget.
    /// A zero entry count disables document caching.
    pub fn set_document_cache_limits(&mut self, max_entries: usize, max_bytes: usize) {
        *self.documents.borrow_mut() = DocumentCache {
            max_entries,
            max_bytes,
            ..Default::default()
        };
    }

    /// Number of retained documents and retained source bytes (not total RAM).
    pub fn document_cache_usage(&self) -> (usize, usize) {
        let cache = self.documents.borrow();
        (cache.entries.len(), cache.bytes)
    }

    pub fn new(args: &Args) -> Result<Self> {
        let config = Config::load_default().map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut dirs = vec![helix_loader::config_dir()];
        dirs.extend(helix_loader::runtime_dirs().iter().cloned());
        #[allow(unused_mut)]
        let mut themes = Arc::new(theme::Loader::new(&dirs));
        let name = config.theme.as_ref().map(|theme| theme.choose(None));
        #[allow(unused_mut)]
        let mut theme = match name {
            Some(name) => themes.load(name)?,
            None => themes.default_theme(true),
        };
        let trust = helix_loader::workspace_trust::WorkspaceTrust::new(
            (&config.editor.workspace_trust).into(),
        );
        let loader = helix_core::config::user_lang_loader(&trust)?;
        loader.set_scopes(theme.scopes().to_vec());
        let icons_loader = helix_view::icons::Loader::new(&dirs);
        let mut icons = match config.icons.as_deref() {
            Some(name) => icons_loader.load(name, &theme, true)?,
            None => icons_loader.default(&theme),
        };
        let loader = Arc::new(ArcSwap::from_pointee(loader));
        let config = Arc::new(ArcSwap::from_pointee(config));
        #[cfg(feature = "steel")]
        if config.load().editor.enable_steel {
            use arc_swap::access::Map;
            let mut headless = (*config.load_full()).clone();
            headless.editor.file_watcher.enable = false;
            headless.editor.lsp.enable = false;
            config.store(Arc::new(headless));
            let handlers = crate::handlers::setup(config.clone());
            let mut editor = helix_view::Editor::new(
                Rect::new(0, 0, args.diff_width.unwrap_or(100), 24),
                themes.clone(),
                loader.clone(),
                Arc::new(Map::new(config.clone(), |config: &Config| &config.editor)),
                handlers,
                trust,
            );
            editor.pager = true;
            editor.set_theme(theme)?;
            editor.set_icons(icons);
            let mut jobs = crate::job::Jobs::new();
            let mut cx = crate::commands::Context {
                register: None,
                count: None,
                editor: &mut editor,
                callback: Vec::new(),
                on_next_key_callback: None,
                jobs: &mut jobs,
            };
            crate::commands::ScriptingEngine::load_headless_configuration(
                &mut cx,
                config.clone(),
                loader.clone(),
            )?;
            themes = editor.theme_loader.clone();
            theme = editor.theme.clone();
            icons = editor.icons.clone();
        }
        Ok(Self {
            themes,
            theme,
            icons,
            loader,
            config: Arc::new(ArcSwap::from_pointee(config.load().editor.clone())),
            documents: Rc::default(),
        })
    }

    pub fn render(
        &mut self,
        args: &Args,
        output: &mut impl Write,
        stdout_is_terminal: bool,
    ) -> Result<()> {
        self.render_inner(args, output, stdout_is_terminal, false)
            .map(|_| ())
    }

    /// Instrument changed text files without altering the rendered bytes.
    pub fn render_profiled(
        &mut self,
        args: &Args,
        output: &mut impl Write,
        stdout_is_terminal: bool,
    ) -> Result<Vec<FileRenderProfile>> {
        self.render_inner(args, output, stdout_is_terminal, true)
    }

    fn render_inner(
        &mut self,
        args: &Args,
        output: &mut impl Write,
        stdout_is_terminal: bool,
        profile: bool,
    ) -> Result<Vec<FileRenderProfile>> {
        let mut theme = self.theme.clone();
        let mut icons = self.icons.clone();
        // Apply per request, including themes dynamically registered by Steel.
        if let Some(name) = args.diff_theme.as_deref() {
            theme = self.themes.load(name)?;
            icons.set_diagnostic_icons_base_style(&theme);
            icons.set_symbolkind_icons_base_style(&theme);
        }
        self.loader.load().set_scopes(theme.scopes().to_vec());
        let renderer = Renderer {
            theme,
            icons,
            loader: self.loader.clone(),
            config: self.config.clone(),
            width: args.diff_width.unwrap_or(100),
            context: args.diff_context.unwrap_or(3),
            layout: args.diff_layout.unwrap_or_default(),
            color: match args.diff_color.unwrap_or_default() {
                ColorMode::Always => true,
                ColorMode::Never => false,
                ColorMode::Auto => stdout_is_terminal,
            },
            profile: profile.then(RefCell::default),
            documents: self.documents.clone(),
        };
        if args.patch {
            anyhow::ensure!(!profile, "profiling requires file or directory inputs");
            renderer.patch(&crate::diff_patch::read(args)?, output)?;
            return Ok(Vec::new());
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
                // Worker writers use flush as a cancellation checkpoint. Ordinary
                // buffered writers also get a natural per-file flush boundary.
                output.flush()?;
                renderer.pair(&left.join(&file), &right.join(&file), &file, output)?;
            }
        } else {
            anyhow::ensure!(
                !left.is_dir() && !right.is_dir(),
                "compare two files or two directories, not a file and directory"
            );
            renderer.pair(left, right, right, output)?;
        }
        Ok(renderer
            .profile
            .map(RefCell::into_inner)
            .unwrap_or_default())
    }
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
            for row in 0..groups[0].len().max(groups[1].len()) {
                let sources = [0, 1].map(|side| {
                    groups[side].get(row).map(|line| SourceLine {
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
                    })
                });
                self.emit_sources(sources, output)?;
            }
        }
        Ok(())
    }
    fn document(&self, bytes: &[u8], path: &Path) -> Result<Rc<Document>> {
        self.document_profiled(bytes, path, None)
    }

    fn document_profiled(
        &self,
        bytes: &[u8],
        path: &Path,
        mut profile: Option<&mut FileRenderProfile>,
    ) -> Result<Rc<Document>> {
        let mut started = profile.as_ref().map(|_| Instant::now());
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
        if let (Some(profile), Some(started)) = (&mut profile, &mut started) {
            profile.prepare += started.elapsed();
            *started = Instant::now();
        }
        let language = doc.detect_language_config(&self.loader.load());
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let digest = hasher.finish();
        if let Some(cached) = self.documents.borrow_mut().get(
            digest,
            language
                .as_ref()
                .map(|language| language.language_id.as_str()),
            bytes,
        ) {
            if let (Some(profile), Some(started)) = (profile, started) {
                profile.syntax += started.elapsed();
                profile.cache_hits += 1;
            }
            return Ok(cached);
        }
        doc.set_language(language, &self.loader.load());
        if let (Some(profile), Some(started)) = (profile, started) {
            profile.syntax += started.elapsed();
        }
        let doc = Rc::new(doc);
        self.documents
            .borrow_mut()
            .insert(digest, bytes, doc.clone());
        Ok(doc)
    }

    fn pair(&self, left: &Path, right: &Path, name: &Path, output: &mut impl Write) -> Result<()> {
        let mut started = self.profile.as_ref().map(|_| Instant::now());
        let mut profile = FileRenderProfile::default();
        let before = read_input(left)?;
        let after = read_input(right)?;
        if let Some(started) = started.as_mut() {
            profile.read = started.elapsed();
        }
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
        if let Some(started) = started.as_mut() {
            *started = Instant::now();
        }
        let mut document = |bytes: &[u8], path: &Path| {
            if self.profile.is_some() {
                self.document_profiled(bytes, path, Some(&mut profile))
            } else {
                self.document(bytes, path)
            }
        };
        let before_doc = document(&before.bytes, left)?;
        output.flush()?;
        let after_doc = document(&after.bytes, right)?;
        output.flush()?;
        let owned_docs = [before_doc, after_doc];
        let docs = owned_docs.each_ref().map(|doc| doc.as_ref());
        if let Some(started) = started.as_mut() {
            profile.document = started.elapsed();
            *started = Instant::now();
        }
        let texts = docs.each_ref().map(|doc| doc.text().clone());
        let layout = DiffLayout::compute(&texts, [0, 0]);
        if let Some(started) = started.as_mut() {
            profile.diff = started.elapsed();
            *started = Instant::now();
        }
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
        if let (Some(profiles), Some(started)) = (&self.profile, started) {
            profile.render = started.elapsed();
            profile.path = name.to_owned();
            profiles.borrow_mut().push(profile);
        }
        Ok(())
    }

    fn side_by_side(
        &self,
        docs: &[&Document; 2],
        layout: &DiffLayout,
        rows: Range<usize>,
        output: &mut impl Write,
    ) -> Result<()> {
        for row in rows {
            let sources = [0, 1].map(|side| {
                let line = line_at(&layout.sides[side].rows, row)
                    .filter(|&line| line < real_lines(docs[side].text()));
                line.map(|line| SourceLine {
                    doc: docs[side],
                    words: &layout.sides[side].words,
                    index: line,
                    numbers: [Some(line + 1), None],
                    removed: layout.sides[side].is_changed(line).then_some(side == 0),
                    change: layout.sides[side].gutter_at(line, true),
                })
            });
            self.emit_sources(sources, output)?;
        }
        Ok(())
    }

    fn inline(
        &self,
        docs: &[&Document; 2],
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
                        let mut numbers = [None; 2];
                        numbers[side] = Some(line + 1);
                        self.emit_sources(
                            [
                                Some(SourceLine {
                                    doc: docs[side],
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
                                }),
                                None,
                            ],
                            output,
                        )?;
                    }
                }
            } else if let Some(line) = lines[1] {
                self.emit_sources(
                    [
                        Some(SourceLine {
                            doc: docs[1],
                            words: &[],
                            index: line,
                            numbers: lines.map(|line| line.map(|line| line + 1)),
                            removed: None,
                            change: None,
                        }),
                        None,
                    ],
                    output,
                )?;
            }
        }
        Ok(())
    }

    fn number(&self, numbers: [Option<usize>; 2]) -> String {
        let numbers =
            numbers.map(|number| number.map(|number| number.to_string()).unwrap_or_default());
        match self.layout {
            Layout::SideBySide => format!("{:>4} ", numbers[0]),
            Layout::Inline => format!("{:>4} {:>4} ", numbers[0], numbers[1]),
        }
    }

    fn line_format(
        &self,
        source: SourceLine<'_>,
        width: u16,
    ) -> (u16, helix_core::doc_formatter::TextFormat) {
        // Even narrow split panes need space for a double-width grapheme.
        let prefix = (self.number(source.numbers).len() as u16 + 2).min(width.saturating_sub(2));
        let mut format = source.doc.text_format(width - prefix, Some(&self.theme));
        format.soft_wrap = true;
        // Flow output uses blank continuation gutters, not inserted source text.
        format.wrap_indicator = "".into();
        (prefix, format)
    }

    fn emit_sources(
        &self,
        sources: [Option<SourceLine<'_>>; 2],
        output: &mut impl Write,
    ) -> Result<()> {
        let widths = if self.layout == Layout::Inline {
            [self.width, 0]
        } else {
            [(self.width - 3) / 2, self.width - 3 - (self.width - 3) / 2]
        };
        let mut heights = [0; 2];
        for side in 0..2 {
            if let Some(source) = sources[side] {
                let (_, format) = self.line_format(source, widths[side]);
                let text = source.doc.text().slice(..);
                let anchor = text.line_to_char(source.index);
                let end = helix_core::line_ending::line_end_char_index(&text, source.index);
                heights[side] = helix_core::visual_offset_from_block(
                    text,
                    anchor,
                    end,
                    &format,
                    &TextAnnotations::default(),
                )
                .0
                .row + 1;
                anyhow::ensure!(
                    heights[side] <= u16::MAX as usize,
                    "single source line exceeds 65535 wrapped flow rows"
                );
            }
        }
        let height = heights[0].max(heights[1]);
        // Bound temporary cell storage even for very long logical lines.
        for offset in (0..height).step_by(128) {
            let chunk = (height - offset).min(128) as u16;
            let mut buffer = Buffer::empty(Rect::new(0, 0, self.width, chunk));
            for side in 0..2 {
                if let Some(source) = sources[side].filter(|_| offset < heights[side]) {
                    let x = if side == 0 { 0 } else { widths[0] + 3 };
                    let area = Rect::new(
                        x,
                        0,
                        widths[side],
                        chunk.min((heights[side] - offset) as u16),
                    );
                    self.line(&mut buffer, area, source, offset);
                }
            }
            if self.layout == Layout::SideBySide {
                for y in 0..chunk {
                    buffer.set_string(widths[0] + 1, y, "│", self.theme.get("ui.window"));
                }
            }
            emit(&buffer, self.color, output)?;
        }
        Ok(())
    }

    fn line(&self, buffer: &mut Buffer, area: Rect, source: SourceLine<'_>, offset: usize) {
        let (prefix, format) = self.line_format(source, area.width);
        let SourceLine {
            doc,
            index: line,
            numbers,
            removed,
            change,
            words,
        } = source;
        let number = if offset == 0 {
            self.number(numbers)
        } else {
            String::new()
        };
        buffer.set_stringn(
            area.x,
            0,
            &number,
            prefix.saturating_sub(2) as usize,
            self.theme.get("ui.linenr"),
        );
        if let Some(change) = change.filter(|_| offset == 0) {
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
        render_document(
            buffer,
            viewport,
            doc,
            ViewPosition {
                anchor,
                vertical_offset: offset,
                ..ViewPosition::default()
            },
            &TextAnnotations::default(),
            highlighter,
            Vec::new(),
            &self.theme,
            decorations,
            Some(format),
        );
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
    for y in 0..buffer.area.height {
        let mut previous = None;
        let mut x = 0;
        while x < buffer.area.width {
            let cell = &buffer[(x, y)];
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_core::unicode::width::UnicodeWidthStr;

    #[test]
    fn parsed_cache_uses_contents_and_language_not_snapshot_path() -> Result<()> {
        let renderer = renderer(Layout::Inline, true);
        let first = renderer.document(b"fn example() {}\n", Path::new("old/snapshot.rs"))?;
        let relocated = renderer.document(b"fn example() {}\n", Path::new("new/snapshot.rs"))?;
        assert!(Rc::ptr_eq(&first, &relocated));
        let plaintext = renderer.document(b"fn example() {}\n", Path::new("snapshot.txt"))?;
        assert!(!Rc::ptr_eq(&first, &plaintext));
        assert_eq!(plaintext.language_name(), None);
        let changed = renderer.document(b"fn changed() {}\n", Path::new("old/snapshot.rs"))?;
        assert!(!Rc::ptr_eq(&first, &changed));
        Ok(())
    }

    #[test]
    fn parsed_cache_verifies_collisions_and_bounds_lru_source_storage() -> Result<()> {
        let renderer = renderer(Layout::Inline, false);
        let a = renderer.document(b"aa", Path::new("a.txt"))?;
        let b = renderer.document(b"bb", Path::new("b.txt"))?;
        let c = renderer.document(b"cc", Path::new("c.txt"))?;
        let mut cache = DocumentCache {
            max_entries: 2,
            max_bytes: 4,
            ..Default::default()
        };
        cache.insert(7, b"aa", a);
        assert!(
            cache.get(7, None, b"bb").is_none(),
            "a hash alone is not a valid cache key"
        );
        assert!(cache.get(7, Some("rust"), b"aa").is_none());
        cache.insert(7, b"bb", b);
        assert!(cache.get(7, None, b"aa").is_some());
        cache.insert(8, b"cc", c);
        assert!(
            cache.get(7, None, b"bb").is_none(),
            "least recently used entry must be evicted"
        );
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.bytes, 4);
        cache.max_bytes = 1;
        cache.insert(
            9,
            b"oversized",
            renderer.document(b"oversized", Path::new("large.txt"))?,
        );
        assert!(cache.get(9, None, b"oversized").is_none());
        // The byte budget must also evict before the entry-count limit is hit.
        cache.entries.clear();
        cache.bytes = 0;
        cache.max_entries = 10;
        cache.max_bytes = 3;
        cache.insert(1, b"aa", renderer.document(b"aa", Path::new("a.txt"))?);
        cache.insert(2, b"bb", renderer.document(b"bb", Path::new("b.txt"))?);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.bytes, 2);
        Ok(())
    }

    #[test]
    fn session_rereads_files_and_keeps_request_options_independent() -> Result<()> {
        let root = tempfile::tempdir()?;
        let left = root.path().join("left.rs");
        let right = root.path().join("right.rs");
        std::fs::write(&left, "fn before() {}\n")?;
        std::fs::write(&right, "fn after() {}\n")?;
        let initial = renderer(Layout::Inline, true);
        let mut session = RenderSession {
            themes: Arc::new(theme::Loader::new(helix_loader::runtime_dirs())),
            theme: initial.theme,
            icons: initial.icons,
            loader: initial.loader,
            config: initial.config,
            documents: Rc::default(),
        };
        let mut args = Args {
            diff_layout: Some(Layout::Inline),
            diff_color: Some(ColorMode::Always),
            ..Args::default()
        };
        args.files.insert(left, Vec::new());
        args.files.insert(right.clone(), Vec::new());
        let mut first = Vec::new();
        session.render(&args, &mut first, false)?;
        args.diff_theme = Some("nonexistent-session-test-theme".into());
        assert!(session.render(&args, &mut Vec::new(), false).is_err());
        args.diff_theme = Some("base16_default_dark".into());
        let mut themed_cached = Vec::new();
        session.render(&args, &mut themed_cached, false)?;
        session.set_document_cache_limits(0, 0);
        let mut themed_fresh = Vec::new();
        session.render(&args, &mut themed_fresh, false)?;
        assert_eq!(
            themed_cached, themed_fresh,
            "cached syntax must follow a new theme"
        );
        assert_eq!(session.document_cache_usage(), (0, 0));
        session.set_document_cache_limits(64, 16 * 1024 * 1024);
        args.diff_theme = None;
        let mut repeated = Vec::new();
        let profile = session.render_profiled(&args, &mut repeated, false)?;
        assert_eq!(profile.len(), 1);
        assert_eq!(profile[0].path, right);
        assert_eq!(
            first, repeated,
            "theme overrides must not leak into later requests"
        );
        args.diff_layout = Some(Layout::SideBySide);
        args.diff_color = Some(ColorMode::Never);
        args.diff_width = Some(80);
        std::fs::write(right, "fn changed_again() {}\n")?;
        let mut changed = Vec::new();
        session.render(&args, &mut changed, false)?;
        let changed = String::from_utf8(changed)?;
        assert!(changed.contains("changed_again"));
        assert!(changed.contains('│'));
        assert!(!changed.contains('\x1b'));
        assert!(!changed.contains("fn after"));
        Ok(())
    }

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
            profile: None,
            documents: Rc::default(),
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
    fn context_is_bounded_and_wide_lines_wrap_without_losing_source() {
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
        assert!(!output.contains('…'));
        assert_eq!(output.matches('界').count(), 100);
        for line in output
            .lines()
            .filter(|line| !line.starts_with("diff ") && !line.starts_with("@@"))
        {
            assert!(line.width() <= 100, "{line}");
        }
    }

    #[test]
    fn wrapped_rows_preserve_graphemes_gutters_and_split_alignment_across_chunks() {
        for layout in [Layout::Inline, Layout::SideBySide] {
            let output = render_pair(
                "short old\nfollowing_context_line\n",
                &format!(
                    "{}{}\nfollowing_context_line\n",
                    "界".repeat(6000),
                    "e\u{301}".repeat(30)
                ),
                layout,
                false,
            );
            assert_eq!(output.matches('界').count(), 6000);
            assert_eq!(output.matches("e\u{301}").count(), 30);
            assert!(output.contains("following_context_line"));
            let body = output
                .lines()
                .filter(|line| !line.starts_with("diff ") && !line.starts_with("@@"));
            for row in body {
                assert!(row.width() <= 100, "{row}");
                if layout == Layout::SideBySide {
                    let (left, _) = row.split_once('│').unwrap();
                    assert_eq!(left.width(), 49);
                    if row.contains('界') && !row.contains("short old") {
                        assert!(left.trim().is_empty(), "short side must be padded: {row}");
                    }
                }
            }
            // A logical line number appears only on its first visual row.
            let gutter = if layout == Layout::Inline { 12 } else { 7 };
            let numbered = output
                .lines()
                .filter(|line| line.contains('界') && line.chars().take(gutter).any(|c| c == '1'))
                .count();
            assert_eq!(numbered, 1);
        }
    }

    #[test]
    fn narrow_flow_and_patch_views_wrap_wide_text_and_keep_continuation_colors() {
        for layout in [Layout::Inline, Layout::SideBySide] {
            let mut renderer = renderer(layout, false);
            renderer.width = 20;
            let patch = crate::diff_patch::parse(&format!(
                "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-old\n+{}\n",
                "界".repeat(40)
            ))
            .unwrap();
            let mut output = Vec::new();
            renderer.patch(&patch, &mut output).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert_eq!(output.matches('界').count(), 40, "{output}");

            renderer.width = 100;
            renderer.color = true;
            let doc = renderer
                .document(
                    format!("fn {}() {{}}\n", "x".repeat(300)).as_bytes(),
                    Path::new("x.rs"),
                )
                .unwrap();
            let word = 3..303;
            let words = std::slice::from_ref(&word);
            let source = SourceLine {
                doc: &doc,
                index: 0,
                numbers: [Some(1), None],
                removed: Some(false),
                change: Some(DiffChange::Added),
                words,
            };
            let mut output = Vec::new();
            renderer
                .emit_sources([Some(source), None], &mut output)
                .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert_eq!(output.matches('x').count(), 300);
            for row in output.lines().filter(|line| line.contains('x')) {
                assert!(
                    row.contains("48;2;24;72;32"),
                    "word background lost on continuation: {row:?}"
                );
            }
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
