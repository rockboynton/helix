//! Diff backgrounds are independent of bright gutter colors.
use helix_view::{graphics::Color, Theme};

/// Render only visible baseline rows into space reserved by the working view's
/// line annotations. These rows never belong to the editable document.
pub fn render_removed(
    editor: &helix_view::Editor,
    doc: &helix_view::Document,
    view: &helix_view::View,
    surface: &mut tui::buffer::Buffer,
) {
    let Some(session) = editor
        .inline_diff_session
        .as_ref()
        .filter(|session| session.views[1] == view.id)
    else {
        return;
    };
    let Some(layout) = session.result.load_full().filter(|layout| {
        layout.versions
            == session
                .documents
                .map(|id| editor.document(id).unwrap().version())
    }) else {
        return;
    };
    let Some(side) = view.diff_side(doc) else {
        return;
    };
    let baseline = editor.document(session.documents[0]).unwrap();
    let offset = doc.view_offset(view.id);
    let line = doc
        .text()
        .char_to_line(offset.anchor.min(doc.text().len_chars()));
    let top = side
        .row(line)
        .saturating_sub(if offset.anchor == 0 { side.rows[0] } else { 0 })
        + offset.vertical_offset;
    let inner = view.inner_area(doc);
    let theme = &editor.theme;
    let loader = editor.syn_loader.load();
    let first = side
        .removed
        .partition_point(|block| block.row + block.lines.len() <= top);
    for block in side.removed[first..]
        .iter()
        .take_while(|block| block.row < top + inner.height as usize)
    {
        for source_line in block.lines.clone() {
            let row = block.row + source_line - block.lines.start;
            if !(top..top + inner.height as usize).contains(&row) {
                continue;
            }
            let viewport = helix_view::graphics::Rect::new(
                inner.x,
                inner.y + (row - top) as u16,
                inner.width,
                1,
            );
            let anchor = baseline.text().line_to_char(source_line);
            let end = anchor + baseline.text().line(source_line).len_chars();
            let words = &layout.sides[0].words;
            let begin = words.partition_point(|range| range.end <= anchor);
            let mut decorations = super::DecorationManager::default();
            decorations.add_decoration(Words(Some(
                words[begin..]
                    .iter()
                    .take_while(|range| range.start < end)
                    .map(|range| (range.clone(), word_background(theme, true)))
                    .collect(),
            )));
            let bg = background(theme, true);
            decorations.add_decoration(
                move |renderer: &mut crate::ui::document::TextRenderer,
                      pos: crate::ui::document::LinePos| {
                    renderer.set_line_background(Some(bg), pos.visual_line);
                },
            );
            let highlighter =
                crate::ui::EditorView::doc_syntax_highlighter(baseline, anchor, 1, &loader);
            let mut format = baseline.text_format(inner.width, Some(theme));
            format.soft_wrap = false;
            crate::ui::document::render_document(
                surface,
                viewport,
                baseline,
                helix_view::view::ViewPosition {
                    anchor,
                    horizontal_offset: offset.horizontal_offset,
                    vertical_offset: 0,
                },
                &helix_core::text_annotations::TextAnnotations::default(),
                highlighter,
                Vec::new(),
                theme,
                decorations,
                Some(format),
            );
            // Respect the configured gutter layout and icons rather than adding patch +/- prefixes.
            let mut x = view.area.x;
            for gutter in &view.gutters.layout {
                let width = gutter.width(view, doc);
                match gutter {
                    helix_view::editor::GutterType::LineNumbers => {
                        surface.set_stringn(
                            x,
                            viewport.y,
                            &format!("{:>width$}", source_line + 1),
                            width,
                            theme.get("ui.linenr"),
                        );
                    }
                    helix_view::editor::GutterType::Diff => {
                        let icon = &editor.icons.diff.deleted;
                        surface.set_stringn(
                            x,
                            viewport.y,
                            &icon.icon_char.to_string(),
                            width,
                            icon.style
                                .map(Into::into)
                                .unwrap_or_else(|| theme.get("diff.minus.gutter")),
                        );
                    }
                    _ => {}
                }
                x = x.saturating_add(width as u16);
            }
        }
    }
}

