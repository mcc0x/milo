//! Draws styled cells and flushes only what differs from the last frame.

use std::io::{self, Write};
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::picture::Picture;
use crate::width::{CellWidth, cell_width, char_width, str_width};

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
/// in that order; bits 14–15 hold the wash (0 = none), bit 16 underlines.
/// Palette colours 0–15 follow the terminal theme; 16–255 are fixed.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Style(u32);

const FG: u32 = 0x1ff;
const BOLD: u32 = 1 << 9;
const ITALIC: u32 = 1 << 10;
const STRIKE: u32 = 1 << 11;
const FAINT: u32 = 1 << 12;
const REVERSE: u32 = 1 << 13;
const WASH_SHIFT: u32 = 14;
const WASH: u32 = 3 << WASH_SHIFT;
const UNDERLINE: u32 = 1 << 16;

impl Style {
    pub const PLAIN: Style = Style(0);

    pub const fn fg(c: u8) -> Style {
        Style(c as u32 + 1)
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
        Style(self.0 & !WASH | (wash as u32) << WASH_SHIFT)
    }

    fn wash_id(self) -> usize {
        ((self.0 & WASH) >> WASH_SHIFT) as usize
    }

    fn sgr(self, out: &mut Vec<u8>, washes: &[u32; 3]) {
        out.extend_from_slice(b"\x1b[0");
        if self.0 & BOLD != 0 {
            out.extend_from_slice(b";1");
        }
        if self.0 & UNDERLINE != 0 {
            out.extend_from_slice(b";4");
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
    glyph: Glyph,
    st: Style,
}

impl Cell {
    const fn new(ch: char, st: Style) -> Self {
        Self { glyph: Glyph(ch as u32), st }
    }
}

/// A Unicode scalar or an index past that range into the frame's multi-scalar glyphs.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Glyph(u32);

const MARKED: u32 = char::MAX as u32 + 1;

impl Glyph {
    fn scalar(self) -> char {
        char::from_u32(self.0).expect("unmarked glyph is a Unicode scalar")
    }

    fn marked(self) -> Option<usize> {
        self.0.checked_sub(MARKED).map(|i| i as usize)
    }
}

/// Only glyphs with attached scalars need spill storage. Both buffers
/// are reused across frames; ordinary cells stay a single scalar wide.
#[derive(Default)]
struct Marks {
    text: String,
    ranges: Vec<Range<usize>>,
}

impl Marks {
    fn clear(&mut self) {
        self.text.clear();
        self.ranges.clear();
    }

    fn get(&self, glyph: Glyph) -> Option<&str> {
        glyph.marked().map(|i| &self.text[self.ranges[i].clone()])
    }

    fn add(&mut self, glyph: &mut Glyph, mark: char) {
        let i = glyph.marked().unwrap_or_else(|| {
            let i = self.ranges.len();
            let start = self.text.len();
            self.text.push(glyph.scalar());
            self.ranges.push(start..start);
            *glyph = Glyph(MARKED + u32::try_from(i).expect("a screen cannot hold four billion marked glyphs"));
            i
        });
        self.text.push(mark);
        self.ranges[i].end = self.text.len();
    }
}

const BLANK: Cell = Cell::new(' ', Style::PLAIN);
/// Trailing half of a double-width character.
const CONT: Glyph = Glyph(0);
const SYNC_ON: &[u8] = b"\x1b[?2026h";
const PICTURE_RESIZE_DELAY: Duration = Duration::from_millis(80);

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
    /// The current request and the last set uploaded, which help can hide briefly.
    wanted: Vec<Placement>,
    uploaded: Vec<Placement>,
    /// IDs whose deletion hasn't been written yet, kept for cleanup if a write fails.
    pending_deletes: Vec<u32>,
    hidden: bool,
    /// The end of the current resize burst, while picture uploads are held.
    resize_due: Option<Instant>,
    /// Data for pictures in `wanted` that are not already on the terminal.
    data: Vec<u8>,
}

impl Default for Pictures {
    fn default() -> Self {
        // Most terminals draw a cell about twice as tall as it is wide.
        Self {
            able: None,
            cell: (8, 16),
            wanted: Vec::new(),
            uploaded: Vec::new(),
            pending_deletes: Vec::new(),
            hidden: false,
            resize_due: None,
            data: Vec::new(),
        }
    }
}

pub struct Screen {
    pub w: usize,
    pub h: usize,
    /// Rows above row 0 that a pane drawn under another's strip leaves alone.
    top: usize,
    cur: Vec<Cell>,
    prev: Vec<Cell>,
    marks: Marks,
    prev_marks: Marks,
    /// Previous cells still refer to `marks` until the next clear swaps arenas.
    marks_flushed: bool,
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
            top: 0,
            cur: Vec::new(),
            prev: Vec::new(),
            marks: Marks::default(),
            prev_marks: Marks::default(),
            marks_flushed: false,
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
        self.marks.clear();
        self.prev_marks.clear();
        self.marks_flushed = false;
        self.prev.resize(w * h, BLANK);
        self.out.extend_from_slice(b"\x1b[0m\x1b[2J");
        // Erasing the display makes some terminals, Rex among them, drop the
        // pictures' data as well. Free it everywhere and upload it afresh.
        for old in self.pictures.uploaded.drain(..) {
            self.pictures.pending_deletes.push(old.id);
            delete_picture(&mut self.out, old.id, 'I');
        }
        self.pictures.wanted.clear();
        self.pictures.data.clear();
        self.pictures.hidden = false;
    }

