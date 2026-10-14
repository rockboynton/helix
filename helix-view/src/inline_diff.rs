//! Immutable presentation documents. Their source documents are never save targets.
use crate::{diff_session::DiffLayout, Document};
use helix_core::Rope;

pub struct InlineDiff {
    pub sources: Vec<Document>,
    pub lines: Vec<InlineLine>,
    pub hunks: Vec<usize>,
    pub max_number: usize,
}

#[derive(Clone, Copy)]
pub struct InlineLine {
    pub source: usize,
    pub line: usize,
    pub numbers: [Option<usize>; 2],
    pub removed: Option<bool>,
}

impl InlineDiff {
    pub fn pair(sources: [Document; 2]) -> (Rope, Self) {
        let texts = sources.each_ref().map(|doc| doc.text().clone());
        let layout = DiffLayout::compute(&texts, [0, 0]);
        let mut model = Self {
            sources: Vec::from(sources),
            lines: Vec::new(),
            hunks: Vec::new(),
            max_number: 1,
        };
        let mut text = String::new();
        let mut before = 0;
        let mut after = 0;
        for hunk in layout.hunks {
            while after < hunk.after.start as usize {
                model.append(
                    &mut text,
                    1,
                    after,
                    [Some(before + 1), Some(after + 1)],
                    None,
                );
                before += 1;
                after += 1;
            }
            model.hunks.push(model.lines.len());
            for line in hunk.before.start as usize..hunk.before.end as usize {
                model.append(&mut text, 0, line, [Some(line + 1), None], Some(true));
            }
            for line in hunk.after.start as usize..hunk.after.end as usize {
                model.append(&mut text, 1, line, [None, Some(line + 1)], Some(false));
            }
            before = hunk.before.end as usize;
            after = hunk.after.end as usize;
        }
        while after < real_lines(&texts[1]) {
            model.append(
                &mut text,
                1,
                after,
                [Some(before + 1), Some(after + 1)],
                None,
            );
            before += 1;
            after += 1;
        }
        (Rope::from(text), model)
    }

    fn append(
        &mut self,
        text: &mut String,
        source: usize,
        line: usize,
        numbers: [Option<usize>; 2],
        removed: Option<bool>,
    ) {
        let rope = self.sources[source].text();
        if line >= real_lines(rope) {
            return;
        }
        let content = rope.line(line).to_string();
        text.push_str(&content);
        if !content.ends_with('\n') {
            text.push('\n');
        }
        self.lines.push(InlineLine {
            source,
            line,
            numbers,
            removed,
        });
        self.max_number = self
            .max_number
            .max(numbers.into_iter().flatten().max().unwrap_or(1));
    }
}

fn real_lines(text: &Rope) -> usize {
    text.len_lines() - usize::from(text.line(text.len_lines() - 1).len_chars() == 0)
}
