//! Paired editing; alignment is view metadata, never document text.
use crate::{DocumentId, ViewId};
use arc_swap::ArcSwapOption;
use helix_core::{diff::line_hunks, text_annotations::LineAnnotation, Position, Rope};
use helix_vcs::Hunk;
use std::{ops::Range, sync::Arc};
use tokio::sync::watch;

#[derive(Debug)]
pub struct DiffLayout<const N: usize = 2> {
    pub versions: [i32; N],
    pub hunks: Vec<Hunk>,
    pub sides: [Arc<DiffSide>; N],
    /// Base-to-left and base-to-right hunks for three-way comparisons.
    pub pairs: Vec<Vec<Hunk>>,
}

#[derive(Debug, Default, Clone)]
pub struct DiffSide {
    /// Aligned row of every real line; gaps are omitted.
    pub rows: Vec<usize>,
    pub changed: Vec<Range<usize>>,
    pub gutter_hunks: Vec<DiffGutterHunk>,
    pub words: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffChange {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone)]
pub struct DiffGutterHunk {
    pub lines: Range<usize>,
    pub change: DiffChange,
}

impl DiffSide {
    pub fn gutter_at(&self, line: usize, first_visual_line: bool) -> Option<DiffChange> {
        // Empty ranges are boundary marks, as in Helix's normal VCS gutter.
        let index = self.gutter_hunks.partition_point(|hunk| {
            if hunk.lines.is_empty() {
                hunk.lines.start.min(self.rows.len() - 1) < line
            } else {
                hunk.lines.end <= line
            }
        });
        let hunk = self.gutter_hunks.get(index)?;
        if hunk.lines.contains(&line)
            || (hunk.lines.is_empty()
                && first_visual_line
                && hunk.lines.start.min(self.rows.len() - 1) == line)
        {
            Some(hunk.change)
        } else {
            None
        }
    }
    pub fn is_changed(&self, line: usize) -> bool {
        let index = self.changed.partition_point(|range| range.end <= line);
        self.changed
            .get(index)
            .is_some_and(|range| range.contains(&line))
    }
    pub fn padding_after(&self, line: usize) -> usize {
        self.rows
            .get(line + 1)
            .map_or(0, |next| next - self.rows[line] - 1)
    }
    pub fn row(&self, line: usize) -> usize {
        self.rows[line.min(self.rows.len() - 1)]
    }
    pub fn position(&self, row: usize) -> (usize, usize) {
        let line = self
            .rows
            .partition_point(|&candidate| candidate <= row)
            .saturating_sub(1);
        (line, row.saturating_sub(self.rows[line]))
    }
}
pub struct Alignment(pub Arc<DiffSide>);
impl LineAnnotation for Alignment {
    fn leading_rows(&self) -> usize {
        self.0.rows[0]
    }
    fn insert_virtual_lines(&mut self, _: usize, _: Position, line: usize) -> Position {
        Position::new(self.0.padding_after(line), 0)
    }
}
impl<const N: usize> DiffLayout<N> {
    pub fn compute(texts: &[Rope; N], versions: [i32; N]) -> Self {
        assert!(N == 2 || N == 3);
        if N == 3 {
            return Self::three(texts, versions);
        }
        let hunks = line_hunks(&texts[0], &texts[1]);
        let mut sides: [DiffSide; N] = std::array::from_fn(|_| DiffSide::default());
        let words = helix_core::diff::word_ranges(&texts[0], &texts[1], &hunks);
        for (side, words) in words.into_iter().enumerate() {
            sides[side].words = words;
        }
        let mut lines = [0usize; 2];
        let mut row = 0;
        for hunk in &hunks {
            let change = if hunk.is_pure_insertion() {
                DiffChange::Added
            } else if hunk.is_pure_removal() {
                DiffChange::Removed
            } else {
                DiffChange::Modified
            };
            let ranges = [hunk.before.clone(), hunk.after.clone()];
            let unchanged = ranges[0].start as usize - lines[0];
            for side in 0..2 {
                sides[side].rows.extend(row..row + unchanged);
                lines[side] += unchanged;
            }
            row += unchanged;
            let height = ranges[0].len().max(ranges[1].len());
            for side in 0..2 {
                let range = ranges[side].start as usize..ranges[side].end as usize;
                sides[side].rows.extend(row..row + range.len());
                lines[side] = range.end;
                sides[side].gutter_hunks.push(DiffGutterHunk {
                    lines: range.clone(),
                    change,
                });
                if !range.is_empty() {
                    sides[side].changed.push(range);
                }
            }
            row += height;
        }
        for side in 0..2 {
            let remaining = texts[side].len_lines() - lines[side];
            sides[side].rows.extend(row..row + remaining);
        }
        Self {
            versions,
            hunks,
            sides: sides.map(Arc::new),
            pairs: Vec::new(),
        }
    }

