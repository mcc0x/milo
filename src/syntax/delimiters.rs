//! Delimiters that depend on the opener rather than a fixed table entry.

use std::rc::Rc;

use super::{Escape, Quote, find_end, is_word, string_end, word_end};

#[derive(Clone, Copy)]
pub(super) enum Dialect {
    Plain,
    Rust,
    Cpp,
    Csharp,
    Php,
    Javascript,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Delimiter {
    RustRaw(u8),
    CppRaw(Rc<[u8]>),
    CsharpRaw(usize),
    Heredoc(Rc<[u8]>),
    Template(Template),
}

impl Dialect {
    /// Returns where a string ends, and the delimiter to keep if it stays open.
    pub(super) fn string(self, b: &[u8], from: usize) -> Option<(usize, Option<Delimiter>)> {
        let rest = &b[from..];
        let (end, open) = match self {
            Self::Rust if matches!(b[from], b'b' | b'c' | b'r') => {
                let (start, hashes) = rust_hashes(rest)?;
                let end = repeated_end(b, from + start, b'"', b'#', hashes.into());
                (end, end.is_none().then_some(Delimiter::RustRaw(hashes)))
            }
            Self::Cpp if matches!(b[from], b'R' | b'u' | b'U' | b'L') => {
                let (start, marks) = cpp_open(rest)?;
                let end = cpp_end(b, from + start, marks);
                (end, end.is_none().then(|| Delimiter::CppRaw(marks.into())))
            }
            Self::Csharp if rest.starts_with(b"\"\"\"") => {
                let width = rest.iter().take_while(|&&c| c == b'"').count();
                let end = repeated_end(b, from + width, b'"', b'"', width - 1);
                (end, end.is_none().then_some(Delimiter::CsharpRaw(width)))
            }
            Self::Php if rest.starts_with(b"<<<") => (None, Some(Delimiter::Heredoc(php_label(&rest[3..])?.into()))),
            Self::Javascript if b[from] == b'`' => {
                let mut template = Template::default();
                let end = template.end(b, from + 1);
                (end, end.is_none().then_some(Delimiter::Template(template)))
            }
            _ => return None,
        };
        Some((end.unwrap_or(b.len()), open))
    }
}

impl Delimiter {
    pub(super) fn end(&mut self, b: &[u8]) -> Option<usize> {
        match self {
            Self::RustRaw(hashes) => repeated_end(b, 0, b'"', b'#', (*hashes).into()),
            Self::CsharpRaw(width) => repeated_end(b, 0, b'"', b'"', *width - 1),
            Self::CppRaw(marks) => cpp_end(b, 0, marks),
            Self::Heredoc(label) => {
                let line = b.trim_ascii_start();
                let after = line.strip_prefix(&**label)?;
                if after.first().is_some_and(|&c| is_word(c)) {
                    return None;
                }
                Some(b.len() - after.len() + usize::from(after.starts_with(b";")))
            }
            Self::Template(template) => template.end(b, 0),
        }
    }
}

fn rust_hashes(rest: &[u8]) -> Option<(usize, u8)> {
    let prefix = usize::from(matches!(rest.first(), Some(b'b' | b'c')));
    let after = rest.get(prefix..)?.strip_prefix(b"r")?;
    let count = after.iter().take_while(|&&c| c == b'#').count();
    (after.get(count) == Some(&b'"')).then_some(())?;
    Some((prefix + count + 2, u8::try_from(count).ok()?))
}

fn repeated_end(b: &[u8], from: usize, first: u8, repeat: u8, count: usize) -> Option<usize> {
    // A failed run contains fewer than `count` repeats. Skip it rather than
    // trying the same bytes again for delimiters made of repeated quotes.
    let mut i = from;
    while i < b.len() {
        if b[i] != first {
            i += 1;
            continue;
        }
        i += 1;
        let n = b[i..].iter().take(count).take_while(|&&c| c == repeat).count();
        i += n;
        if n == count {
            return Some(i);
        }
    }
    None
}

fn cpp_open(rest: &[u8]) -> Option<(usize, &[u8])> {
    let prefix = [b"R\"".as_slice(), b"u8R\"", b"uR\"", b"UR\"", b"LR\""]
        .iter()
        .find(|&&prefix| rest.starts_with(prefix))?
        .len();
    let marks = &rest[prefix..];
    let end = marks.iter().take(17).position(|&c| c == b'(')?;
    let marks = &marks[..end];
    marks.iter().all(|&c| c.is_ascii_graphic() && !matches!(c, b')' | b'\\')).then_some((prefix + end + 1, marks))
}

fn cpp_end(b: &[u8], from: usize, marks: &[u8]) -> Option<usize> {
    let offset = marks.len() + 1;
    (from..b.len().saturating_sub(offset))
        .find(|&i| b[i] == b')' && b[i + 1..i + offset] == *marks && b[i + offset] == b'"')
        .map(|i| i + offset + 1)
}

fn php_label(after: &[u8]) -> Option<&[u8]> {
    // Check the label first so overlapping `<<<` candidates cannot all scan the line's trailing whitespace.
    let after = after.trim_ascii_start();
    let (label, quote) = match after {
        [quote @ (b'\'' | b'"'), inner @ ..] => (inner, Some(quote)),
        _ => (after, None),
    };
    if !label.first().is_some_and(|&c| is_word(c) && !c.is_ascii_digit()) {
        return None;
    }
    let (label, tail) = label.split_at(word_end(label, 1));
    let tail = match quote {
        Some(quote) => tail.strip_prefix(&[*quote])?,
        None => tail,
    };
    tail.trim_ascii_start().is_empty().then_some(label)
}

/// Inside `${…}` we only track what can hide a closing brace or backtick.
#[derive(Clone, Debug, Default)]
pub struct Template {
    /// Brace depth inside `${…}`, or `None` while in the text of a template.
    braces: Option<usize>,
    parent: Option<Rc<Parent>>,
    barrier: Option<Barrier>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Barrier {
    Comment,
    Quote(u8),
}

impl Barrier {
    fn end(self, b: &[u8], i: usize) -> Option<usize> {
        match self {
            Self::Comment => find_end(b, i, "*/"),
            Self::Quote(mark) => {
                let close = if mark == b'\'' { "'" } else { "\"" };
                let q = Quote { open: close, close, multiline: false, escape: Escape::Backslash, extra_quotes: 0 };
                string_end(b, i, &q)
            }
        }
    }
}

/// Share unchanged ancestors between line starts rather than copying the stack.
#[derive(Debug)]
struct Parent {
    braces: usize,
    parent: Option<Rc<Self>>,
}

impl Drop for Parent {
    fn drop(&mut self) {
        // A file can contain deeply nested templates. Drop unique ancestors iteratively.
        let mut next = self.parent.take();
        while let Some(mut parent) = next.and_then(Rc::into_inner) {
            next = parent.parent.take();
        }
    }
}

impl PartialEq for Template {
    fn eq(&self, other: &Self) -> bool {
        self.braces == other.braces
            && self.barrier == other.barrier
            && match (&self.parent, &other.parent) {
                (None, None) => true,
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                _ => false,
            }
    }
}

impl Template {
    fn end(&mut self, b: &[u8], mut i: usize) -> Option<usize> {
        if let Some(barrier) = self.barrier {
            i = barrier.end(b, i)?;
            self.barrier = None;
        }
        while i < b.len() {
            let rest = &b[i..];
            let barrier = if let Some(braces) = self.braces {
                match rest {
                    [b'/', b'/', ..] => return None,
                    [b'/', b'*', ..] => Some((Barrier::Comment, i + 2)),
                    [mark @ (b'\'' | b'"'), ..] => Some((Barrier::Quote(*mark), i + 1)),
                    _ => {
                        match b[i] {
                            b'{' => self.braces = Some(braces + 1),
                            b'}' => self.braces = braces.checked_sub(1),
                            b'`' => {
                                self.parent = Some(Rc::new(Parent { braces, parent: self.parent.take() }));
                                self.braces = None;
                            }
                            _ => {}
                        }
                        None
                    }
                }
            } else {
                match rest {
                    [b'`', ..] => {
                        let Some(parent) = self.parent.take() else { return Some(i + 1) };
                        self.braces = Some(parent.braces);
                        self.parent = parent.parent.clone();
                    }
                    [b'$', b'{', ..] => {
                        self.braces = Some(0);
                        i += 1;
                    }
                    [b'\\', ..] => i += 1,
                    _ => {}
                }
                None
            };
            if let Some((barrier, from)) = barrier {
                let Some(end) = barrier.end(b, from) else {
                    self.barrier = Some(barrier);
                    return None;
                };
                i = end;
            } else {
                i += 1;
            }
        }
        None
    }
}
