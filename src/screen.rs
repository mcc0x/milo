//! Draws styled cells and flushes only what differs from the last frame.

use std::io::{self, Write};

use crate::picture::Picture;
use crate::width::{char_width, str_width};

// ANSI palette indices, so colours follow the terminal theme.
pub const RED: u8 = 1;
pub const GREEN: u8 = 2;
pub const YELLOW: u8 = 3;
pub const BLUE: u8 = 4;
pub const MAGENTA: u8 = 5;
pub const CYAN: u8 = 6;

/// A background laid behind text without changing the text.
#[derive(Clone, Copy)]
pub enum Wash {
    /// The bar under the cursor.
    Bar = 1,
    /// A line the file gained.
    Added = 2,
    /// A line the file lost.
    Removed = 3,
}

/// Packed cell style: bits 0–8 hold the text colour (0 = default, n+1 =
/// palette colour n); bits 9–13 hold bold, italic, strike, faint and reverse
/// in that order; bits 14–15 hold the wash (0 = none).
/// Palette colours 0–15 follow the terminal theme; 16–255 are fixed.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Style(u16);

const FG: u16 = 0x1ff;
const BOLD: u16 = 1 << 9;
const ITALIC: u16 = 1 << 10;
const STRIKE: u16 = 1 << 11;
const FAINT: u16 = 1 << 12;
const REVERSE: u16 = 1 << 13;
const WASH_SHIFT: u16 = 14;
const WASH: u16 = 3 << WASH_SHIFT;

const _: () = assert!(
    Wash::Bar as u16 == 1
        && Wash::Added as u16 == 2
        && Wash::Removed as u16 == 3
        && Wash::Removed as u16 == WASH >> WASH_SHIFT
);

impl Style {
    pub const PLAIN: Style = Style(0);

    pub const fn fg(c: u8) -> Style {
        Style(c as u16 + 1)
    }

    pub const fn bold(self) -> Style {
        Style(self.0 | BOLD)
    }

    pub const fn italic(self) -> Style {
        Style(self.0 | ITALIC)
    }

    pub const fn strike(self) -> Style {
        Style(self.0 | STRIKE)
    }

    /// Receded, relative to whatever the theme's text colour is.
    pub const fn faint(self) -> Style {
        Style(self.0 | FAINT)
    }

    /// Text and background swapped.
    pub const fn reverse(self) -> Style {
        Style(self.0 | REVERSE)
    }

    fn with_wash(self, wash: Wash) -> Style {
        Style(self.0 & !WASH | (wash as u16) << WASH_SHIFT)
    }

    fn wash_id(self) -> usize {
        ((self.0 & WASH) >> WASH_SHIFT) as usize
    }

    fn sgr(self, out: &mut Vec<u8>, washes: &[u32; 3]) {
        out.extend_from_slice(b"\x1b[0");
        if self.0 & BOLD != 0 {
            out.extend_from_slice(b";1");
        }
        if self.0 & ITALIC != 0 {
            out.extend_from_slice(b";3");
        }
        if self.0 & FAINT != 0 {
            out.extend_from_slice(b";2");
        }
        if self.0 & REVERSE != 0 {
            out.extend_from_slice(b";7");
        }
        if self.0 & STRIKE != 0 {
            out.extend_from_slice(b";9");
        }
        let _ = match (self.0 & FG).checked_sub(1) {
            None => Ok(()),
            Some(c @ 0..8) => write!(out, ";{}", 30 + c),
            Some(c @ 8..16) => write!(out, ";{}", 82 + c),
            Some(c) => write!(out, ";38;5;{c}"),
        };
        // A wash is mixed from the terminal's own background. Where that is
        // not known the bar falls back on the theme's grey, and the other
        // washes on nothing: the lines they mark are marked in the margin too.
        let wash = self.wash_id();
        if wash != 0 {
            let rgb = washes[wash - 1];
            let _ = match (rgb, wash) {
                (0, wash) if wash == Wash::Bar as usize => write!(out, ";100"),
                (0, _) => Ok(()),
                _ => write!(out, ";48;2;{};{};{}", rgb >> 16 & 255, rgb >> 8 & 255, rgb & 255),
            };
        }
        out.push(b'm');
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    st: Style,
}

const BLANK: Cell = Cell { ch: ' ', st: Style::PLAIN };
/// Trailing half of a double-width character.
const CONT: char = '\0';
const SYNC_ON: &[u8] = b"\x1b[?2026h";

/// Where a picture sits on the screen, in cells.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Placement {
    id: u32,
    x: usize,
    y: usize,
    cols: usize,
    rows: usize,
}

