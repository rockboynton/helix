//! View-local line folds. Neither document text nor edit coordinates are projected.
use crate::{diff_session::DiffSide, Document, DocumentId, Editor, ViewId};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Default)]
pub struct Folds {
    document: Option<DocumentId>,
    version: i32,
    pub ranges: Vec<Range<usize>>,
    /// All closed syntax regions, including descendants hidden by a parent.
    nested: Vec<Range<usize>>,
    /// Diff folds share indices across panes and must open together.
    pub(crate) comparison: bool,
}

impl Folds {
    /// A conservative visible source window; virtual rows/soft wrap can only
    /// reduce it. Jump over folds instead of traversing their hidden lines.
    pub fn visible_lines(&self, doc: &Document, anchor: usize, height: usize) -> Vec<usize> {
        let mut line = doc.text().char_to_line(anchor.min(doc.text().len_chars()));
        let ranges = if self.valid(doc) {
            self.ranges.as_slice()
        } else {
            &[]
        };
        let mut fold = ranges.partition_point(|range| range.end <= doc.text().line_to_char(line));
        if let Some(range) = ranges
            .get(fold)
            .filter(|range| range.contains(&doc.text().line_to_char(line)))
        {
            line = doc.text().char_to_line(range.start);
        }
        let mut lines = Vec::with_capacity(height);
        for _ in 0..height {
            if line >= doc.text().len_lines() {
                break;
            }
            lines.push(line);
            if let Some(range) = ranges
                .get(fold)
                .filter(|range| doc.text().line_to_char(line) == range.start)
            {
                line = doc.text().char_to_line(range.end);
                fold += 1;
            } else {
                line += 1;
            }
        }
        lines
    }

    pub fn valid(&self, doc: &Document) -> bool {
        self.document == Some(doc.id()) && self.version == doc.version()
    }

    fn new(doc: &Document, ranges: Vec<Range<usize>>, comparison: bool) -> Self {
        Self {
            document: Some(doc.id()),
            version: doc.version(),
            ranges,
            nested: Vec::new(),
            comparison,
        }
    }

    /// Translate an aligned diff row into the folded presentation.
    pub fn row(&self, doc: &Document, side: &DiffSide, row: usize) -> usize {
        if !self.valid(doc) {
            return row;
        }
        row - self
            .ranges
            .iter()
            .map(|range| {
                let start = side.row(doc.text().char_to_line(range.start));
                let lines =
                    doc.text().char_to_line(range.end) - doc.text().char_to_line(range.start);
                row.saturating_sub(start).min(lines.saturating_sub(1))
            })
            .sum::<usize>()
    }

    fn project_nested(&mut self) {
        self.nested
            .sort_unstable_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
        self.nested.dedup();
        self.ranges.clear();
        for range in &self.nested {
            if self
                .ranges
                .last()
                .is_none_or(|parent| parent.end <= range.start)
            {
                self.ranges.push(range.clone());
            }
        }
    }

    fn reveal(&mut self, cursors: &[usize]) {
        if self.nested.is_empty() {
            self.ranges.retain(|range| {
                !cursors
                    .iter()
                    .any(|&cursor| range.start < cursor && cursor < range.end)
            });
        } else {
            let previous_len = self.nested.len();
            self.nested.retain(|range| {
                !cursors
                    .iter()
                    .any(|&cursor| range.start < cursor && cursor < range.end)
            });
            if self.nested.len() != previous_len {
                self.project_nested();
            }
        }
    }
}

/// Runs where every compared side has a real, unchanged line at the same row.
/// Padding and deletion-only boundaries must never be concealed.
fn context_ranges(sides: &[Arc<DiffSide>], context: usize) -> Vec<Range<usize>> {
    let first = &sides[0];
    let mut runs = Vec::<Range<usize>>::new();
    for (line, &row) in first.rows.iter().enumerate() {
        if first.is_changed(line)
            || !sides.iter().all(|side| {
                side.rows
                    .binary_search(&row)
                    .is_ok_and(|line| !side.is_changed(line))
            })
        {
            continue;
        }
        if let Some(run) = runs.last_mut().filter(|run| run.end == row) {
            run.end += 1;
        } else {
            runs.push(row..row + 1);
        }
    }
    runs.into_iter()
        .filter_map(|run| {
            let start = run.start.saturating_add(context);
            let end = run.end.saturating_sub(context);
            (end.saturating_sub(start) > 1).then_some(start..end)
        })
        .collect()
}

