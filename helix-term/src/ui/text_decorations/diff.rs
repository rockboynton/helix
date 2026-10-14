//! Diff backgrounds are independent of bright gutter colors.
use helix_view::{graphics::Color, Theme};

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
