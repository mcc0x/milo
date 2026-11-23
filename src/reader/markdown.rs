//! A local Markdown view: render text into Screen cells, never into terminal commands.
//! URLs and image paths are discarded; HTML, references and unsupported syntax stay text.

use std::time::Instant;

use crate::diff::{Line, LineKind};
use crate::screen::{CYAN, MAGENTA, Screen, Style, YELLOW};
use crate::ui::{self, FAINT};
use crate::width::{CellWidth, cell_width};

mod updates;

use updates::{Marks, Matches};

pub(super) const EMPTY: &str = "No Markdown content.";

const MAX_BYTES: usize = 512 << 10;
const MAX_LINES: usize = 20_000;
const MAX_DIFF_LINES: usize = 4 * MAX_LINES;
const MAX_ROWS: usize = 32_000;
const MAX_INLINE_DEPTH: usize = 12;
const TRIMMED: &str = "Markdown preview trimmed; switch to source for the rest.";
const HEADING: Style = Style::fg(MAGENTA).bold();
const CODE: Style = Style::fg(YELLOW);
const LINK: Style = Style::fg(CYAN);

/// Owns both the parsed text and the rows for the current width. Scrolling only
/// changes which cached rows are drawn; source view has its own position.
pub(super) struct Preview {
    doc: Document,
    layout: Layout,
    top: usize,
    height: usize,
    // The passage being read can outlive rounding to a wrapped row or clamping at the end.
    anchor: Option<Position>,
    pending_line: Option<usize>,
    following: bool,
    marks: Marks,
}

impl Preview {
    pub(super) fn new(lines: &[Line]) -> Self {
        Self {
            doc: Document::parse(lines),
            layout: Layout::default(),
            top: 0,
            height: 0,
            anchor: None,
            pending_line: None,
            following: false,
            marks: Marks::default(),
        }
    }

    pub(super) fn replace(&mut self, lines: &[Line]) {
        self.replace_at(lines, Instant::now());
    }

    fn replace_at(&mut self, lines: &[Line], now: Instant) {
        self.expire_marks(now);
        let doc = Document::parse(lines);
        // Source lines can move even when the rendered document does not.
        if self.doc.text == doc.text
            && self.doc.blocks == doc.blocks
            && self.doc.runs == doc.runs
            && self.doc.trimmed == doc.trimmed
        {
            self.doc.sources = doc.sources;
            return;
        }
        let matches = Matches::between(&self.doc, &doc);
        self.anchor = if self.following {
            None
        } else {
            self.position().map(|position| matches.translate(position, &self.doc, &doc))
        };
        self.marks.update(&matches, &self.doc, &doc, now);
        self.doc = doc;
        if self.layout.width != 0 {
            self.layout.reflow(&self.doc, self.layout.width);
            self.place();
        }
    }

    /// Also returns true when manual navigation turns off follow without moving.
    pub(super) fn scroll(&mut self, delta: isize) -> bool {
        let following = self.following;
        self.stop_follow();
        self.anchor = None;
        let top = self.top.saturating_add_signed(delta).min(self.last_top());
        let moved = top != self.top;
        self.top = top;
        moved || following
    }

    /// Pins the viewport to the last page. Returns whether the visible state changed.
    pub(super) fn follow_end(&mut self) -> bool {
        let changed = !self.following || self.top != self.last_top();
        self.following = true;
        self.anchor = None;
        self.pending_line = None;
        self.top = self.last_top();
        changed
    }

    pub(super) fn following(&self) -> bool {
        self.following
    }

    pub(super) fn stop_follow(&mut self) {
        self.following = false;
    }

    pub(super) fn mark_deadline(&self) -> Option<Instant> {
        self.marks.deadline()
    }

    pub(super) fn expire_marks(&mut self, now: Instant) {
        self.marks.expire(now);
    }

    /// Places an editor's top line in view once the preview has a viewport.
    pub(super) fn go_to_line(&mut self, line: usize) {
        self.stop_follow();
        if self.layout.width == 0 {
            self.pending_line = Some(line);
        } else {
            self.anchor = Some(self.line_position(line));
            self.place();
        }
    }

    /// The working file line at the top of the rendered view, counted from zero.
    pub(super) fn source_line(&self) -> usize {
        self.pending_line.unwrap_or_else(|| {
            let position = self.top_row().map_or((0, 0), |row| row.position());
            let at = self.doc.sources.partition_point(|source| source.position <= position).saturating_sub(1);
            self.doc.sources[at].line
        })
    }

    pub(super) fn draw(&mut self, screen: &mut Screen, rows: usize) {
        let height = rows.min(screen.h.saturating_sub(1));
        if screen.w == 0 || height == 0 {
            return;
        }
        let width = screen.w - usize::from(screen.w > 1);
        if self.pending_line.is_some() || width != self.layout.width || height != self.height {
            if !self.following {
                self.anchor = self.pending_line.take().map(|line| self.line_position(line)).or_else(|| self.position());
            }
            self.height = height;
            if width != self.layout.width {
                self.layout.reflow(&self.doc, width);
            }
            self.place();
        }
        let shown = &self.layout.rows[self.top.min(self.layout.rows.len())..];
        let shown = &shown[..shown.len().min(self.height)];
        for (i, row) in shown.iter().enumerate() {
            self.doc.draw_row(screen, i + 1, width, *row);
        }
        let total = self.layout.len();
        if self.layout.trimmed && total - 1 - self.top < self.height {
            screen.put_fit(0, total - self.top, width, TRIMMED, FAINT);
        }
        if screen.w > 1 {
            ui::thumb(screen, width, 1, self.height, total, self.top);
        }
        self.marks.draw(screen, shown);
    }

    fn last_top(&self) -> usize {
        self.layout.len().saturating_sub(self.height.max(1))
    }

    fn top_row(&self) -> Option<&Row> {
        self.layout.rows.get(self.top).or_else(|| self.layout.rows.last())
    }

    fn position(&self) -> Option<Position> {
        if let Some(anchor) = self.anchor {
            return Some(anchor);
        }
        let row = self.top_row()?;
        Some(Position {
            block: row.block,
            offset: row.start - self.doc.blocks[row.block].start,
            limit: (self.layout.trimmed && self.top == self.layout.rows.len())
                || (self.doc.trimmed && row.block + 1 == self.doc.blocks.len()),
        })
    }

