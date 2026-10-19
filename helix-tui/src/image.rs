//! Kitty Unicode placements: image pixels follow ordinary compositor cells.
use crate::buffer::Buffer;
use base64::{engine::general_purpose::STANDARD, Engine};
use helix_view::graphics::{Color, Rect, Style};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

// Protocol data, first 128 entries of Kitty's rowcolumn-diacritics.txt:
// https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders
const MARKS: &[u32] = &[
    0x305, 0x30d, 0x30e, 0x310, 0x312, 0x33d, 0x33e, 0x33f, 0x346, 0x34a, 0x34b, 0x34c, 0x350,
    0x351, 0x352, 0x357, 0x35b, 0x363, 0x364, 0x365, 0x366, 0x367, 0x368, 0x369, 0x36a, 0x36b,
    0x36c, 0x36d, 0x36e, 0x36f, 0x483, 0x484, 0x485, 0x486, 0x487, 0x592, 0x593, 0x594, 0x595,
    0x597, 0x598, 0x599, 0x59c, 0x59d, 0x59e, 0x59f, 0x5a0, 0x5a1, 0x5a8, 0x5a9, 0x5ab, 0x5ac,
    0x5af, 0x5c4, 0x610, 0x611, 0x612, 0x613, 0x614, 0x615, 0x616, 0x617, 0x657, 0x658, 0x659,
    0x65a, 0x65b, 0x65d, 0x65e, 0x6d6, 0x6d7, 0x6d8, 0x6d9, 0x6da, 0x6db, 0x6dc, 0x6df, 0x6e0,
    0x6e1, 0x6e2, 0x6e4, 0x6e7, 0x6e8, 0x6eb, 0x6ec, 0x730, 0x732, 0x733, 0x735, 0x736, 0x73a,
    0x73d, 0x73f, 0x740, 0x741, 0x743, 0x745, 0x747, 0x749, 0x74a, 0x7eb, 0x7ec, 0x7ed, 0x7ee,
    0x7ef, 0x7f0, 0x7f1, 0x7f3, 0x816, 0x817, 0x818, 0x819, 0x81b, 0x81c, 0x81d, 0x81e, 0x81f,
    0x820, 0x821, 0x822, 0x823, 0x825, 0x826, 0x827, 0x829, 0x82a, 0x82b, 0x82c,
];

#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    id: u32,
    encoded: String,
    pub width: u32,
    pub height: u32,
}

impl Image {
    /// Bounded PNG transport. Decoding belongs to the terminal, never the UI thread.
    pub fn png(bytes: &[u8]) -> Result<Arc<Self>, &'static str> {
        if bytes.len() < 33
            || bytes.len() > 16 * 1024 * 1024
            || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
            || bytes[8..12] != 13u32.to_be_bytes()
            || &bytes[12..16] != b"IHDR"
        {
            return Err("Invalid or oversized PNG (limit: 16 MiB)");
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 32_000_000 {
            return Err("PNG dimensions exceed preview limit (32 megapixels)");
        }
        static NEXT_ID: AtomicU32 = AtomicU32::new(1);
        // 24-bit foreground IDs; reserve a process-specific range to avoid other TUIs.
        let id = ((std::process::id()
            .wrapping_mul(4099)
            .wrapping_add(NEXT_ID.fetch_add(1, Ordering::Relaxed)))
            & 0xffffff)
            .max(1);
        Ok(Arc::new(Self {
            id,
            encoded: STANDARD.encode(bytes),
            width,
            height,
        }))
    }

    pub fn byte_size(&self) -> usize {
        self.encoded.len()
    }