    pub(crate) fn resize_interactive(&mut self, w: usize, h: usize, now: Instant) {
        // The first picture can go out immediately. Once erased, wait for
        // the last resize before encoding and uploading it all over again.
        if self.pictures.resize_due.is_some() || !self.pictures.uploaded.is_empty() {
            self.pictures.resize_due = Some(now + PICTURE_RESIZE_DELAY);
        }
        self.resize(w, h);
    }

    pub(crate) fn picture_redraw_due(&self) -> Option<Instant> {
        self.pictures.resize_due
    }

    pub(crate) fn resume_pictures(&mut self) {
        self.pictures.resize_due = None;
    }

    /// Draws with the top `rows` rows left out: row 0 for `draw` is the row under them.
    pub fn below(&mut self, rows: usize, draw: impl FnOnce(&mut Screen)) {
        let rows = rows.min(self.h);
        self.top += rows;
        self.h -= rows;
        draw(self);
        self.top -= rows;
        self.h += rows;
    }

    pub fn clear(&mut self) {
        self.cur.fill(BLANK);
        if std::mem::take(&mut self.marks_flushed) {
            std::mem::swap(&mut self.marks, &mut self.prev_marks);
        }
        self.marks.clear();
        self.pictures.wanted.clear();
        self.pictures.data.clear();
    }

    pub fn underline(&mut self, y: usize, span: Range<usize>) {
        for cell in self.styled_span(span.start, y, span.end.saturating_sub(span.start)) {
            cell.st.0 |= UNDERLINE;
        }
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
        if pictures.resize_due.is_some() {
            return;
        }
        let at = Placement { id: picture.id, x, y: y + self.top, cols, rows };
        if let Some(wanted) = pictures.wanted.iter_mut().find(|wanted| wanted.id == at.id) {
            *wanted = at;
            return;
        }
        if !pictures.uploaded.iter().any(|old| old.id == at.id) {
            picture.transmit(&mut pictures.data);
        }
        pictures.wanted.push(at);
    }

    fn row_room(&self, x: usize, y: usize, max_w: usize) -> usize {
        if y < self.h { max_w.min(self.w.saturating_sub(x)) } else { 0 }
    }

