use crate::doc_formatter::{DocumentFormatter, TextFormat};
use crate::text_annotations::{InlineAnnotation, Overlay, TextAnnotations};

#[test]
fn folded_lines_preserve_source_positions_and_checkpoint_mapping() {
    let text = crate::Rope::from("head\r\nαβ\r\nhidden\r\ntail\r\n");
    let start = text.line_to_char(1);
    let end = text.line_to_char(3);
    let mut annotations = TextAnnotations::default();
    annotations.add_fold(start..end, "… 2 lines …".into(), None);
    for soft_wrap in [false, true] {
        let format = TextFormat::new_test(soft_wrap);
        let rendered =
            DocumentFormatter::new_at_prev_checkpoint(text.slice(..), &format, &annotations, 0)
                .collect::<Vec<_>>();
        assert!(!rendered.iter().any(|g| g.line_idx == 2));
        let tail = rendered.iter().find(|g| g.char_idx == end).unwrap();
        assert_eq!(tail.line_idx, 3);
        assert_eq!(tail.visual_pos.row, 2);
        let mut inside = DocumentFormatter::new_at_prev_checkpoint(
            text.slice(..),
            &format,
            &annotations,
            text.line_to_char(2),
        );
        assert_eq!(inside.next().unwrap().char_idx, start);
        drop(inside);
        drop(rendered);
        let range = crate::Range::point(start);
        let next = crate::movement::move_vertically(
            text.slice(..),
            range,
            crate::movement::Direction::Forward,
            1,
            crate::movement::Movement::Move,
            &format,
            &mut annotations,
        );
        assert_eq!(text.char_to_line(next.cursor(text.slice(..))), 3);
        let prev = crate::movement::move_vertically(
            text.slice(..),
            next,
            crate::movement::Direction::Backward,
            1,
            crate::movement::Movement::Move,
            &format,
            &mut annotations,
        );
        assert_eq!(prev.cursor(text.slice(..)), start);
    }
    assert_eq!(text.to_string(), "head\r\nαβ\r\nhidden\r\ntail\r\n");
}

#[test]
fn folding_skips_hidden_annotations_and_large_ranges_without_visiting_each_line() {
    let text = crate::Rope::from(format!("head\n{}tail\n", "hidden\n".repeat(100_000)));
    let end = text.line_to_char(100_001);
    let inline = [
        InlineAnnotation::new(10, "HIDDEN"),
        InlineAnnotation::new(end, "VISIBLE"),
    ];
    let overlays = [Overlay::new(12, "X"), Overlay::new(end, "T")];
    let mut annotations = TextAnnotations::default();
    annotations
        .add_inline_annotations(&inline, None)
        .add_overlay(&overlays, None);
    annotations.add_fold(5..end, "fold".into(), None);
    let format = TextFormat::new_test(false);
    let rendered =
        DocumentFormatter::new_at_prev_checkpoint(text.slice(..), &format, &annotations, 0)
            .collect::<Vec<_>>();
    assert!(rendered.len() < 30, "hidden lines must be jumped over");
    assert!(!rendered
        .iter()
        .any(|g| g.line_idx > 0 && g.line_idx < 100_001 && g.char_idx != 5));
    let tail = rendered
        .iter()
        .filter(|g| g.line_idx == 100_001)
        .map(|g| g.raw.to_string())
        .collect::<String>();
    assert!(tail.starts_with("VISIBLETail"), "{tail:?}");
}

impl TextFormat {
    fn new_test(softwrap: bool) -> Self {
        TextFormat {
            soft_wrap: softwrap,
            tab_width: 2,
            max_wrap: 3,
            max_indent_retain: 4,
            wrap_indicator: ".".into(),
            wrap_indicator_highlight: None,
            // use a prime number to allow lining up too often with repeat
            viewport_width: 17,
            soft_wrap_at_text_width: false,
        }
    }
}

impl<'t> DocumentFormatter<'t> {
    fn collect_to_str(&mut self) -> String {
        use std::fmt::Write;
        let mut res = String::new();
        let viewport_width = self.text_fmt.viewport_width;
        let soft_wrap_at_text_width = self.text_fmt.soft_wrap_at_text_width;
        let mut line = 0;

        for grapheme in self {
            if grapheme.visual_pos.row != line {
                line += 1;
                assert_eq!(grapheme.visual_pos.row, line);
                write!(res, "\n{}", ".".repeat(grapheme.visual_pos.col)).unwrap();
            }
            if !soft_wrap_at_text_width {
                assert!(
                    grapheme.visual_pos.col <= viewport_width as usize,
                    "softwrapped failed {}<={viewport_width}",
                    grapheme.visual_pos.col
                );
            }
            write!(res, "{}", grapheme.raw).unwrap();
        }

        res
    }
}

fn softwrap_text(text: &str) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(true),
        &TextAnnotations::default(),
        0,
    )
    .collect_to_str()
}

