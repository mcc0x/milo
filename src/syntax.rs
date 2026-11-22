//! Finds colour spans in source text using delimiters and word lists.
//!
//! Open comments and multiline strings carry on to the next line. This
//! scanner does not parse a grammar, so it can mistake text for an opener
//! and colour later lines as a string or comment until a closer appears.

mod delimiters;
mod langs;
mod markdown;
mod markup;

use delimiters::{Delimiter, Dialect};
pub use langs::for_file;

/// What a coloured piece of source text resembles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Comment,
    Str,
    /// Numbers, and named constants such as `true` or `MAX_SIZE`.
    Number,
    Keyword,
    Type,
    Function,
    /// A field name or table header in a data file.
    Key,
}

/// Bytes `start..end` of a line are of one class.
pub type Span = (usize, usize, Class);

/// A pair of string delimiters and the rules for text between them.
struct Quote {
    open: &'static str,
    close: &'static str,
    /// May run on over several lines.
    multiline: bool,
    escape: Escape,
    /// How many quotes may appear immediately before the closing delimiter.
    extra_quotes: u8,
}

#[derive(Clone, Copy)]
enum Escape {
    Backslash,
    Doubled,
    None,
}

/// The delimiters, words and conventions the scanner uses for one language.
pub struct Lang {
    /// Markers that turn the rest of a line into a comment.
    line_comments: &'static [&'static str],
    /// Pairs of delimiters for comments that can cross line breaks.
    block_comments: &'static [(&'static str, &'static str)],
    dialect: Dialect,
    /// How a line is read: as code, as tags around text, or as prose.
    grammar: Grammar,
    /// Tried in order, so a longer opener must come before its prefix.
    quotes: &'static [Quote],
    /// Words reserved by the language.
    keywords: &'static [&'static str],
    /// Words that stand for a value: `true`, `nil`, `None`.
    constants: &'static [&'static str],
    /// Treat an apostrophe as an opener only if it looks like a character literal.
    chars: bool,
    /// Colour a leading `#` and its directive name as a keyword.
    directives: bool,
    /// Distinguish names that resemble types, calls and constants.
    names: bool,
    /// Recognise colon-separated field names in JSON and YAML.
    colon_keys: bool,
    /// Start line comments only at the start of a line or after whitespace.
    spaced_comments: bool,
    /// Match quotes and named values only where a YAML value can begin.
    scalars: bool,
    /// Ignore likely apostrophes and URLs in JSX prose.
    jsx: bool,
    /// Read PHP's `#[` as an attribute opener rather than a comment.
    attributes: bool,
    /// Recognise TOML dotted keys and table headers, which may contain quoted parts.
    toml_keys: bool,
    /// Match keywords and constants in any letter case.
    caseless: bool,
    /// Reserve keywords only as the first word of a line, after Make's optional `-`.
    leading_keywords: bool,
    /// A block comment inside another must close before the outer one does.
    nested_comments: bool,
    /// Open a block comment only at the start of a line, as Ruby's `=begin` does.
    leading_block_comments: bool,
    /// Colour CSS `@rules` as keywords and `#rgb` values as numbers.
    css: bool,
}

/// The shape of a file's lines, which decides the walker that reads them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grammar {
    Code,
    /// Tags and attributes around text that is not code, as in HTML. The
    /// text of `script` and `style` elements is code in another language.
    Markup,
    /// The same without those elements: in XML, `style` is a name like any other.
    Xml,
    Markdown,
}