    /// The cells of row `y` from `x`, at most `w` wide, clipped to the screen.
    fn span(&mut self, x: usize, y: usize, w: usize) -> (&mut [Cell], &mut Marks) {
        let width = self.row_room(x, y, w);
        let cells = if width == 0 {
            &mut [][..]
        } else {
            let start = (y + self.top) * self.w + x;
            &mut self.cur[start..start + width]
        };
        (cells, &mut self.marks)
    }

    /// A wide glyph has one style, even when the requested span hits only one of its columns.
    fn styled_span(&mut self, x: usize, y: usize, w: usize) -> &mut [Cell] {
        let width = self.row_room(x, y, w);
        if width == 0 {
            return &mut [];
        }
        let y = y + self.top;
        let row = &mut self.cur[y * self.w..(y + 1) * self.w];
        let start = x - usize::from(row[x].glyph == CONT);
        let end = x + width;
        let end = end + usize::from(row.get(end).is_some_and(|cell| cell.glyph == CONT));
        &mut row[start..end]
    }

    /// Overwriting one column of a wide glyph erases the other column too.
    fn cut_wide(&mut self, x: usize, y: usize, used: usize, starts_on_cont: bool) {
        if used == 0 {
            return;
        }
        let start = (y + self.top) * self.w;
        if starts_on_cont && x > 0 {
            self.cur[start + x - 1].glyph = BLANK.glyph;
        }
        let end = x + used;
        if end < self.w && self.cur[start + end].glyph == CONT {
            self.cur[start + end].glyph = BLANK.glyph;
        }
    }

    /// Draws `s` at (x, y) in at most `max_w` columns. Returns the columns used.
    pub fn put(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        let (row, marks) = self.span(x, y, max_w);
        let starts_on_cont = row.first().is_some_and(|cell| cell.glyph == CONT);
        let (mut used, mut base) = (0, 0);
        for ch in s.chars() {
            let cw = match cell_width(ch) {
                CellWidth::Attach => {
                    if used > 0 {
                        marks.add(&mut row[base].glyph, ch);
                    }
                    continue;
                }
                CellWidth::Skip => continue,
                CellWidth::Narrow => 1,
                CellWidth::Wide => 2,
            };
            if used + cw > row.len() {
                break;
            }
            base = used;
            row[base] = Cell::new(ch, st);
            if cw == 2 {
                row[base + 1] = Cell { glyph: CONT, st };
            }
            used += cw;
        }
        self.cut_wide(x, y, used, starts_on_cont);
        used
    }

    /// Draws text ending just before `end`. Returns the columns used.
    pub fn put_right(&mut self, end: usize, y: usize, text: &str, st: Style) -> usize {
        let end = self.row_room(0, y, end);
        if end == 0 {
            return 0;
        }
        let mut width = 0;
        for ch in text.chars() {
            let next = width + char_width(ch);
            if next > end {
                break;
            }
            width = next;
        }
        self.put(end - width, y, width, text, st)
    }

    /// Draws a decimal number ending just before column `end`.
    pub fn put_number(&mut self, end: usize, y: usize, mut number: u64, st: Style) {
        let end = end.min(self.w);
        let (mut used, mut starts_on_cont) = (0, false);
        for cell in self.span(0, y, end).0.iter_mut().rev() {
            let digit = u8::try_from(number % 10).expect("remainder is a decimal digit");
            let ch = char::from(b'0' + digit);
            used += 1;
            starts_on_cont = cell.glyph == CONT;
            *cell = Cell::new(ch, st);
            number /= 10;
            if number == 0 {
                break;
            }
        }
        self.cut_wide(end - used, y, used, starts_on_cont);
    }

    /// Like `put`, but ends a string that does not fit with an ellipsis.
    pub fn put_fit(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        let max_w = self.row_room(x, y, max_w);
        if max_w == 0 {
            return 0;
        }
        if str_width(s) <= max_w {
            return self.put(x, y, max_w, s, st);
        }
        let used = self.put(x, y, max_w - 1, s, st);
        used + self.put(x + used, y, 1, "…", st)
    }

