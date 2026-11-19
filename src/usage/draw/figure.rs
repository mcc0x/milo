//! Makes a coloured PNG from each figure's precompressed alpha mask.

use std::cell::RefCell;

use crate::picture::Picture;
use crate::screen::{GREEN, RED, Screen, Style, YELLOW};

use super::{Area, LARGE_ROWS};

const HEIGHT: usize = 64;
const PITCH: usize = 38;
const PERCENT: usize = 25;
const MASKS: &[u8] = include_bytes!("../marks/figures.alpha");
const FIGURES: usize = 103;
const INDEX_SIZE: usize = 4 * (FIGURES + 1);

/// The generator stores 0–100, then the two qualified figures. Each entry
/// is a complete PNG IDAT chunk, without the header or colour palette.
fn mask(text: &str) -> &'static [u8] {
    let number = match text {
        "<1%" => 101,
        "99+%" => 102,
        _ => text[..text.len() - 1].parse::<usize>().expect("a figure is a percent"),
    };
    assert!(number < FIGURES);
    let offset = |n| {
        let bytes = MASKS[4 * n..4 * n + 4].try_into().expect("the atlas has an offset for each figure");
        INDEX_SIZE + u32::from_be_bytes(bytes) as usize
    };
    &MASKS[offset(number)..offset(number + 1)]
}

/// Writes the small PNG chunks that aren't already in the atlas.
fn chunk(png: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).expect("a PNG chunk length fits");
    png.extend_from_slice(&len.to_be_bytes());
    let start = png.len();
    png.extend_from_slice(name);
    png.extend_from_slice(data);
    let mut crc = u32::MAX;
    for byte in &png[start..] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 * (crc & 1));
        }
    }
    png.extend_from_slice(&(!crc).to_be_bytes());
}

/// Text and images use different palettes but must keep the same warning level.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Green,
    Yellow,
    Red,
}

impl Level {
    fn style(self) -> Style {
        Style::fg(match self {
            Self::Green => GREEN,
            Self::Yellow => YELLOW,
            Self::Red => RED,
        })
    }

    fn rgb(self, light: bool) -> [u8; 3] {
        // Images cannot use theme palette entries. Pick fixed colours with
        // readable contrast against dark and light terminal backgrounds.
        const PALETTE: [[[u8; 3]; 3]; 2] = [
            [[0x3f, 0xb9, 0x50], [0xd2, 0x99, 0x22], [0xff, 0x7b, 0x72]],
            [[0x19, 0x70, 0x36], [0x86, 0x56, 0x00], [0xb4, 0x23, 0x2b]],
        ];
        PALETTE[usize::from(light)][self as usize]
    }
}

/// Share the displayed text and warning level between the terminal's drawing modes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Figure {
    pub text: &'static str,
    level: Level,
}

impl Figure {
    pub(super) fn new(text: &'static str, percent: f64) -> Self {
        let level = match percent {
            90.0.. => Level::Red,
            70.0..90.0 => Level::Yellow,
            _ => Level::Green,
        };
        Self { text, level }
    }

    pub(super) fn style(self) -> Style {
        self.level.style()
    }

    /// Cells across at the figure's natural aspect ratio, rounded up.
    pub(super) fn cols(self, (cell_w, cell_h): (u16, u16)) -> usize {
        (self.width() * usize::from(cell_h) * LARGE_ROWS).div_ceil(HEIGHT * usize::from(cell_w))
    }

    /// The caller has checked that the figure fits across `area`.
    pub(super) fn draw(self, screen: &mut Screen, area: Area, y: usize, slot: usize) {
        let light = screen.light_background();
        let cols = self.cols(screen.cell_size());
        IMAGES.with_borrow_mut(|images| {
            let cached = &mut images[slot];
            if cached.as_ref().is_none_or(|(figure, theme, _)| *figure != self || *theme != light) {
                *cached = Some((self, light, self.picture(light)));
            }
            let (_, _, picture) = cached.as_ref().expect("the image is cached");
            screen.show_picture(picture, area.x, y, cols, LARGE_ROWS);
        });
    }

    fn width(self) -> usize {
        (self.text.len() - 1) * PITCH + PERCENT
    }

