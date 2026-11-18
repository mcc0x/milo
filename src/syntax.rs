//! Colouring source text by language: comments, strings, numbers, keywords,
//! and names that look like types or calls.
//!
//! One small scanner serves every language, each described by a table: how
//! its comments and strings are written and which words it reserves. It
//! reads a line at a time and remembers, between lines, whether a comment or
//! a string is still open. It knows nothing of grammar, so it can be wrong
//! where a language is subtle (a regular expression holding a quote, say),
//! but never by more than the rest of that line.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Comment,
    Str,
    /// Numbers, and named constants such as `true` or `MAX_SIZE`.
    Number,
    Keyword,
    Type,
    Function,
    /// The name of a field in a data file.
    Key,
}

/// Bytes `start..end` of a line are of one class.
pub type Span = (usize, usize, Class);

struct Quote {
    open: &'static str,
    close: &'static str,
    /// May run on over several lines.
    multiline: bool,
    /// A backslash does not escape anything inside.
    raw: bool,
}

const fn quote(open: &'static str, close: &'static str, multiline: bool, raw: bool) -> Quote {
    Quote { open, close, multiline, raw }
}

pub struct Lang {
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
    /// Tried in order, so a longer opener must come before its prefix.
    quotes: &'static [Quote],
    keywords: &'static [&'static str],
    /// Words that stand for a value: `true`, `nil`, `None`.
    constants: &'static [&'static str],
    /// `'` writes a single character, as in `'a'`; a lone one is not a string.
    chars: bool,
    /// A line starting with `#` names a preprocessor directive.
    directives: bool,
    /// The names of things are worth telling apart: `Type`, `call()`, `CONST`.
    names: bool,
    /// In a data file, what ends a field's name at the start of a line.
    key_end: Option<u8>,
}

/// What is still open at the end of a line: nothing, a block comment or a
/// string, by its place in the language's table.
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

/// Where `needle` first ends in `b` at or after `from`.
fn find_end(b: &[u8], from: usize, needle: &str) -> Option<usize> {
    let n = needle.as_bytes();
    (from..=b.len().checked_sub(n.len())?).find(|&i| b[i..].starts_with(n)).map(|i| i + n.len())
}

/// Where a string that is open at `from` ends, and whether it closed.
fn string_end(b: &[u8], from: usize, q: &Quote) -> (usize, bool) {
    let close = q.close.as_bytes();
    let mut i = from;
    while i < b.len() {
        if !q.raw && b[i] == b'\\' {
            i += 2;
        } else if b[i..].starts_with(close) {
            return (i + close.len(), true);
        } else {
            i += 1;
        }
    }
    (b.len(), false)
}

/// Whether the `'` at `i` opens a single character such as `'a'` or `'\n'`,
/// and not, say, a Rust lifetime.
fn is_char(b: &[u8], i: usize) -> bool {
    match b.get(i + 1) {
        Some(b'\\') => b[i + 2..].iter().take(8).any(|&c| c == b'\''),
        // The character may be several bytes long.
        Some(_) => b[i + 2..].iter().take(4).position(|&c| c == b'\'').is_some_and(|p| {
            p == 0 || b[i + 1] >= 0x80
        }),
        None => false,
    }
}