    fn three(texts: &[Rope; N], versions: [i32; N]) -> Self {
        let pairs = [0, 2].map(|side| {
            DiffLayout::<2>::compute(
                &[texts[1].clone(), texts[side].clone()],
                [versions[1], versions[side]],
            )
        });
        let count = texts[1].len_lines();
        let mut common = Vec::with_capacity(count);
        let mut row = 0;
        for line in 0..count {
            let gaps = pairs.each_ref().map(|pair| {
                let rows = &pair.sides[0].rows;
                rows[line] - if line == 0 { 0 } else { rows[line - 1] + 1 }
            });
            row += gaps[0].max(gaps[1]);
            common.push(row);
            row += 1;
        }
        let mut sides: [DiffSide; N] = std::array::from_fn(|_| DiffSide::default());
        sides[1].rows = common.clone();
        for (index, side) in [0, 2].into_iter().enumerate() {
            let pair = &pairs[index];
            sides[side] = (*pair.sides[1]).clone();
            let mut next = 0;
            for position in &mut sides[side].rows {
                while next < count && pair.sides[0].rows[next] < *position {
                    next += 1;
                }
                *position = if next < count && pair.sides[0].rows[next] == *position {
                    common[next]
                } else {
                    let (old_start, new_start) = if next == 0 {
                        (0, 0)
                    } else {
                        (pair.sides[0].rows[next - 1] + 1, common[next - 1] + 1)
                    };
                    new_start + *position - old_start
                };
            }
            sides[1].changed.extend(pair.sides[0].changed.clone());
            sides[1].words.extend(pair.sides[0].words.clone());
            sides[1]
                .gutter_hunks
                .extend(pair.sides[0].gutter_hunks.clone());
        }
        sides[1].changed.sort_by_key(|range| range.start);
        let mut changed: Vec<Range<usize>> = Vec::new();
        for range in std::mem::take(&mut sides[1].changed) {
            if let Some(last) = changed.last_mut().filter(|last| last.end >= range.start) {
                last.end = last.end.max(range.end);
            } else {
                changed.push(range);
            }
        }
        sides[1].changed = changed;
        sides[1].words.sort_by_key(|range| range.start);
        let mut words: Vec<Range<usize>> = Vec::new();
        for range in std::mem::take(&mut sides[1].words) {
            if let Some(last) = words.last_mut().filter(|last| last.end >= range.start) {
                last.end = last.end.max(range.end);
            } else {
                words.push(range);
            }
        }
        sides[1].words = words;
        // Union base ranges, including zero-width insertion boundary markers.
        sides[1]
            .gutter_hunks
            .sort_by_key(|hunk| (hunk.lines.start, hunk.lines.end));
        let mut gutters: Vec<DiffGutterHunk> = Vec::new();
        for hunk in std::mem::take(&mut sides[1].gutter_hunks) {
            if let Some(last) = gutters
                .last_mut()
                .filter(|last| last.lines.end >= hunk.lines.start)
            {
                last.lines.end = last.lines.end.max(hunk.lines.end);
                last.change = DiffChange::Modified;
            } else {
                gutters.push(hunk);
            }
        }
        sides[1].gutter_hunks = gutters;
        Self {
            versions,
            hunks: Vec::new(),
            sides: sides.map(Arc::new),
            pairs: pairs.into_iter().map(|pair| pair.hunks).collect(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct DiffView {
    pub document: DocumentId,
    pub version: i32,
    pub side: Arc<DiffSide>,
}
#[derive(Clone)]
struct Request<const N: usize> {
    texts: [Rope; N],
    versions: [i32; N],
}
pub struct DiffSession<const N: usize = 2> {
    pub documents: [DocumentId; N],
    pub views: [ViewId; N],
    pub result: Arc<ArcSwapOption<DiffLayout<N>>>,
    versions: [i32; N],
    sender: watch::Sender<Request<N>>,
    worker: tokio::task::JoinHandle<()>,
}
impl<const N: usize> DiffSession<N> {
    pub fn new(
        documents: [DocumentId; N],
        views: [ViewId; N],
        texts: [Rope; N],
        versions: [i32; N],
    ) -> Self {
        let (sender, mut receiver) = watch::channel(Request { texts, versions });
        let result = Arc::new(ArcSwapOption::empty());
        let output = result.clone();
        let worker = tokio::spawn(async move {
            loop {
                let request = receiver.borrow_and_update().clone();
                let versions = request.versions;
                let Ok(layout) = tokio::task::spawn_blocking(move || {
                    DiffLayout::compute(&request.texts, versions)
                })
                .await
                else {
                    break;
                };
                // Latest-value input bounds queued work; discard stale results.
                if receiver.borrow().versions == versions {
                    output.store(Some(Arc::new(layout)));
                    helix_event::request_redraw();
                }
                if receiver.changed().await.is_err() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        });
        Self {
            documents,
            views,
            result,
            versions,
            sender,
            worker,
        }
    }
    pub fn update(&mut self, texts: [Rope; N], versions: [i32; N]) {
        if self.versions != versions {
            self.versions = versions;
            self.sender.send_replace(Request { texts, versions });
        }
    }
}
impl<const N: usize> Drop for DiffSession<N> {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn three_way_alignment_combines_both_variants_gaps_and_base_changes() {
        let texts = [
            Rope::from_str("left\nleft2\na\nc\n"),
            Rope::from_str("a\nb\nc\n"),
            Rope::from_str("a\nright\nb\nc\n"),
        ];
        let layout = DiffLayout::<3>::compute(&texts, [1, 2, 3]);
        assert_eq!(layout.versions, [1, 2, 3]);
        assert_eq!(layout.sides[0].rows[2], layout.sides[1].rows[0]);
        assert_eq!(layout.sides[2].rows[0], layout.sides[1].rows[0]);
        assert_eq!(layout.sides[0].rows[3], layout.sides[1].rows[2]);
        assert_eq!(layout.sides[2].rows[3], layout.sides[1].rows[2]);
        assert!(layout.sides[1].is_changed(1));
        for side in &layout.sides {
            assert!(side.rows.windows(2).all(|pair| pair[0] < pair[1]));
        }
        for input in [
            ["", "", "extra\n"],
            ["a", "a\n", ""],
            ["x\na\n", "a\n", "y\nz\na\n"],
        ] {
            let layout = DiffLayout::<3>::compute(&input.map(Rope::from_str), [0; 3]);
            for side in &layout.sides {
                assert!(side.rows.windows(2).all(|pair| pair[0] < pair[1]));
            }
        }
    }
    #[test]
    fn gutter_classifies_hunks_instead_of_signs_for_each_side() {
        for (before, after, change) in [
            ("a\nb\n", "a\nextra\nb\n", DiffChange::Added),
            ("a\nextra\nb\n", "a\nb\n", DiffChange::Removed),
            ("a\nb\n", "a\nx\n", DiffChange::Modified),
        ] {
            let layout =
                DiffLayout::compute(&[Rope::from_str(before), Rope::from_str(after)], [0, 0]);
            for side in &layout.sides {
                assert_eq!(side.gutter_at(0, true), None);
                assert_eq!(side.gutter_at(1, true), Some(change));
                assert_eq!(side.gutter_at(100, true), None);
                if side.gutter_hunks[0].lines.is_empty() {
                    assert_eq!(side.gutter_at(1, false), None);
                }
            }
        }
    }
    use helix_core::{
        doc_formatter::{DocumentFormatter, TextFormat},
        text_annotations::TextAnnotations,
        visual_offset_from_anchor,
    };

    #[test]
    fn formatter_and_cursor_positions_include_alignment_gaps() {
        for (left, right) in [
            ("a\nb\nc\n", "a\nextra\nb\nc\n"),
            ("extra\na\n", "a\n"),
            ("", "extra\n"),
        ] {
            let texts = [Rope::from_str(left), Rope::from_str(right)];
            let layout = DiffLayout::compute(&texts, [0, 0]);
            for side in 0..2 {
                let mut annotations = TextAnnotations::default();
                annotations.add_line_annotation(Box::new(Alignment(layout.sides[side].clone())));
                let format = TextFormat::default();
                let mut seen = vec![false; texts[side].len_lines()];
                for grapheme in DocumentFormatter::new_at_prev_checkpoint(
                    texts[side].slice(..),
                    &format,
                    &annotations,
                    0,
                ) {
                    if !grapheme.is_virtual() && !seen[grapheme.line_idx] {
                        assert_eq!(
                            grapheme.visual_pos.row,
                            layout.sides[side].rows[grapheme.line_idx]
                        );
                        seen[grapheme.line_idx] = true;
                    }
                }
                assert!(seen.into_iter().all(|seen| seen));
                let pos = visual_offset_from_anchor(
                    texts[side].slice(..),
                    0,
                    0,
                    &format,
                    &annotations,
                    100,
                )
                .unwrap()
                .0;
                assert_eq!(pos.row, layout.sides[side].rows[0]);
            }
        }
    }

    #[tokio::test]
    async fn worker_publishes_latest_version_only() {
        let views = [ViewId::default(); 2];
        let mut session = DiffSession::new(
            [DocumentId::new(1), DocumentId::new(2)],
            views,
            [Rope::from_str("a\n"), Rope::from_str("b\n")],
            [0, 0],
        );
        for version in 1..20 {
            session.update(
                [Rope::from_str("same\n"), Rope::from_str("same\n")],
                [version, version],
            );
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(layout) = session.result.load_full() {
                    assert_eq!(layout.versions, [19, 19]);
                    assert!(layout.hunks.is_empty());
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[test]
    fn alignment_does_not_change_text() {
        for (left, right) in [
            ("a\nb\nc\n", "a\nextra\nb\nc\n"),
            ("a\nb\n", "a\nx\ny\n"),
            ("", "added\n"),
            ("same", "same\n"),
            ("extra\na\n", "a\n"),
        ] {
            let texts = [Rope::from_str(left), Rope::from_str(right)];
            let layout = DiffLayout::compute(&texts, [1, 2]);
            for side in 0..2 {
                assert_eq!(layout.sides[side].rows.len(), texts[side].len_lines());
                assert!(layout.sides[side]
                    .rows
                    .windows(2)
                    .all(|rows| rows[0] < rows[1]));
                for (line, &row) in layout.sides[side].rows.iter().enumerate() {
                    assert_eq!(layout.sides[side].position(row), (line, 0));
                }
            }
            assert_eq!(texts[0].to_string(), left);
            assert_eq!(texts[1].to_string(), right);
        }
    }
}
