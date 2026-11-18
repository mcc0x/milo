//! Finds colour spans in source text using delimiters and word lists.
//!
//! Open comments and multiline strings carry on to the next line. This
//! scanner does not parse a grammar, so it can mistake text for an opener
//! and colour later lines as a string or comment until a closer appears.

mod langs;

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
    /// A backslash does not escape anything inside.
    raw: bool,
}

/// The delimiters, words and conventions the scanner uses for one language.
pub struct Lang {
    /// Markers that turn the rest of a line into a comment.
    line_comments: &'static [&'static str],
    /// Pairs of delimiters for comments that can cross line breaks.
    block_comments: &'static [(&'static str, &'static str)],
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
    /// The separator after a field name in a data file, if any.
    key_end: Option<u8>,
    /// Start line comments only at the start of a line or after whitespace.
    spaced_comments: bool,
    /// Match quotes and named values only where a YAML value can begin.
    scalars: bool,
    /// Ignore likely apostrophes and URLs in JSX prose.
    jsx: bool,
    /// Read PHP's `#[` as an attribute opener rather than a comment.
    attributes: bool,
    /// Colour bracketed TOML headers at the start of a line as keys.
    table_headers: bool,
}

/// What is still open between lines, by its place in a language's table.
/// A state from another language is discarded if its index is invalid.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum State {
    #[default]
    Code,
    Comment(u8),
    Quote(u8),
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