    fn line_position(&self, line: usize) -> Position {
        let at = self.doc.sources.partition_point(|source| source.line < line);
        // If the truncation note shares a line number, choose the actual text first.
        let at =
            if self.doc.sources.get(at).is_some_and(|source| source.line == line) { at } else { at.saturating_sub(1) };
        let (block, byte) = self.doc.sources[at].position;
        Position { block, offset: byte - self.doc.blocks[block].start, limit: false }
    }

    fn place(&mut self) {
        if self.following {
            self.top = self.last_top();
        } else if let Some(position) = self.anchor {
            self.restore(position);
        } else {
            self.top = self.top.min(self.last_top());
        }
    }

    fn restore(&mut self, position: Position) {
        if position.limit && self.layout.trimmed {
            self.top = self.last_top();
            return;
        }
        let (block, offset) = if position.limit && self.doc.trimmed {
            (self.doc.blocks.len() - 1, 0)
        } else {
            (position.block, position.offset)
        };
        let start = self.doc.blocks[block].start;
        let at = (block, start + offset.min(self.doc.end(block).saturating_sub(start + 1)));
        if self.layout.trimmed
            && self.layout.rows.last().is_some_and(|last| at >= (last.block, last.end) && at > last.position())
        {
            self.top = self.last_top();
            return;
        }
        self.top = self.layout.rows.partition_point(|row| row.position() <= at).saturating_sub(1);
        self.top = self.top.min(self.last_top());
    }
}

#[derive(Clone, Copy)]
struct Position {
    block: usize,
    offset: usize,
    limit: bool,
}

#[derive(Clone, Copy)]
struct Source {
    position: (usize, usize), // block, then rendered byte within the document
    line: usize,
}

#[derive(Default)]
struct Document {
    text: String,
    runs: Vec<Run>,
    blocks: Vec<Block>,
    sources: Vec<Source>,
    trimmed: bool,
}

#[derive(PartialEq, Eq)]
struct Run {
    // The input caps keep even expanded Markdown well below 4 GiB.
    start: u32,
    style: Style,
}

impl Run {
    fn start(&self) -> usize {
        self.start as usize
    }
}

#[derive(PartialEq, Eq)]
struct Block {
    start: usize,
    format: Format,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Format {
    quotes: usize,
    indent: usize,
    hanging: usize,
    literal: bool,
}

impl Format {
    /// Keep room for a wide character rather than filling a narrow pane with
    /// decoration. Code keeps its source whitespace apart from this margin.
    fn margins(self, width: usize, continued: bool) -> (usize, usize) {
        let room = width.saturating_sub(2);
        let quotes = self.quotes.min(room / 2);
        let indent = self.indent + if continued { self.hanging } else { 0 };
        (quotes, indent.min(room - 2 * quotes))
    }
}

impl Document {
    fn parse(lines: &[Line]) -> Self {
        let mut doc = Self::default();
        let mut fence: Option<Fence> = None;
        let mut paragraph = None;
        let mut bytes = 0;
        let mut source = 0;
        let seen = lines.len().min(MAX_DIFF_LINES);
        let mut input = lines[..seen].iter().filter(|line| line.kind != LineKind::Del).peekable();
        let mut trimmed = seen < lines.len();
        let mut remaining = MAX_LINES;
        while let Some(line) = input.next() {
            if remaining == 0 || bytes == MAX_BYTES {
                trimmed = true;
                break;
            }
            remaining -= 1;
            source = line.number.saturating_sub(1) as usize;
            let len = line.text.floor_char_boundary((MAX_BYTES - bytes).min(line.text.len()));
            let raw = &line.text[..len];
            let raw = if source == 0 { raw.strip_prefix('\u{feff}').unwrap_or(raw) } else { raw };
            bytes += len;
            trimmed |= len != line.text.len();

            if let Some(open) = fence {
                let (quotes, text) = quote_prefix(raw, open.quotes);
                if quotes == open.quotes {
                    if open.closes(text) {
                        fence = None;
                    } else {
                        let indent = leading_spaces(text, open.indent);
                        doc.begin(source, Format { quotes, indent: 2, literal: true, ..Format::default() });
                        doc.write(&text[indent..], CODE);
                    }
                    if len != line.text.len() {
                        break;
                    }
                    continue;
                }
                fence = None; // Leaving a quote also leaves its fence.
            }

            let (quotes, raw) = quote_prefix(raw, 8);
            let format = Format { quotes, ..Format::default() };
            let indent = leading_spaces(raw, raw.len());
            let text = &raw[indent..];
            if let Some(open) = (indent <= 3).then(|| Fence::open(text, quotes, indent)).flatten() {
                fence = Some(open);
                paragraph = None;
            } else if let Some(title) = (indent <= 3).then(|| atx_heading(text)).flatten() {
                doc.begin(source, format);
                doc.inline(title, HEADING);
                paragraph = None;
            } else if text.is_empty() {
                doc.begin(source, format);
                paragraph = None;
            } else if indent >= 4
                && !paragraph.is_some_and(|(depth, hanging)| depth == quotes && hanging != 0 && indent >= hanging)
            {
                doc.begin(source, Format { indent: 2, literal: true, ..format });
                doc.write(&raw[4..], CODE);
                paragraph = None;
            } else if text.starts_with('|') {
                // Preserve pipe tables row for row, even when we cannot fit every column.
                doc.begin(source, Format { literal: true, ..format });
                doc.inline(text, Style::PLAIN);
                paragraph = None;
            } else if indent <= 3 && rule(text) {
                doc.begin(source, format);
                doc.write("────────", FAINT);
                paragraph = None;
            } else if let Some((marker, text)) = list_marker(text) {
                let margin = indent.min(12);
                let hanging = if marker.len() == 1 { 2 } else { marker.len() + 1 };
                doc.begin(source, Format { indent: margin, hanging, ..format });
                doc.write(if marker.len() == 1 { "•" } else { marker }, LINK);
                doc.write(" ", Style::PLAIN);
                let (text, hard_break) = line_end(text);
                doc.inline(text, Style::PLAIN);
                paragraph = (!hard_break).then_some((quotes, indent + hanging));
            } else if indent <= 3
                && remaining > 0
                && let Some(next) = input.peek().filter(|next| next.text.len() <= MAX_BYTES - bytes)
                && setext_heading(&next.text, quotes)
            {
                doc.begin(source, format);
                doc.inline(text.trim_end_matches(' '), HEADING);
                bytes += next.text.len();
                input.next();
                remaining -= 1;
                paragraph = None;
            } else {
                let (text, hard_break) = line_end(text);
                if paragraph.is_some_and(|(depth, min_indent)| depth == quotes && indent >= min_indent) {
                    doc.write(" ", Style::PLAIN);
                    doc.mark(source);
                } else {
                    doc.begin(source, format);
                    paragraph = Some((quotes, 0));
                }
                doc.inline(text, Style::PLAIN);
                if hard_break {
                    paragraph = None;
                }
            }
            if len != line.text.len() {
                break;
            }
        }
        if trimmed || doc.blocks.is_empty() {
            doc.begin(source, Format::default());
            doc.write(if trimmed { TRIMMED } else { EMPTY }, FAINT);
        }
        doc.trimmed = trimmed;
        doc
    }