/// What is known of the terminal's way with pictures, and the ones on it.
struct Pictures {
    /// Whether the terminal shows pictures; `None` until it has said.
    able: Option<bool>,
    /// The size of one cell in pixels: width, height.
    cell: (u16, u16),
    /// Placements asked for in this frame, and those sent in the last one.
    wanted: Vec<Placement>,
    shown: Vec<Placement>,
    /// A resize erases the placements, but the terminal still has the pictures.
    erased: bool,
    /// Data for pictures in `wanted` that are not already on the terminal.
    data: Vec<u8>,
}

impl Default for Pictures {
    fn default() -> Self {
        // Most terminals draw a cell about twice as tall as it is wide.
        Self { able: None, cell: (8, 16), wanted: Vec::new(), shown: Vec::new(), erased: false, data: Vec::new() }
    }
}

pub struct Screen {
    pub w: usize,
    pub h: usize,
    cur: Vec<Cell>,
    prev: Vec<Cell>,
    out: Vec<u8>,
    light_background: bool,
    /// Each wash is 0 while unknown, otherwise 0x01RRGGBB.
    washes: [u32; 3],
    /// The wash colours used for the cells on the terminal.
    prev_washes: [u32; 3],
    pictures: Pictures,
}

impl Screen {
    pub fn new() -> Screen {
        Screen {
            w: 0,
            h: 0,
            cur: Vec::new(),
            prev: Vec::new(),
            out: SYNC_ON.to_vec(),
            light_background: false,
            washes: [0; 3],
            prev_washes: [0; 3],
            pictures: Pictures::default(),
        }
    }

    /// Mixes the washes with the terminal's background to match its theme:
    /// the bar towards the text, gained lines towards green, lost lines towards red.
    pub fn set_background(&mut self, bg: [u8; 3]) {
        let bg = bg.map(u32::from);
        self.light_background = bg[0] * 299 + bg[1] * 587 + bg[2] * 114 >= 128_000;
        let towards = [if self.light_background { [0; 3] } else { [255; 3] }, [0, 190, 70], [230, 50, 50]];
        for (wash, (to, part)) in self.washes.iter_mut().zip(towards.into_iter().zip([14, 16, 16])) {
            let mix = |i: usize| (bg[i] * (100 - part) + to[i] * part) / 100;
            *wash = 1 << 24 | mix(0) << 16 | mix(1) << 8 | mix(2);
        }
    }

    /// Whether OSC 11 reported a light background; unknown is treated as dark.
    pub fn light_background(&self) -> bool {
        self.light_background
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.cur.clear();
        self.cur.resize(w * h, BLANK);
        self.prev.clear();
        // The erase below already puts plain blanks in every cell.
        self.prev.resize(w * h, BLANK);
        self.out.extend_from_slice(b"\x1b[0m\x1b[2J");
        self.pictures.erased = true;
    }

    pub fn clear(&mut self) {
        self.cur.fill(BLANK);
        self.pictures.wanted.clear();
        self.pictures.data.clear();
    }

    /// Notes that the terminal can, or cannot, show pictures.
    pub fn set_pictures(&mut self, able: bool) {
        self.pictures.able = Some(able);
    }

