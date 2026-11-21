//! Colours tags, attributes, comments and entities in HTML and XML.
//!
//! Text between tags is prose, where an apostrophe is not a quote. The text
//! of a `script` or `style` element is read as JavaScript or CSS.

use super::{Class, Lang, Span, State, find_end, langs};

/// An element whose text is code in another language.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Raw {
    Script,
    Style,
}

impl Raw {
    fn named(name: &[u8]) -> Option<Self> {
        if name.eq_ignore_ascii_case(b"script") {
            Some(Self::Script)
        } else if name.eq_ignore_ascii_case(b"style") {
            Some(Self::Style)
        } else {
            None
        }
    }

    fn close(self) -> &'static [u8] {
        match self {
            Self::Script => b"</script",
            Self::Style => b"</style",
        }
    }

    fn lang(self) -> &'static Lang {
        match self {
            Self::Script => langs::script(),
            Self::Style => langs::style(),
        }
    }
}

/// With `embedded`, the text of a `script` or `style` element is read as code.
pub(super) fn walk<const DRAW: bool>(
    text: &str,
    mut i: usize,
    embedded: bool,
    state: &mut State,
    emit: &mut impl FnMut(Span),
) {
    let b = text.as_bytes();
    while i < b.len() {
        i = match state {
            State::Tag { quote, raw } => {
                let (quote, raw) = (*quote, *raw);
                tag(b, i, quote, raw, state, emit)
            }
            State::Raw(raw, inner) => {
                let end = find_caseless(b, i, raw.close()).unwrap_or(b.len());
                // The closing tag starts with `<`, so both ends are character boundaries.
                raw.lang().walk_within::<DRAW>(&text[i..end], inner, &mut |(from, to, class)| {
                    emit((from + i, to + i, class));
                });
                if end < b.len() {
                    *state = State::Code;
                }
                end
            }
            State::Code => prose(b, i, embedded, state, emit),
            _ => {
                *state = State::Code;
                i
            }
        };
    }
}

fn find_caseless(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let last = b.len().checked_sub(needle.len())?;
    (from..=last).find(|&i| b[i] == needle[0] && b[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Skips text up to the next tag, comment or entity, and colours that.
fn prose(b: &[u8], from: usize, embedded: bool, state: &mut State, emit: &mut impl FnMut(Span)) -> usize {
    let Some(at) = b[from..].iter().position(|&c| matches!(c, b'<' | b'&')) else { return b.len() };
    let at = from + at;
    if b[at] == b'&' {
        return entity(b, at, emit);
    }
    if b[at..].starts_with(b"<!--") {
        let close = find_end(b, at + 4, "-->");
        let end = close.unwrap_or(b.len());
        emit((at, end, Class::Comment));
        if close.is_none() {
            *state = State::Comment(0, 1);
        }
        return end;
    }
    let closing = b.get(at + 1) == Some(&b'/');
    let name = at + 1 + usize::from(matches!(b.get(at + 1), Some(b'/' | b'!' | b'?')));
    // A `<` before anything else is text, as in `a < b`.
    if !b.get(name).is_some_and(u8::is_ascii_alphabetic) {
        return at + 1;
    }
    let end = name + b[name..].iter().take_while(|&&c| !c.is_ascii_whitespace() && !matches!(c, b'>' | b'/')).count();
    emit((name, end, Class::Keyword));
    let raw = if closing || !embedded { None } else { Raw::named(&b[name..end]) };
    *state = State::Tag { quote: None, raw };
    end
}

/// A character reference such as `&amp;` or `&#x2014;`.
fn entity(b: &[u8], at: usize, emit: &mut impl FnMut(Span)) -> usize {
    let name = at + 1 + usize::from(b.get(at + 1) == Some(&b'#'));
    let len = b[name..].iter().take(32).take_while(|c| c.is_ascii_alphanumeric()).count();
    if len > 0 && b.get(name + len) == Some(&b';') {
        emit((at, name + len + 1, Class::Number));
        name + len + 1
    } else {
        at + 1
    }
}

/// What cannot be part of an attribute's name, including the `?` that closes `<?xml … ?>`.
fn attribute_end(c: u8) -> bool {
    c.is_ascii_whitespace() || matches!(c, b'=' | b'>' | b'/' | b'"' | b'\'' | b'?')
}

/// Colours attribute names and quoted values up to the end of the tag or the line.
fn tag(
    b: &[u8],
    mut i: usize,
    quote: Option<u8>,
    raw: Option<Raw>,
    state: &mut State,
    emit: &mut impl FnMut(Span),
) -> usize {
    if let Some(mark) = quote {
        let Some(close) = b[i..].iter().position(|&c| c == mark) else {
            emit((i, b.len(), Class::Str));
            return b.len();
        };
        emit((i, i + close + 1, Class::Str));
        i += close + 1;
        *state = State::Tag { quote: None, raw };
    }
    while i < b.len() {
        match b[i] {
            b'>' => {
                let empty = i > 0 && b[i - 1] == b'/';
                *state = match raw {
                    Some(raw) if !empty => State::Raw(raw, Box::default()),
                    _ => State::Code,
                };
                return i + 1;
            }
            mark @ (b'"' | b'\'') => {
                let Some(close) = b[i + 1..].iter().position(|&c| c == mark) else {
                    emit((i, b.len(), Class::Str));
                    *state = State::Tag { quote: Some(mark), raw };
                    return b.len();
                };
                emit((i, i + close + 2, Class::Str));
                i += close + 2;
            }
            b'=' => {
                i += 1 + b[i + 1..].iter().take_while(|c| c.is_ascii_whitespace()).count();
                // A value without quotes is not another attribute's name.
                if !matches!(b.get(i), Some(b'"' | b'\'')) {
                    i += b[i..].iter().take_while(|&&c| !c.is_ascii_whitespace() && c != b'>').count();
                }
            }
            c if attribute_end(c) => i += 1,
            _ => {
                let end = i + b[i..].iter().take_while(|&&c| !attribute_end(c)).count();
                emit((i, end, Class::Key));
                i = end;
            }
        }
    }
    i
}