pub fn word_background(theme: &Theme, removed: bool) -> Color {
    let scope = if removed {
        "ui.diff.minus.word"
    } else {
        "ui.diff.plus.word"
    };
    theme
        .try_get_exact(scope)
        .and_then(|style| style.bg)
        .unwrap_or_else(|| {
            let light = matches!(
                default_background(theme.get("ui.background").bg, removed),
                Color::Rgb(255, 224, 224) | Color::Rgb(224, 246, 224)
            );
            match (light, removed) {
                (false, true) => Color::Rgb(80, 24, 24),
                (false, false) => Color::Rgb(24, 72, 32),
                (true, true) => Color::Rgb(255, 184, 184),
                (true, false) => Color::Rgb(184, 226, 184),
            }
        })
}

pub struct Words(pub Option<Vec<(std::ops::Range<usize>, Color)>>);
impl super::Decoration for Words {
    fn decorate_line(
        &mut self,
        renderer: &mut crate::ui::document::TextRenderer,
        _: crate::ui::document::LinePos,
    ) {
        if let Some(words) = self.0.take() {
            renderer.set_word_backgrounds(words);
        }
    }
}

pub fn background(theme: &Theme, removed: bool) -> Color {
    let (scope, legacy) = if removed {
        ("ui.diff.minus", "diff.minus")
    } else {
        ("ui.diff.plus", "diff.plus")
    };
    theme
        .try_get_exact(scope)
        .and_then(|style| style.bg)
        .or_else(|| theme.try_get_exact(legacy).and_then(|style| style.bg))
        .unwrap_or_else(|| default_background(theme.get("ui.background").bg, removed))
}

fn default_background(background: Option<Color>, removed: bool) -> Color {
    // A transparent/reset background has no known RGB value to blend against.
    // Use Delta-like dark fills, never the theme's bright foreground colors.
    let light = match background {
        Some(Color::Rgb(r, g, b)) => {
            299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) >= 128_000
        }
        Some(Color::White | Color::LightGray | Color::Indexed(7 | 15 | 252..=255)) => true,
        _ => false,
    };
    match (light, removed) {
        (false, true) => Color::Rgb(48, 0, 0),
        (false, false) => Color::Rgb(0, 40, 0),
        (true, true) => Color::Rgb(255, 224, 224),
        (true, false) => Color::Rgb(224, 246, 224),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bright_foregrounds_never_become_backgrounds_and_overrides_win() {
        let mut theme = Theme::default();
        theme.set(
            "diff.minus".into(),
            helix_view::graphics::Style::default().fg(Color::Rgb(255, 0, 0)),
        );
        theme.set(
            "diff.plus".into(),
            helix_view::graphics::Style::default().fg(Color::Rgb(255, 255, 0)),
        );
        assert_eq!(background(&theme, true), Color::Rgb(48, 0, 0));
        assert_eq!(background(&theme, false), Color::Rgb(0, 40, 0));
        theme.set(
            "diff.plus".into(),
            helix_view::graphics::Style::default().bg(Color::Rgb(1, 2, 3)),
        );
        assert_eq!(background(&theme, false), Color::Rgb(1, 2, 3));
        theme.set(
            "ui.diff.plus".into(),
            helix_view::graphics::Style::default().bg(Color::Rgb(4, 5, 6)),
        );
        assert_eq!(background(&theme, false), Color::Rgb(4, 5, 6));
    }
    #[test]
    fn transparent_reset_and_dark_themes_use_low_intensity_fills() {
        for bg in [
            None,
            Some(Color::Reset),
            Some(Color::Black),
            Some(Color::Rgb(40, 40, 40)),
        ] {
            assert_eq!(default_background(bg, true), Color::Rgb(48, 0, 0));
            assert_eq!(default_background(bg, false), Color::Rgb(0, 40, 0));
        }
    }
    #[test]
    fn light_themes_use_pale_fills() {
        for bg in [
            Color::White,
            Color::LightGray,
            Color::Indexed(255),
            Color::Rgb(250, 248, 240),
        ] {
            assert_eq!(
                default_background(Some(bg), true),
                Color::Rgb(255, 224, 224)
            );
            assert_eq!(
                default_background(Some(bg), false),
                Color::Rgb(224, 246, 224)
            );
        }
    }
}
