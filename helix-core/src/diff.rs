use std::ops::Range;
use std::time::Instant;

use imara_diff::{Algorithm, Diff, Hunk, IndentHeuristic, IndentLevel, InternedInput, Token};
use ropey::RopeSlice;

use crate::{ChangeSet, Rope, Tendril, Transaction};

struct ChangeSetBuilder<'a> {
    res: ChangeSet,
    after: RopeSlice<'a>,
    file: &'a InternedInput<RopeSlice<'a>>,
    current_hunk: InternedInput<char>,
    char_diff: Diff,
    pos: u32,
}

impl ChangeSetBuilder<'_> {
    fn process_hunk(&mut self, before: Range<u32>, after: Range<u32>) {
        let len = self.file.before[self.pos as usize..before.start as usize]
            .iter()
            .map(|&it| self.file.interner[it].len_chars())
            .sum();
        self.res.retain(len);
        self.pos = before.end;

        // do not perform diffs on large hunks
        let len_before = before.end - before.start;
        let len_after = after.end - after.start;

        // Pure insertions/removals do not require a character diff.
        // Very large changes are ignored because their character diff is expensive to compute
        // TODO adjust heuristic to detect large changes?
        if len_before == 0
            || len_after == 0
            || len_after > 5 * len_before
            || 5 * len_after < len_before && len_before > 10
            || len_before + len_after > 200
        {
            let remove = self.file.before[before.start as usize..before.end as usize]
                .iter()
                .map(|&it| self.file.interner[it].len_chars())
                .sum();
            self.res.delete(remove);
            let mut fragment = Tendril::new();
            if len_after > 500 {
                // copying a rope line by line is slower then copying the entire
                // rope. Use to_string for very large changes instead..
                if self.file.after.len() == after.end as usize {
                    if after.start == 0 {
                        fragment = self.after.to_string().into();
                    } else {
                        let start = self.after.line_to_char(after.start as usize);
                        fragment = self.after.slice(start..).to_string().into();
                    }
                } else if after.start == 0 {
                    let end = self.after.line_to_char(after.end as usize);
                    fragment = self.after.slice(..end).to_string().into();
                } else {
                    let start = self.after.line_to_char(after.start as usize);
                    let end = self.after.line_to_char(after.end as usize);
                    fragment = self.after.slice(start..end).to_string().into();
                }
            } else {
                for &line in &self.file.after[after.start as usize..after.end as usize] {
                    for chunk in self.file.interner[line].chunks() {
                        fragment.push_str(chunk)
                    }
                }
            };
            self.res.insert(fragment);
        } else {
            // for reasonably small hunks, generating a ChangeSet from char diff can save memory
            // TODO use a tokenizer (word diff?) for improved performance
            let hunk_before = self.file.before[before.start as usize..before.end as usize]
                .iter()
                .flat_map(|&it| self.file.interner[it].chars());
            let hunk_after = self.file.after[after.start as usize..after.end as usize]
                .iter()
                .flat_map(|&it| self.file.interner[it].chars());
            self.current_hunk.update_before(hunk_before);
            self.current_hunk.update_after(hunk_after);
            // the histogram heuristic does not work as well
            // for characters because the same characters often reoccur
            // use myer diff instead
            self.char_diff.compute_with(
                Algorithm::Myers,
                &self.current_hunk.before,
                &self.current_hunk.after,
                self.current_hunk.interner.num_tokens(),
            );
            let mut pos = 0;
            for Hunk { before, after } in self.char_diff.hunks() {
                self.res.retain((before.start - pos) as usize);
                self.res.delete(before.len());
                pos = before.end;

                let res = self.current_hunk.after[after.start as usize..after.end as usize]
                    .iter()
                    .map(|&token| self.current_hunk.interner[token])
                    .collect();

                self.res.insert(res);
            }
            self.res
                .retain(self.current_hunk.before.len() - pos as usize);
            // reuse allocations
            self.current_hunk.clear();
        }
    }

    fn finish(mut self) -> ChangeSet {
        let len = self.file.before[self.pos as usize..]
            .iter()
            .map(|&it| self.file.interner[it].len_chars())
            .sum();

        self.res.retain(len);
        self.res
    }
}

struct RopeLines<'a>(RopeSlice<'a>);

struct WordLine<'a> {
    tokens: Vec<&'a str>,
    positions: Vec<usize>,
}