impl Lang {
    /// The classes found in one line, in order. `state` carries what was
    /// left open by the lines before, and is left ready for the next.
    pub fn line(&self, text: &str, state: &mut State) -> Vec<Span> {
        let b = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        match *state {
            State::Code => {}
            State::Comment(k) => match find_end(b, 0, self.block_comments[k as usize].1) {
                Some(end) => {
                    out.push((0, end, Class::Comment));
                    (i, *state) = (end, State::Code);
                }
                None => {
                    out.push((0, b.len(), Class::Comment));
                    return out;
                }
            },
            State::Quote(k) => {
                let (end, closed) = string_end(b, 0, &self.quotes[k as usize]);
                out.push((0, end, Class::Str));
                if !closed {
                    return out;
                }
                (i, *state) = (end, State::Code);
            }
        }

        let indent = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
        if i <= indent {
            i = self.line_start(b, indent, &mut out);
        }
        while i < b.len() {
            let rest = &b[i..];
            if self.line_comments.iter().any(|c| rest.starts_with(c.as_bytes())) {
                out.push((i, b.len(), Class::Comment));
                break;
            }
            if let Some(k) = self.block_comments.iter().position(|(open, _)| rest.starts_with(open.as_bytes())) {
                let (open, close) = self.block_comments[k];
                match find_end(b, i + open.len(), close) {
                    Some(end) => {
                        out.push((i, end, Class::Comment));
                        i = end;
                        continue;
                    }
                    None => {
                        out.push((i, b.len(), Class::Comment));
                        *state = State::Comment(k as u8);
                        break;
                    }
                }
            }
            let opens = |q: &Quote| rest.starts_with(q.open.as_bytes()) && (q.open != "'" || !self.chars || is_char(b, i));
            if let Some(k) = self.quotes.iter().position(opens) {
                let q = &self.quotes[k];
                let (end, closed) = string_end(b, i + q.open.len(), q);
                // In a data file a string before a colon names a field.
                let names_field = closed
                    && self.key_end == Some(b':')
                    && b[end..].iter().find(|c| !c.is_ascii_whitespace()) == Some(&b':');
                out.push((i, end, if names_field { Class::Key } else { Class::Str }));
                if !closed && q.multiline {
                    *state = State::Quote(k as u8);
                }
                i = end;
                continue;
            }
            let c = b[i];
            if c.is_ascii_digit() {
                let end = i + rest.iter().take_while(|&&c| is_word(c) || c == b'.').count();
                out.push((i, end, Class::Number));
                i = end;
            } else if is_word(c) {
                let end = i + rest.iter().take_while(|&&c| is_word(c)).count();
                if let Some(class) = self.word(&text[i..end], &b[end..]) {
                    out.push((i, end, class));
                }
                i = end;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Handles what only the start of a line can hold: a preprocessor
    /// directive, or a field's name in a data file. Returns where the rest
    /// of the line begins.
    fn line_start(&self, b: &[u8], indent: usize, out: &mut Vec<Span>) -> usize {
        if self.directives && b.get(indent) == Some(&b'#') {
            let name = indent + 1 + b[indent + 1..].iter().take_while(|c| c.is_ascii_whitespace()).count();
            let end = name + b[name..].iter().take_while(|&&c| is_word(c)).count();
            out.push((indent, end, Class::Keyword));
            return end;
        }
        let Some(sep) = self.key_end else { return 0 };
        // "- name: value" in a list is still a field.
        let start = indent + b[indent..].iter().take_while(|&&c| c == b'-' || c == b' ').count();
        let name = b[start..].iter().take_while(|&&c| is_word(c) || matches!(c, b'.' | b'-' | b' ')).count();
        let end = start + b[start..start + name].trim_ascii_end().len();
        let after = b[start + name..].first() == Some(&sep);
        // A colon only ends a name when something other than more name follows.
        let spaced = sep != b':' || matches!(b.get(start + name + 1), None | Some(b' '));
        if end > start && after && spaced {
            out.push((start, end, Class::Key));
            return start + name;
        }
        0
    }

    /// What a word is, given what comes straight after it.
    fn word(&self, word: &str, after: &[u8]) -> Option<Class> {
        if self.keywords.contains(&word) {
            return Some(Class::Keyword);
        }
        if self.constants.contains(&word) {
            return Some(Class::Number);
        }
        if !self.names {
            return None;
        }
        // `name(` is a call, and so is a Rust macro, `name!(`.
        let call = matches!(after, [b'(', ..] | [b'!', b'(' | b'[' | b'{', ..]);
        let mut letters = word.bytes().filter(u8::is_ascii_alphabetic);
        let upper = word.as_bytes()[0].is_ascii_uppercase();
        match (call, upper) {
            (true, _) => Some(Class::Function),
            // `Name` reads as a type, `NAME` as a constant.
            (false, true) if letters.any(|c| c.is_ascii_lowercase()) => Some(Class::Type),
            (false, true) if word.len() > 1 => Some(Class::Number),
            _ => None,
        }
    }
}

const C_COMMENTS: &[(&str, &str)] = &[("/*", "*/")];
const C_QUOTES: &[Quote] = &[quote("\"", "\"", false, false), quote("'", "'", false, false)];
const NONE: &[&str] = &[];

const fn code(
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
    quotes: &'static [Quote],
    keywords: &'static [&'static str],
    constants: &'static [&'static str],
) -> Lang {
    Lang { line_comments, block_comments, quotes, keywords, constants, chars: false, directives: false, names: true, key_end: None }
}

const fn data(line_comments: &'static [&'static str], quotes: &'static [Quote], key_end: u8) -> Lang {
    Lang {
        line_comments,
        block_comments: &[],
        quotes,
        keywords: NONE,
        constants: &["true", "false", "null", "yes", "no", "on", "off"],
        chars: false,
        directives: false,
        names: false,
        key_end: Some(key_end),
    }
}

