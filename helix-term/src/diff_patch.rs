//! Read-only Git unified patches and Jujutsu's native color-words output.
//! Coordinates label fragments; they never cause allocation of omitted lines.
use anyhow::{Context, Result};
use helix_core::regex::Regex;
use std::{path::PathBuf, sync::OnceLock};

use arc_swap::{access::DynAccess, ArcSwap};
use helix_core::{syntax, Rope};
use helix_view::{
    document::DocumentAccess,
    inline_diff::{InlineDiff, InlineLine},
    Document,
};
use std::sync::Arc;

/// Build immutable, pathless presentation documents. Header paths are used
/// only to select grammars for individual fragments, never opened or written.
pub fn document(
    patch: &Patch,
    side: Option<usize>,
    config: Arc<dyn DynAccess<helix_view::editor::Config>>,
    loader: Arc<ArcSwap<syntax::Loader>>,
) -> Document {
    let mut model = InlineDiff {
        sources: Vec::new(),
        lines: Vec::new(),
        hunks: Vec::new(),
        max_number: 1,
    };
    let mut presentation = String::new();
    let mut changed = false;
    for section in &patch.sections {
        let base = model.sources.len();
        let mut texts = [String::new(), String::new()];
        let mut indexes = [0; 2];
        for row in &section.rows {
            let metadata = row.numbers == [None; 2];
            let included = [0, 1].map(|s| metadata || row.numbers[s].is_some());
            let source = side.unwrap_or(usize::from(row.removed != Some(true)));
            if side.is_none() || included[source] {
                if row.removed.is_some() && !changed {
                    model.hunks.push(model.lines.len());
                }
                changed = row.removed.is_some();
                model.max_number = model
                    .max_number
                    .max(row.numbers.into_iter().flatten().max().unwrap_or(1));
                model.lines.push(InlineLine {
                    source: base + source,
                    line: indexes[source],
                    numbers: row.numbers,
                    removed: row.removed,
                });
                presentation.push_str(&row.text);
                presentation.push('\n');
            }
            for s in 0..2 {
                if included[s] {
                    texts[s].push_str(&row.text);
                    texts[s].push('\n');
                    indexes[s] += 1;
                }
            }
        }
        for text in texts {
            let mut doc = Document::from(Rope::from(text), None, config.clone(), loader.clone());
            doc.access = DocumentAccess::ReadOnly;
            if let Some(path) = &section.path {
                let loader = ArcSwap::load(&loader);
                let language = loader
                    .language_for_filename(path)
                    .or_else(|| loader.language_for_shebang(doc.text().slice(..)))
                    .map(|id| loader.language(id).config().clone());
                doc.set_language(language, &loader);
            }
            model.sources.push(doc);
        }
        changed = false;
    }
    let mut doc = Document::from(Rope::from(presentation), None, config, loader);
    doc.access = DocumentAccess::ReadOnly;
    doc.name = Some(
        match side {
            Some(0) => "Patch: before",
            Some(_) => "Patch: after",
            None => "Patch: inline",
        }
        .into(),
    );
    doc.inline_diff = Some(Box::new(model));
    doc
}