impl<'a> WordLine<'a> {
    fn new(text: &'a str, start: usize) -> Self {
        use crate::unicode::segmentation::UnicodeSegmentation;
        let ending = crate::line_ending::get_line_ending_of_str(text);
        let text = &text[..text.len() - ending.map_or(0, |ending| ending.as_str().len())];
        let tokens: Vec<_> = text.split_word_bounds().collect();
        let mut positions = vec![start];
        for token in &tokens {
            positions.push(positions.last().unwrap() + token.chars().count());
        }
        Self { tokens, positions }
    }
}

fn word_line_distance(before: &[Token], after: &[Token], row: &mut Vec<usize>) -> usize {
    // Token insert/delete distance. Across all candidate line pairs the work
    // is bounded by the hunk's existing 2048-token-per-side limit. Intern once
    // and reuse scratch space instead of comparing/allocating strings per pair.
    row.clear();
    row.resize(after.len() + 1, 0);
    for left in before {
        let mut diagonal = 0;
        for (index, right) in after.iter().enumerate() {
            let previous = row[index + 1];
            row[index + 1] = if left == right {
                diagonal + 1
            } else {
                row[index].max(previous)
            };
            diagonal = previous;
        }
    }
    before.len() + after.len() - 2 * row[after.len()]
}

fn word_line_pairs(before: &[&[Token]], after: &[&[Token]]) -> Vec<(usize, usize)> {
    // Equal-sized replacements retain their source-line correspondence, even
    // when several similar actions/bodies move between those lines.
    if before.len() == after.len() {
        return (0..before.len()).map(|line| (line, line)).collect();
    }
    let mut costs = vec![vec![0; after.len() + 1]; before.len() + 1];
    let mut paired = vec![vec![false; after.len() + 1]; before.len() + 1];
    let mut scratch = Vec::new();
    for (index, line) in before.iter().enumerate() {
        costs[index + 1][0] = costs[index][0] + line.len() + 1;
    }
    for (index, line) in after.iter().enumerate() {
        costs[0][index + 1] = costs[0][index] + line.len() + 1;
    }
    for (left, old) in before.iter().enumerate() {
        for (right, new) in after.iter().enumerate() {
            let replacement = costs[left][right] + word_line_distance(old, new, &mut scratch);
            let gap = (costs[left][right + 1] + old.len() + 1)
                .min(costs[left + 1][right] + new.len() + 1);
            paired[left + 1][right + 1] = replacement <= gap;
            costs[left + 1][right + 1] = replacement.min(gap);
        }
    }
    let (mut left, mut right) = (before.len(), after.len());
    let mut pairs = Vec::new();
    while left > 0 && right > 0 {
        if paired[left][right] {
            pairs.push((left - 1, right - 1));
            left -= 1;
            right -= 1;
        } else if costs[left][right] == costs[left - 1][right] + before[left - 1].len() + 1 {
            left -= 1;
        } else {
            right -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// Line-level changes, without constructing an editing transaction.
pub fn line_hunks(before: &Rope, after: &Rope) -> Vec<Hunk> {
    let input = InternedInput::new(RopeLines(before.slice(..)), RopeLines(after.slice(..)));
    Diff::compute(Algorithm::Histogram, &input)
        .hunks()
        .collect()
}

/// Word emphasis in replacement hunks, in source character coordinates.
/// Bound refinement work; pathological/minified hunks keep line-only coloring.
pub fn word_ranges(before: &Rope, after: &Rope, hunks: &[Hunk]) -> [Vec<Range<usize>>; 2] {
    let mut result: [Vec<Range<usize>>; 2] = Default::default();
    for hunk in hunks {
        if hunk.before.is_empty() || hunk.after.is_empty() {
            continue;
        }
        let texts = [before, after];
        let lines = [hunk.before.clone(), hunk.after.clone()];
        let starts = std::array::from_fn::<_, 2, _>(|side| {
            texts[side].line_to_char(lines[side].start as usize)
        });
        let ends = std::array::from_fn::<_, 2, _>(|side| {
            texts[side].line_to_char(lines[side].end as usize)
        });
        if (0..2).any(|side| ends[side] - starts[side] > 16_384 || lines[side].len() > 128) {
            continue;
        }
        let strings = [0, 1].map(|side| {
            texts[side]
                .slice(starts[side]..ends[side])
                .lines()
                .take(lines[side].len())
                .map(|line| line.to_string())
                // A terminal empty Rope line is an EOF boundary, not a line
                // that should steal the pairing from real replaced text.
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
        });
        let word_lines = [0, 1].map(|side| {
            let mut offset = starts[side];
            strings[side]
                .iter()
                .map(|text| {
                    let line = WordLine::new(text, offset);
                    offset += text.chars().count();
                    line
                })
                .collect::<Vec<_>>()
        });
        if word_lines
            .iter()
            .any(|lines| lines.iter().map(|line| line.tokens.len()).sum::<usize>() > 2048)
        {
            continue;
        }
        let mut input = InternedInput::default();
        input.update_before(
            word_lines[0]
                .iter()
                .flat_map(|line| line.tokens.iter().copied()),
        );
        input.update_after(
            word_lines[1]
                .iter()
                .flat_map(|line| line.tokens.iter().copied()),
        );
        let tokens = [&input.before, &input.after];
        let token_lines = [0, 1].map(|side| {
            let mut offset = 0;
            word_lines[side]
                .iter()
                .map(|line| {
                    let end = offset + line.tokens.len();
                    let range = &tokens[side][offset..end];
                    offset = end;
                    range
                })
                .collect::<Vec<_>>()
        });
        let mut diff = Diff::default();
        for (left, right) in word_line_pairs(&token_lines[0], &token_lines[1]) {
            let pair = [&word_lines[0][left], &word_lines[1][right]];
            diff.compute_with(
                Algorithm::Myers,
                token_lines[0][left],
                token_lines[1][right],
                input.interner.num_tokens(),
            );
            for word in diff.hunks() {
                for (side, range) in [word.before, word.after].into_iter().enumerate() {
                    if !range.is_empty() {
                        result[side].push(
                            pair[side].positions[range.start as usize]
                                ..pair[side].positions[range.end as usize],
                        );
                    }
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod word_tests {
    use super::*;

    const SWAPPED_BEFORE: &str = concat!(
        "    Ctrl+Right { focus-column-or-monitor-right; }\n",
        "    Ctrl+Left { focus-column-or-monitor-left; }\n",
        "    Mod+Ctrl+Right    { focus-window-or-workspace-down; }\n",
        "    Mod+Ctrl+Left     { focus-window-or-workspace-up; }\n",
    );
    const SWAPPED_AFTER: &str = concat!(
        "    Ctrl+Right    { focus-window-or-workspace-down; }\n",
        "    Ctrl+Left     { focus-window-or-workspace-up; }\n",
        "    Mod+Ctrl+Right { focus-column-or-monitor-right; }\n",
        "    Mod+Ctrl+Left { focus-column-or-monitor-left; }\n",
    );

    #[test]
    fn swapped_actions_do_not_emphasize_unchanged_keys_or_cross_lines() {
        let texts = [
            Rope::from_str(SWAPPED_BEFORE),
            Rope::from_str(SWAPPED_AFTER),
        ];
        let ranges = word_ranges(&texts[0], &texts[1], &line_hunks(&texts[0], &texts[1]));
        for (text, ranges) in texts.iter().zip(ranges) {
            for range in &ranges {
                let emphasis = text.slice(range.clone()).to_string();
                assert!(!emphasis.contains(['\n', '\r']), "{emphasis:?}");
                for unchanged in ["Ctrl", "Mod", "focus"] {
                    assert!(!emphasis.contains(unchanged), "{emphasis:?}");
                }
            }
            for line in 0..4 {
                let start = text.line_to_char(line);
                let end = text.line_to_char(line + 1);
                assert!(ranges
                    .iter()
                    .any(|range| range.start >= start && range.end <= end));
            }
        }
    }

    #[test]
    fn unequal_replacements_align_changed_lines_past_an_insertion() {
        let texts = [
            Rope::from_str("α = old_one;\r\nβ = old_two;\r\n"),
            Rope::from_str("// newly inserted\r\nα = new_one;\r\nβ = new_two;\r\n"),
        ];
        let ranges = word_ranges(&texts[0], &texts[1], &line_hunks(&texts[0], &texts[1]));
        for (side, expected) in [["old_one", "old_two"], ["new_one", "new_two"]]
            .iter()
            .enumerate()
        {
            let emphasis = ranges[side]
                .iter()
                .map(|range| texts[side].slice(range.clone()).to_string())
                .collect::<Vec<_>>();
            assert_eq!(&emphasis, expected);
        }
        let reversed = word_ranges(&texts[1], &texts[0], &line_hunks(&texts[1], &texts[0]));
        assert_eq!(reversed, [ranges[1].clone(), ranges[0].clone()]);
    }

    #[test]
    fn line_endings_and_missing_final_newline_are_not_word_changes() {
        for (old, new) in [("old\r\n", "new\n"), ("old", "new"), ("old\n", "new")] {
            let texts = [Rope::from_str(old), Rope::from_str(new)];
            let ranges = word_ranges(&texts[0], &texts[1], &line_hunks(&texts[0], &texts[1]));
            assert_eq!(ranges, [vec![0..3], vec![0..3]]);
        }
        let texts = [Rope::from_str("same\r\n"), Rope::from_str("same\n")];
        assert!(
            word_ranges(&texts[0], &texts[1], &line_hunks(&texts[0], &texts[1]))
                .iter()
                .all(Vec::is_empty)
        );
    }

    #[test]
    fn oversized_replacement_blocks_keep_bounded_line_only_fallback() {
        for (old, new) in [
            ("old ".repeat(1050), "new ".repeat(1050)),
            (
                (0..129).map(|line| format!("old_{line}\n")).collect(),
                (0..129).map(|line| format!("new_{line}\n")).collect(),
            ),
        ] {
            let texts = [Rope::from_str(&old), Rope::from_str(&new)];
            assert!(
                word_ranges(&texts[0], &texts[1], &line_hunks(&texts[0], &texts[1]))
                    .iter()
                    .all(Vec::is_empty)
            );
        }
    }

    #[test]
    fn unrelated_insertions_and_deletions_keep_line_only_coloring() {
        let before = Rope::from_str("unchanged\n");
        let after = Rope::from_str("new line\nunchanged\n");
        assert!(word_ranges(&before, &after, &line_hunks(&before, &after))
            .iter()
            .all(Vec::is_empty));
    }

    #[test]
    fn replacement_words_preserve_unicode_and_unchanged_tokens() {
        let before = Rope::from_str("let café = old_value;\n");
        let after = Rope::from_str("let café = new_value;\n");
        let ranges = word_ranges(&before, &after, &line_hunks(&before, &after));
        assert_eq!(
            ranges[0]
                .iter()
                .map(|range| before.slice(range.clone()).to_string())
                .collect::<Vec<_>>(),
            ["old_value"]
        );
        assert_eq!(
            ranges[1]
                .iter()
                .map(|range| after.slice(range.clone()).to_string())
                .collect::<Vec<_>>(),
            ["new_value"]
        );
        let long = Rope::from_str(&"x".repeat(20_000));
        assert!(word_ranges(&before, &long, &line_hunks(&before, &long))
            .iter()
            .all(Vec::is_empty));
    }
}

impl<'a> imara_diff::TokenSource for RopeLines<'a> {
    type Token = RopeSlice<'a>;
    type Tokenizer = ropey::iter::Lines<'a>;

    fn tokenize(&self) -> Self::Tokenizer {
        self.0.lines()
    }

    fn estimate_tokens(&self) -> u32 {
        // we can provide a perfect estimate which is very nice for performance
        self.0.len_lines() as u32
    }
}

/// Compares `old` and `new` to generate a [`Transaction`] describing
/// the steps required to get from `old` to `new`.
pub fn compare_ropes(before: &Rope, after: &Rope) -> Transaction {
    let start = Instant::now();
    let res = ChangeSet::with_capacity(32);
    let after = after.slice(..);
    let file = InternedInput::new(RopeLines(before.slice(..)), RopeLines(after));
    let mut builder = ChangeSetBuilder {
        res,
        file: &file,
        after,
        pos: 0,
        current_hunk: InternedInput::default(),
        char_diff: Diff::default(),
    };
    let mut diff = Diff::compute(Algorithm::Histogram, &file);
    diff.postprocess_with_heuristic(
        &file,
        IndentHeuristic::new(|token| IndentLevel::for_ascii_line(file.interner[token].bytes(), 4)),
    );
    for hunk in diff.hunks() {
        builder.process_hunk(hunk.before, hunk.after)
    }
    let res = builder.finish().into();

    log::debug!(
        "rope diff took {}s",
        Instant::now().duration_since(start).as_secs_f64()
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity(a: &str, b: &str) {
        let mut old = Rope::from(a);
        let new = Rope::from(b);
        compare_ropes(&old, &new).apply(&mut old);
        assert_eq!(old, new);
    }

    quickcheck::quickcheck! {
        fn test_compare_ropes(a: String, b: String) -> bool {
            let mut old = Rope::from(a);
            let new = Rope::from(b);
            compare_ropes(&old, &new).apply(&mut old);
            old == new
        }
    }

    #[test]
    fn equal_files() {
        test_identity("foo", "foo");
    }

    #[test]
    fn trailing_newline() {
        test_identity("foo\n", "foo");
        test_identity("foo", "foo\n");
    }

    #[test]
    fn new_file() {
        test_identity("", "foo");
    }

    #[test]
    fn deleted_file() {
        test_identity("foo", "");
    }
}