/// What is still open at the start of a line.
#[derive(Clone, Default, PartialEq, Debug)]
pub enum State {
    #[default]
    Code,
    Comment(u8, usize),
    Quote(u8),
    Delimited(Delimiter),
    /// Inside a markup tag, and inside an attribute value if a quote is given.
    Tag {
        quote: Option<u8>,
        raw: Option<markup::Raw>,
    },
    /// Inside a markup element whose text is code, with what that code has open.
    Raw(markup::Raw, Box<State>),
    /// Inside a Markdown code block that a run of `len` `mark`s closes.
    Fence {
        mark: u8,
        len: usize,
    },
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn word_end(b: &[u8], start: usize) -> usize {
    start + b[start..].iter().take_while(|&&c| is_word(c)).count()
}

fn number_end(b: &[u8], start: usize) -> usize {
    let end = word_end(b, start);
    // A decimal point must have a digit after it, unlike a range or member access.
    if b.get(end) == Some(&b'.') && b.get(end + 1).is_some_and(u8::is_ascii_digit) { word_end(b, end + 1) } else { end }
}

/// Most delimiters can be ruled out by looking at just the first byte.
fn begins(rest: &[u8], mark: &str) -> bool {
    let mark = mark.as_bytes();
    mark.first().is_none_or(|first| rest.first() == Some(first) && rest.starts_with(mark))
}

fn listed(words: &[&str], word: &str) -> bool {
    let first = word.as_bytes().first();
    words.iter().any(|w| w.as_bytes().first() == first && *w == word)
}

/// Where `needle` first ends in `b` at or after `from`.
fn find_end(b: &[u8], from: usize, needle: &str) -> Option<usize> {
    let last = b.len().checked_sub(needle.len())?;
    (from..=last).find(|&i| begins(&b[i..], needle)).map(|i| i + needle.len())
}

/// Rust comments nest; C-style comments in other languages end at the first closer.
fn comment_end(b: &[u8], mut i: usize, close: &str, depth: &mut usize, nested: bool) -> Option<usize> {
    if !nested {
        return find_end(b, i, close);
    }
    while i < b.len() {
        match &b[i..] {
            [b'/', b'*', ..] => *depth += 1,
            [b'*', b'/', ..] => {
                *depth -= 1;
                if *depth == 0 {
                    return Some(i + 2);
                }
            }
            _ => {
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    None
}

/// Where the closing quote ends, skipping escaped delimiters.
fn string_end(b: &[u8], from: usize, q: &Quote) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if matches!(q.escape, Escape::Backslash) && b[i] == b'\\' {
            i += 2;
        } else if begins(&b[i..], q.close) {
            let end = i + q.close.len();
            if !matches!(q.escape, Escape::Doubled) || !begins(&b[end..], q.close) {
                // TOML permits one or two literal quotes just before its closing triple.
                let extra = b[end..].iter().take(q.extra_quotes.into()).take_while(|&&c| c == b[i]).count();
                return Some(end + extra);
            }
            i = end + q.close.len();
        } else {
            i += 1;
        }
    }
    None
}

/// Recognise a character, including Rust's `'\u{1F600}'`, not a lifetime.
fn is_char(b: &[u8], i: usize) -> bool {
    match b.get(i + 1) {
        Some(b'\\') => b[i + 2..].iter().take(12).any(|&c| c == b'\''),
        Some(_) => b[i + 2..].iter().take(4).position(|&c| c == b'\'').is_some_and(|p| p == 0 || b[i + 1] >= 0x80),
        None => false,
    }
}

fn previous(b: &[u8], i: usize) -> Option<usize> {
    b[..i].iter().rposition(|c| !c.is_ascii_whitespace())
}

fn scalar_start(b: &[u8], i: usize) -> bool {
    previous(b, i).is_none_or(|p| matches!(b[p], b':' | b'[' | b'{' | b',') || (b[p] == b'-' && p + 1 < i))
}

impl Lang {
    /// Replace `out` with ordered, non-empty byte spans for this line.
    /// Use the same `state` on consecutive lines to keep open comments and strings.
    #[cfg(test)]
    pub fn scan(&self, text: &str, state: &mut State, out: &mut Vec<Span>) {
        out.clear();
        self.walk::<true>(text, usize::MAX, state, &mut |span| out.push(span));
    }

    /// Replace `out` with the spans of what a view shows of this line: its
    /// bytes before `shown`. `text` runs on past them so that each span ends
    /// and is classed as in the whole line; spans that start later may be
    /// left out. Nothing is carried on to the next line.
    pub fn scan_shown(&self, text: &str, shown: usize, state: &State, out: &mut Vec<Span>) {
        out.clear();
        self.walk::<true>(text, shown, &mut state.clone(), &mut |span| out.push(span));
    }

    /// Carry open delimiters forward without classifying tokens or keeping spans.
    pub fn advance(&self, text: &str, state: &mut State) {
        self.walk::<false>(text, usize::MAX, state, &mut |_| {});
    }

    /// Reads code as far as byte `until`, and whatever starts before it to its end.
    fn walk<const DRAW: bool>(&self, text: &str, until: usize, state: &mut State, emit: &mut impl FnMut(Span)) {
        let b = text.as_bytes();
        let Some(mut i) = self.resume(b, state, emit) else { return };
        match self.grammar {
            Grammar::Code => {}
            Grammar::Markup => return markup::walk::<DRAW>(text, i, true, state, emit),
            Grammar::Xml => return markup::walk::<DRAW>(text, i, false, state, emit),
            Grammar::Markdown => return markdown::walk(text, i, state, emit),
        }
        // Only the other grammars leave anything else open.
        *state = State::Code;
        let indent = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
        if i <= indent {
            i = self.line_start(b, indent, emit);
        }
        let openers = self.openers();
        let until = until.min(b.len());
        while i < until {
            let c = b[i];
            if c.is_ascii_whitespace() {
                i += 1;
            } else if c.is_ascii() && openers >> c & 1 == 0 {
                // Most bytes begin neither a comment nor a string, in megabytes of one line too.
                i = self.code::<DRAW>(text, i, emit);
            } else if let Some(end) = self.comment(b, i, state, emit) {
                i = end;
            } else if let Some(end) = self.quote::<DRAW>(b, i, state, emit) {
                i = end;
            } else {
                i = self.code::<DRAW>(text, i, emit);
            }
        }
    }

    /// A bit for each ASCII byte at which a comment or a string can begin, so
    /// that neither is looked for at any other. An empty mark begins at them all.
    fn openers(&self) -> u128 {
        let comments = self.line_comments.iter().chain(self.block_comments.iter().map(|(open, _)| open));
        let marks = comments.chain(self.quotes.iter().map(|quote| &quote.open));
        marks.fold(self.dialect.openers(), |bits, mark| match mark.as_bytes().first() {
            Some(&first) if first.is_ascii() => bits | 1 << first,
            Some(_) => bits,
            None => u128::MAX,
        })
    }

    /// Scans part of a line for another walker. The fixed callback type keeps
    /// the two walkers from being compiled once for every caller of each other.
    fn walk_within<const DRAW: bool>(&self, text: &str, state: &mut State, emit: &mut dyn FnMut(Span)) {
        self.walk::<DRAW>(text, usize::MAX, state, &mut |span| emit(span));
    }

    /// Finish any open comment or string, or return `None` if it stays open.
    fn resume(&self, b: &[u8], state: &mut State, emit: &mut impl FnMut(Span)) -> Option<usize> {
        let open = match state {
            State::Code | State::Tag { .. } | State::Raw(..) | State::Fence { .. } => return Some(0),
            State::Comment(k, depth) => self
                .block_comments
                .get(usize::from(*k))
                .map(|(_, close)| (comment_end(b, 0, close, depth, self.nested_comments), Class::Comment)),
            State::Quote(k) => self.quotes.get(usize::from(*k)).map(|q| (string_end(b, 0, q), Class::Str)),
            State::Delimited(mark) => {
                let class = mark.class();
                Some((mark.end(b), class))
            }
        };
        let Some((close, class)) = open else {
            *state = State::Code;
            return Some(0);
        };
        let end = close.unwrap_or(b.len());
        if end > 0 {
            emit((0, end, class));
        }
        if close.is_some() {
            *state = State::Code;
        }
        close
    }

    #[inline]
    fn comment(&self, b: &[u8], i: usize, state: &mut State, emit: &mut impl FnMut(Span)) -> Option<usize> {
        if let Some((end, open)) = self.dialect.comment(b, i) {
            if let Some(open) = open {
                *state = State::Delimited(open);
            }
            emit((i, end, Class::Comment));
            return Some(end);
        }
        let rest = &b[i..];
        let spaced = !self.spaced_comments || i == 0 || b[i - 1].is_ascii_whitespace();
        let attribute = self.attributes && begins(rest, "#[");
        // Suppress likely prose or URL markers without suppressing `code // comment`.
        let jsx_text = self.jsx && i > 0 && (is_word(b[i - 1]) || matches!(b[i - 1], b'>' | b':'));
        if spaced && !attribute && !jsx_text && self.line_comments.iter().any(|c| begins(rest, c)) {
            emit((i, b.len(), Class::Comment));
            return Some(b.len());
        }
        if self.leading_block_comments && i != 0 {
            return None;
        }
        let k = self.block_comments.iter().position(|(open, _)| begins(rest, open))?;
        let (open, close) = self.block_comments[k];
        let mut depth = 1;
        let end = comment_end(b, i + open.len(), close, &mut depth, self.nested_comments).unwrap_or_else(|| {
            *state = State::Comment(u8::try_from(k).expect("block comment index fits in a byte"), depth);
            b.len()
        });
        emit((i, end, Class::Comment));
        Some(end)
    }

    pub fn lookahead(&self) -> usize {
        // A raw string or long bracket can repeat a mark in its opener up to 255 times.
        // In prose, inline code or a link is coloured only once its closer has been read.
        let far = matches!(self.dialect, Dialect::Rust | Dialect::Lua | Dialect::Swift);
        if far || self.grammar == Grammar::Markdown { 260 } else { 32 }
    }

    #[inline]
    fn quote<const DRAW: bool>(
        &self,
        b: &[u8],
        i: usize,
        state: &mut State,
        emit: &mut impl FnMut(Span),
    ) -> Option<usize> {
        if let Some((end, open)) = self.dialect.string(b, i) {
            if DRAW {
                emit((i, end, Class::Str));
            }
            if let Some(open) = open {
                *state = State::Delimited(open);
            }
            return Some(end);
        }
        let k = self.quotes.iter().position(|q| self.opens_quote(b, i, q))?;
        let q = &self.quotes[k];
        let close = string_end(b, i + q.open.len(), q);
        let end = close.unwrap_or(b.len());
        if DRAW {
            let field = close.is_some() && self.colon_keys && b[end..].trim_ascii_start().first() == Some(&b':');
            emit((i, end, if field { Class::Key } else { Class::Str }));
        }
        if close.is_none() && q.multiline {
            *state = State::Quote(u8::try_from(k).expect("quote index fits in a byte"));
        }
        Some(end)
    }

    fn opens_quote(&self, b: &[u8], i: usize, q: &Quote) -> bool {
        if !begins(&b[i..], q.open) || (self.scalars && !scalar_start(b, i)) {
            return false;
        }
        if q.open != "'" {
            return true;
        }
        let jsx_text = self.jsx && previous(b, i).is_some_and(|p| is_word(b[p]) || b[p] == b'>');
        !jsx_text && (!self.chars || is_char(b, i))
    }

    #[inline]
    fn code<const DRAW: bool>(&self, text: &str, i: usize, emit: &mut impl FnMut(Span)) -> usize {
        let b = text.as_bytes();
        let c = b[i];
        if c == b'\\' && b.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
            // Escaped punctuation cannot open a quote or comment.
            // Keep UTF-8 and PHP namespaces such as `\Http` intact.
            i + 2
        } else if c.is_ascii_digit() {
            let end = number_end(b, i);
            emit((i, end, Class::Number));
            end
        } else if self.css && matches!(c, b'@' | b'#') {
            Self::css_mark(b, i, emit)
        } else if is_word(c) {
            let end = word_end(b, i);
            if DRAW {
                self.colour_word(text, i, end, emit);
            }
            end
        } else {
            i + 1
        }
    }

    /// An `@rule` name, or a `#rgb` value after a declaration's colon. A
    /// selector's colon, as in `a:hover #add`, has a name straight after it.
    fn css_mark(b: &[u8], i: usize, emit: &mut impl FnMut(Span)) -> usize {
        let end = i + 1 + b[i + 1..].iter().take_while(|&&c| is_word(c) || c == b'-').count();
        let name = &b[i + 1..end];
        // Look a short way back, so a long run of marks is not read again from
        // each one, and no further than the start of this declaration.
        let declared = |b: &[u8]| {
            let near = &b[b.len().saturating_sub(256)..];
            let from = near.iter().rposition(|c| matches!(c, b'{' | b'}' | b';')).map_or(0, |at| at + 1);
            let near = &near[from..];
            (0..near.len()).any(|at| near[at] == b':' && !near.get(at + 1).is_some_and(u8::is_ascii_alphabetic))
        };
        if b[i] == b'@' {
            if !name.is_empty() {
                emit((i, end, Class::Keyword));
            }
        } else if matches!(name.len(), 3 | 4 | 6 | 8) && name.iter().all(u8::is_ascii_hexdigit) && declared(&b[..i]) {
            emit((i, end, Class::Number));
        }
        end
    }

    fn colour_word(&self, text: &str, i: usize, end: usize, emit: &mut impl FnMut(Span)) {
        let b = text.as_bytes();
        let member = previous(b, i).is_some_and(|p| b[p] == b'$' || (b[p] == b'.' && (p == 0 || b[p - 1] != b'.')));
        let scalar = !self.scalars || scalar_start(b, i);
        if let Some(class) = self.classify_word(&text[i..end], &b[end..], member, scalar) {
            emit((i, end, class));
        }
    }

    /// Handle a directive, leading keyword, data field or TOML header at the line's start.
    fn line_start(&self, b: &[u8], indent: usize, emit: &mut impl FnMut(Span)) -> usize {
        // Swift's `#"raw"#` is a string, not a directive.
        if self.directives && b.get(indent) == Some(&b'#') && self.dialect.string(b, indent).is_none() {
            let name = indent + 1 + b[indent + 1..].iter().take_while(|c| c.is_ascii_whitespace()).count();
            let end = word_end(b, name);
            emit((indent, end, Class::Keyword));
            return end;
        }
        if self.leading_keywords {
            let start = indent + usize::from(b.get(indent) == Some(&b'-'));
            let end = word_end(b, start);
            // Make needs a blank after a directive; `include/x.h:` is a target.
            let ends = b.get(end).is_none_or(u8::is_ascii_whitespace);
            if ends && self.keywords.iter().any(|word| word.as_bytes() == &b[start..end]) {
                emit((start, end, Class::Keyword));
                return end;
            }
        }
        if self.toml_keys && let Some(end) = self.toml_key(b, indent) {
            emit((indent, end, Class::Key));
            return end;
        }
        if self.colon_keys { Self::leading_field(b, indent, emit) } else { 0 }
    }

    fn toml_key(&self, b: &[u8], indent: usize) -> Option<usize> {
        let opener = if b[indent..].starts_with(b"[[") { 2 } else { usize::from(b.get(indent) == Some(&b'[')) };
        let mut at = indent + opener;
        loop {
            at = b.len() - b[at..].trim_ascii_start().len();
            let end = match b.get(at)? {
                b'\'' | b'"' => {
                    let q = self.quotes.iter().find(|q| q.open.as_bytes() == &b[at..at + 1])?;
                    string_end(b, at + 1, q)?
                }
                _ => {
                    let len = b[at..]
                        .iter()
                        .take_while(|&&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                        .count();
                    if len == 0 {
                        return None;
                    }
                    at + len
                }
            };
            let after = b[end..].trim_ascii_start();
            if after.starts_with(b".") {
                at = b.len() - after.len() + 1;
            } else if opener == 0 {
                return after.starts_with(b"=").then_some(end);
            } else {
                return after.starts_with(&b"]]"[..opener]).then_some(b.len() - after.len() + opener);
            }
        }
    }

    fn leading_field(b: &[u8], indent: usize, emit: &mut impl FnMut(Span)) -> usize {
        // "- name: value" in a list is still a field.
        let start = indent + b[indent..].iter().take_while(|&&c| c == b'-' || c == b' ').count();
        let name =
            b[start..].iter().take_while(|&&c| is_word(c) || matches!(c, b'.' | b'-' | b' ' | b'/' | b'$')).count();
        let end = start + b[start..start + name].trim_ascii_end().len();
        let after = b[start + name..].first() == Some(&b':');
        let spaced = b.get(start + name + 1).is_none_or(u8::is_ascii_whitespace);
        if end > start && after && spaced {
            emit((start, end, Class::Key));
            return start + name;
        }
        0
    }

    /// Whether `word` is in a keyword or constant list, in the letter case this language allows.
    fn reserves(&self, words: &[&str], word: &str) -> bool {
        if self.caseless { words.iter().any(|w| w.eq_ignore_ascii_case(word)) } else { listed(words, word) }
    }

    fn classify_word(&self, word: &str, after: &[u8], member: bool, scalar: bool) -> Option<Class> {
        if !self.leading_keywords && (!member || word == "await") && self.reserves(self.keywords, word) {
            return Some(Class::Keyword);
        }
        if scalar && self.reserves(self.constants, word) {
            return Some(Class::Number);
        }
        if !self.names {
            return None;
        }
        let call = matches!(after, [b'(', ..] | [b'!', b'(' | b'[' | b'{', ..]);
        let upper = word.as_bytes()[0].is_ascii_uppercase();
        match (call, upper) {
            (true, _) => Some(Class::Function),
            (false, true) if word.as_bytes().iter().any(u8::is_ascii_lowercase) => Some(Class::Type),
            (false, true) if word.len() > 1 => Some(Class::Number),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::langs::{FILE_LANGS, NAMED_LANGS};
    use super::*;

    fn lang(name: &str) -> &'static Lang {
        for_file(name).unwrap()
    }

    /// Turn byte spans into the text they colour so assertions can name the pieces.
    pub(super) fn pieces<'a>(lang: &Lang, text: &'a str, state: &mut State) -> Vec<(&'a str, Class)> {
        let mut out = Vec::new();
        lang.scan(text, state, &mut out);
        out.into_iter().map(|(a, b, class)| (&text[a..b], class)).collect()
    }

    pub(super) fn one<'a>(lang: &Lang, text: &'a str) -> Vec<(&'a str, Class)> {
        pieces(lang, text, &mut State::Code)
    }

    #[test]
    fn a_lifetime_is_not_a_string_but_a_char_is() {
        let rust = lang("test.rs");
        assert_eq!(one(rust, "fn f<'a>(x: &'a str) {}"), [("fn", Class::Keyword)]);
        assert_eq!(one(rust, "let c = 'x'; let n = '\\n';").iter().filter(|p| p.1 == Class::Str).count(), 2);
        assert_eq!(
            one(rust, "'\\u{1F600}' '\\u{10FFFF}'"),
            [("'\\u{1F600}'", Class::Str), ("'\\u{10FFFF}'", Class::Str),]
        );
    }

    #[test]
    fn a_macro_is_a_call() {
        assert_eq!(one(lang("test.rs"), "println!(\"hi\")"), [("println", Class::Function), ("\"hi\"", Class::Str)]);
    }

    #[test]
    fn a_block_comment_runs_over_lines() {
        let c = lang("test.c");
        let mut state = State::Code;
        assert_eq!(pieces(c, "int a; /* one", &mut state), [("int", Class::Keyword), ("/* one", Class::Comment)]);
        assert_eq!(pieces(c, "two", &mut state), [("two", Class::Comment)]);
        assert_eq!(
            pieces(c, "three */ return 0;", &mut state)[..2],
            [("three */", Class::Comment), ("return", Class::Keyword)]
        );
        assert_eq!(state, State::Code);
    }

    #[test]
    fn a_python_docstring_runs_over_lines_but_a_plain_string_does_not() {
        let python = lang("test.py");
        let mut state = State::Code;
        assert_eq!(pieces(python, "def f(): \"\"\"start", &mut state).last(), Some(&("\"\"\"start", Class::Str)));
        assert_eq!(pieces(python, "end\"\"\" + x", &mut state), [("end\"\"\"", Class::Str)]);
        assert_eq!(pieces(python, "s = 'unclosed", &mut state), [("'unclosed", Class::Str)]);
        assert_eq!(pieces(python, "return None", &mut state), [("return", Class::Keyword), ("None", Class::Number)]);
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        assert_eq!(
            one(lang("test.js"), r#"const s = "a \" b"; // c"#)[1..],
            [(r#""a \" b""#, Class::Str), ("// c", Class::Comment)]
        );
    }

    #[test]
    fn a_comment_marker_inside_a_string_is_text() {
        assert_eq!(one(lang("test.js"), "x = \"http://a\""), [("\"http://a\"", Class::Str)]);
    }

    #[test]
    fn spans_leave_utf8_characters_whole() {
        let line = "let café = \"naïve ☕\"; // ünïcode";
        let mut out = Vec::new();
        let mut state = State::Code;
        lang("test.rs").scan(line, &mut state, &mut out);
        for (a, b, _) in out {
            assert!(line.is_char_boundary(a) && line.is_char_boundary(b));
        }
    }

    #[test]
    fn numbers_leave_ranges_and_members_alone() {
        let rust = lang("test.rs");
        for line in ["for i in 0..n", "self.0.len()", "(0..self.h)"] {
            let spans = one(rust, line);
            assert!(spans.contains(&("0", Class::Number)), "{line}: {spans:?}");
            assert!(!spans.iter().any(|p| p.1 == Class::Number && p.0.contains('.')));
        }
        assert_eq!(
            one(rust, "1.25f32 0xff 3.0.1"),
            [("1.25f32", Class::Number), ("0xff", Class::Number), ("3.0", Class::Number), ("1", Class::Number),]
        );
    }

    #[test]
    fn escaped_code_punctuation_cannot_open_comments_or_strings() {
        let javascript = lang("test.js");
        let mut state = State::Code;
        for line in [r#"url.replace(/\/*$/, "")"#, r"/^https?:\/\//.test(url)"] {
            assert!(!pieces(javascript, line, &mut state).iter().any(|p| p.1 == Class::Comment));
            assert_eq!(state, State::Code);
        }
        assert_eq!(
            one(lang("test.sh"), r"'don'\''t stop'; echo next"),
            [("'don'", Class::Str), ("'t stop'", Class::Str),]
        );
        assert!(one(lang("test.php"), r"App\Http").contains(&("Http", Class::Type)));
        assert_eq!(one(lang("test.rs"), "\\é foo()"), [("foo", Class::Function)]);
    }

    #[test]
    fn member_and_variable_names_are_not_keywords() {
        for lang in [lang("test.js"), lang("test.ts")] {
            for line in ["cache.get(key)", "cache.set(k, v)", "action.type", "Array.from(x)", "z.string()"] {
                assert!(!one(lang, line).iter().any(|p| p.1 == Class::Keyword), "{line}");
            }
        }
        let rust = lang("test.rs");
        assert_eq!(one(lang("test.py"), "re.match("), [("match", Class::Function)]);
        assert_eq!(one(lang("test.php"), "$array $class $default $function"), []);
        assert_eq!(one(rust, "future.await"), [("await", Class::Keyword)]);
        assert!(one(rust, "0..self.h").contains(&("self", Class::Keyword)));
        assert_eq!(one(lang("test.ts"), "action. type"), []);
    }

    fn valid_spans(text: &str, spans: &[Span]) {
        let mut previous = 0;
        for &(start, end, _) in spans {
            assert!(previous <= start && start < end && end <= text.len(), "{text:?}: {spans:?}");
            assert!(text.is_char_boundary(start) && text.is_char_boundary(end), "{text:?}: {spans:?}");
            previous = end;
        }
    }

    #[test]
    fn malformed_source_produces_ordered_spans_on_character_boundaries() {
        let lines = [
            "", " ", "////", "/*", "", "*/", "'''", "\"\"\"", "'", "\"", "`", "\\", "\\é", "é☕中文", "'unterminated",
            "\"unterminated", "/* unterminated", "", "r##\"raw", "\"##", "<<<TEXT", "TEXT;", "#[", "[[", "0..é",
            "0.1.foo", "*/", "--[==[", "é]]", "]==]", "[=", "--[", "-", "#\"\"\"", "é\"\"\"\"#", "#\"é", "##", "]]",
        ];
        let mut out = Vec::new();
        for (_, lang) in FILE_LANGS.iter().chain(NAMED_LANGS) {
            let mut state = State::Code;
            for text in lines {
                lang.scan(text, &mut state, &mut out);
                valid_spans(text, &out);
            }
            // Each line also follows a fresh start, so that no earlier opener swallows it.
            for pair in lines.windows(2) {
                let mut state = State::Code;
                for text in pair {
                    lang.scan(text, &mut state, &mut out);
                    valid_spans(text, &out);
                }
            }
            let open = [
                State::Comment(u8::MAX, 1),
                State::Quote(u8::MAX),
                State::Tag { quote: Some(b'"'), raw: None },
                State::Fence { mark: b'`', len: 3 },
            ];
            for mut state in open {
                lang.scan("é", &mut state, &mut out);
                valid_spans("é", &out);
                if lang.grammar == Grammar::Code {
                    assert_eq!(state, State::Code);
                }
            }
        }
    }

    #[test]
    fn comments_and_strings_begin_only_at_the_bytes_a_language_lists_as_openers() {
        let langs = || FILE_LANGS.iter().chain(NAMED_LANGS);
        // Openers whose closers depend on them, then every mark of every language.
        let mut marks: Vec<String> = [
            "r#\"x\"#", "br\"x\"", "c\"x\"", "R\"(x)\"", "u8R\"(x)\"", "uR\"(x)\"", "UR\"(x)\"", "LR\"(x)\"",
            "\"\"\"x\"\"\"", "<<<EOT", "<<<'EOT'", "<<EOT", "<<~'EOT'", "<<-\"EOT\"", "`x`", "[[x]]", "[==[x]==]",
            "--[[x]]", "--[=[x]=]", "#\"x\"#", "##\"\"\"x",
        ]
        .map(String::from)
        .into();
        for (_, lang) in langs() {
            marks.extend(lang.line_comments.iter().map(|mark| (*mark).to_owned()));
            marks.extend(lang.block_comments.iter().map(|(open, _)| (*open).to_owned()));
            marks.extend(lang.quotes.iter().map(|quote| format!("{}x{}", quote.open, quote.close)));
        }
        // Any byte before a quote or itself, as `b"` and `//` are.
        for byte in (0..128).map(char::from) {
            marks.extend([format!("{byte}\"x\""), format!("{byte}'x'"), format!("{byte}{byte}x"), byte.into()]);
        }
        for (names, lang) in langs() {
            let openers = lang.openers();
            for mark in &marks {
                for before in ["", " ", "a", "(", "#"] {
                    let text = format!("{before}{mark} x");
                    let (b, i) = (text.as_bytes(), before.len());
                    let comment = lang.comment(b, i, &mut State::Code, &mut |_| {}).is_some();
                    let string = lang.quote::<true>(b, i, &mut State::Code, &mut |_| {}).is_some();
                    let listed = !b[i].is_ascii() || openers >> b[i] & 1 == 1;
                    assert!(listed || !(comment || string), "{names:?}: {text:?}");
                }
            }
        }
    }

    #[test]
    fn spans_of_what_is_shown_are_those_of_the_whole_line_that_start_in_it() {
        const PIECES: [&str; 16] = [
            "let x = ", "\"text\" ", "r#\"raw \"# ", "// note ", "/* c */ ", "call(", "Type ", "0x2a ", "'c' ", "é ",
            "界", "name: ", "`t ${x}` ", "# ", "<<EOT ", "[[long]] ",
        ];
        let mut seed = 0x9e37_79b9_usize;
        let mut next = |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        let (mut whole, mut part) = (Vec::new(), Vec::new());
        for (names, lang) in FILE_LANGS.iter().chain(NAMED_LANGS) {
            for _ in 0..200 {
                let text: String = (0..next(12)).map(|_| PIECES[next(PIECES.len())]).collect();
                let shown = next(text.len() + 2);
                for open in [State::Code, State::Comment(0, 1), State::Quote(0)] {
                    lang.scan(&text, &mut open.clone(), &mut whole);
                    lang.scan_shown(&text, shown, &open, &mut part);
                    assert_eq!(part, whole[..part.len()], "{names:?}: {text:?} to {shown}");
                    assert!(whole[part.len()..].iter().all(|span| span.0 >= shown), "{names:?}: {text:?} to {shown}");
                }
            }
        }
    }

    #[test]
    fn scan_replaces_spans_from_the_previous_line() {
        let rust = lang("test.rs");
        let mut out = Vec::new();
        let mut state = State::Code;
        rust.scan("let x = 1;", &mut state, &mut out);
        rust.scan("// done", &mut state, &mut out);
        assert_eq!(out, [(0, 7, Class::Comment)]);
        rust.scan("", &mut state, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn scanner_sources_finish_in_code_state() {
        let rust = lang("test.rs");
        let mut out = Vec::new();
        for source in [include_str!("syntax.rs"), include_str!("syntax/langs.rs")] {
            let mut state = State::Code;
            for text in source.lines() {
                rust.scan(text, &mut state, &mut out);
                valid_spans(text, &out);
            }
            assert_eq!(state, State::default());
        }
    }
}