pub fn read(args: &crate::args::Args) -> Result<Patch> {
    use std::io::{IsTerminal, Read};
    let bytes = if let Some(path) = args.files.keys().next() {
        std::fs::read(path)?
    } else {
        anyhow::ensure!(
            !std::io::stdin().is_terminal(),
            "patch input requires piped stdin or a patch file"
        );
        let mut bytes = Vec::new();
        std::io::stdin().lock().read_to_end(&mut bytes)?;
        bytes
    };
    parse(std::str::from_utf8(&bytes).context("patch input must be UTF-8")?)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Patch {
    pub sections: Vec<Section>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Section {
    pub path: Option<PathBuf>,
    pub rows: Vec<Row>,
    native_last: [Option<(usize, usize)>; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub numbers: [Option<usize>; 2],
    pub removed: Option<bool>,
}

impl Patch {
    /// Preserve file headers and notices with their file, without consulting
    /// any of the paths named by untrusted patch input.
    pub fn files(&self) -> Vec<(PathBuf, Patch)> {
        let mut files: Vec<(PathBuf, Patch)> = Vec::new();
        for section in &self.sections {
            if let Some(path) = &section.path {
                if files.is_empty() {
                    files.push((path.clone(), Patch::default()));
                }
                let file = files.last_mut().unwrap();
                file.0 = path.clone();
                file.1.sections.push(section.clone());
                continue;
            }
            for row in &section.rows {
                let header = row.text.starts_with("diff --git ");
                let native = native_header(&row.text);
                let next_unified_file = row.text.starts_with("--- ")
                    && files.last().is_some_and(|(_, patch)| {
                        patch.sections.iter().any(|section| section.path.is_some())
                    });
                if header || native.is_some() || files.is_empty() || next_unified_file {
                    let name = native
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(format!("change-{}", files.len() + 1)));
                    files.push((name, Patch::default()));
                }
                let file = files.last_mut().unwrap();
                if let Some(path) = row
                    .text
                    .strip_prefix("+++ ")
                    .or_else(|| row.text.strip_prefix("--- "))
                {
                    if let Ok(Some(path)) = git_path(path) {
                        file.0 = path;
                    }
                }
                let mut metadata = section.clone();
                metadata.rows = vec![row.clone()];
                file.1.sections.push(metadata);
            }
        }
        files
    }
    fn metadata(&mut self, text: String) {
        if self
            .sections
            .last()
            .is_none_or(|section| section.path.is_some())
        {
            self.sections.push(Section::default());
        }
        self.sections.last_mut().unwrap().rows.push(Row {
            text,
            numbers: [None; 2],
            removed: None,
        });
    }
    fn fragment(&mut self, path: Option<PathBuf>) {
        self.sections.push(Section {
            path,
            rows: Vec::new(),
            native_last: [None; 2],
        });
    }
    fn row(&mut self, text: String, numbers: [Option<usize>; 2], removed: Option<bool>) {
        self.sections.last_mut().unwrap().rows.push(Row {
            text,
            numbers,
            removed,
        });
    }

    fn native_row(&mut self, text: String, number: usize, side: usize) {
        let section = self.sections.last_mut().unwrap();
        let rows = &mut section.rows;
        // Color-words can wrap one logical old line across several new lines
        // (and vice versa), repeating that side's coordinate. Join fragments
        // by coordinate rather than inventing extra source lines.
        if let Some((_, index)) =
            section.native_last[side].filter(|&(previous, _)| previous == number)
        {
            rows[index].text.push_str(&text);
        } else {
            let mut numbers = [None; 2];
            numbers[side] = Some(number);
            rows.push(Row {
                text,
                numbers,
                removed: Some(side == 0),
            });
            section.native_last[side] = Some((number, rows.len() - 1));
        }
    }
}

pub fn parse(input: &str) -> Result<Patch> {
    static HUNK: OnceLock<Regex> = OnceLock::new();
    let hunk =
        HUNK.get_or_init(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap());
    let mut patch = Patch::default();
    let mut path = None;
    let mut before_path = None;
    let mut coords = [0usize; 2];
    let mut remaining = [0usize; 2];
    let mut native = false;
    let mut recognized = input.trim().is_empty();
    let mut last_numbers: Option<[Option<usize>; 2]> = None;
    for (index, raw) in input.lines().enumerate() {
        let (line, marks) = decode_sgr(raw);
        if remaining != [0, 0] {
            if line.starts_with("\\ No newline") {
                patch.metadata(line);
                patch.fragment(path.clone());
                continue;
            }
            let marker = line.as_bytes().first().copied();
            let sides = match marker {
                Some(b' ') => [true, true],
                Some(b'-') => [true, false],
                Some(b'+') => [false, true],
                _ => anyhow::bail!("invalid/truncated unified hunk at input line {}", index + 1),
            };
            let mut numbers = [None; 2];
            for side in 0..2 {
                if sides[side] {
                    anyhow::ensure!(
                        remaining[side] != 0,
                        "hunk count exceeded at input line {}",
                        index + 1
                    );
                    numbers[side] = Some(coords[side]);
                    coords[side] = coords[side]
                        .checked_add(1)
                        .context("line coordinate overflow")?;
                    remaining[side] -= 1;
                }
            }
            patch.row(
                line[1..].into(),
                numbers,
                match marker {
                    Some(b'-') => Some(true),
                    Some(b'+') => Some(false),
                    _ => None,
                },
            );
            continue;
        }
        if line.starts_with("diff --git ") {
            recognized = true;
            native = false;
            path = None;
            before_path = None;
            patch.metadata(line);
        } else if let Some(header) = line.strip_prefix("--- ") {
            recognized = true;
            before_path = git_path(header)?;
            path = before_path.clone();
            patch.metadata(line);
        } else if let Some(header) = line.strip_prefix("+++ ") {
            path = git_path(header)?.or_else(|| before_path.clone());
            patch.metadata(line);
        } else if let Some(captures) = hunk.captures(&line) {
            recognized = true;
            for side in 0..2 {
                coords[side] = captures[1 + 2 * side].parse()?;
                remaining[side] = captures
                    .get(2 + 2 * side)
                    .map_or(Ok(1), |n| n.as_str().parse())?;
                anyhow::ensure!(
                    coords[side] > 0 || remaining[side] == 0,
                    "nonempty hunk starts at line zero"
                );
            }
            patch.metadata(line);
            patch.fragment(path.clone());
        } else if line.starts_with("@@@")
            || line.starts_with("diff --cc ")
            || line.starts_with("diff --combined ")
        {
            anyhow::bail!("combined merge patches are ambiguous; use git diff --diff-merges=separate or jj diff --git");
        } else if line.starts_with("@@") {
            anyhow::bail!("malformed unified hunk header at input line {}", index + 1);
        } else if let Some(name) = native_header(&line) {
            recognized = true;
            native = true;
            path = Some(PathBuf::from(name));
            last_numbers = None;
            patch.metadata(line);
            patch.fragment(path.clone());
        } else if native {
            if let Some((numbers, offset)) = native_numbers(&line) {
                // Discontinuous native coordinates start a fresh syntax fragment.
                if last_numbers.is_some_and(|last| (0..2).any(|side| matches!((last[side], numbers[side]), (Some(a), Some(b)) if b > a.saturating_add(1)))) {
                    patch.metadata("… omitted context …".into());
                    patch.fragment(path.clone());
                }
                let body = &line[offset..];
                let kinds = &marks[offset..];
                match numbers {
                    [Some(old), Some(new)] if kinds.iter().any(|&kind| kind != 0) => {
                        let mut texts = [String::new(), String::new()];
                        for (byte, character) in body.char_indices() {
                            if kinds[byte] != 1 {
                                texts[0].push(character);
                            }
                            if kinds[byte] != -1 {
                                texts[1].push(character);
                            }
                        }
                        patch.native_row(texts[0].clone(), old, 0);
                        patch.native_row(texts[1].clone(), new, 1);
                    }
                    [Some(old), None] => patch.native_row(body.into(), old, 0),
                    [None, Some(new)] => patch.native_row(body.into(), new, 1),
                    _ => patch.row(body.into(), numbers, None),
                }
                last_numbers = Some(numbers);
            } else {
                patch.metadata(line);
                patch.fragment(path.clone());
                last_numbers = None;
            }
        } else {
            patch.metadata(line);
        }
    }
    anyhow::ensure!(
        remaining == [0, 0],
        "truncated unified patch: missing hunk lines"
    );
    anyhow::ensure!(
        recognized,
        "input is not a Git unified patch or native jj diff (use jj diff --color-words or --git)"
    );
    for section in &mut patch.sections {
        let mut start = 0;
        while start < section.rows.len() {
            if section.rows[start].removed.is_none() {
                start += 1;
                continue;
            }
            let end = start
                + section.rows[start..]
                    .iter()
                    .take_while(|row| row.removed.is_some())
                    .count();
            section.rows[start..end].sort_by_key(|row| row.removed == Some(false));
            start = end;
        }
    }
    Ok(patch)
}

fn native_header(line: &str) -> Option<&str> {
    if !["Modified ", "Added ", "Removed ", "Renamed ", "Copied "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    {
        return None;
    }
    let (_, path) = line
        .split_once(" file ")
        .or_else(|| line.split_once(" symlink "))?;
    path.strip_suffix(':')
}

fn native_numbers(line: &str) -> Option<([Option<usize>; 2], usize)> {
    let colon = line.find(':')?;
    let prefix = &line[..colon];
    if prefix.len() < 3
        || !prefix
            .bytes()
            .all(|byte| byte == b' ' || byte.is_ascii_digit())
    {
        return None;
    }
    let (old, new) = prefix.split_at(prefix.len() / 2);
    let number = |s: &str| {
        if s.trim().is_empty() {
            Some(None)
        } else {
            s.trim().parse::<usize>().ok().filter(|&n| n != 0).map(Some)
        }
    };
    let numbers = [number(old)?, number(new)?];
    if numbers == [None; 2] {
        return None;
    }
    let offset = colon + 1 + usize::from(line.as_bytes().get(colon + 1) == Some(&b' '));
    Some((numbers, offset))
}

fn git_path(raw: &str) -> Result<Option<PathBuf>> {
    let raw = raw.split('\t').next().unwrap();
    let decoded = if raw.starts_with('"') {
        let mut bytes = Vec::new();
        let mut chars = raw.as_bytes()[1..].iter().copied();
        let mut closed = false;
        while let Some(byte) = chars.next() {
            match byte {
                b'"' => {
                    closed = true;
                    break;
                }
                b'\\' => {
                    let escaped = chars.next().context("truncated quoted Git path")?;
                    bytes.push(match escaped {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'b' => 8,
                        b'f' => 12,
                        b'v' => 11,
                        b'a' => 7,
                        b'0'..=b'7' => {
                            let second = chars
                                .next()
                                .filter(|b| matches!(b, b'0'..=b'7'))
                                .context("invalid octal Git path")?;
                            let third = chars
                                .next()
                                .filter(|b| matches!(b, b'0'..=b'7'))
                                .context("invalid octal Git path")?;
                            let value = u16::from(escaped - b'0') * 64
                                + u16::from(second - b'0') * 8
                                + u16::from(third - b'0');
                            u8::try_from(value).context("invalid octal Git path")?
                        }
                        b'\\' | b'"' => escaped,
                        _ => anyhow::bail!("invalid escape in Git path"),
                    });
                }
                _ => bytes.push(byte),
            }
        }
        anyhow::ensure!(closed, "unterminated quoted Git path");
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        raw.to_owned()
    };
    if decoded == "/dev/null" {
        return Ok(None);
    }
    let name = decoded
        .strip_prefix("a/")
        .or_else(|| decoded.strip_prefix("b/"))
        .unwrap_or(&decoded);
    Ok(Some(PathBuf::from(name)))
}

/// Interpret only SGR foregrounds needed for jj's default red/green tokens.
/// Other terminal escapes remain literal data and are never executed.
fn decode_sgr(raw: &str) -> (String, Vec<i8>) {
    let mut text = String::new();
    let mut marks = Vec::new();
    let mut kind = 0;
    let mut pos = 0;
    while pos < raw.len() {
        if raw[pos..].starts_with("\x1b[") {
            let tail = &raw[pos + 2..];
            let count = tail
                .bytes()
                .take_while(|byte| byte.is_ascii_digit() || *byte == b';')
                .count();
            if tail.as_bytes().get(count) == Some(&b'm') {
                let params = if count == 0 {
                    vec![0]
                } else {
                    tail[..count]
                        .split(';')
                        .map(|n| n.parse::<u16>().unwrap_or(0))
                        .collect()
                };
                let mut index = 0;
                while index < params.len() {
                    match params[index] {
                        0 | 39 => kind = 0,
                        31 | 91 => kind = -1,
                        32 | 92 => kind = 1,
                        38 | 48
                            if params.get(index + 1) == Some(&5) && index + 2 < params.len() =>
                        {
                            if params[index] == 38 {
                                kind = match params[index + 2] {
                                    1 | 9 => -1,
                                    2 | 10 => 1,
                                    _ => 0,
                                };
                            }
                            index += 2;
                        }
                        38 | 48
                            if params.get(index + 1) == Some(&2) && index + 4 < params.len() =>
                        {
                            if params[index] == 38 {
                                let [r, g, b] =
                                    [params[index + 2], params[index + 3], params[index + 4]];
                                kind = if r > g && r > b {
                                    -1
                                } else if g > r && g > b {
                                    1
                                } else {
                                    0
                                };
                            }
                            index += 4;
                        }
                        30..=37 | 90..=97 => kind = 0,
                        _ => {}
                    }
                    index += 1;
                }
                pos += count + 3;
                continue;
            }
        }
        let character = raw[pos..].chars().next().unwrap();
        text.push(character);
        marks.extend(std::iter::repeat_n(kind, character.len_utf8()));
        pos += character.len_utf8();
    }
    (text, marks)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unified_coordinates_headers_counts_and_quoted_paths() {
        let patch = parse("diff --git a/x b/x\n--- a/x\n+++ \"b/space name.rs\"\n@@ -100,2 +200,2 @@\n context\n-old\n+new\n\\ No newline at end of file\n").unwrap();
        let section = patch
            .sections
            .iter()
            .find(|section| section.path.is_some())
            .unwrap();
        assert_eq!(
            section.path.as_deref(),
            Some(std::path::Path::new("space name.rs"))
        );
        assert_eq!(section.rows[0].numbers, [Some(100), Some(200)]);
        assert_eq!(section.rows[1].removed, Some(true));
        assert_eq!(section.rows[2].numbers, [None, Some(201)]);
        assert!(parse("--- a/x\n+++ b/x\n@@ -1,2 +1,1 @@\n-old\n+new\n").is_err());
        assert!(parse("@@ -0 +1 @@\n-x\n+y\n").is_err());
        assert!(parse("diff --git a/x b/x\n@@ malformed @@\n").is_err());
        assert_eq!(
            git_path("\"b/\\303\\251.rs\"").unwrap(),
            Some(PathBuf::from("é.rs"))
        );
    }

    #[test]
    fn files_keep_notices_with_their_file_and_do_not_read_paths() {
        let patch = parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n").unwrap();
        let files = patch.files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, PathBuf::from("a.rs"));
        assert_eq!(files[1].0, PathBuf::from("b.rs"));
        assert!(files[0]
            .1
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .any(|row| row.text.starts_with("\\ No newline")));
        assert!(!files[1]
            .1
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .any(|row| row.text.starts_with("\\ No newline")));
        let unified = parse("--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n").unwrap();
        assert_eq!(unified.files().len(), 2);
    }
    #[test]
    fn native_jj_numbers_gaps_and_color_words() {
        let patch = parse("Modified regular file x.rs:\n   1    1: same\n   2     : old\n        2: new\n    ...\n 100  100: fn \x1b[38;5;1mold\x1b[39m\x1b[38;5;2mnew\x1b[39m() {}\n").unwrap();
        let rows = patch
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .filter(|row| row.numbers != [None; 2])
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].numbers, [Some(1), Some(1)]);
        assert_eq!(rows[1].text, "old");
        assert_eq!(rows[2].removed, Some(false));
        assert_eq!(rows[3].text, "fn old() {}");
        assert_eq!(rows[4].text, "fn new() {}");
        assert!(parse("not a patch").is_err());
    }

    #[test]
    fn native_multiline_tokens_join_repeated_source_coordinates() {
        let patch = parse("Modified regular file x.rs:\n 203  203:             \x1b[32mif condition {\x1b[39m\n      204: \x1b[32m    extra;\x1b[39m\n 203  205: \x1b[32m    \x1b[39mold();\n").unwrap();
        let rows = &patch.sections[1].rows;
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].text, "            old();");
        assert_eq!(rows[0].numbers, [Some(203), None]);
        assert_eq!(rows[3].text, "    old();");
        let patch = parse("Modified regular file x:\n   1    1: \x1b[31mold-first\x1b[39m\n   2    1: \x1b[31mold-second\x1b[39m\x1b[32mnew\x1b[39m\n").unwrap();
        assert_eq!(patch.sections[1].rows.last().unwrap().text, "new");
    }
    #[test]
    fn empty_binary_metadata_controls_and_huge_coordinates_are_safe() {
        assert!(parse("").unwrap().sections.is_empty());
        assert!(parse("diff --git a/a b/a\nGIT binary patch\nliteral 0\n").is_ok());
        let patch =
            parse("--- /dev/null\n+++ b/a\n@@ -0,0 +1000000000 @@\n+\x1b]52;c;bad\x07\n").unwrap();
        let row = &patch.sections[1].rows[0];
        assert_eq!(row.numbers, [None, Some(1_000_000_000)]);
        assert!(row.text.contains("\x1b]52"));
        assert!(parse("diff --cc x\n@@@ -1 -1 +1 @@@\n").is_err());
    }
}