impl Editor {
    /// Discover syntax regions only on explicit commands, never while rendering.
    /// The parse tree is already maintained by the document's syntax engine.
    pub fn fold_syntax(&mut self, all: bool, toggle: bool) -> anyhow::Result<()> {
        let id = self.tree.focus;
        let view = self.tree.get(id);
        let doc = &self.documents[&view.doc];
        anyhow::ensure!(
            !view.is_diff(doc),
            "use diff-fold in a comparison to preserve pane alignment"
        );
        let cursor = doc.selection(id).primary().cursor(doc.text().slice(..));
        if toggle
            && view.folds.valid(doc)
            && view.folds.ranges.iter().any(|range| {
                let header = doc
                    .text()
                    .line_to_char(doc.text().char_to_line(range.start).saturating_sub(1));
                range.contains(&cursor)
                    || (!view.folds.nested.is_empty() && header <= cursor && cursor < range.end)
            })
        {
            self.open_fold(false);
            return Ok(());
        }
        let syntax = doc
            .syntax()
            .ok_or_else(|| anyhow::anyhow!("no syntax tree available for this document"))?;
        let candidates = syntax.fold_ranges(doc.text().slice(..), &self.syn_loader.load());
        anyhow::ensure!(
            !candidates.is_empty(),
            "no foldable regions; this language may need folds.scm queries"
        );
        let mut closed = if view.folds.valid(doc) {
            // Preserve existing manual regions when first using syntax folds.
            if view.folds.nested.is_empty() {
                view.folds.ranges.clone()
            } else {
                view.folds.nested.clone()
            }
        } else {
            Vec::new()
        };
        let target = if all {
            closed = candidates;
            None
        } else {
            let range = candidates
                .into_iter()
                .filter(|range| {
                    let header = doc
                        .text()
                        .line_to_char(doc.text().char_to_line(range.start) - 1);
                    header <= cursor && cursor < range.end && !closed.contains(range)
                })
                .min_by_key(|range| range.end - range.start)
                .ok_or_else(|| anyhow::anyhow!("no open foldable region at the cursor"))?;
            // Crossing manual folds cannot coexist with the syntax hierarchy.
            closed.retain(|old| {
                old.end <= range.start
                    || old.start >= range.end
                    || (old.start <= range.start && old.end >= range.end)
                    || (range.start <= old.start && range.end >= old.end)
            });
            closed.push(range.clone());
            Some(
                doc.text()
                    .line_to_char(doc.text().char_to_line(range.start) - 1),
            )
        };
        let mut folds = Folds::new(doc, Vec::new(), false);
        folds.nested = closed;
        folds.project_nested();
        // Closing is intentional: relocate every selection hidden by a new
        // region before the normal search/goto reveal logic runs.
        let slice = doc.text().slice(..);
        let selection = if let Some(target) = target {
            helix_core::Selection::point(target)
        } else {
            doc.selection(id).clone().transform(|range| {
                let cursor = range.cursor(slice);
                folds
                    .ranges
                    .iter()
                    .find(|fold| fold.contains(&cursor))
                    .map_or(range, |fold| helix_core::Range::point(fold.start))
            })
        };
        self.tree.get_mut(id).folds = folds;
        self.documents
            .get_mut(&self.tree.get(id).doc)
            .unwrap()
            .set_selection(id, selection);
        Ok(())
    }

    fn fold_views(&self) -> Vec<ViewId> {
        let focus = self.tree.focus;
        if let Some(session) = &self.diff_session {
            if session.views.contains(&focus) {
                return session.views.to_vec();
            }
        }
        if let Some(session) = &self.three_diff_session {
            if session.views.contains(&focus) {
                return session.views.to_vec();
            }
        }
        vec![focus]
    }

