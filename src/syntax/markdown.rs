//! Colours the structure of Markdown: headings, code, links, list marks and quotes.
//!
//! Emphasis, tables and embedded HTML stay plain. A code block is one colour
//! whatever language it holds.

use super::{Class, Span, State, find_end, is_word};

/// Start from `start`, which is past the end of a comment closed on this line.
pub(super) fn walk(text: &str, start: usize, state: &mut State, emit: &mut impl FnMut(Span)) {
    let b = text.as_bytes();
    let indent = b.iter().take_while(|&&c| c == b' ').count();
    // At any depth: a code block in a list item is indented with the item.
    let fence = fence_run(&b[indent..]);
    match state {
        State::Fence { mark, len } => {
            let closes = fence.is_some_and(|(m, n, after)| m == *mark && n >= *len && after.trim_ascii().is_empty());
            if closes {
                emit((0, b.len(), Class::Keyword));
                *state = State::Code;
            } else if !b.is_empty() {
                emit((0, b.len(), Class::Str));
            }
            return;
        }
        State::Code => {}
        _ => *state = State::Code,
    }
    let mut from = start;
    if start == 0 {
        // A block can open on the line of the bullet it belongs to.
        let fence = fence.or_else(|| {
            let item = list_mark(b, &mut |_| {});
            let text = item + b[item..].iter().take_while(|&&c| c == b' ').count();
            (item > 0).then(|| fence_run(&b[text..])).flatten()
        });
        // The words after a backtick fence cannot contain a backtick; that is inline code.
        if let Some((mark, len, after)) = fence
            && (mark == b'~' || !after.contains(&b'`'))
        {
            emit((0, b.len(), Class::Keyword));
            *state = State::Fence { mark, len };
            return;
        }
        let rest = &b[indent..];
        if indent <= 3 && heading(rest) {
            emit((0, b.len(), Class::Keyword));
            return;
        }
        if indent <= 3 && rest.first() == Some(&b'>') {
            emit((0, b.len(), Class::Comment));
            return;
        }
        from = list_mark(b, emit);
    }
    Inline::default().walk(b, from, state, emit);
}

/// The mark, length and following text of a run that can open or close a code block.
fn fence_run(rest: &[u8]) -> Option<(u8, usize, &[u8])> {
    let mark = *rest.first().filter(|c| matches!(c, b'`' | b'~'))?;
    let len = rest.iter().take_while(|&&c| c == mark).count();
    (len >= 3).then_some((mark, len, &rest[len..]))
}

fn heading(rest: &[u8]) -> bool {
    let level = rest.iter().take_while(|&&c| c == b'#').count();
    (1..=6).contains(&level) && rest.get(level).is_none_or(|&c| c == b' ')
}

/// Colours a bullet or number that starts a list item, and returns where its text begins.
fn list_mark(b: &[u8], emit: &mut impl FnMut(Span)) -> usize {
    let at = b.iter().take_while(|&&c| c == b' ').count();
    let digits = b[at..].iter().take(9).take_while(|c| c.is_ascii_digit()).count();
    let end = match b.get(at + digits) {
        Some(b'-' | b'*' | b'+') if digits == 0 => at + 1,
        Some(b'.' | b')') if digits > 0 => at + digits + 1,
        _ => return 0,
    };
    if b.get(end) != Some(&b' ') {
        return 0;
    }
    emit((at, end, Class::Number));
    end
}

/// What failed searches on this line have already ruled out, so that a long
/// run of openers is not searched for a closer once for each of them.
#[derive(Default)]
struct Inline {
    /// Bit `n - 1` is set when no run of exactly `n` backticks follows.
    no_ticks: u64,
    /// The next `]`, once looked for.
    bracket: Option<usize>,
    no_bracket: bool,
    no_paren: bool,
    /// No `]` closes a `[text][name]` reference from here on.
    no_reference: bool,
}

impl Inline {
    fn walk(&mut self, b: &[u8], mut i: usize, state: &mut State, emit: &mut impl FnMut(Span)) {
        while i < b.len() {
            i = match b[i] {
                b'\\' => i + 2,
                b'`' => self.code(b, i, emit),
                b'[' => self.link(b, i, emit),
                b'<' if b[i..].starts_with(b"<!--") => {
                    let close = find_end(b, i + 4, "-->");
                    emit((i, close.unwrap_or(b.len()), Class::Comment));
                    let Some(end) = close else {
                        *state = State::Comment(0, 1);
                        return;
                    };
                    end
                }
                b'h' if (i == 0 || !is_word(b[i - 1]))
                    && (b[i..].starts_with(b"http://") || b[i..].starts_with(b"https://")) =>
                {
                    let end = i + b[i..]
                        .iter()
                        .take_while(|&&c| !c.is_ascii_whitespace() && !matches!(c, b')' | b'>' | b'"' | b'`'))
                        .count();
                    emit((i, end, Class::Type));
                    end
                }
                _ => i + 1,
            };
        }
    }

    /// Inline code ends at a run of as many backticks as opened it.
    fn code(&mut self, b: &[u8], i: usize, emit: &mut impl FnMut(Span)) -> usize {
        let len = b[i..].iter().take_while(|&&c| c == b'`').count();
        let open = i + len;
        let Some(bit) = len.checked_sub(1).filter(|&n| n < 64).map(|n| 1u64 << n) else { return open };
        if self.no_ticks & bit != 0 {
            return open;
        }
        let mut at = open;
        while let Some(next) = b[at..].iter().position(|&c| c == b'`') {
            let run = b[at + next..].iter().take_while(|&&c| c == b'`').count();
            at += next + run;
            if run == len {
                emit((i, at, Class::Str));
                return at;
            }
        }
        self.no_ticks |= bit;
        open
    }

    /// `[text](target)`, `[text][name]` or a `[name]:` definition.
    fn link(&mut self, b: &[u8], i: usize, emit: &mut impl FnMut(Span)) -> usize {
        if !self.no_bracket && self.bracket.is_none_or(|close| close <= i) {
            self.bracket = b[i + 1..].iter().position(|&c| c == b']').map(|at| i + 1 + at);
            self.no_bracket = self.bracket.is_none();
        }
        let Some(close) = self.bracket.filter(|&close| close > i) else { return i + 1 };
        let after = close + 1;
        let target = match b.get(after) {
            Some(b'(') if !self.no_paren => {
                let end = b[after..].iter().position(|&c| c == b')');
                self.no_paren = end.is_none();
                end.map(|at| (after + at + 1, Class::Type))
            }
            Some(b'[') if !self.no_reference => {
                let end = b[after..].iter().position(|&c| c == b']');
                self.no_reference = end.is_none();
                end.map(|at| (after + at + 1, Class::Key))
            }
            Some(b':') => Some((after, Class::Key)),
            _ => None,
        };
        let Some((end, class)) = target else { return i + 1 };
        emit((i, after, Class::Key));
        if end > after {
            emit((after, end, class));
        }
        end
    }
}