static RUST: Lang = Lang {
    chars: true,
    ..code(
        &["//"],
        C_COMMENTS,
        &[
            quote("r#\"", "\"#", true, true),
            quote("r\"", "\"", true, true),
            quote("b\"", "\"", true, false),
            quote("\"", "\"", true, false),
            quote("'", "'", false, false),
        ],
        &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn",
            "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
            "Self", "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while",
        ],
        &["true", "false"],
    )
};

static PYTHON: Lang = code(
    &["#"],
    &[],
    &[
        quote("\"\"\"", "\"\"\"", true, false),
        quote("'''", "'''", true, false),
        quote("\"", "\"", false, false),
        quote("'", "'", false, false),
    ],
    &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal",
        "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    &["True", "False", "None", "self", "cls"],
);

const JS_QUOTES: &[Quote] =
    &[quote("`", "`", true, false), quote("\"", "\"", false, false), quote("'", "'", false, false)];
const JS_WORDS: &[&str] = &[
    "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch", "class", "const", "continue",
    "debugger", "declare", "default", "delete", "do", "else", "enum", "export", "extends", "finally", "for", "from",
    "function", "get", "if", "implements", "import", "in", "instanceof", "interface", "is", "keyof", "let",
    "namespace", "never", "new", "number", "of", "private", "protected", "public", "readonly", "return", "satisfies",
    "set", "static", "string", "super", "switch", "this", "throw", "try", "type", "typeof", "unknown", "var", "void",
    "while", "with", "yield",
];
static JAVASCRIPT: Lang =
    code(&["//"], C_COMMENTS, JS_QUOTES, JS_WORDS, &["true", "false", "null", "undefined", "NaN", "Infinity"]);

static GO: Lang = Lang {
    chars: true,
    ..code(
        &["//"],
        C_COMMENTS,
        &[quote("`", "`", true, true), quote("\"", "\"", false, false), quote("'", "'", false, false)],
        &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func",
            "go", "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct",
            "switch", "type", "var",
        ],
        &["true", "false", "nil", "iota"],
    )
};

static JAVA: Lang = Lang {
    chars: true,
    ..code(
        &["//"],
        C_COMMENTS,
        &[quote("\"\"\"", "\"\"\"", true, false), quote("\"", "\"", false, false), quote("'", "'", false, false)],
        &[
            "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const", "continue",
            "default", "do", "double", "else", "enum", "extends", "final", "finally", "float", "for", "if",
            "implements", "import", "instanceof", "int", "interface", "long", "native", "new", "package", "private",
            "protected", "public", "record", "return", "short", "static", "super", "switch", "synchronized", "this",
            "throw", "throws", "transient", "try", "var", "void", "volatile", "while", "yield",
        ],
        &["true", "false", "null"],
    )
};