    /// Like `put`, but shortens a string that does not fit in the middle,
    /// so a file name keeps both its start and its extension.
    pub fn put_middle(&mut self, x: usize, y: usize, max_w: usize, s: &str, st: Style) -> usize {
        let max_w = self.row_room(x, y, max_w);
        if max_w < 8 {
            return self.put_fit(x, y, max_w, s, st);
        }
        if str_width(s) <= max_w {
            return self.put(x, y, max_w, s, st);
        }
        let tail_chars = (max_w / 3).min(8);
        let tail_at =
            s.char_indices().rev().filter(|&(_, ch)| char_width(ch) > 0).nth(tail_chars - 1).map_or(0, |(i, _)| i);
        let tail_w = str_width(&s[tail_at..]);
        let mut used = self.put(x, y, max_w.saturating_sub(tail_w + 1), s, st);
        used += self.put(x + used, y, 1, "…", st);
        used + self.put(x + used, y, tail_w, &s[tail_at..], st)
    }

    /// Gives a span a style, keeping what is written there.
    pub fn paint(&mut self, x: usize, y: usize, w: usize, st: Style) {
        for cell in self.styled_span(x, y, w) {
            cell.st = st;
        }
    }

    /// Inverts a span, to pick out the changed part of a line.
    pub fn emphasise(&mut self, x: usize, y: usize, w: usize) {
        for cell in self.styled_span(x, y, w) {
            cell.st = cell.st.reverse();
        }
    }

    /// Lays a wash behind a span, keeping each cell's own text and colour.
    pub fn wash(&mut self, x: usize, y: usize, w: usize, wash: Wash) {
        for cell in self.styled_span(x, y, w) {
            cell.st = cell.st.with_wash(wash);
        }
    }

    /// Release even the hidden uploads before giving the terminal back.
    pub fn release_pictures(&mut self, send: impl FnOnce(&[u8])) {
        if self.pictures.uploaded.is_empty() && self.pictures.pending_deletes.is_empty() {
            return;
        }
        self.out.truncate(SYNC_ON.len());
        // An interrupted frame may have stopped in the middle of a control sequence.
        self.out.push(0x18);
        for id in self.pictures.pending_deletes.drain(..) {
            delete_picture(&mut self.out, id, 'I');
        }
        for old in self.pictures.uploaded.drain(..) {
            delete_picture(&mut self.out, old.id, 'I');
        }
        send(&self.out[SYNC_ON.len()..]);
        self.out.truncate(SYNC_ON.len());
    }