#[test]
fn basic_softwrap() {
    assert_eq!(
        softwrap_text(&"foo ".repeat(10)),
        "foo foo foo foo \n.foo foo foo foo \n.foo foo  "
    );
    assert_eq!(
        softwrap_text(&"fooo ".repeat(10)),
        "fooo fooo fooo \n.fooo fooo fooo \n.fooo fooo fooo \n.fooo  "
    );

    // check that we don't wrap unnecessarily
    assert_eq!(softwrap_text("\t\txxxx1xxxx2xx\n"), "    xxxx1xxxx2xx \n ");
}

#[test]
fn softwrap_indentation() {
    assert_eq!(
        softwrap_text("\t\tfoo1 foo2 foo3 foo4 foo5 foo6\n"),
        "    foo1 foo2 \n.....foo3 foo4 \n.....foo5 foo6 \n "
    );
    assert_eq!(
        softwrap_text("\t\t\tfoo1 foo2 foo3 foo4 foo5 foo6\n"),
        "      foo1 foo2 \n.foo3 foo4 foo5 \n.foo6 \n "
    );
}

#[test]
fn long_word_softwrap() {
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n"),
        "    xxxx1xxxx2xxx\n.....x3xxxx4xxxx5\n.....xxxx6xxxx7xx\n.....xx8xxxx9xxx \n "
    );
    assert_eq!(
        softwrap_text("xxxxxxxx1xxxx2xxx\n"),
        "xxxxxxxx1xxxx2xxx\n. \n "
    );
    assert_eq!(
        softwrap_text("\t\txxxx1xxxx 2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n"),
        "    xxxx1xxxx \n.....2xxxx3xxxx4x\n.....xxx5xxxx6xxx\n.....x7xxxx8xxxx9\n.....xxx \n "
    );
    assert_eq!(
        softwrap_text("\t\txxxx1xxx 2xxxx3xxxx4xxxx5xxxx6xxxx7xxxx8xxxx9xxx\n"),
        "    xxxx1xxx 2xxx\n.....x3xxxx4xxxx5\n.....xxxx6xxxx7xx\n.....xx8xxxx9xxx \n "
    );
}

#[test]
fn softwrap_multichar_grapheme() {
    assert_eq!(
        softwrap_text("xxxx xxxx xxx a\u{0301}bc\n"),
        "xxxx xxxx xxx \n.ábc \n "
    )
}

fn softwrap_text_at_text_width(text: &str) -> String {
    let mut text_fmt = TextFormat::new_test(true);
    text_fmt.soft_wrap_at_text_width = true;
    let annotations = TextAnnotations::default();
    let mut formatter =
        DocumentFormatter::new_at_prev_checkpoint(text.into(), &text_fmt, &annotations, 0);
    formatter.collect_to_str()
}
#[test]
fn long_word_softwrap_text_width() {
    assert_eq!(
        softwrap_text_at_text_width("xxxxxxxx1xxxx2xxx\nxxxxxxxx1xxxx2xxx"),
        "xxxxxxxx1xxxx2xxx \nxxxxxxxx1xxxx2xxx "
    );
}

fn overlay_text(text: &str, char_pos: usize, softwrap: bool, overlays: &[Overlay]) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(softwrap),
        TextAnnotations::default().add_overlay(overlays, None),
        char_pos,
    )
    .collect_to_str()
}

#[test]
fn overlay() {
    assert_eq!(
        overlay_text(
            "foobar",
            0,
            false,
            &[Overlay::new(0, "X"), Overlay::new(2, "\t")],
        ),
        "Xo  bar "
    );
    assert_eq!(
        overlay_text(
            &"foo ".repeat(10),
            0,
            true,
            &[
                Overlay::new(2, "\t"),
                Overlay::new(5, "\t"),
                Overlay::new(16, "X"),
            ]
        ),
        "fo   f  o foo \n.foo Xoo foo foo \n.foo foo foo  "
    );
}

fn annotate_text(text: &str, softwrap: bool, annotations: &[InlineAnnotation]) -> String {
    DocumentFormatter::new_at_prev_checkpoint(
        text.into(),
        &TextFormat::new_test(softwrap),
        TextAnnotations::default().add_inline_annotations(annotations, None),
        0,
    )
    .collect_to_str()
}

#[test]
fn annotation() {
    assert_eq!(
        annotate_text("bar", false, &[InlineAnnotation::new(0, "foo")]),
        "foobar "
    );
    assert_eq!(
        annotate_text(
            &"foo ".repeat(10),
            true,
            &[InlineAnnotation::new(0, "foo ")]
        ),
        "foo foo foo foo \n.foo foo foo foo \n.foo foo foo  "
    );
}

#[test]
fn annotation_and_overlay() {
    let annotations = [InlineAnnotation {
        char_idx: 0,
        text: "fooo".into(),
    }];
    let overlay = [Overlay {
        char_idx: 0,
        grapheme: "\t".into(),
    }];
    assert_eq!(
        DocumentFormatter::new_at_prev_checkpoint(
            "bbar".into(),
            &TextFormat::new_test(false),
            TextAnnotations::default()
                .add_inline_annotations(annotations.as_slice(), None)
                .add_overlay(overlay.as_slice(), None),
            0,
        )
        .collect_to_str(),
        "fooo  bar "
    );
}