const C_WORDS: &[&str] = &[
    "alignas", "alignof", "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr",
    "continue", "decltype", "default", "delete", "do", "double", "else", "enum", "explicit", "extern", "final",
    "float", "for", "friend", "goto", "if", "inline", "int", "long", "mutable", "namespace", "new", "noexcept",
    "operator", "override", "private", "protected", "public", "register", "restrict", "return", "short", "signed",
    "sizeof", "static", "struct", "switch", "template", "this", "throw", "try", "typedef", "typename", "union",
    "unsigned", "using", "virtual", "void", "volatile", "while",
];
static C: Lang = Lang {
    chars: true,
    directives: true,
    ..code(&["//"], C_COMMENTS, C_QUOTES, C_WORDS, &["true", "false", "NULL", "nullptr"])
};

static CSHARP: Lang = Lang {
    chars: true,
    directives: true,
    ..code(
        &["//"],
        C_COMMENTS,
        &[quote("@\"", "\"", true, true), quote("\"", "\"", false, false), quote("'", "'", false, false)],
        &[
            "abstract", "as", "async", "await", "base", "bool", "break", "byte", "case", "catch", "char", "class",
            "const", "continue", "decimal", "default", "delegate", "do", "double", "else", "enum", "event",
            "explicit", "extern", "finally", "fixed", "float", "for", "foreach", "get", "if", "implicit", "in", "int",
            "interface", "internal", "is", "lock", "long", "namespace", "new", "object", "operator", "out",
            "override", "params", "private", "protected", "public", "readonly", "record", "ref", "return", "sealed",
            "set", "short", "sizeof", "static", "string", "struct", "switch", "this", "throw", "try", "typeof",
            "uint", "ulong", "using", "var", "virtual", "void", "volatile", "while", "yield",
        ],
        &["true", "false", "null"],
    )
};

static PHP: Lang = code(
    &["//", "#"],
    C_COMMENTS,
    C_QUOTES,
    &[
        "abstract", "and", "array", "as", "break", "callable", "case", "catch", "class", "clone", "const", "continue",
        "declare", "default", "do", "echo", "else", "elseif", "empty", "enum", "extends", "final", "finally", "fn",
        "for", "foreach", "function", "global", "if", "implements", "include", "instanceof", "interface", "isset",
        "match", "namespace", "new", "or", "print", "private", "protected", "public", "readonly", "require",
        "return", "static", "switch", "throw", "trait", "try", "use", "var", "while", "yield",
    ],
    &["true", "false", "null", "TRUE", "FALSE", "NULL"],
);

static SHELL: Lang = Lang {
    names: false,
    ..code(
        &["#"],
        &[],
        C_QUOTES,
        &[
            "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local",
            "return", "then", "until", "while",
        ],
        &["true", "false"],
    )
};

static JSON: Lang = data(&["//"], &[quote("\"", "\"", false, false)], b':');
static YAML: Lang = data(&["#"], C_QUOTES, b':');
static TOML: Lang =
    data(&["#"], &[quote("\"\"\"", "\"\"\"", true, false), quote("\"", "\"", false, false), quote("'", "'", false, true)], b'=');