    /// Places, moves or removes the pictures asked for in this frame.
    /// They lie over the cells, outside the frame of text.
    fn flush_picture(&mut self) -> usize {
        let pictures = &mut self.pictures;
        if pictures.wanted.is_empty() {
            if !pictures.hidden && !pictures.uploaded.is_empty() {
                for old in &pictures.uploaded {
                    delete_picture(&mut self.out, old.id, 'i');
                }
                pictures.hidden = true;
            }
            return self.out.len();
        }
        let place_again = pictures.hidden;
        if !place_again && pictures.wanted == pictures.uploaded {
            return self.out.len();
        }
        for old in &pictures.uploaded {
            match pictures.wanted.iter().find(|at| at.id == old.id) {
                None => {
                    pictures.pending_deletes.push(old.id);
                    delete_picture(&mut self.out, old.id, 'I');
                }
                Some(at) if !pictures.hidden && at != old => {
                    delete_picture(&mut self.out, old.id, 'i');
                }
                Some(_) => {}
            }
        }
        let data_at = self.out.len();
        for at in &pictures.wanted {
            if place_again || !pictures.uploaded.contains(at) {
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
        pictures.uploaded.clone_from(&pictures.wanted);
        pictures.hidden = false;
        data_at
    }

    /// Writes only the cells that changed since the last flush.
    pub fn flush(&mut self, mut send: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        let washes = &self.washes;
        let prev_washes = &self.prev_washes;
        let palette_changed = washes != prev_washes;
        let same_arena = self.marks_flushed;
        let prev_marks = if same_arena { &self.marks } else { &self.prev_marks };
        let mut active = None;
        let mut utf8 = [0u8; 4];
        for y in 0..self.h {
            let cur = &self.cur[y * self.w..(y + 1) * self.w];
            let prev = &mut self.prev[y * self.w..(y + 1) * self.w];
            let mut next_col = usize::MAX;
            for (x, (&cell, old)) in cur.iter().zip(prev).enumerate() {
                let same = cell.st == old.st
                    && if cell.glyph.0 < MARKED || same_arena && cell.glyph == old.glyph {
                        cell.glyph == old.glyph
                    } else {
                        self.marks.get(cell.glyph) == prev_marks.get(old.glyph)
                    };
                if cell != *old {
                    *old = cell;
                }
                if cell.glyph == CONT
                    || same
                        && (!palette_changed
                            || !cell.st.wash_id().checked_sub(1).is_some_and(|i| washes[i] != prev_washes[i]))
                {
                    continue;
                }
                if x != next_col {
                    let _ = write!(self.out, "\x1b[{};{}H", y + 1, x + 1);
                }
                if active != Some(cell.st) {
                    cell.st.sgr(&mut self.out, washes);
                    active = Some(cell.st);
                }
                if let Some(text) = self.marks.get(cell.glyph) {
                    self.out.extend_from_slice(text.as_bytes());
                } else {
                    self.out.extend_from_slice(cell.glyph.scalar().encode_utf8(&mut utf8).as_bytes());
                }
                let wide = cur.get(x + 1).is_some_and(|cell| cell.glyph == CONT);
                next_col = x + 1 + usize::from(wide);
            }
        }
        let data_at = self.flush_picture();
        self.prev_washes = self.washes;
        self.marks_flushed = true;
        if self.out.len() > SYNC_ON.len() {
            self.out.extend_from_slice(b"\x1b[0m\x1b[?2026l");
            let result = if self.pictures.data.is_empty() {
                send(&self.out)
            } else {
                let (before, after) = self.out.split_at(data_at);
                let result = send(before).and_then(|()| send(&self.pictures.data)).and_then(|()| send(after));
                self.pictures.data.clear();
                result
            };
            if result.is_ok() {
                self.pictures.pending_deletes.clear();
            }
            self.out.truncate(SYNC_ON.len());
            result
        } else {
            Ok(())
        }
    }
}

fn delete_picture(out: &mut Vec<u8>, id: u32, delete: char) {
    let _ = write!(out, "\x1b_Ga=d,d={delete},i={id},q=2\x1b\\");
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
                let mut line = String::new();
                for cell in cells.iter().filter(|cell| cell.glyph != CONT) {
                    if let Some(text) = self.marks.get(cell.glyph) {
                        line.push_str(text);
                    } else {
                        line.push(cell.glyph.scalar());
                    }
                }
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
        self.flush(|part| {
            bytes.extend_from_slice(part);
            Ok(())
        })
        .unwrap();
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
    fn overwriting_either_half_of_a_wide_character_clears_its_other_half() {
        let mut screen = Screen::sized(4, 2);
        screen.put(0, 0, 4, "界y", Style::PLAIN);
        screen.put(0, 1, 4, "界y", Style::PLAIN);
        screen.emitted();

        screen.put(0, 0, 1, "x", Style::PLAIN);
        screen.put_number(2, 1, 1, Style::PLAIN);
        assert_eq!(screen.text(), ["x y", " 1y"]);
        let updated = frame(&mut screen);
        assert!(updated.contains("\x1b[1;1H\x1b[0mx ") && updated.contains("\x1b[2;1H 1"), "{updated:?}");
    }

    #[test]
    fn right_alignment_counts_columns_not_bytes() {
        let mut screen = Screen::sized(8, 2);
        assert_eq!(screen.put_right(7, 0, "−界", Style::PLAIN), 3);
        assert_eq!(screen.put_right(2, 1, "a界", Style::PLAIN), 1);
        assert_eq!(screen.text(), ["    −界", " a"]);
    }

    #[test]
    fn a_shortened_file_name_keeps_its_extension() {
        let mut screen = Screen::sized(12, 1);
        screen.put_middle(0, 0, 12, "very-long-name.txt", Style::PLAIN);
        assert_eq!(screen.text(), ["very-lo….txt"]);
    }

    #[test]
    fn accents_stay_with_their_base_before_and_after_an_ellipsis() {
        let mut screen = Screen::sized(8, 2);
        assert_eq!(screen.put_fit(0, 0, 4, "界e\u{301}more", Style::PLAIN), 4);
        screen.put_middle(0, 1, 8, "abcde-long-e\u{301}x\u{302}", Style::PLAIN);
        assert_eq!(screen.text(), ["界e\u{301}…", "abcde…e\u{301}x\u{302}"]);
    }

    #[test]
    fn ellipses_fit_in_the_actual_remaining_columns() {
        let mut screen = Screen::sized(8, 2);
        assert_eq!(screen.put_fit(4, 0, 10, "very-long-name.txt", Style::PLAIN), 4);
        assert_eq!(screen.put_middle(2, 1, 10, "very-long-name.txt", Style::PLAIN), 6);
        assert_eq!(screen.text(), ["    ver…", "  very-…"]);
    }

    #[test]
    fn styling_either_half_repaints_the_whole_wide_glyph() {
        let mut screen = Screen::sized(4, 1);
        screen.put(1, 0, 2, "界", Style::PLAIN);
        assert!(frame(&mut screen).contains("\x1b[1;2H\x1b[0m界"));

        screen.paint(2, 0, 1, Style::fg(RED));
        assert!(frame(&mut screen).contains("\x1b[1;2H\x1b[0;31m界"));
        screen.emphasise(1, 0, 1);
        assert!(frame(&mut screen).contains("\x1b[1;2H\x1b[0;7;31m界"));
        screen.underline(0, 2..8);
        assert!(frame(&mut screen).contains("\x1b[1;2H\x1b[0;4;7;31m界"));
        assert!(frame(&mut screen).is_empty());
    }

    #[test]
    fn only_attached_printable_marks_are_sent_and_changed_accent_cells_are_redrawn() {
        let mut screen = Screen::sized(6, 1);
        let text = "\u{301}\x1b\u{202e}界\u{301}e\u{200d}\u{302}\u{34f}\u{180b}\u{1d173}\u{fe0f}x";
        screen.put(0, 0, 6, text, Style::PLAIN);
        let first = frame(&mut screen);
        assert!(first.contains("界\u{301}e\u{302}x"), "{first:?}");
        assert!(!first.contains("\u{301}界"));

        screen.clear();
        screen.put(0, 0, 6, "界\u{301}e\u{302}x", Style::PLAIN);
        assert_eq!(frame(&mut screen), "");
        screen.clear();
        screen.clear();
        screen.put(0, 0, 6, "界\u{308}e\u{302}x", Style::PLAIN);
        assert!(frame(&mut screen).contains("界\u{308}"));
        screen.put(0, 0, 6, "界\u{300}ex", Style::PLAIN);
        let updated = frame(&mut screen);
        assert!(updated.contains("\x1b[1;1H") && updated.contains("界\u{300}e"), "{updated:?}");
        assert!(!updated.contains('x'));
    }

    #[test]
    fn repaint_preserves_unicode_text_and_clears_the_rest_of_the_row() {
        for (text, cols) in [("กิกิกิ", 3), ("क़कि", 3), ("한글", 4), ("ꥠힰퟋ", 2)]
        {
            let mut screen = Screen::sized(16, 1);
            screen.put(0, 0, 16, "0123456789012345", Style::PLAIN);
            frame(&mut screen);

            screen.clear();
            assert_eq!(screen.put(0, 0, cols, text, Style::PLAIN), cols, "{text:?}");
            screen.put(cols, 0, 1, "|", Style::PLAIN);
            let output = frame(&mut screen);
            let expected = format!("\x1b[1;1H\x1b[0m{text}|{}", " ".repeat(15 - cols));
            assert!(output.contains(&expected), "{output:?}");
        }
    }

    #[test]
    fn scrolling_mixed_width_symbols_repaints_without_shifting_unchanged_columns() {
        let mut screen = Screen::sized(6, 2);
        frame(&mut screen);
        for (top, bottom, changed) in [
            ("🟢|", "🛠\u{fe0f} |", "\x1b[1;1H\x1b[0m🟢|\x1b[2;1H🛠\x1b[2;3H|"),
            ("🛠\u{fe0f} |", "🟢|", "\x1b[1;1H\x1b[0m🛠 \x1b[2;1H🟢"),
            ("🟢|", "🛠\u{fe0f} |", "\x1b[1;1H\x1b[0m🟢\x1b[2;1H🛠 "),
        ] {
            screen.clear();
            assert_eq!(screen.put(0, 0, 6, top, Style::PLAIN), 3);
            assert_eq!(screen.put(0, 1, 6, bottom, Style::PLAIN), 3);
            assert_eq!(frame(&mut screen), format!("\x1b[?2026h{changed}\x1b[0m\x1b[?2026l"));
            assert_eq!(frame(&mut screen), "");
        }
    }

    #[test]
    fn only_the_narrow_symbol_can_be_drawn_in_the_last_column() {
        let mut screen = Screen::sized(4, 1);
        assert_eq!(screen.put(3, 0, 1, "🟢", Style::PLAIN), 0);
        assert_eq!(screen.put(3, 0, 1, "🛠", Style::PLAIN), 1);
        assert!(frame(&mut screen).contains("\x1b[1;4H\x1b[0m🛠"));
    }

    #[test]
    fn emoji_skin_tone_is_not_filtered_from_terminal_output() {
        let mut screen = Screen::sized(8, 1);
        screen.put(0, 0, 8, "👍🏽", Style::PLAIN);
        assert!(frame(&mut screen).contains("👍🏽"));
    }

    fn frame(screen: &mut Screen) -> String {
        String::from_utf8(screen.emitted()).expect("screen output is UTF-8")
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

        // Help hides the placement, but closing it does not upload the picture again.
        screen.clear();
        assert!(frame(&mut screen).contains("a=d,d=i,i=1"));
        assert_eq!(frame(&mut screen), "");
        screen.show_picture(&picture, 4, 1, 5, 2);
        let shown_again = frame(&mut screen);
        assert!(shown_again.contains("a=p,i=1") && !shown_again.contains("a=t"));
        screen.clear();
        frame(&mut screen);
        let mut released = false;
        screen.release_pictures(|bytes| released = bytes.starts_with(b"\x18\x1b_Ga=d,d=I,i=1,"));
        assert!(released);
    }

    #[test]
    fn two_pictures_move_and_leave_independently() {
        let png = Picture::sample_png(16, 16);
        let a = Picture::png(png.clone().into()).unwrap();
        let b = Picture::png(png.into()).unwrap();
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

        // A resize erases the display, and with it the terminal may drop the pictures.
        screen.resize(24, 6);
        screen.clear();
        screen.show_picture(&a, 3, 1, 4, 2);
        screen.show_picture(&b, 12, 1, 4, 2);
        let resized = frame(&mut screen);
        assert_eq!(resized.matches("a=t,f=100,").count(), 2);
        assert_eq!(resized.matches("a=p,").count(), 2);

        screen.clear();
        let hidden = frame(&mut screen);
        assert!(hidden.contains(&format!("a=d,d=i,i={},", a.id)));
        assert!(hidden.contains(&format!("a=d,d=i,i={},", b.id)));
        screen.show_picture(&b, 12, 1, 4, 2);
        let replaced = frame(&mut screen);
        assert!(replaced.contains(&format!("a=d,d=I,i={},", a.id)));
        assert!(!replaced.contains("a=t") && replaced.contains(&format!("a=p,i={},", b.id)));
    }

    #[test]
    fn cleanup_covers_both_old_and_new_pictures_after_a_failed_frame() {
        let png = Picture::sample_png(16, 16);
        let a = Picture::png(png.clone().into()).unwrap();
        let b = Picture::png(png.into()).unwrap();
        for resize in [false, true] {
            let mut screen = Screen::sized(20, 6);
            screen.show_picture(&a, 2, 1, 4, 2);
            frame(&mut screen);
            screen.clear();
            if resize {
                screen.resize(24, 6);
            }
            screen.show_picture(&b, 2, 1, 4, 2);
            assert!(screen.flush(|_| Err(io::ErrorKind::BrokenPipe.into())).is_err());
            let mut released = false;
            screen.release_pictures(|bytes| {
                let commands = std::str::from_utf8(bytes).unwrap();
                assert!(commands.starts_with('\x18'));
                for picture in [&a, &b] {
                    assert!(commands.contains(&format!("a=d,d=I,i={},", picture.id)));
                }
                released = true;
            });
            assert!(released);
        }
    }

    #[test]
    fn interactive_resize_repaints_text_and_uploads_only_the_settled_picture() {
        let picture = Picture::sample(40, 20);
        let start = Instant::now();
        let mut screen = Screen::new();
        screen.resize_interactive(20, 6, start);
        screen.show_picture(&picture, 2, 1, 5, 2);
        assert!(screen.picture_redraw_due().is_none());
        assert_eq!(frame(&mut screen).matches("a=t,f=100,").count(), 1);

        for (step, width) in (21..=23).enumerate() {
            let now = start + Duration::from_millis(10 * step as u64);
            screen.resize_interactive(width, 6, now);
            assert_eq!(screen.picture_redraw_due(), Some(now + PICTURE_RESIZE_DELAY));
            screen.put(0, 0, width, "heading", Style::PLAIN);
            screen.show_picture(&picture, width - 7, 1, 5, 2);
            let resized = frame(&mut screen);
            assert!(resized.contains("heading"), "{resized:?}");
            assert!(!resized.contains("a=t") && !resized.contains("a=p"), "{resized:?}");
            assert_eq!(resized.matches("a=d,d=I,").count(), usize::from(step == 0));
        }

        screen.resume_pictures();
        screen.show_picture(&picture, 16, 1, 5, 2);
        let settled = frame(&mut screen);
        assert_eq!(settled.matches("a=t,f=100,").count(), 1);
        assert!(settled.contains("\x1b[2;17H\x1b_Ga=p,i=1,c=5,r=2,"), "{settled:?}");
        assert!(frame(&mut screen).is_empty());
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
        let first = screen.emitted();
        assert!(first.starts_with(b"\x1b[?2026h\x1b[0m\x1b[2J"));
        assert!(first.ends_with(b"\x1b[0m\x1b[?2026l"));

        screen.set_background([0; 3]);
        assert_eq!(
            screen.emitted(),
            concat!("\x1b[?2026h\x1b[1;1H\x1b[0;48;2;35;35;35mA", "\x1b[1;4H\x1b[0;48;2;0;30;11m界\x1b[0m\x1b[?2026l")
                .as_bytes()
        );
        screen.set_background([0; 3]);
        screen.flush(|_| panic!("the palette did not change")).unwrap();
    }
}