    fn end(&self, block: usize) -> usize {
        self.blocks.get(block + 1).map_or(self.text.len(), |next| next.start)
    }

    fn key(&self, index: usize) -> (Format, &str) {
        let block = &self.blocks[index];
        (block.format, &self.text[block.start..self.end(index)])
    }

    fn styles(&self, block: usize) -> impl Iterator<Item = (usize, Style)> + '_ {
        let start = self.blocks[block].start;
        let end = self.end(block);
        let first = self.runs.partition_point(|run| run.start() <= start).saturating_sub(1);
        self.runs[first..]
            .iter()
            .take_while(move |run| start < end && run.start() < end)
            .map(move |run| (run.start().saturating_sub(start), run.style))
    }

    fn begin(&mut self, source: usize, format: Format) {
        self.blocks.push(Block { start: self.text.len(), format });
        self.mark(source);
    }

    fn mark(&mut self, line: usize) {
        self.sources.push(Source { position: (self.blocks.len() - 1, self.text.len()), line });
    }

    fn run(&mut self, style: Style) {
        if self.runs.last().is_none_or(|run| run.style != style) {
            let start = u32::try_from(self.text.len()).expect("Markdown preview offset fits in u32");
            self.runs.push(Run { start, style });
        }
    }

    fn write(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        if text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
            self.run(style);
            self.text.push_str(text);
            return;
        }
        let start = self.blocks.last().expect("begin a block before writing").start;
        let mut styled = false;
        for ch in text.chars() {
            match cell_width(ch) {
                CellWidth::Skip => {}
                // A leading combining mark goes with the preceding run, not the next cell.
                CellWidth::Attach if self.text.len() > start => self.text.push(ch),
                CellWidth::Attach => {}
                CellWidth::Narrow | CellWidth::Wide => {
                    if !styled {
                        self.run(style);
                        styled = true;
                    }
                    self.text.push(ch);
                }
            }
        }
    }

    fn inline(&mut self, text: &str, style: Style) {
        // Failed searches also spend budget. Dense unmatched punctuation must
        // not turn line parsing quadratic; beyond the budget it stays literal.
        Inline { doc: self, budget: text.len() * 4 }.render(text, style, 0);
    }

    fn draw_row(&self, screen: &mut Screen, y: usize, width: usize, row: Row) {
        let block = &self.blocks[row.block];
        let (quotes, indent) = block.format.margins(width, row.start != block.start);
        for q in 0..quotes {
            screen.put(2 * q, y, 1, "│", FAINT);
        }
        let mut x = 2 * quotes + indent;
        if row.narrow_fallback {
            screen.put(x, y, 1, "…", FAINT);
            return;
        }
        let mut start = row.start;
        let mut run = self.runs.partition_point(|run| run.start() <= start).saturating_sub(1);
        while start < row.end {
            let end = self.runs.get(run + 1).map_or(row.end, |next| row.end.min(next.start()));
            x += screen.put(x, y, width - x, &self.text[start..end], self.runs[run].style);
            start = end;
            run += 1;
        }
    }
}

fn leading_spaces(text: &str, max: usize) -> usize {
    text.bytes().take(max).take_while(|byte| *byte == b' ').count()
}

fn quote_prefix(mut text: &str, max: usize) -> (usize, &str) {
    for quotes in 0..max {
        let indent = leading_spaces(text, 4);
        if indent > 3 {
            return (quotes, text);
        }
        let Some(tail) = text[indent..].strip_prefix('>') else {
            return (quotes, text);
        };
        text = tail.strip_prefix(' ').unwrap_or(tail);
    }
    (max, text)
}

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    count: usize,
    quotes: usize,
    indent: usize,
}

impl Fence {
    fn open(text: &str, quotes: usize, indent: usize) -> Option<Self> {
        let marker = *text.as_bytes().first()?;
        if !matches!(marker, b'`' | b'~') {
            return None;
        }
        let count = run_len(text, marker);
        (count >= 3 && (marker != b'`' || !text[count..].contains('`'))).then_some(Self {
            marker,
            count,
            quotes,
            indent,
        })
    }

    fn closes(self, text: &str) -> bool {
        let indent = leading_spaces(text, 4);
        let text = &text[indent..];
        let count = run_len(text, self.marker);
        indent <= 3 && count >= self.count && text[count..].bytes().all(|byte| byte == b' ')
    }
}

fn run_len(text: &str, marker: u8) -> usize {
    text.bytes().take_while(|byte| *byte == marker).count()
}

fn atx_heading(text: &str) -> Option<&str> {
    let hashes = run_len(text, b'#');
    let tail = &text[hashes..];
    if !(1..=6).contains(&hashes) || !(tail.is_empty() || tail.starts_with(' ')) {
        return None;
    }
    let tail = tail.trim_end_matches(' ');
    let content = tail.trim_end_matches('#');
    Some(if content.ends_with(' ') { content } else { tail }.trim_matches(' '))
}