/// Where the closing quote ends, skipping escaped bytes in non-raw strings.
fn string_end(b: &[u8], from: usize, q: &Quote) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if !q.raw && b[i] == b'\\' {
            i += 2;
        } else if begins(&b[i..], q.close) {
            return Some(i + q.close.len());
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
    pub fn scan(&self, text: &str, state: &mut State, out: &mut Vec<Span>) {
        out.clear();
        let b = text.as_bytes();
        let Some(mut i) = self.resume(b, state, out) else { return };
        let indent = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
        if i <= indent {
            i = self.line_start(b, indent, out);
        }
        while i < b.len() {
            if let Some(end) = self.comment(b, i, state, out) {
                i = end;
            } else if let Some(end) = self.quote(b, i, state, out) {
                i = end;
            } else {
                i = self.code(text, i, out);
            }
        }
    }

    /// Finish any open comment or string, or return `None` if it stays open.
    fn resume(&self, b: &[u8], state: &mut State, out: &mut Vec<Span>) -> Option<usize> {
        let open = match *state {
            State::Code => return Some(0),
            State::Comment(k) => {
                self.block_comments.get(usize::from(k)).map(|(_, close)| (find_end(b, 0, close), Class::Comment))
            }
            State::Quote(k) => self.quotes.get(usize::from(k)).map(|q| (string_end(b, 0, q), Class::Str)),
        };
        let Some((close, class)) = open else {
            *state = State::Code;
            return Some(0);
        };
        let end = close.unwrap_or(b.len());
        if end > 0 {
            out.push((0, end, class));
        }
        if close.is_some() {
            *state = State::Code;
        }
        close
    }

    #[inline]
    fn comment(&self, b: &[u8], i: usize, state: &mut State, out: &mut Vec<Span>) -> Option<usize> {
        let rest = &b[i..];
        let spaced = !self.spaced_comments || i == 0 || b[i - 1].is_ascii_whitespace();
        let attribute = self.attributes && begins(rest, "#[");
        // Immediate whitespace still permits ordinary `code // comment`.
        let jsx_text = self.jsx && i > 0 && (is_word(b[i - 1]) || matches!(b[i - 1], b'>' | b':'));
        if spaced && !attribute && !jsx_text && self.line_comments.iter().any(|c| begins(rest, c)) {
            out.push((i, b.len(), Class::Comment));
            return Some(b.len());
        }
        let k = self.block_comments.iter().position(|(open, _)| begins(rest, open))?;
        let (open, close) = self.block_comments[k];
        let end = find_end(b, i + open.len(), close).unwrap_or_else(|| {
            *state = State::Comment(u8::try_from(k).expect("block comment index fits in a byte"));
            b.len()
        });
        out.push((i, end, Class::Comment));
        Some(end)
    }

    #[inline]
    fn quote(&self, b: &[u8], i: usize, state: &mut State, out: &mut Vec<Span>) -> Option<usize> {
        let k = self.quotes.iter().position(|q| self.opens_quote(b, i, q))?;
        let q = &self.quotes[k];
        let close = string_end(b, i + q.open.len(), q);
        let end = close.unwrap_or(b.len());
        let field = close.is_some() && self.key_end == Some(b':') && b[end..].trim_ascii_start().first() == Some(&b':');
        out.push((i, end, if field { Class::Key } else { Class::Str }));
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
    fn code(&self, text: &str, i: usize, out: &mut Vec<Span>) -> usize {
        let b = text.as_bytes();
        let c = b[i];
        if c == b'\\' && b.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
            // Escaped punctuation cannot open a quote or comment.
            // Keep UTF-8 and PHP namespaces such as `\Http` intact.
            i + 2
        } else if c.is_ascii_digit() {
            let end = number_end(b, i);
            out.push((i, end, Class::Number));
            end
        } else if is_word(c) {
            self.scan_word(text, i, out)
        } else {
            i + 1
        }
    }

    fn scan_word(&self, text: &str, i: usize, out: &mut Vec<Span>) -> usize {
        let b = text.as_bytes();
        let end = word_end(b, i);
        let member = previous(b, i).is_some_and(|p| b[p] == b'$' || (b[p] == b'.' && (p == 0 || b[p - 1] != b'.')));
        let scalar = !self.scalars || scalar_start(b, i);
        if let Some(class) = self.classify_word(&text[i..end], &b[end..], member, scalar) {
            out.push((i, end, class));
        }
        end
    }

    /// Handle a directive, data field or TOML header at the line's start.
    fn line_start(&self, b: &[u8], indent: usize, out: &mut Vec<Span>) -> usize {
        if self.directives && b.get(indent) == Some(&b'#') {
            let name = indent + 1 + b[indent + 1..].iter().take_while(|c| c.is_ascii_whitespace()).count();
            let end = word_end(b, name);
            out.push((indent, end, Class::Keyword));
            return end;
        }
        if self.table_headers && b.get(indent) == Some(&b'[') {
            let close = if b.get(indent + 1) == Some(&b'[') { "]]" } else { "]" };
            if let Some(end) = find_end(b, indent + close.len(), close) {
                out.push((indent, end, Class::Key));
                return end;
            }
        }
        self.key_end.map_or(0, |sep| Self::leading_field(b, indent, sep, out))
    }

    fn leading_field(b: &[u8], indent: usize, sep: u8, out: &mut Vec<Span>) -> usize {
        // "- name: value" in a list is still a field.
        let start = indent + b[indent..].iter().take_while(|&&c| c == b'-' || c == b' ').count();
        let name =
            b[start..].iter().take_while(|&&c| is_word(c) || matches!(c, b'.' | b'-' | b' ' | b'/' | b'$')).count();
        let end = start + b[start..start + name].trim_ascii_end().len();
        let after = b[start + name..].first() == Some(&sep);
        let spaced = sep != b':' || b.get(start + name + 1).is_none_or(u8::is_ascii_whitespace);
        if end > start && after && spaced {
            out.push((start, end, Class::Key));
            return start + name;
        }
        0
    }

    fn classify_word(&self, word: &str, after: &[u8], member: bool, scalar: bool) -> Option<Class> {
        if (!member || word == "await") && listed(self.keywords, word) {
            return Some(Class::Keyword);
        }
        if scalar && listed(self.constants, word) {
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
    use super::langs::FILE_LANGS;
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
    fn adversarial_lines_preserve_span_invariants_with_carried_state() {
        let lines = [
            "", " ", "////", "/*", "", "*/", "'''", "\"\"\"", "'", "\"", "`", "\\", "\\é", "é☕中文", "'unterminated",
            "\"unterminated", "/* unterminated", "", "r##\"raw", "\"##", "<<<TEXT", "TEXT;", "#[", "[[", "0..é",
            "0.1.foo", "*/",
        ];
        let mut out = Vec::new();
        for (_, lang) in FILE_LANGS {
            let mut state = State::Code;
            for text in lines {
                lang.scan(text, &mut state, &mut out);
                valid_spans(text, &out);
            }
            for mut state in [State::Comment(u8::MAX), State::Quote(u8::MAX)] {
                lang.scan("é", &mut state, &mut out);
                valid_spans("é", &out);
                assert_eq!(state, State::Code);
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