    /// Notes the size of one cell in pixels, by which a picture is fitted.
    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        if width > 0 && height > 0 {
            self.pictures.cell = (width, height);
        }
    }

    /// Notes that the terminal has answered all it will: one that has not
    /// said it shows pictures by now does not.
    pub fn answered(&mut self) {
        self.pictures.able.get_or_insert(false);
    }

    /// Whether the terminal shows pictures; `None` until it has said.
    pub fn pictures(&self) -> Option<bool> {
        self.pictures.able
    }

    pub fn cell_size(&self) -> (u16, u16) {
        self.pictures.cell
    }

    /// Asks for `picture` over the cells from (x, y), `cols` wide and `rows`
    /// tall, in the frame being drawn. Its data goes to the terminal once.
    pub fn show_picture(&mut self, picture: &Picture, x: usize, y: usize, cols: usize, rows: usize) {
        let pictures = &mut self.pictures;
        let at = Placement { id: picture.id, x, y, cols, rows };
        if let Some(wanted) = pictures.wanted.iter_mut().find(|wanted| wanted.id == at.id) {
            *wanted = at;
            return;
        }
        if !pictures.shown.iter().any(|shown| shown.id == at.id) {
            picture.transmit(&mut pictures.data);
        }
        pictures.wanted.push(at);
    }

    /// Blanks one row, including its colours.
    pub fn clear_row(&mut self, y: usize) {
        self.span(0, y, self.w).fill(BLANK);
    }

    /// The cells of row `y` from `x`, at most `w` wide, clipped to the screen.
    fn span(&mut self, x: usize, y: usize, w: usize) -> &mut [Cell] {
        if y >= self.h || x >= self.w {
            return &mut [];
        }
        let start = y * self.w + x;
        &mut self.cur[start..start + w.min(self.w - x)]
    }

    /// Draws `s` at (x, y) in at most `max_w` columns. Returns the columns used.
    pub fn put(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        let row = self.span(x, y, max_w);
        let mut used = 0;
        for ch in s.chars() {
            let cw = char_width(ch);
            if cw == 0 {
                continue;
            }
            if used + cw > row.len() {
                break;
            }
            row[used] = Cell { ch, st };
            if cw == 2 {
                row[used + 1] = Cell { ch: CONT, st };
            }
            used += cw;
        }
        used
    }

    /// Draws text ending just before `end`. Returns the columns used.
    pub fn put_right(&mut self, end: usize, y: usize, text: &str, st: Style) -> usize {
        let end = end.min(self.w);
        let width = str_width(text).min(end);
        self.put(end - width, y, width, text, st)
    }

    /// Draws a decimal number ending just before column `end`.
    pub fn put_number(&mut self, end: usize, y: usize, mut number: u32, st: Style) {
        for cell in self.span(0, y, end).iter_mut().rev() {
            let ch = char::from_digit(number % 10, 10).expect("remainder is a decimal digit");
            *cell = Cell { ch, st };
            number /= 10;
            if number == 0 {
                break;
            }
        }
    }

    /// Like `put`, but ends a string that does not fit with an ellipsis.
    pub fn put_fit(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        if max_w == 0 || str_width(s) <= max_w {
            return self.put(x, y, max_w, s, st);
        }
        let used = self.put(x, y, max_w - 1, s, st);
        used + self.put(x + used, y, 1, "…", st)
    }

    /// Like `put`, but shortens a string that does not fit in the middle,
    /// so a file name keeps both its start and its extension.
    pub fn put_middle(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        if max_w < 8 {
            return self.put_fit(x, y, max_w, s, st);
        }
        if str_width(s) <= max_w {
            return self.put(x, y, max_w, s, st);
        }
        let tail_chars = (max_w / 3).min(8);
        let tail_at = s.char_indices().rev().nth(tail_chars - 1).map_or(0, |(i, _)| i);
        let tail_w = str_width(&s[tail_at..]);
        let mut used = self.put(x, y, max_w.saturating_sub(tail_w + 1), s, st);
        used += self.put(x + used, y, 1, "…", st);
        used + self.put(x + used, y, tail_w, &s[tail_at..], st)
    }

    /// Gives a span a style, keeping what is written there.
    pub fn paint(&mut self, x: usize, y: usize, w: usize, st: Style) {
        for cell in self.span(x, y, w) {
            cell.st = st;
        }
    }

    /// Inverts a span, to pick out the changed part of a line.
    pub fn emphasise(&mut self, x: usize, y: usize, w: usize) {
        for cell in self.span(x, y, w) {
            cell.st = cell.st.reverse();
        }
    }

    /// Lays a wash behind a span, keeping each cell's own text and colour.
    pub fn wash(&mut self, x: usize, y: usize, w: usize, wash: Wash) {
        for cell in self.span(x, y, w) {
            cell.st = cell.st.with_wash(wash);
        }
    }

    /// Writes only the cells that changed since the last flush.
    pub fn flush(&mut self) {
        self.flush_to(|frame| {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(frame);
            let _ = stdout.flush();
        });
    }

    /// Places, moves or removes the pictures asked for in this frame.
    /// They lie over the cells, outside the frame of text.
    fn flush_picture(&mut self) {
        let pictures = &mut self.pictures;
        if !pictures.erased && pictures.wanted == pictures.shown {
            return;
        }
        for old in &pictures.shown {
            match pictures.wanted.iter().find(|at| at.id == old.id) {
                None => {
                    let _ = write!(self.out, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", old.id);
                }
                Some(at) if at != old || pictures.erased => {
                    // Take only the placement away; the data is still needed.
                    let _ = write!(self.out, "\x1b_Ga=d,d=i,i={},q=2\x1b\\", old.id);
                }
                Some(_) => {}
            }
        }
        self.out.append(&mut pictures.data);
        for at in &pictures.wanted {
            if pictures.erased || !pictures.shown.contains(at) {
                let _ = write!(
                    self.out,
                    "\x1b[{};{}H\x1b_Ga=p,i={},c={},r={},C=1,q=2\x1b\\",
                    at.y + 1,
                    at.x + 1,
                    at.id,
                    at.cols,
                    at.rows
                );
            }
        }
        pictures.shown.clone_from(&pictures.wanted);
        pictures.erased = false;
    }

    fn flush_to(&mut self, write_frame: impl FnOnce(&[u8])) {
        let washes = &self.washes;
        let prev_washes = &self.prev_washes;
        let palette_changed = washes != prev_washes;
        let mut active = None;
        let mut utf8 = [0u8; 4];
        for y in 0..self.h {
            let cur = &self.cur[y * self.w..(y + 1) * self.w];
            let prev = &mut self.prev[y * self.w..(y + 1) * self.w];
            let mut next_col = usize::MAX;
            for (x, (&cell, old)) in cur.iter().zip(prev).enumerate() {
                if cell != *old {
                    *old = cell;
                } else if !palette_changed
                    || !cell.st.wash_id().checked_sub(1).is_some_and(|i| washes[i] != prev_washes[i])
                {
                    continue;
                }
                // A wide glyph must be drawn from its first column, unless
                // the terminal has already drawn it while painting this row.
                let (first, glyph) = if cell.ch == CONT {
                    if next_col == x + 1 {
                        continue;
                    }
                    (x - 1, cur[x - 1])
                } else {
                    (x, cell)
                };
                if first != next_col {
                    let _ = write!(self.out, "\x1b[{};{}H", y + 1, first + 1);
                }
                if active != Some(glyph.st) {
                    glyph.st.sgr(&mut self.out, washes);
                    active = Some(glyph.st);
                }
                self.out.extend_from_slice(glyph.ch.encode_utf8(&mut utf8).as_bytes());
                let wide = cur.get(first + 1).is_some_and(|cell| cell.ch == CONT);
                next_col = first + 1 + usize::from(wide);
            }
        }
        self.flush_picture();
        self.prev_washes = self.washes;
        if self.out.len() > SYNC_ON.len() {
            self.out.extend_from_slice(b"\x1b[0m\x1b[?2026l");
            write_frame(&self.out);
            self.out.truncate(SYNC_ON.len());
        }
    }
}