fn setext_heading(text: &str, depth: usize) -> bool {
    let (quotes, text) = quote_prefix(text, 8);
    let indent = leading_spaces(text, 4);
    let text = text[indent..].trim_end_matches(' ');
    quotes == depth
        && indent <= 3
        && text
            .as_bytes()
            .first()
            .is_some_and(|mark| matches!(mark, b'=' | b'-') && text.bytes().all(|byte| byte == *mark))
}

fn rule(text: &str) -> bool {
    let Some(mark @ (b'-' | b'*' | b'_')) = text.bytes().next() else { return false };
    let mut count = 0;
    for byte in text.bytes() {
        if byte == mark {
            count += 1;
        } else if byte != b' ' {
            return false;
        }
    }
    count >= 3
}

fn list_marker(text: &str) -> Option<(&str, &str)> {
    let end = if matches!(text.as_bytes().first(), Some(b'-' | b'+' | b'*')) {
        1
    } else {
        let digits = text.bytes().take(10).take_while(u8::is_ascii_digit).count();
        if !(1..=9).contains(&digits) || !matches!(text.as_bytes().get(digits), Some(b'.' | b')')) {
            return None;
        }
        digits + 1
    };
    let tail = &text[end..];
    if tail.is_empty() || tail.starts_with(' ') { Some((&text[..end], tail.trim_start_matches(' '))) } else { None }
}

fn line_end(text: &str) -> (&str, bool) {
    let trimmed = text.trim_end_matches(' ');
    if text.len() - trimmed.len() >= 2 {
        (trimmed, true)
    } else if trimmed.bytes().rev().take_while(|byte| *byte == b'\\').count() % 2 == 1 {
        (&trimmed[..trimmed.len() - 1], true)
    } else {
        (trimmed, false)
    }
}

struct Inline<'a> {
    doc: &'a mut Document,
    budget: usize,
}

impl Inline<'_> {
    fn render(&mut self, text: &str, style: Style, depth: usize) {
        let bytes = text.as_bytes();
        let (mut at, mut plain) = (0, 0);
        while depth < MAX_INLINE_DEPTH && self.budget > 0 && at < bytes.len() {
            let start = at;
            let marker = bytes[at];
            at += 1;
            match marker {
                b'\\' if bytes.get(at).is_some_and(u8::is_ascii_punctuation) => {
                    self.doc.write(&text[plain..start], style);
                    plain = at;
                    at += 1;
                }
                b'`' | b'*' | b'_' => {
                    at += run_len(&text[at..], marker);
                    let count = at - start;
                    let code = marker == b'`';
                    if (code || (count <= 3 && emphasis_side(text, start, at, marker, true)))
                        && let Some(end) = marker_end(text, at, marker, count, &mut self.budget)
                    {
                        self.doc.write(&text[plain..start], style);
                        let inner = &text[at..end];
                        if code {
                            let kept = inner.strip_prefix(' ').and_then(|s| s.strip_suffix(' '));
                            self.doc.write(kept.filter(|s| !s.trim_matches(' ').is_empty()).unwrap_or(inner), CODE);
                        } else {
                            let style = match count {
                                1 => style.italic(),
                                2 => style.bold(),
                                _ => style.bold().italic(),
                            };
                            self.render(inner, style, depth + 1);
                        }
                        at = end + count;
                        plain = at;
                    }
                }
                b'[' | b'!' => {
                    let image = marker == b'!';
                    if image {
                        if bytes.get(at) != Some(&b'[') {
                            continue;
                        }
                        at += 1;
                    }
                    if let Some((label, end)) = link_end(text, at, &mut self.budget) {
                        self.doc.write(&text[plain..start], style);
                        self.render(&text[at..label], if image { FAINT } else { LINK }, depth + 1);
                        at = end;
                        plain = at;
                    }
                }
                _ => {}
            }
        }
        self.doc.write(&text[plain..], style);
    }
}

fn emphasis_side(text: &str, start: usize, end: usize, marker: u8, open: bool) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let inside = if open { after } else { before };
    inside.is_some_and(|c| !c.is_whitespace())
        && (marker != b'_' || !(before.is_some_and(char::is_alphanumeric) && after.is_some_and(char::is_alphanumeric)))
}

struct Probe<'a> {
    text: &'a str,
    at: usize,
    budget: &'a mut usize,
}

impl Probe<'_> {
    fn next(&mut self) -> Option<(usize, u8)> {
        let byte = *self.text.as_bytes().get(self.at)?;
        *self.budget = self.budget.checked_sub(1)?;
        let at = self.at;
        self.at += 1;
        Some((at, byte))
    }

    fn run(&mut self, marker: u8) -> Option<()> {
        while self.text.as_bytes().get(self.at) == Some(&marker) {
            self.next()?;
        }
        Some(())
    }

    fn skip_code(&mut self, start: usize) -> Option<()> {
        self.run(b'`')?;
        let count = self.at - start;
        if let Some(end) = marker_end(self.text, self.at, b'`', count, self.budget) {
            self.at = end + count;
        }
        Some(())
    }
}

fn marker_end(text: &str, at: usize, marker: u8, count: usize, budget: &mut usize) -> Option<usize> {
    let mut probe = Probe { text, at, budget };
    while let Some((start, byte)) = probe.next() {
        if byte == marker {
            probe.run(marker)?;
            if probe.at - start == count && (marker == b'`' || emphasis_side(text, start, probe.at, marker, false)) {
                return Some(start);
            }
        } else if marker != b'`' {
            if byte == b'\\' && text.as_bytes().get(probe.at).is_some_and(u8::is_ascii_punctuation) {
                probe.next()?;
            } else if byte == b'`' {
                probe.skip_code(start)?;
            }
        }
    }
    None
}

