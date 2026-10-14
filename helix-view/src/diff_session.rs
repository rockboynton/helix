//! Paired editing; alignment is view metadata, never document text.
use crate::{DocumentId, ViewId};
use arc_swap::ArcSwapOption;
use helix_core::{diff::line_hunks, text_annotations::LineAnnotation, Position, Rope};
use helix_vcs::Hunk;
use std::{ops::Range, sync::Arc};
use tokio::sync::watch;

#[derive(Debug)]
pub struct DiffLayout {
    pub versions: [i32; 2],
    pub hunks: Vec<Hunk>,
    pub sides: [Arc<DiffSide>; 2],
}

#[derive(Debug, Default)]
pub struct DiffSide {
    /// Aligned row of every real line; gaps are omitted.
    pub rows: Vec<usize>,
    pub changed: Vec<Range<usize>>,
}
impl DiffSide {
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
impl DiffLayout {
    pub fn compute(texts: &[Rope; 2], versions: [i32; 2]) -> Self {
        let hunks = line_hunks(&texts[0], &texts[1]);
        let mut sides = [DiffSide::default(), DiffSide::default()];
        let mut lines = [0usize; 2];
        let mut row = 0;
        for hunk in &hunks {
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
struct Request {
    texts: [Rope; 2],
    versions: [i32; 2],
}
pub struct DiffSession {
    pub documents: [DocumentId; 2],
    pub views: [ViewId; 2],
    pub result: Arc<ArcSwapOption<DiffLayout>>,
    versions: [i32; 2],
    sender: watch::Sender<Request>,
    worker: tokio::task::JoinHandle<()>,
}
impl DiffSession {
    pub fn new(
        documents: [DocumentId; 2],
        views: [ViewId; 2],
        texts: [Rope; 2],
        versions: [i32; 2],
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
    pub fn update(&mut self, texts: [Rope; 2], versions: [i32; 2]) {
        if self.versions != versions {
            self.versions = versions;
            self.sender.send_replace(Request { texts, versions });
        }
    }
}
impl Drop for DiffSession {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