    pub fn fold_diff_context(&mut self, context: usize) -> anyhow::Result<()> {
        self.refresh_diff();
        let views = self.fold_views();
        let sides = views
            .iter()
            .map(|&id| {
                let view = self.tree.get(id);
                let doc = &self.documents[&view.doc];
                view.diff_side(doc)
                    .cloned()
                    .or_else(|| {
                        doc.inline_diff.as_ref().map(|model| {
                            Arc::new(DiffSide {
                                rows: (0..doc.text().len_lines()).collect(),
                                changed: model
                                    .lines
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, line)| line.removed.is_some())
                                    .map(|(line, _)| line..line + 1)
                                    .collect(),
                                ..Default::default()
                            })
                        })
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "no diff layout available; wait for the diff to finish updating"
                        )
                    })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let runs = context_ranges(&sides, context);
        for (&id, side) in views.iter().zip(&sides) {
            let doc = &self.documents[&self.tree.get(id).doc];
            let ranges = runs
                .iter()
                .map(|run| {
                    let start = side.rows.binary_search(&run.start).unwrap();
                    let end = side.rows.binary_search(&(run.end - 1)).unwrap() + 1;
                    doc.text().line_to_char(start)..doc.text().line_to_char(end)
                })
                .collect();
            self.tree.get_mut(id).folds = Folds::new(doc, ranges, views.len() > 1);
        }
        self.refresh_folds();
        Ok(())
    }

    pub fn fold_selection(&mut self) -> anyhow::Result<()> {
        let id = self.tree.focus;
        let view = self.tree.get(id);
        let doc = &self.documents[&view.doc];
        anyhow::ensure!(
            !view.is_diff(doc),
            "use diff-fold in a comparison to preserve pane alignment"
        );
        let selection = doc.selection(id).primary();
        let start = doc.text().char_to_line(selection.from());
        let end = (doc
            .text()
            .char_to_line(selection.to().saturating_sub(1).max(selection.from()))
            + 1)
        .min(doc.text().len_lines() - 1);
        anyhow::ensure!(
            end > start + 1,
            "select at least two complete lines to fold"
        );
        let range = doc.text().line_to_char(start)..doc.text().line_to_char(end);
        let mut ranges = if view.folds.valid(doc) {
            view.folds.ranges.clone()
        } else {
            Vec::new()
        };
        ranges.retain(|old| old.end <= range.start || old.start >= range.end);
        ranges.push(range.clone());
        ranges.sort_by_key(|range| range.start);
        self.tree.get_mut(id).folds = Folds::new(doc, ranges, false);
        self.documents
            .get_mut(&self.tree.get(id).doc)
            .unwrap()
            .set_selection(id, helix_core::Selection::point(range.start));
        Ok(())
    }

    pub fn open_fold(&mut self, all: bool) {
        let views = self.fold_views();
        let focus = self.tree.get(self.tree.focus);
        let doc = &self.documents[&focus.doc];
        let cursor = doc
            .selection(focus.id)
            .primary()
            .cursor(doc.text().slice(..));
        let index = focus.folds.ranges.iter().position(|range| {
            focus.folds.valid(doc)
                && (range.contains(&cursor)
                    || (!focus.folds.nested.is_empty()
                        && doc.text().char_to_line(cursor) + 1
                            == doc.text().char_to_line(range.start)))
        });
        for id in views {
            let folds = &mut self.tree.get_mut(id).folds;
            if all {
                folds.ranges.clear();
                folds.nested.clear();
            } else if let Some(index) = index.filter(|&index| index < folds.ranges.len()) {
                let range = folds.ranges.remove(index);
                if !folds.nested.is_empty() {
                    folds.nested.retain(|closed| closed != &range);
                    folds.project_nested();
                }
            }
        }
    }

    /// Edits invalidate folds rather than risk concealing newly changed text.
    /// Search/goto/cursor movement into a fold reveals it, in every paired pane.
    pub fn refresh_folds(&mut self) {
        let invalid_comparison = self.tree.views().any(|(view, _)| {
            view.folds.comparison
                && !view.folds.ranges.is_empty()
                && !view.folds.valid(&self.documents[&view.doc])
        });
        let mut reveal = Vec::new();
        for (view, _) in self.tree.views_mut() {
            let doc = &self.documents[&view.doc];
            if !view.folds.valid(doc) || (invalid_comparison && view.folds.comparison) {
                view.folds = Folds::default();
                continue;
            }
            let hidden = view
                .folds
                .ranges
                .iter()
                .enumerate()
                .filter_map(|(index, range)| {
                    doc.selection(view.id)
                        .ranges()
                        .iter()
                        .any(|selection| {
                            let cursor = selection.cursor(doc.text().slice(..));
                            range.start < cursor && cursor < range.end
                        })
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            if view.folds.comparison {
                reveal.extend(hidden);
            } else {
                let cursors = doc
                    .selection(view.id)
                    .ranges()
                    .iter()
                    .map(|range| range.cursor(doc.text().slice(..)))
                    .collect::<Vec<_>>();
                view.folds.reveal(&cursors);
            }
        }
        reveal.sort_unstable();
        reveal.dedup();
        for (view, _) in self.tree.views_mut() {
            if view.folds.comparison {
                for &index in reveal.iter().rev() {
                    if index < view.folds.ranges.len() {
                        view.folds.ranges.remove(index);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_projection_reveals_children_and_search_reveals_ancestors() {
        let mut folds = Folds {
            nested: vec![10..100, 20..50, 25..40, 60..80, 110..130],
            ..Default::default()
        };
        folds.project_nested();
        assert_eq!(folds.ranges, vec![10..100, 110..130]);
        folds.nested.remove(0);
        folds.project_nested();
        assert_eq!(folds.ranges, vec![20..50, 60..80, 110..130]);
        folds.reveal(&[30]);
        assert_eq!(folds.ranges, vec![60..80, 110..130]);
        assert_eq!(folds.nested, folds.ranges);
    }
    #[test]
    fn context_keeps_edges_changes_and_alignment_gaps_visible() {
        let left = Arc::new(DiffSide {
            rows: (0..40).collect(),
            changed: vec![10..12, 30..31],
            ..Default::default()
        });
        let right = Arc::new(DiffSide {
            rows: (0..40).filter(|&row| row != 20).collect(),
            changed: vec![10..12, 29..30],
            ..Default::default()
        });
        assert_eq!(
            context_ranges(&[left, right], 2),
            vec![2..8, 14..18, 23..28, 33..38]
        );
    }
}