fn link_end(text: &str, at: usize, budget: &mut usize) -> Option<(usize, usize)> {
    let mut probe = Probe { text, at, budget };
    let mut depth = 1;
    let label = loop {
        let (at, byte) = probe.next()?;
        match byte {
            b'\\' if text.as_bytes().get(probe.at).is_some_and(u8::is_ascii_punctuation) => {
                probe.next()?;
            }
            b'`' => probe.skip_code(at)?,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break at;
                }
            }
            _ => {}
        }
    };
    if probe.next()?.1 != b'(' {
        return None;
    }
    // URL and optional title are consumed, not stored. Parentheses in quoted
    // titles or angle-bracket destinations don't end the target.
    let (mut depth, mut delimiter, mut first, mut space) = (1, None, true, false);
    loop {
        let (_, byte) = probe.next()?;
        if byte == b'\\' && text.as_bytes().get(probe.at).is_some_and(u8::is_ascii_punctuation) {
            probe.next()?;
        } else if let Some(close) = delimiter {
            if close == byte {
                delimiter = None;
            }
        } else {
            match byte {
                b'<' if first => delimiter = Some(b'>'),
                b'\'' | b'"' if depth == 1 && space => delimiter = Some(byte),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((label, probe.at));
                    }
                }
                _ => {}
            }
        }
        space = byte == b' ';
        first &= space;
    }
}

#[derive(Default)]
struct Layout {
    width: usize,
    rows: Vec<Row>,
    trimmed: bool,
}

#[derive(Clone, Copy)]
struct Row {
    block: usize,
    start: usize,
    end: usize,
    narrow_fallback: bool,
}

impl Row {
    fn position(self) -> (usize, usize) {
        (self.block, self.start)
    }
}

impl Layout {
    fn len(&self) -> usize {
        self.rows.len() + usize::from(self.trimmed)
    }

    fn reflow(&mut self, doc: &Document, width: usize) {
        self.width = width;
        self.rows.clear();
        self.trimmed = false;
        for (index, block) in doc.blocks.iter().enumerate() {
            let mut start = block.start;
            let end = doc.end(index);
            loop {
                if self.rows.len() == MAX_ROWS {
                    self.trimmed = true;
                    return;
                }
                let (quotes, indent) = block.format.margins(width, start != block.start);
                let (shown, next, narrow_fallback) =
                    wrap(&doc.text[start..end], width - 2 * quotes - indent, block.format.literal);
                self.rows.push(Row { block: index, start, end: start + shown, narrow_fallback });
                start += next;
                if start == end {
                    break;
                }
            }
        }
    }
}