/// The language a file is written in, going by its name. `None` for one
/// this does not colour.
pub fn for_file(name: &str) -> Option<&'static Lang> {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => &RUST,
        "py" | "pyi" => &PYTHON,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => &JAVASCRIPT,
        "go" => &GO,
        "java" => &JAVA,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" => &C,
        "cs" => &CSHARP,
        "php" => &PHP,
        "sh" | "bash" | "zsh" => &SHELL,
        "json" | "jsonc" => &JSON,
        "yaml" | "yml" => &YAML,
        "toml" => &TOML,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classed pieces of one line, as text.
    fn pieces<'a>(lang: &Lang, text: &'a str, state: &mut State) -> Vec<(&'a str, Class)> {
        lang.line(text, state).into_iter().map(|(a, b, class)| (&text[a..b], class)).collect()
    }

    fn one<'a>(lang: &Lang, text: &'a str) -> Vec<(&'a str, Class)> {
        pieces(lang, text, &mut State::Code)
    }

    #[test]
    fn rust_tells_words_apart() {
        assert_eq!(
            one(&RUST, "let n: Option<u8> = parse(MAX, 42); // done"),
            [
                ("let", Class::Keyword),
                ("Option", Class::Type),
                ("parse", Class::Function),
                ("MAX", Class::Number),
                ("42", Class::Number),
                ("// done", Class::Comment),
            ]
        );
    }

    #[test]
    fn a_lifetime_is_not_a_string_but_a_char_is() {
        assert_eq!(one(&RUST, "fn f<'a>(x: &'a str) {}"), [("fn", Class::Keyword)]);
        assert_eq!(one(&RUST, "let c = 'x'; let n = '\\n';").iter().filter(|p| p.1 == Class::Str).count(), 2);
    }

    #[test]
    fn a_macro_is_a_call() {
        assert_eq!(one(&RUST, "println!(\"hi\")"), [("println", Class::Function), ("\"hi\"", Class::Str)]);
    }

    #[test]
    fn a_block_comment_runs_over_lines() {
        let mut state = State::Code;
        assert_eq!(pieces(&C, "int a; /* one", &mut state), [("int", Class::Keyword), ("/* one", Class::Comment)]);
        assert_eq!(pieces(&C, "two", &mut state), [("two", Class::Comment)]);
        assert_eq!(pieces(&C, "three */ return 0;", &mut state)[..2], [("three */", Class::Comment), ("return", Class::Keyword)]);
        assert_eq!(state, State::Code);
    }

    #[test]
    fn a_python_docstring_runs_over_lines_but_a_plain_string_does_not() {
        let mut state = State::Code;
        assert_eq!(pieces(&PYTHON, "def f(): \"\"\"start", &mut state).last(), Some(&("\"\"\"start", Class::Str)));
        assert_eq!(pieces(&PYTHON, "end\"\"\" + x", &mut state), [("end\"\"\"", Class::Str)]);
        assert_eq!(pieces(&PYTHON, "s = 'unclosed", &mut state), [("'unclosed", Class::Str)]);
        assert_eq!(pieces(&PYTHON, "return None", &mut state), [("return", Class::Keyword), ("None", Class::Number)]);
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        assert_eq!(one(&JAVASCRIPT, r#"const s = "a \" b"; // c"#)[1..], [(r#""a \" b""#, Class::Str), ("// c", Class::Comment)]);
    }

    #[test]
    fn a_comment_marker_inside_a_string_is_text() {
        assert_eq!(one(&JAVASCRIPT, "x = \"http://a\""), [("\"http://a\"", Class::Str)]);
    }

    #[test]
    fn c_directives_are_keywords() {
        assert_eq!(one(&C, "#include <stdio.h>")[0], ("#include", Class::Keyword));
    }

    #[test]
    fn data_files_colour_their_field_names() {
        assert_eq!(one(&JSON, r#"  "name": "strata", "n": 3, "ok": true"#).iter().map(|p| p.1).collect::<Vec<_>>(), [
            Class::Key,
            Class::Str,
            Class::Key,
            Class::Number,
            Class::Key,
            Class::Number
        ]);
        assert_eq!(one(&TOML, "opt-level = 3  # fast"), [("opt-level", Class::Key), ("3", Class::Number), ("# fast", Class::Comment)]);
        assert_eq!(one(&YAML, "  - name: build"), [("name", Class::Key)]);
        assert_eq!(one(&YAML, "url: http://x"), [("url", Class::Key)]);
    }

    #[test]
    fn text_that_is_not_ascii_is_left_whole() {
        let line = "let café = \"naïve ☕\"; // ünïcode";
        for (a, b, _) in RUST.line(line, &mut State::Code) {
            assert!(line.is_char_boundary(a) && line.is_char_boundary(b));
        }
    }

    #[test]
    fn languages_are_found_by_file_name() {
        assert!(for_file("main.rs").is_some() && for_file("App.TSX").is_some() && for_file("Cargo.toml").is_some());
        assert!(for_file("README.md").is_none() && for_file("Makefile").is_none());
    }
}