#[cfg(test)]
impl Screen {
    /// A screen for drawing on without a terminal.
    pub fn sized(w: usize, h: usize) -> Self {
        let mut screen = Self::new();
        screen.resize(w, h);
        screen
    }

    /// The text of each row, without trailing blanks.
    pub fn text(&self) -> Vec<String> {
        (0..self.h)
            .map(|y| {
                let cells = &self.cur[y * self.w..(y + 1) * self.w];
                let mut line: String = cells.iter().filter(|c| c.ch != CONT).map(|c| c.ch).collect();
                line.truncate(line.trim_end_matches(' ').len());
                line
            })
            .collect()
    }

    /// Whether the row starts with the cursor's background shade.
    pub fn bar_at(&self, y: usize) -> bool {
        self.cur[y * self.w].st.wash_id() == Wash::Bar as usize
    }

    /// Where the frame being drawn asks for a picture: x, y, columns, rows.
    pub fn picture_at(&self) -> Option<(usize, usize, usize, usize)> {
        self.picture_placements().next()
    }

    /// All the pictures asked for in this frame, in drawing order.
    pub fn picture_placements(&self) -> impl Iterator<Item = (usize, usize, usize, usize)> + '_ {
        self.pictures.wanted.iter().map(|at| (at.x, at.y, at.cols, at.rows))
    }

    /// The placement of a particular picture in the current frame.
    pub fn placement_of(&self, picture: &Picture) -> Option<(usize, usize, usize, usize)> {
        self.pictures.wanted.iter().find(|at| at.id == picture.id).map(|at| (at.x, at.y, at.cols, at.rows))
    }

    /// Captures exactly what a flush would send to the terminal.
    pub fn emitted(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.flush_to(|frame| bytes.extend_from_slice(frame));
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_wider_than_the_screen_stops_at_the_end_of_its_row() {
        let mut screen = Screen::sized(4, 2);
        screen.put(1, 0, 8, "abcdef", Style::PLAIN);
        assert_eq!(screen.text(), [" abc", ""]);
    }

    #[test]
    fn a_wide_character_fits_only_when_both_columns_are_in_its_row() {
        let mut screen = Screen::sized(4, 2);
        screen.put(2, 0, 4, "界", Style::PLAIN);
        screen.put(3, 0, 4, "界 should not spill", Style::PLAIN);
        screen.put(3, 1, 4, "界", Style::PLAIN);
        assert_eq!(screen.text(), ["  界", ""]);
    }

    #[test]
    fn right_alignment_counts_columns_not_bytes() {
        let mut screen = Screen::sized(8, 1);
        assert_eq!(screen.put_right(7, 0, "−界", Style::PLAIN), 3);
        assert_eq!(screen.text(), ["    −界"]);
    }

    #[test]
    fn a_shortened_file_name_keeps_its_extension() {
        let mut screen = Screen::sized(12, 1);
        screen.put_middle(0, 0, 12, "very-long-name.txt", Style::PLAIN);
        assert_eq!(screen.text(), ["very-lo….txt"]);
    }

    fn frame(screen: &mut Screen) -> String {
        let mut written = String::new();
        screen.flush_to(|frame| written = String::from_utf8_lossy(frame).into_owned());
        written
    }

    #[test]
    fn a_picture_is_sent_once_then_only_moved_and_at_last_taken_away() {
        let picture = Picture::sample(40, 20);
        let mut screen = Screen::sized(20, 6);
        screen.show_picture(&picture, 2, 1, 5, 2);
        let first = frame(&mut screen);
        assert!(
            first.contains("a=t,f=100")
                && first.ends_with("\x1b[2;3H\x1b_Ga=p,i=1,c=5,r=2,C=1,q=2\x1b\\\x1b[0m\x1b[?2026l")
        );

        // The same picture in the same place needs nothing said.
        screen.clear();
        screen.show_picture(&picture, 2, 1, 5, 2);
        assert_eq!(frame(&mut screen), "");

        // Moved, it is placed again without its data.
        screen.clear();
        screen.show_picture(&picture, 4, 1, 5, 2);
        let moved = frame(&mut screen);
        assert!(!moved.contains("a=t") && moved.contains("a=d,d=i,i=1") && moved.contains("\x1b[2;5H\x1b_Ga=p"));

        // A frame without it takes it off the terminal altogether.
        screen.clear();
        assert!(frame(&mut screen).contains("a=d,d=I,i=1"));
        assert_eq!(frame(&mut screen), "");
    }

    #[test]
    fn two_pictures_move_and_leave_independently() {
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x10\0\0\0\x10";
        let a = Picture::embedded(PNG).unwrap();
        let b = Picture::embedded(PNG).unwrap();
        let mut screen = Screen::sized(20, 6);
        screen.show_picture(&a, 2, 1, 4, 2);
        screen.show_picture(&b, 12, 1, 4, 2);
        let first = frame(&mut screen);
        for (picture, col) in [(&a, 3), (&b, 13)] {
            assert_eq!(first.matches(&format!("a=t,f=100,i={},", picture.id)).count(), 1);
            assert!(first.contains(&format!("\x1b[2;{col}H\x1b_Ga=p,i={},c=4,r=2,", picture.id)));
        }

        screen.clear();
        screen.show_picture(&a, 3, 1, 4, 2);
        screen.show_picture(&b, 12, 1, 4, 2);
        let moved = frame(&mut screen);
        assert!(!moved.contains("a=t"));
        assert_eq!(moved.matches("a=p,").count(), 1);
        assert!(moved.contains(&format!("a=d,d=i,i={},", a.id)));
        assert!(moved.contains(&format!("\x1b[2;4H\x1b_Ga=p,i={},", a.id)));

        screen.resize(24, 6);
        let resized = frame(&mut screen);
        assert!(!resized.contains("a=t"));
        assert_eq!(resized.matches("a=p,").count(), 2);

        screen.clear();
        let empty = frame(&mut screen);
        assert!(empty.contains(&format!("a=d,d=I,i={},", a.id)));
        assert!(empty.contains(&format!("a=d,d=I,i={},", b.id)));
        assert_eq!(frame(&mut screen), "");
    }

    #[test]
    fn resizing_erases_old_text_without_rewriting_blank_cells() {
        let mut screen = Screen::sized(4, 1);
        screen.put(0, 0, 4, "old!", Style::PLAIN);
        assert!(frame(&mut screen).contains("old!"));

        screen.resize(80, 24);
        assert_eq!(frame(&mut screen), "\x1b[?2026h\x1b[0m\x1b[2J\x1b[0m\x1b[?2026l");
        assert_eq!(frame(&mut screen), "");
    }

    #[test]
    fn a_background_reply_repaints_only_the_washed_cells() {
        let mut screen = Screen::new();
        screen.resize(5, 1);
        screen.put(0, 0, 5, "ABC界", Style::PLAIN);
        screen.wash(0, 0, 1, Wash::Bar);
        screen.wash(3, 0, 2, Wash::Added);
        screen.flush_to(|frame| {
            assert!(frame.starts_with(b"\x1b[?2026h\x1b[0m\x1b[2J"));
            assert!(frame.ends_with(b"\x1b[0m\x1b[?2026l"));
        });

        screen.set_background([0; 3]);
        let mut wrote = false;
        screen.flush_to(|frame| {
            wrote = true;
            assert_eq!(
                frame,
                concat!(
                    "\x1b[?2026h\x1b[1;1H\x1b[0;48;2;35;35;35mA",
                    "\x1b[1;4H\x1b[0;48;2;0;30;11m界\x1b[0m\x1b[?2026l"
                )
                .as_bytes()
            );
        });
        assert!(wrote);
        screen.set_background([0; 3]);
        screen.flush_to(|_| panic!("the palette did not change"));
    }
}