/// Bytes shown and bytes consumed in one row. Reflow first uses a word break
/// where possible, otherwise leaves the first nonfitting glyph for the next row.
fn wrap(text: &str, width: usize, literal: bool) -> (usize, usize, bool) {
    let (mut columns, mut gap, mut space) = (0, None, false);
    for (at, ch) in text.char_indices() {
        let cw = match cell_width(ch) {
            CellWidth::Narrow => 1,
            CellWidth::Wide => 2,
            CellWidth::Attach | CellWidth::Skip => continue,
        };
        if !literal && ch == ' ' && !space && columns > 0 {
            gap = Some(at);
        }
        if columns + cw > width {
            if columns == 0 {
                // A two-column glyph cannot be drawn in a one-column pane.
                // Still consume its marks and make forward progress.
                let mut end = at + ch.len_utf8();
                for mark in text[end..].chars().take_while(|c| matches!(cell_width(*c), CellWidth::Attach)) {
                    end += mark.len_utf8();
                }
                return (end, end, true);
            }
            return if let Some(gap) = gap {
                (gap, gap + leading_spaces(&text[gap..], text.len() - gap), false)
            } else {
                (at, at, false)
            };
        }
        columns += cw;
        space = ch == ' ';
    }
    (text.len(), text.len(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<Line> {
        text.iter().zip(1..).map(|(text, number)| Line::new(LineKind::Same, number, text)).collect()
    }

    #[test]
    fn renders_blocks_and_inline_styles_without_targets() {
        let mut input = lines(&[
            "\u{feff}# Heading ###",
            "A *soft* and **bold** `[literal]` [label](https://invalid/(nest) \"a title)\") ![alt](/private).",
            "- First",
            "  continuation",
            "3. Second",
            "> > quote",
            "```rust",
            "    **code** > [literal](target)",
            "```",
            "Underlined",
            "===",
        ]);
        input[1] = Line::new(LineKind::Add, 2, &input[1].text);
        input.insert(2, Line::new(LineKind::Del, 2, "```removed"));
        let mut preview = Preview::new(&input);
        let mut screen = Screen::sized(100, 10);
        preview.draw(&mut screen, 9);
        assert_eq!(
            &screen.text()[1..9],
            [
                "Heading",
                "A soft and bold [literal] label alt.",
                "• First continuation",
                "3. Second",
                "│ │ quote",
                "      **code** > [literal](target)",
                "Underlined",
                "",
            ]
        );
        let doc = &preview.doc;
        for (text, style) in [
            ("Heading", HEADING),
            ("soft", Style::PLAIN.italic()),
            ("bold", Style::PLAIN.bold()),
            ("[literal]", CODE),
            ("label", LINK),
            ("alt", FAINT),
        ] {
            let start = doc.text.find(text).unwrap();
            let run = doc.runs.partition_point(|run| run.start() <= start) - 1;
            assert!(doc.runs[run].style == style, "wrong style for {text}");
        }
    }

    #[test]
    fn compact_runs_keep_styles_at_large_byte_offsets() {
        assert_eq!(size_of::<Run>(), 8);
        let earlier = "x".repeat(1 << 16);
        let mut preview = Preview::new(&lines(&[&earlier, "", "Guide: `café` e\u{0301}nd"]));
        preview.go_to_line(2);
        let mut screen = Screen::sized(40, 2);
        preview.draw(&mut screen, 1);
        assert!(screen.text()[1].starts_with("Guide: café e\u{0301}nd"));
        let code = "\x1b[0;33mcafé".as_bytes();
        assert!(screen.emitted().windows(code.len()).any(|bytes| bytes == code));
    }

    #[test]
    fn keeps_table_rows_separate_and_indented_code_literal() {
        let input = lines(&["| A | B |", "|---|---|", "| 1 | 2 |", "", "    let a = *x*;", "      use_it(a);"]);
        let mut preview = Preview::new(&input);
        let mut screen = Screen::sized(40, 9);
        preview.draw(&mut screen, 8);
        assert_eq!(
            &screen.text()[1..7],
            ["| A | B |", "|---|---|", "| 1 | 2 |", "", "  let a = *x*;", "    use_it(a);"]
        );
    }

    #[test]
    fn sanitizes_controls_and_keeps_html_and_image_alt_inert() {
        let mut input = lines(&[""]);
        input[0].text =
            "<b>literal</b> \x1b]8;;x\x07\u{009b}0m \u{202e}safe ![a *plain* alt](file:///private)".to_owned();
        let mut preview = Preview::new(&input);
        let mut screen = Screen::sized(80, 3);
        preview.draw(&mut screen, 2);
        assert_eq!(screen.text()[1], "<b>literal</b> ]8;;x0m safe a plain alt");
        assert!(!preview.doc.text.chars().any(|ch| matches!(cell_width(ch), CellWidth::Skip)));
        assert!(!screen.emitted().windows(3).any(|bytes| bytes == b"\x1b]8"));
    }

    #[test]
    fn returns_to_the_editor_line_even_before_the_first_preview_draw() {
        let mut preview = Preview::new(&lines(&["# Before", "", "# Point of edit", "", "After"]));
        preview.go_to_line(2);
        let mut screen = Screen::sized(30, 3);
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("Point of edit"));
        assert_eq!(preview.source_line(), 2);

        preview.go_to_line(0);
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("Before"));

        // Escape and a scroll key can arrive together before the next draw.
        preview.go_to_line(2);
        assert!(preview.scroll(-2));
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("Before"));
    }

    #[test]
    fn wraps_by_cells_and_keeps_scroll_position_across_width_changes() {
        let mut preview = Preview::new(&lines(&["one two three four", "five six", "", "終e\u{0301}端", "", "last"]));
        let mut small = Screen::sized(8, 3);
        preview.draw(&mut small, 2);
        assert!(small.text()[1].starts_with("one two"));
        assert!(preview.scroll(2));
        small.clear();
        preview.draw(&mut small, 2);
        assert!(small.text()[1].starts_with("four"));
        let mut wide = Screen::sized(14, 3);
        preview.draw(&mut wide, 2);
        assert!(wide.text()[1].starts_with("four five six"));
        assert_eq!(preview.source_line(), 0);
        assert!(preview.scroll(2));
        assert_eq!(preview.source_line(), 3);
        preview.replace(&lines(&["first", "", "", "終e\u{0301}端", "", "last"]));
        wide.clear();
        preview.draw(&mut wide, 2);
        assert!(wide.text()[1].starts_with("終e\u{0301}端"));
        assert_eq!(preview.source_line(), 3);
        let mut narrow = Screen::sized(1, 3);
        preview.draw(&mut narrow, 2);
        assert_eq!(&narrow.text()[1..], ["…", "e\u{0301}"]);
        preview.replace(&lines(&["first", "", "", "界o\u{0308}端", "", "last"]));
        narrow.clear();
        preview.draw(&mut narrow, 2);
        assert_eq!(&narrow.text()[1..], ["…", "o\u{0308}"]);
        assert!(preview.scroll(isize::MAX));
        assert!(!preview.scroll(isize::MAX));
        assert!(preview.scroll(isize::MIN));
        assert_eq!(preview.source_line(), 0);
    }

    #[test]
    fn keeps_a_wrapped_anchor_after_inserting_and_deleting_blocks_above_it() {
        let now = Instant::now();
        let paragraph = "alpha beta gamma delta epsilon zeta eta theta";
        let mut preview = Preview::new(&lines(&["# Before", "# Delete later", paragraph, "# End", "# Tail"]));
        let mut screen = Screen::sized(13, 3);
        preview.go_to_line(2);
        preview.draw(&mut screen, 2);
        assert!(preview.scroll(1));

        preview.replace_at(&lines(&["# Inserted", "# Before", "# Delete later", paragraph, "# End", "# Tail"]), now);
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));
        assert_eq!(preview.source_line(), 3);

        preview.replace_at(&lines(&["# Inserted", paragraph, "# End", "# Tail"]), now);
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));
        assert_eq!(preview.source_line(), 1);
    }

    #[test]
    fn keeps_reading_the_same_text_when_a_line_is_inserted_inside_the_paragraph() {
        let now = Instant::now();
        let old = ["# Lead", "alpha beta", "gamma delta epsilon zeta", "eta theta omega", "# End"];
        let mut preview = Preview::new(&lines(&old));
        let mut screen = Screen::sized(13, 3);
        preview.go_to_line(2);
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));

        let inserted = ["# Lead", "alpha beta", "prefix long", "gamma delta epsilon zeta", "eta theta omega", "# End"];
        preview.replace_at(&lines(&inserted), now);
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));
        assert_eq!(preview.source_line(), 3);

        let disjoint = ["# Lead", "alpha beta", "gamma delta epsilon zeta", "eta theta revised", "# End"];
        preview.replace_at(&lines(&disjoint), now);
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));
        assert!(screen.text()[1].ends_with('!'));
        assert_eq!(preview.source_line(), 2);
    }

    #[test]
    fn keeps_the_source_line_anchor_when_a_paragraph_splits_and_merges() {
        let now = Instant::now();
        let merged = lines(&["# Lead", "gamma beta", "gamma delta", "eta theta omega", "# End"]);
        let split = lines(&["# Lead", "gamma beta", "", "gamma delta", "eta theta omega", "# End"]);
        let mut preview = Preview::new(&merged);
        let mut screen = Screen::sized(13, 3);
        preview.go_to_line(2);
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));

        for (input, source) in [(&split, 3), (&merged, 2)] {
            preview.replace_at(input, now);
            screen.clear();
            preview.draw(&mut screen, 2);
            assert!(screen.text()[1].starts_with("gamma delta"));
            assert_eq!(preview.source_line(), source);
        }
    }

    #[test]
    fn keeps_a_wrapped_anchor_in_a_uniquely_moved_paragraph() {
        let paragraph = "alpha beta gamma delta epsilon zeta eta theta";
        let old = lines(&["# Lead", paragraph, "# Kept one", "# Kept two", "# Tail"]);
        let mut preview = Preview::new(&old);
        let mut screen = Screen::sized(13, 3);
        preview.go_to_line(1);
        preview.draw(&mut screen, 2);
        assert!(preview.scroll(1));

        let new = lines(&["# Lead", "# Kept one", "# Kept two", paragraph, "# Tail"]);
        preview.replace_at(&new, Instant::now());
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("gamma delta"));
        assert_eq!(preview.source_line(), 3);
    }

    #[test]
    fn continues_at_the_next_surviving_block_when_the_anchor_is_deleted() {
        let mut preview = Preview::new(&lines(&["# Above", "# Removed", "# Next", "# Last"]));
        let mut screen = Screen::sized(24, 2);
        preview.go_to_line(1);
        preview.draw(&mut screen, 1);
        preview.replace_at(&lines(&["# Above", "# Next", "# Last"]), Instant::now());
        screen.clear();
        preview.draw(&mut screen, 1);
        assert!(screen.text()[1].starts_with("Next"));
        assert!(!screen.text()[1].ends_with('!'));
        assert_eq!(preview.source_line(), 1);
        assert_eq!(preview.mark_deadline(), None);
    }

    #[test]
    fn keeps_an_unchanged_anchor_after_an_insertion_beyond_local_lookahead() {
        let old = ["# Old head", "# Anchor", "# Still here", "# Old tail"];
        let mut preview = Preview::new(&lines(&old));
        let mut screen = Screen::sized(24, 4);
        preview.go_to_line(1);
        preview.draw(&mut screen, 3);

        let mut new = vec!["# Inserted"; 40];
        new.insert(0, "# New head");
        new.extend(["# Anchor", "# Still here", "# New tail"]);
        preview.replace_at(&lines(&new), Instant::now());
        screen.clear();
        preview.draw(&mut screen, 3);
        let rows = screen.text();
        assert_eq!(&rows[1..3], ["Anchor", "Still here"]);
        assert!(rows[3].starts_with("New tail") && rows[3].ends_with('!'));
        assert_eq!(preview.source_line(), 41);
    }

    #[test]
    fn updates_source_lines_without_marking_identical_rendered_blocks() {
        let mut preview = Preview::new(&lines(&["# Title", "# Anchor", "# Tail"]));
        let mut screen = Screen::sized(24, 2);
        preview.go_to_line(1);
        preview.draw(&mut screen, 1);
        preview.replace_at(&lines(&["Title", "===", "# Anchor", "# Tail"]), Instant::now());
        screen.clear();
        preview.draw(&mut screen, 1);
        assert!(screen.text()[1].starts_with("Anchor"));
        assert_eq!(preview.source_line(), 2);
        assert_eq!(preview.mark_deadline(), None);
    }

    #[test]
    fn marks_disjoint_changes_without_marking_a_repeated_middle() {
        let mut old = vec!["# Same"; 80];
        old[0] = "# First old";
        old[79] = "# Last old";
        let mut new = old.clone();
        new[0] = "# First new";
        new[79] = "# Last new";
        new.insert(1, "# Inserted");
        new.remove(79);
        let mut preview = Preview::new(&lines(&old));
        preview.replace_at(&lines(&new), Instant::now());
        let mut screen = Screen::sized(30, 81);
        preview.draw(&mut screen, 80);
        let rows = screen.text();
        assert!(rows[1].starts_with("First new") && rows[1].ends_with('!'));
        assert!(rows[2].starts_with("Inserted") && rows[2].ends_with('!'));
        assert!(rows[80].starts_with("Last new") && rows[80].ends_with('!'));
        assert!(rows[3..80].iter().all(|line| line == "Same"));
    }

    #[test]
    fn keeps_repeated_anchors_unmarked_after_a_large_insertion() {
        let mut old = vec!["# Same"; 42];
        old[0] = "# Old head";
        old[41] = "# Old tail";
        let mut preview = Preview::new(&lines(&old));
        let mut screen = Screen::sized(24, 3);
        preview.go_to_line(10);
        preview.draw(&mut screen, 2);

        let mut new = vec!["# Inserted"; 41];
        new[0] = "# New head";
        new.extend_from_slice(&old[1..41]);
        new.push("# New tail");
        preview.replace_at(&lines(&new), Instant::now());
        screen.clear();
        preview.draw(&mut screen, 2);
        assert_eq!(preview.source_line(), 50);
        assert!(screen.text()[1..].iter().all(|line| line.starts_with("Same") && !line.ends_with('!')));

        preview.go_to_line(80);
        screen.clear();
        preview.draw(&mut screen, 2);
        let rows = screen.text();
        assert!(rows[1].starts_with("Same") && !rows[1].ends_with('!'));
        assert!(rows[2].starts_with("New tail") && rows[2].ends_with('!'));
    }

    #[test]
    fn unchanged_refresh_keeps_the_mark_deadline_and_later_edits_have_their_own() {
        use std::time::Duration;

        let now = Instant::now();
        let mut preview = Preview::new(&lines(&["# Start", "# First", "# Middle", "# Last"]));
        let first = lines(&["# Start", "# First changed", "# Middle", "# Last"]);
        preview.replace_at(&first, now);
        let deadline = preview.mark_deadline().unwrap();
        let next = now + Duration::from_millis(100);
        preview.replace_at(&first, next);
        assert_eq!(preview.mark_deadline(), Some(deadline));

        let second = lines(&["# Start", "# Inserted", "# First changed", "# Middle", "# Last changed"]);
        preview.replace_at(&second, next);
        assert_eq!(preview.mark_deadline(), Some(deadline));
        let mut screen = Screen::sized(30, 6);
        preview.draw(&mut screen, 5);
        assert!(screen.text()[3].ends_with('!'));
        assert!(!screen.text()[4].ends_with('!'));

        preview.expire_marks(deadline);
        screen.clear();
        preview.draw(&mut screen, 5);
        let rows = screen.text();
        assert!(rows[2].ends_with('!') && rows[5].ends_with('!'));
        assert_eq!(rows[3], "First changed");
        assert_eq!(preview.mark_deadline(), Some(deadline + (next - now)));
        preview.expire_marks(preview.mark_deadline().unwrap());
        assert_eq!(preview.mark_deadline(), None);
        screen.clear();
        preview.draw(&mut screen, 5);
        assert!(screen.text().iter().all(|line| !line.ends_with('!')));
    }

    #[test]
    fn marks_rendered_style_changes_but_not_hidden_link_targets() {
        let now = Instant::now();
        let mut preview = Preview::new(&lines(&["[label](old)"]));
        preview.replace_at(&lines(&["[label](new)"]), now);
        assert_eq!(preview.mark_deadline(), None);
        preview.replace_at(&lines(&["label"]), now);
        assert!(preview.mark_deadline().is_some());
        let mut screen = Screen::sized(15, 2);
        preview.draw(&mut screen, 1);
        assert!(screen.text()[1].ends_with('!'));
    }

    #[test]
    fn follow_end_repins_on_replacement_and_resize_until_manual_navigation() {
        let now = Instant::now();
        let mut preview = Preview::new(&lines(&["# First", "# Middle", "# Last"]));
        assert!(!preview.following());
        assert!(preview.follow_end());
        assert!(!preview.follow_end());
        let mut screen = Screen::sized(25, 3);
        preview.draw(&mut screen, 2);
        assert!(screen.text()[2].starts_with("Last"));

        preview.replace_at(&lines(&["# First", "# Middle", "# Last", "# Wide text here"]), now);
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[2].starts_with("Wide text here"));
        let mut narrow = Screen::sized(7, 4);
        preview.draw(&mut narrow, 3);
        assert!(narrow.text()[1].starts_with("Wide"));
        assert!(narrow.text()[3].starts_with("here"));

        assert!(preview.scroll(isize::MAX));
        assert!(!preview.following());
        assert!(!preview.scroll(isize::MAX));
        preview.replace_at(&lines(&["# First", "# Middle", "# Last", "# Wide text here", "# New"]), now);
        narrow.clear();
        preview.draw(&mut narrow, 3);
        assert!(narrow.text()[3].starts_with("here"));
        assert!(preview.follow_end());
        narrow.clear();
        preview.draw(&mut narrow, 3);
        assert!(narrow.text()[3].starts_with("New"));

        preview.replace_at(&lines(&["# Short"]), now);
        assert!(preview.following());
        assert!(preview.scroll(-1));
        assert!(!preview.following());
        assert!(preview.follow_end());
        preview.go_to_line(0);
        assert!(!preview.following());
    }

    #[test]
    fn keeps_the_logical_anchor_across_width_rounding_and_height_clamping() {
        let paragraph = (0..98).map(|i| format!("w{i:04}")).collect::<Vec<_>>().join(" ");
        let mut preview = Preview::new(&lines(&["# Lead", &paragraph]));
        let mut narrow = Screen::sized(43, 8);
        let mut wide = Screen::sized(49, 8);
        let mut tall = Screen::sized(43, 13);
        preview.draw(&mut narrow, 6);
        assert!(preview.scroll(6));
        for _ in 0..3 {
            narrow.clear();
            preview.draw(&mut narrow, 6);
            assert!(narrow.text()[1].starts_with("w0035"));
            wide.clear();
            preview.draw(&mut wide, 6);
            assert!(wide.text()[1].starts_with("w0032") && wide.text()[1].contains("w0035"));
            tall.clear();
            preview.draw(&mut tall, 11);
            assert!(tall.text()[1].starts_with("w0021"));
        }
        // Even a scroll that can't move at the temporary boundary chooses the visible position.
        assert!(!preview.scroll(isize::MAX));
        narrow.clear();
        preview.draw(&mut narrow, 6);
        assert!(narrow.text()[1].starts_with("w0021"));
    }

    #[test]
    fn consecutive_replacements_keep_the_retained_byte_in_bounds() {
        let mut text = ["abcdefghijklmnopqrstuvwxyz0123456789", "# Tail A", "# Tail B", "# Tail C"];
        let mut preview = Preview::new(&lines(&text));
        let mut screen = Screen::sized(8, 3);
        preview.draw(&mut screen, 2);
        assert!(preview.scroll(2));
        screen.clear();
        preview.draw(&mut screen, 2);
        assert!(screen.text()[1].starts_with("opqrstu"));

        let mut narrow = Screen::sized(3, 3);
        for (edited, expected) in [("東西南北終", "終"), ("海鳥", "鳥"), ("unrelated text", "re")] {
            text[0] = edited;
            preview.replace(&lines(&text));
            narrow.clear();
            preview.draw(&mut narrow, 2);
            assert!(narrow.text()[1].starts_with(expected), "{edited}: {:?}", narrow.text());
        }
    }

    #[test]
    fn bounds_malformed_markup_input_and_narrow_reflow() {
        let raw = format!("{}{}", "[".repeat(MAX_BYTES - 2), "終ignored");
        let mut preview = Preview::new(&lines(&[&raw]));
        assert_eq!(preview.doc.text.len(), MAX_BYTES - 2 + TRIMMED.len());
        let mut screen = Screen::sized(1, 2);
        preview.draw(&mut screen, 1);
        assert_eq!(screen.text()[1], "[");
        assert!(preview.layout.trimmed);
        assert_eq!(preview.layout.rows.len(), MAX_ROWS);
        assert!(preview.scroll(isize::MAX));
        assert!(!preview.scroll(1));

        let mut wide = Screen::sized(100, 2);
        preview.draw(&mut wide, 1);
        assert!(wide.text()[1].starts_with(TRIMMED));
        // The byte cap interrupted source line zero. An editor jump to that
        // line must show its start, not the notice that shares the number.
        preview.go_to_line(0);
        wide.clear();
        preview.draw(&mut wide, 1);
        assert!(wide.text()[1].starts_with('['));

        let removed: Vec<_> = (0..=MAX_DIFF_LINES).map(|_| Line::new(LineKind::Del, 1, "")).collect();
        assert_eq!(Document::parse(&removed).text, TRIMMED);

        let headings: Vec<_> = (1..=u32::try_from(MAX_LINES + 2).unwrap())
            .map(|line| Line::new(LineKind::Same, line, if line % 2 == 1 { "heading" } else { "===" }))
            .collect();
        let doc = Document::parse(&headings);
        assert_eq!(doc.blocks.len(), MAX_LINES / 2 + 1);
        assert!(doc.text.ends_with(TRIMMED));
    }
}