    pub fn render(self: &Arc<Self>, area: Rect, surface: &mut Buffer, background: Style) {
        let area = area.intersection(surface.area);
        let width = area.width.min(MARKS.len() as u16);
        let height = area.height.min(MARKS.len() as u16);
        if width == 0 || height == 0 {
            return;
        }
        let style = background.fg(Color::Rgb(
            (self.id >> 16) as u8,
            (self.id >> 8) as u8,
            self.id as u8,
        ));
        for y in 0..height {
            for x in 0..width {
                let symbol = format!(
                    "\u{10eeee}{}{}",
                    char::from_u32(MARKS[y as usize]).unwrap(),
                    char::from_u32(MARKS[x as usize]).unwrap()
                );
                surface[(area.x + x, area.y + y)].reset();
                surface[(area.x + x, area.y + y)]
                    .set_symbol(&symbol)
                    .set_style(style);
            }
        }
        surface.images.push(Placement {
            image: self.clone(),
            width,
            height,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    image: Arc<Image>,
    width: u16,
    height: u16,
}

/// Only enables known Unicode-placement terminals, not every Kitty-like terminal.
/// A patched Zellij server can advertise its Unicode-placement interpreter.
/// Stock Zellij and tmux are not assumed to support it.
pub fn supported() -> bool {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    supported_in_environment(
        std::env::var_os("TMUX").is_some(),
        std::env::var_os("ZELLIJ").is_some(),
        std::env::var("ZELLIJ_KITTY_UNICODE_PLACEHOLDERS").is_ok_and(|value| value == "1"),
        &program,
        &term,
    )
}

fn supported_in_environment(
    tmux: bool,
    zellij: bool,
    zellij_unicode: bool,
    program: &str,
    term: &str,
) -> bool {
    !tmux
        && (!zellij || zellij_unicode)
        && (program.eq_ignore_ascii_case("ghostty")
            || program.eq_ignore_ascii_case("kitty")
            || term == "xterm-ghostty"
            || term == "xterm-kitty")
}

#[derive(Debug, Default)]
pub(crate) struct ImageState(BTreeMap<u32, (u16, u16)>);
impl ImageState {
    pub fn update(&mut self, placements: &[Placement]) -> io::Result<Vec<u8>> {
        let mut commands = Vec::new();
        for id in self
            .0
            .keys()
            .filter(|id| !placements.iter().any(|p| p.image.id == **id))
        {
            write!(commands, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
        }
        self.0
            .retain(|id, _| placements.iter().any(|p| p.image.id == *id));
        for p in placements {
            let image = &p.image;
            if !self.0.contains_key(&image.id) {
                log::debug!(
                    "Kitty image upload: id={}, {} × {} pixels, {} × {} cells, {} encoded bytes",
                    image.id,
                    image.width,
                    image.height,
                    p.width,
                    p.height,
                    image.byte_size()
                );
                let chunks = image.encoded.as_bytes().chunks(4096);
                let last = chunks.len().saturating_sub(1);
                for (i, chunk) in chunks.enumerate() {
                    if i == 0 {
                        write!(
                            commands,
                            "\x1b_Ga=t,f=100,t=d,i={},q=2,m={};",
                            image.id,
                            u8::from(i != last)
                        )?;
                    } else {
                        write!(commands, "\x1b_Gm={},q=2;", u8::from(i != last))?;
                    }
                    commands.write_all(chunk)?;
                    commands.write_all(b"\x1b\\")?;
                }
            }
            if self.0.get(&image.id) != Some(&(p.width, p.height)) {
                write!(
                    commands,
                    "\x1b_Ga=p,U=1,i={},p=1,c={},r={},q=2\x1b\\",
                    image.id, p.width, p.height
                )?;
                self.0.insert(image.id, (p.width, p.height));
            }
        }
        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_a_capable_outer_terminal_and_a_verified_zellij_server() {
        for (tmux, zellij, patched, program, term, expected) in [
            (false, false, false, "ghostty", "xterm-256color", true),
            (false, true, false, "ghostty", "xterm-256color", false),
            (false, true, true, "ghostty", "xterm-256color", true),
            (true, true, true, "ghostty", "xterm-256color", false),
            (false, true, true, "unknown", "xterm-256color", false),
            (false, true, true, "", "xterm-kitty", true),
        ] {
            assert_eq!(
                supported_in_environment(tmux, zellij, patched, program, term),
                expected,
                "tmux={tmux}, zellij={zellij}, patched={patched}, program={program}, term={term}"
            );
        }
    }

    fn image() -> Arc<Image> {
        Image::png(include_bytes!("../../contrib/helix.png")).unwrap()
    }

    #[test]
    fn placeholders_are_single_cells_and_overlays_erase_them_normally() {
        let mut surface = Buffer::empty(Rect::new(0, 0, 12, 8));
        let image = image();
        image.render(Rect::new(2, 1, 5, 4), &mut surface, Style::default());
        assert_eq!(surface.images.len(), 1);
        assert_eq!(surface[(2, 1)].symbol.as_str(), "\u{10eeee}\u{305}\u{305}");
        assert_eq!(surface[(3, 2)].symbol.as_str(), "\u{10eeee}\u{30d}\u{30d}");
        assert_eq!(surface[(3, 2)].width(), 1);
        assert_eq!(surface[(0, 0)].symbol.as_str(), " ");
        surface.clear(Rect::new(3, 2, 1, 1));
        assert_eq!(surface[(3, 2)].symbol.as_str(), " ");
        assert!(surface[(4, 2)].symbol.starts_with('\u{10eeee}'));
        surface.reset();
        assert!(surface.images.is_empty());
    }

    #[test]
    fn uploads_are_chunked_reused_resized_and_deleted_by_own_id_only() {
        let image = image();
        let mut state = ImageState::default();
        let mut placement = Placement {
            image: image.clone(),
            width: 30,
            height: 20,
        };
        let commands = state.update(&[placement.clone()]).unwrap();
        let string = String::from_utf8(commands.clone()).unwrap();
        assert!(string.contains("a=t,f=100,t=d"));
        assert!(string.contains("a=p,U=1"));
        let mut png = String::new();
        for command in string.split("\x1b_G").skip(1) {
            if let Some((header, body)) = command.strip_suffix("\x1b\\").unwrap().split_once(';') {
                assert!(header.contains("q=2"));
                assert!(body.len() <= 4096);
                png.push_str(body);
            }
        }
        assert_eq!(
            STANDARD.decode(png).unwrap(),
            include_bytes!("../../contrib/helix.png")
        );
        assert!(state.update(&[placement.clone()]).unwrap().is_empty());
        placement.width = 40;
        let resized = String::from_utf8(state.update(&[placement]).unwrap()).unwrap();
        assert!(!resized.contains("a=t"));
        assert!(resized.contains("c=40"));
        let deleted = String::from_utf8(state.update(&[]).unwrap()).unwrap();
        assert_eq!(deleted, format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", image.id));
        assert!(state.update(&[]).unwrap().is_empty());
    }

    #[test]
    fn reject_unsafe_sizes_and_never_emit_input_as_raw_terminal_controls() {
        assert!(Image::png(b"\x1b]52;bad").is_err());
        let mut bytes = include_bytes!("../../contrib/helix.png").to_vec();
        bytes[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(Image::png(&bytes).is_err());
    }
}