    /// Reuse the compressed mask; only the PNG wrapper and palette vary.
    fn picture(self, light: bool) -> Picture {
        const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
        const ALPHA: [u8; 16] = [0, 17, 34, 51, 68, 85, 102, 119, 136, 153, 170, 187, 204, 221, 238, 255];
        const END: &[u8] = b"\0\0\0\0IEND\xaeB`\x82";
        let data = mask(self.text);
        let width = u8::try_from(self.width()).expect("a figure is fewer than 256 pixels across");
        let height = u8::try_from(HEIGHT).expect("the glyph height fits");
        let header = [0, 0, 0, width, 0, 0, 0, height, 4, 3, 0, 0, 0]; // 4-bit indexed PNG
        let palette = [self.level.rgb(light); ALPHA.len()];
        let header_len = SIGNATURE.len() + 3 * 12 + header.len() + palette.as_flattened().len() + ALPHA.len();
        let mut png = Vec::with_capacity(header_len + data.len() + END.len());
        png.extend_from_slice(SIGNATURE);
        chunk(&mut png, b"IHDR", &header);
        chunk(&mut png, b"PLTE", palette.as_flattened());
        chunk(&mut png, b"tRNS", &ALPHA);
        png.extend_from_slice(data);
        png.extend_from_slice(END);
        Picture::png(png.into()).expect("the assembled figure is a PNG")
    }
}

thread_local! {
    // One ID per displayed window: two equal figures may still be placed at
    // different cells. When a reading changes only that window is replaced.
    static IMAGES: RefCell<[Option<(Figure, bool, Picture)>; 5]> = const { RefCell::new([const { None }; 5]) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::draw::{Chart, Hint, Metric, Report, Tool};
    use crate::usage::{Activity, Source, Window};

    fn metric(percent: f64) -> Metric<'static> {
        Metric::limit(Window { minutes: 300, percent, resets: None, observed: 0 }, 0, Source::CodexSession)
    }

    #[test]
    fn confirmation_selects_pictures_and_leaves_other_terminals_with_blocks() {
        let area = Area { x: 4, y: 2, w: 16, h: 5 };
        let metric = metric(73.0);
        for able in [None, Some(false), Some(true)] {
            let mut screen = Screen::sized(20, 7);
            if let Some(able) = able {
                screen.set_pictures(able);
            }
            metric.panel(&mut screen, area, Chart::new(area.h), 0);
            let rows = screen.text();
            assert_eq!(rows[2], "    5h");
            if able == Some(true) {
                assert_eq!(screen.picture_at(), Some((4, 3, 10, 3)));
                assert!(rows[3..6].iter().all(String::is_empty));
            } else {
                assert_eq!(screen.picture_at(), None);
                assert_eq!(&rows[3..6], ["    ▀▀▙ ▀▀█", "    ▟▀▀  ▟▀", "    █▄▄  █ %"]);
            }
        }
    }

    #[test]
    fn short_panels_keep_plain_text_even_when_pictures_are_supported() {
        let mut screen = Screen::sized(32, 2);
        screen.set_pictures(true);
        let mut report = Report::new(Tool::Codex, None, Hint::no_limits(Tool::Codex), 0);
        report.first = metric(73.0);
        report.panel(&mut screen, Area { x: 0, y: 0, w: 32, h: 2 }, 2, Activity::default);
        assert_eq!(screen.text(), ["██ Codex 5h 27% left", ""]);
        assert_eq!(screen.picture_at(), None);
    }

    #[test]
    fn equal_figures_can_be_placed_twice_and_are_not_sent_again() {
        let mut screen = Screen::sized(32, 5);
        screen.set_pictures(true);
        let area = Area { x: 0, y: 0, w: 16, h: 5 };
        let draw = |screen: &mut Screen, percent| {
            screen.clear();
            metric(27.0).panel(screen, area, Chart::new(area.h), 0);
            metric(percent).panel(screen, Area { x: 16, ..area }, Chart::new(area.h), 1);
        };
        let uploads =
            |screen: &mut Screen| String::from_utf8(screen.emitted()).unwrap().matches("\x1b_Ga=t,f=100,").count();
        draw(&mut screen, 27.0);
        assert_eq!(screen.picture_placements().collect::<Vec<_>>(), [(0, 1, 10, 3), (16, 1, 10, 3)]);
        assert_eq!(uploads(&mut screen), 2);
        draw(&mut screen, 27.0);
        assert!(screen.emitted().is_empty());
        draw(&mut screen, 73.0);
        assert_eq!(uploads(&mut screen), 1);
    }

    #[test]
    fn the_three_levels_choose_matching_text_and_picture_hues() {
        for (percent, ansi, dark, light) in [
            (69.9, GREEN, [0x3f, 0xb9, 0x50], [0x19, 0x70, 0x36]),
            (70.0, YELLOW, [0xd2, 0x99, 0x22], [0x86, 0x56, 0x00]),
            (89.9, YELLOW, [0xd2, 0x99, 0x22], [0x86, 0x56, 0x00]),
            (90.0, RED, [0xff, 0x7b, 0x72], [0xb4, 0x23, 0x2b]),
        ] {
            let figure = metric(percent).number.unwrap();
            assert!(figure.style() == Style::fg(ansi));
            assert_eq!(figure.level.rgb(false), dark);
            assert_eq!(figure.level.rgb(true), light);
        }
    }
}
