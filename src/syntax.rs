//! Colouring source text with small language tables, not a grammar.
//!
//! Comments and multiline strings carry state between lines. Ambiguous
//! syntax can be misclassified, including an apparent opener that keeps
//! later lines in a string or comment until its closing delimiter appears.

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

struct Quote {
    open: &'static str,
    close: &'static str,
    /// May run on over several lines.
    multiline: bool,
    /// A backslash does not escape anything inside.
    raw: bool,
}

pub struct Lang {
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
    /// Tried in order, so a longer opener must come before its prefix.
    quotes: &'static [Quote],
    keywords: &'static [&'static str],
    /// Words that stand for a value: `true`, `nil`, `None`.
    constants: &'static [&'static str],
    /// A lone apostrophe is not a character literal.
    chars: bool,
    directives: bool,
    /// Distinguish names that resemble types, calls and constants.
    names: bool,
    key_end: Option<u8>,
    /// Shell and YAML require whitespace before a comment marker.
    spaced_comments: bool,
    /// YAML quotes and constants only begin at a scalar boundary.
    scalars: bool,
    /// Avoid treating apostrophes and URLs in JSX prose as code.
    jsx: bool,
    /// PHP's `#[` starts an attribute, not a comment.
    attributes: bool,
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

/// Recognise a character, including Rust's `'\u{1F600}'`, not a lifetime.
fn is_char(b: &[u8], i: usize) -> bool {
    match b.get(i + 1) {
        Some(b'\\') => b[i + 2..].iter().take(12).any(|&c| c == b'\''),
        Some(_) => b[i + 2..].iter().take(4).position(|&c| c == b'\'').is_some_and(|p| {
            p == 0 || b[i + 1] >= 0x80
        }),
        None => false,
    }
}

fn previous(b: &[u8], i: usize) -> Option<usize> {
    b[..i].iter().rposition(|c| !c.is_ascii_whitespace())
}

fn scalar_start(b: &[u8], i: usize) -> bool {
    previous(b, i).is_none_or(|p| {
        matches!(b[p], b':' | b'[' | b'{' | b',') || (b[p] == b'-' && p + 1 < i)
    })
}

impl Lang {
    /// Replace `out` with ordered, non-empty byte spans for this line.
    /// The caller can reuse its allocation; `state` carries open delimiters.
    pub fn scan(&self, text: &str, state: &mut State, out: &mut Vec<Span>) {
        out.clear();
        let b = text.as_bytes();
        let mut i = 0;
        match *state {
            State::Code => {}
            State::Comment(k) => {
                if let Some((_, close)) = self.block_comments.get(k as usize) {
                    match find_end(b, 0, close) {
                        Some(end) => {
                            out.push((0, end, Class::Comment));
                            i = end;
                        }
                        None => {
                            if !b.is_empty() {
                                out.push((0, b.len(), Class::Comment));
                            }
                            return;
                        }
                    }
                }
                *state = State::Code;
            }
            State::Quote(k) => {
                if let Some(q) = self.quotes.get(k as usize) {
                    let (end, closed) = string_end(b, 0, q);
                    if end > 0 {
                        out.push((0, end, Class::Str));
                    }
                    if !closed {
                        return;
                    }
                    i = end;
                }
                *state = State::Code;
            }
        }

        let indent = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
        if i <= indent {
            i = self.line_start(b, indent, out);
        }
        while i < b.len() {
            let rest = &b[i..];
            let spaced = !self.spaced_comments || i == 0 || b[i - 1].is_ascii_whitespace();
            let attribute = self.attributes && rest.starts_with(b"#[");
            // Immediate whitespace still permits ordinary `code // comment`.
            let jsx_text = self.jsx && i > 0 && (is_word(b[i - 1]) || matches!(b[i - 1], b'>' | b':'));
            if spaced && !attribute && !jsx_text
                && self.line_comments.iter().any(|c| rest.starts_with(c.as_bytes()))
            {
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
            let opens = |q: &Quote| {
                rest.starts_with(q.open.as_bytes())
                    && (!self.scalars || scalar_start(b, i))
                    && (q.open != "'" || ((!self.jsx || previous(b, i).is_none_or(|p| !is_word(b[p]) && b[p] != b'>'))
                        && (!self.chars || is_char(b, i))))
            };
            if let Some(k) = self.quotes.iter().position(opens) {
                let q = &self.quotes[k];
                let (end, closed) = string_end(b, i + q.open.len(), q);
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
            if c == b'\\' && b.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
                // Escaped punctuation in code cannot open a quote or comment.
                // Do not skip a UTF-8 byte or a PHP namespace such as `\Http`.
                i += 2;
            } else if c.is_ascii_digit() {
                let mut end = i + rest.iter().take_while(|&&c| is_word(c)).count();
                // A fraction, but not a range or a tuple field's method.
                if b.get(end) == Some(&b'.') && b.get(end + 1).is_some_and(u8::is_ascii_digit) {
                    end += 1 + b[end + 1..].iter().take_while(|&&c| is_word(c)).count();
                }
                out.push((i, end, Class::Number));
                i = end;
            } else if is_word(c) {
                let end = i + rest.iter().take_while(|&&c| is_word(c)).count();
                let member = previous(b, i).is_some_and(|p| {
                    b[p] == b'$' || (b[p] == b'.' && (p == 0 || b[p - 1] != b'.'))
                });
                let scalar = !self.scalars || scalar_start(b, i);
                if let Some(class) = self.word(&text[i..end], &b[end..], member, scalar) {
                    out.push((i, end, class));
                }
                i = end;
            } else {
                i += 1;
            }
        }
    }

    /// Handle a directive, data field or TOML header at the line's start.
    fn line_start(&self, b: &[u8], indent: usize, out: &mut Vec<Span>) -> usize {
        if self.directives && b.get(indent) == Some(&b'#') {
            let name = indent + 1 + b[indent + 1..].iter().take_while(|c| c.is_ascii_whitespace()).count();
            let end = name + b[name..].iter().take_while(|&&c| is_word(c)).count();
            out.push((indent, end, Class::Keyword));
            return end;
        }
        if self.table_headers && b.get(indent) == Some(&b'[') {
            let double = b.get(indent + 1) == Some(&b'[');
            if let Some(end) = find_end(b, indent + if double { 2 } else { 1 }, if double { "]]" } else { "]" }) {
                out.push((indent, end, Class::Key));
                return end;
            }
        }
        let Some(sep) = self.key_end else { return 0 };
        // "- name: value" in a list is still a field.
        let start = indent + b[indent..].iter().take_while(|&&c| c == b'-' || c == b' ').count();
        let name = b[start..].iter().take_while(|&&c| {
            is_word(c) || matches!(c, b'.' | b'-' | b' ' | b'/' | b'$')
        }).count();
        let end = start + b[start..start + name].trim_ascii_end().len();
        let after = b[start + name..].first() == Some(&sep);
        let spaced = sep != b':' || b.get(start + name + 1).is_none_or(u8::is_ascii_whitespace);
        if end > start && after && spaced {
            out.push((start, end, Class::Key));
            return start + name;
        }
        0
    }

    fn word(&self, word: &str, after: &[u8], member: bool, scalar: bool) -> Option<Class> {
        if (!member || word == "await") && self.keywords.contains(&word) {
            return Some(Class::Keyword);
        }
        if scalar && self.constants.contains(&word) {
            return Some(Class::Number);
        }
        if !self.names {
            return None;
        }
        let call = matches!(after, [b'(', ..] | [b'!', b'(' | b'[' | b'{', ..]);
        let mut letters = word.bytes().filter(u8::is_ascii_alphabetic);
        let upper = word.as_bytes()[0].is_ascii_uppercase();
        match (call, upper) {
            (true, _) => Some(Class::Function),
            (false, true) if letters.any(|c| c.is_ascii_lowercase()) => Some(Class::Type),
            (false, true) if word.len() > 1 => Some(Class::Number),
            _ => None,
        }
    }
}

const PLAIN: Lang = Lang {
    line_comments: &[],
    block_comments: &[],
    quotes: &[],
    keywords: &[],
    constants: &[],
    chars: false,
    directives: false,
    names: true,
    key_end: None,
    spaced_comments: false,
    scalars: false,
    jsx: false,
    attributes: false,
    table_headers: false,
};

const DOUBLE: Quote = Quote { open: "\"", close: "\"", multiline: false, raw: false };
const SINGLE: Quote = Quote { open: "'", close: "'", ..DOUBLE };
const C_COMMENTS: &[(&str, &str)] = &[("/*", "*/")];
const C_QUOTES: &[Quote] = &[DOUBLE, SINGLE];

static RUST: Lang = Lang {
    chars: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: &[
        Quote { open: "r###\"", close: "\"###", multiline: true, raw: true },
        Quote { open: "r##\"", close: "\"##", multiline: true, raw: true },
        Quote { open: "r#\"", close: "\"#", multiline: true, raw: true },
        Quote { open: "r\"", multiline: true, raw: true, ..DOUBLE },
        Quote { open: "br###\"", close: "\"###", multiline: true, raw: true },
        Quote { open: "br##\"", close: "\"##", multiline: true, raw: true },
        Quote { open: "br#\"", close: "\"#", multiline: true, raw: true },
        Quote { open: "br\"", multiline: true, raw: true, ..DOUBLE },
        Quote { open: "b\"", multiline: true, ..DOUBLE },
        Quote { multiline: true, ..DOUBLE },
        SINGLE,
    ],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
        "Self", "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while",
    ],
    constants: &["true", "false"],
    ..PLAIN
};

static PYTHON: Lang = Lang {
    line_comments: &["#"],
    quotes: &[
        Quote { open: "\"\"\"", close: "\"\"\"", multiline: true, ..DOUBLE },
        Quote { open: "'''", close: "'''", multiline: true, ..SINGLE },
        DOUBLE,
        SINGLE,
    ],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal",
        "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    constants: &["True", "False", "None", "self", "cls"],
    ..PLAIN
};

const JS_QUOTES: &[Quote] = &[Quote { open: "`", close: "`", multiline: true, ..DOUBLE }, DOUBLE, SINGLE];
const JS_WORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
    "do", "else", "export", "extends", "finally", "for", "from", "function", "if", "import", "in", "instanceof",
    "let", "new", "of", "return", "static", "super", "switch", "this", "throw", "try", "typeof", "var", "void",
    "while", "with", "yield",
];
const TS_WORDS: &[&str] = &[
    "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch", "class", "const", "continue",
    "debugger", "declare", "default", "delete", "do", "else", "enum", "export", "extends", "finally", "for", "from",
    "function", "get", "if", "implements", "import", "in", "instanceof", "interface", "is", "keyof", "let",
    "namespace", "never", "new", "number", "of", "private", "protected", "public", "readonly", "return", "satisfies",
    "set", "static", "string", "super", "switch", "this", "throw", "try", "type", "typeof", "unknown", "var", "void",
    "while", "with", "yield",
];
static JAVASCRIPT: Lang = Lang {
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: JS_QUOTES,
    keywords: JS_WORDS,
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    ..PLAIN
};
static TYPESCRIPT: Lang = Lang { keywords: TS_WORDS, ..JAVASCRIPT };
static JSX: Lang = Lang { jsx: true, ..JAVASCRIPT };
static TSX: Lang = Lang { jsx: true, ..TYPESCRIPT };

static GO: Lang = Lang {
    chars: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: &[Quote { open: "`", close: "`", multiline: true, raw: true }, DOUBLE, SINGLE],
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func",
        "go", "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct",
        "switch", "type", "var",
    ],
    constants: &["true", "false", "nil", "iota"],
    ..PLAIN
};

static JAVA: Lang = Lang {
    chars: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: &[Quote { open: "\"\"\"", close: "\"\"\"", multiline: true, ..DOUBLE }, DOUBLE, SINGLE],
    keywords: &[
        "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const", "continue",
        "default", "do", "double", "else", "enum", "extends", "final", "finally", "float", "for", "if",
        "implements", "import", "instanceof", "int", "interface", "long", "native", "new", "package", "permits", "private",
        "protected", "public", "record", "return", "sealed", "short", "static", "super", "switch", "synchronized", "this",
        "throw", "throws", "transient", "try", "var", "void", "volatile", "while", "yield",
    ],
    constants: &["true", "false", "null"],
    ..PLAIN
};

const C_WORDS: &[&str] = &[
    "alignas", "alignof", "auto", "bool", "break", "case", "char", "const", "constexpr", "continue", "default",
    "do", "double", "else", "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "static_assert", "struct", "switch", "thread_local",
    "typedef", "typeof", "typeof_unqual", "union", "unsigned", "void", "volatile", "while",
    "_Alignas", "_Alignof", "_Atomic", "_BitInt", "_Bool", "_Complex", "_Decimal32", "_Decimal64",
    "_Decimal128", "_Generic", "_Imaginary", "_Noreturn", "_Static_assert", "_Thread_local",
];
const CPP_WORDS: &[&str] = &[
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
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: C_QUOTES,
    keywords: C_WORDS,
    constants: &["true", "false", "NULL", "nullptr"],
    ..PLAIN
};
static CPP: Lang = Lang {
    keywords: CPP_WORDS,
    quotes: &[Quote { open: "R\"(", close: ")\"", multiline: true, raw: true }, DOUBLE, SINGLE],
    ..C
};

static CSHARP: Lang = Lang {
    chars: true,
    directives: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: &[
        Quote { open: "\"\"\"", close: "\"\"\"", multiline: true, raw: true },
        Quote { open: "@\"", multiline: true, raw: true, ..DOUBLE },
        DOUBLE,
        SINGLE,
    ],
    keywords: &[
        "abstract", "as", "async", "await", "base", "bool", "break", "byte", "case", "catch", "char", "class",
        "const", "continue", "decimal", "default", "delegate", "do", "double", "else", "enum", "event",
        "explicit", "extern", "finally", "fixed", "float", "for", "foreach", "get", "goto", "if", "implicit", "in", "init", "int",
        "interface", "internal", "is", "lock", "long", "namespace", "new", "object", "operator", "out",
        "override", "params", "partial", "private", "protected", "public", "readonly", "record", "ref", "return", "sbyte", "sealed",
        "set", "short", "sizeof", "static", "string", "struct", "switch", "this", "throw", "try", "typeof",
        "uint", "ulong", "unsafe", "ushort", "using", "var", "virtual", "void", "volatile", "when", "where", "while", "yield",
    ],
    constants: &["true", "false", "null"],
    ..PLAIN
};

static PHP: Lang = Lang {
    line_comments: &["//", "#"],
    block_comments: C_COMMENTS,
    attributes: true,
    quotes: &[
        // Heredoc labels are not parsed: protect its body through the first
        // semicolon, normally the one following the closing label.
        Quote { open: "<<<", close: ";", multiline: true, raw: true },
        Quote { multiline: true, ..DOUBLE },
        Quote { multiline: true, ..SINGLE },
    ],
    keywords: &[
        "abstract", "and", "array", "as", "break", "callable", "case", "catch", "class", "clone", "const", "continue",
        "declare", "default", "do", "echo", "else", "elseif", "empty", "enum", "extends", "final", "finally", "fn",
        "for", "foreach", "function", "global", "if", "implements", "include", "include_once", "instanceof", "interface", "isset",
        "match", "namespace", "new", "or", "print", "private", "protected", "public", "readonly", "require", "require_once",
        "return", "static", "switch", "throw", "trait", "try", "use", "var", "while", "yield",
    ],
    constants: &["true", "false", "null", "TRUE", "FALSE", "NULL"],
    ..PLAIN
};

static SHELL: Lang = Lang {
    names: false,
    spaced_comments: true,
    line_comments: &["#"],
    quotes: &[Quote { multiline: true, ..DOUBLE }, Quote { multiline: true, raw: true, ..SINGLE }],
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local",
        "return", "then", "until", "while",
    ],
    constants: &["true", "false"],
    ..PLAIN
};

static JSON: Lang = Lang {
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    quotes: &[DOUBLE],
    constants: &["true", "false", "null"],
    names: false,
    key_end: Some(b':'),
    ..PLAIN
};
static YAML: Lang = Lang {
    line_comments: &["#"],
    quotes: C_QUOTES,
    constants: &["true", "false", "null", "yes", "no", "on", "off"],
    names: false,
    key_end: Some(b':'),
    spaced_comments: true,
    scalars: true,
    ..PLAIN
};
static TOML: Lang = Lang {
    line_comments: &["#"],
    quotes: &[
        Quote { open: "\"\"\"", close: "\"\"\"", multiline: true, ..DOUBLE },
        Quote { open: "'''", close: "'''", multiline: true, raw: true },
        DOUBLE,
        Quote { raw: true, ..SINGLE },
    ],
    constants: &["true", "false"],
    names: false,
    key_end: Some(b'='),
    table_headers: true,
    ..PLAIN
};

/// The language a file is written in, going by its name.
/// Return `None` for a language this scanner does not colour.
pub fn for_file(name: &str) -> Option<&'static Lang> {
    let (_, ext) = name.rsplit_once('.')?;
    const FILE_LANGS: &[(&[&str], &Lang)] = &[
        (&["rs"], &RUST),
        (&["py", "pyi"], &PYTHON),
        (&["js", "mjs", "cjs"], &JAVASCRIPT),
        (&["ts", "mts", "cts"], &TYPESCRIPT),
        (&["jsx"], &JSX),
        (&["tsx"], &TSX),
        (&["go"], &GO),
        (&["java"], &JAVA),
        (&["c", "h"], &C),
        (&["cc", "cpp", "cxx", "hpp", "hh"], &CPP),
        (&["cs"], &CSHARP),
        (&["php"], &PHP),
        (&["sh", "bash", "zsh"], &SHELL),
        (&["json", "jsonc"], &JSON),
        (&["yaml", "yml"], &YAML),
        (&["toml"], &TOML),
    ];
    FILE_LANGS.iter().find_map(|(extensions, lang)| {
        extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)).then_some(*lang)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGUAGES: &[&Lang] = &[
        &RUST, &PYTHON, &JAVASCRIPT, &TYPESCRIPT, &JSX, &TSX, &GO, &JAVA,
        &C, &CPP, &CSHARP, &PHP, &SHELL, &JSON, &YAML, &TOML,
    ];

    /// The classed pieces of one line, as text.
    fn pieces<'a>(lang: &Lang, text: &'a str, state: &mut State) -> Vec<(&'a str, Class)> {
        let mut out = Vec::new();
        lang.scan(text, state, &mut out);
        out.into_iter().map(|(a, b, class)| (&text[a..b], class)).collect()
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
        let mut out = Vec::new();
        RUST.scan(line, &mut State::Code, &mut out);
        for (a, b, _) in out {
            assert!(line.is_char_boundary(a) && line.is_char_boundary(b));
        }
    }

    #[test]
    fn languages_are_found_by_file_name() {
        assert!(for_file("main.rs").is_some() && for_file("App.TSX").is_some() && for_file("Cargo.toml").is_some());
        assert!(for_file("README.md").is_none() && for_file("Makefile").is_none());
    }

    #[test]
    fn shell_and_yaml_hashes_need_whitespace() {
        let line = "if [ $# -lt 2 ]; then echo \"usage\" >&2; exit 1; fi";
        let spans = one(&SHELL, line);
        assert!(spans.contains(&("if", Class::Keyword)));
        assert!(spans.contains(&("fi", Class::Keyword)));
        assert!(!spans.iter().any(|p| p.1 == Class::Comment));
        for line in ["${#array[@]}", "${path##*/}", "$((16#ff))", "foo#bar"] {
            assert!(!one(&SHELL, line).iter().any(|p| p.1 == Class::Comment), "{line}");
        }
        assert_eq!(one(&YAML, "url: http://example.com/#anchor"), [("url", Class::Key)]);
        for lang in [&SHELL, &YAML] {
            assert_eq!(one(lang, "# comment"), [("# comment", Class::Comment)]);
            assert_eq!(one(lang, "foo\t# comment").last(), Some(&("# comment", Class::Comment)));
        }
    }

    #[test]
    fn numbers_leave_ranges_and_members_alone() {
        for line in ["for i in 0..n", "self.0.len()", "(0..self.h)"] {
            let spans = one(&RUST, line);
            assert!(spans.contains(&("0", Class::Number)), "{line}: {spans:?}");
            assert!(!spans.iter().any(|p| p.1 == Class::Number && p.0.contains('.')));
        }
        assert_eq!(one(&RUST, "1.25f32 0xff 3.0.1"), [
            ("1.25f32", Class::Number), ("0xff", Class::Number),
            ("3.0", Class::Number), ("1", Class::Number),
        ]);
    }

    #[test]
    fn escaped_code_punctuation_cannot_open_comments_or_strings() {
        let mut state = State::Code;
        for line in [r#"url.replace(/\/*$/, "")"#, r"/^https?:\/\//.test(url)"] {
            assert!(!pieces(&JAVASCRIPT, line, &mut state).iter().any(|p| p.1 == Class::Comment));
            assert_eq!(state, State::Code);
        }
        assert_eq!(one(&SHELL, r"'don'\''t stop'; echo next"), [
            ("'don'", Class::Str), ("'t stop'", Class::Str),
        ]);
        assert!(one(&PHP, r"App\Http").contains(&("Http", Class::Type)));
        assert_eq!(one(&RUST, "\\é foo()"), [("foo", Class::Function)]);
    }

    #[test]
    fn rust_raw_and_byte_strings_keep_their_delimiters() {
        let text = "r##\"Use \"#\" ... and \" for inches.\"##;";
        assert_eq!(one(&RUST, text), [(&text[..text.len() - 1], Class::Str)]);
        assert_eq!(one(&RUST, "br#\"x\"y\"#"), [("br#\"x\"y\"#", Class::Str)]);
        for (open, close) in [
            ("r###\"", "\"###"), ("r##\"", "\"##"), ("r#\"", "\"#"), ("r\"", "\""),
            ("br###\"", "\"###"), ("br##\"", "\"##"), ("br#\"", "\"#"), ("br\"", "\""),
        ] {
            let mut state = State::Code;
            assert_eq!(pieces(&RUST, open, &mut state), [(open, Class::Str)]);
            assert_ne!(state, State::Code);
            assert_eq!(pieces(&RUST, "/* raw \\ text", &mut state), [("/* raw \\ text", Class::Str)]);
            assert_eq!(pieces(&RUST, close, &mut state), [(close, Class::Str)]);
            assert_eq!(state, State::Code);
            assert_eq!(pieces(&RUST, "let next = 1;", &mut state)[0], ("let", Class::Keyword));
        }
    }

    #[test]
    fn member_and_variable_names_are_not_keywords() {
        for lang in [&JAVASCRIPT, &TYPESCRIPT] {
            for line in ["cache.get(key)", "cache.set(k, v)", "action.type", "Array.from(x)", "z.string()"] {
                assert!(!one(lang, line).iter().any(|p| p.1 == Class::Keyword), "{line}");
            }
        }
        assert_eq!(one(&PYTHON, "re.match("), [("match", Class::Function)]);
        assert_eq!(one(&PHP, "$array $class $default $function"), []);
        assert_eq!(one(&RUST, "future.await"), [("await", Class::Keyword)]);
        assert!(one(&RUST, "0..self.h").contains(&("self", Class::Keyword)));
        assert_eq!(one(&TYPESCRIPT, "action. type"), []);
    }

    #[test]
    fn javascript_and_c_have_their_own_keywords() {
        let ts = "type number string any declare interface namespace readonly keyof satisfies unknown never abstract implements private protected public enum is as boolean get set";
        for ext in ["js", "jsx", "mjs", "cjs"] {
            assert_eq!(one(for_file(&format!("file.{ext}")).unwrap(), ts), []);
        }
        for ext in ["ts", "tsx", "mts", "cts"] {
            let spans = one(for_file(&format!("file.{ext}")).unwrap(), ts);
            assert!(spans.iter().all(|p| p.1 == Class::Keyword));
            assert_eq!(spans.len(), ts.split_whitespace().count());
        }
        let cpp = "new private class this template namespace delete friend virtual";
        for ext in ["c", "h"] {
            assert_eq!(one(for_file(&format!("file.{ext}")).unwrap(), cpp), []);
        }
        assert_eq!(one(&CPP, cpp).len(), cpp.split_whitespace().count());
        assert_eq!(one(&C, "_Atomic int n = 0;")[..2], [("_Atomic", Class::Keyword), ("int", Class::Keyword)]);
    }

    #[test]
    fn yaml_plain_scalars_are_not_code() {
        assert_eq!(one(&YAML, "- name: Don't cache on push"), [("name", Class::Key)]);
        assert_eq!(one(&YAML, "run: npm ci --no-cache"), [("run", Class::Key)]);
        assert_eq!(one(&YAML, "$ref: value"), [("$ref", Class::Key)]);
        assert_eq!(one(&YAML, "foo/bar: value"), [("foo/bar", Class::Key)]);
        assert_eq!(one(&YAML, "text: words \"not a string\" on"), [("text", Class::Key)]);
        assert_eq!(one(&YAML, "enabled: [yes, no, on, off]"), [
            ("enabled", Class::Key), ("yes", Class::Number), ("no", Class::Number),
            ("on", Class::Number), ("off", Class::Number),
        ]);
        assert_eq!(one(&YAML, "- 'quoted'"), [("'quoted'", Class::Str)]);
        assert_eq!(one(&YAML, "name: \"quoted\""), [("name", Class::Key), ("\"quoted\"", Class::Str)]);
        for lang in [&JSON, &TOML] {
            assert_eq!(one(lang, "yes no on off"), []);
        }
    }

    #[test]
    fn php_multiline_strings_and_heredocs_protect_comment_markers() {
        for quote in ["\"", "'"] {
            let mut state = State::Code;
            pieces(&PHP, quote, &mut state);
            assert_eq!(pieces(&PHP, "/* not a comment", &mut state), [("/* not a comment", Class::Str)]);
            pieces(&PHP, quote, &mut state);
            assert_eq!(state, State::Code);
        }
        for opener in ["<<<TEXT", "<<<'TEXT'"] {
            let mut state = State::Code;
            assert_eq!(pieces(&PHP, opener, &mut state), [(opener, Class::Str)]);
            assert_eq!(pieces(&PHP, "/* plain text", &mut state), [("/* plain text", Class::Str)]);
            assert_eq!(pieces(&PHP, "TEXT; echo 1;", &mut state), [
                ("TEXT;", Class::Str), ("echo", Class::Keyword), ("1", Class::Number),
            ]);
            assert_eq!(state, State::Code);
        }
    }

    #[test]
    fn cpp_raw_strings_can_contain_quotes_and_comments() {
        let mut state = State::Code;
        assert_eq!(pieces(&CPP, "auto s = R\"(a \"quote\"", &mut state), [
            ("auto", Class::Keyword), ("R\"(a \"quote\"", Class::Str),
        ]);
        assert_eq!(pieces(&CPP, "/* raw */", &mut state), [("/* raw */", Class::Str)]);
        assert_eq!(pieces(&CPP, ")\"; return;", &mut state), [(")\"", Class::Str), ("return", Class::Keyword)]);
        assert_eq!(state, State::Code);
    }

    #[test]
    fn jsx_text_keeps_apostrophes_and_urls() {
        for lang in [&JSX, &TSX] {
            for line in ["<p>Don't do that, it's bad</p>", "<a>See http://example.com</a>", "<p>'Hello'</p>"] {
                assert!(!one(lang, line).iter().any(|p| matches!(p.1, Class::Str | Class::Comment)), "{line}");
            }
            assert_eq!(one(lang, "const x = 'value'; // comment").last(), Some(&("// comment", Class::Comment)));
            assert!(one(lang, "const x = 'value';").contains(&("'value'", Class::Str)));
            assert_eq!(one(lang, "return x // comment").last(), Some(&("// comment", Class::Comment)));
            assert!(one(lang, "<p title='hello'>").contains(&("'hello'", Class::Str)));
        }
    }

    #[test]
    fn data_tables_cover_json_comments_and_toml_literals_and_headers() {
        assert_eq!(one(&JSON, "/* tsconfig */ {\"ok\": true}"), [
            ("/* tsconfig */", Class::Comment), ("\"ok\"", Class::Key), ("true", Class::Number),
        ]);
        let mut state = State::Code;
        assert_eq!(pieces(&TOML, "text = '''literal", &mut state), [("text", Class::Key), ("'''literal", Class::Str)]);
        assert_eq!(pieces(&TOML, "\\raw # text'''", &mut state), [("\\raw # text'''", Class::Str)]);
        assert_eq!(state, State::Code);
        assert_eq!(one(&TOML, " [table] # name"), [("[table]", Class::Key), ("# name", Class::Comment)]);
        assert_eq!(one(&TOML, "[[array.table]]"), [("[[array.table]]", Class::Key)]);
    }

    #[test]
    fn csharp_java_php_and_rust_table_gaps_are_covered() {
        let mut state = State::Code;
        assert_eq!(pieces(&CSHARP, "var s = \"\"\"a \" quote", &mut state), [
            ("var", Class::Keyword), ("\"\"\"a \" quote", Class::Str),
        ]);
        assert_eq!(pieces(&CSHARP, "/* raw */\"\"\";", &mut state), [("/* raw */\"\"\"", Class::Str)]);
        assert_eq!(state, State::Code);
        let words = "partial where when goto unsafe init ushort sbyte";
        assert_eq!(one(&CSHARP, words).len(), words.split_whitespace().count());
        assert!(one(&CSHARP, words).iter().all(|p| p.1 == Class::Keyword));
        assert_eq!(one(&JAVA, "sealed permits"), [("sealed", Class::Keyword), ("permits", Class::Keyword)]);
        assert_eq!(one(&PHP, "#[Route('/')]"), [("Route", Class::Function), ("'/'", Class::Str)]);
        assert_eq!(one(&PHP, "require_once include_once"), [
            ("require_once", Class::Keyword), ("include_once", Class::Keyword),
        ]);
        assert_eq!(one(&RUST, "'\\u{1F600}' '\\u{10FFFF}'"), [
            ("'\\u{1F600}'", Class::Str), ("'\\u{10FFFF}'", Class::Str),
        ]);
    }

    #[test]
    fn idiomatic_go_java_csharp_php_and_shell_lines() {
        assert_eq!(one(&GO, "func main() { fmt.Println(`hi`); return }"), [
            ("func", Class::Keyword), ("main", Class::Function), ("Println", Class::Function),
            ("`hi`", Class::Str), ("return", Class::Keyword),
        ]);
        assert_eq!(one(&JAVA, "public static void main(String[] args) { return; }"), [
            ("public", Class::Keyword), ("static", Class::Keyword), ("void", Class::Keyword),
            ("main", Class::Function), ("String", Class::Type), ("return", Class::Keyword),
        ]);
        assert_eq!(one(&CSHARP, "public string Name { get; init; }"), [
            ("public", Class::Keyword), ("string", Class::Keyword), ("Name", Class::Type),
            ("get", Class::Keyword), ("init", Class::Keyword),
        ]);
        assert_eq!(one(&PHP, "foreach ($items as $item) { echo \"hi\"; }"), [
            ("foreach", Class::Keyword), ("as", Class::Keyword), ("echo", Class::Keyword), ("\"hi\"", Class::Str),
        ]);
        assert_eq!(one(&SHELL, "for file in *.rs; do echo \"$file\"; done # done"), [
            ("for", Class::Keyword), ("in", Class::Keyword), ("do", Class::Keyword),
            ("\"$file\"", Class::Str), ("done", Class::Keyword), ("# done", Class::Comment),
        ]);
    }

    #[test]
    fn longer_openers_precede_their_prefixes_in_every_table() {
        fn ordered(openers: &[&str]) {
            for (i, open) in openers.iter().enumerate() {
                assert!(!open.is_empty());
                for later in &openers[i + 1..] {
                    assert!(!later.starts_with(open), "{later:?} must precede {open:?}");
                }
            }
        }
        for lang in LANGUAGES {
            ordered(&lang.quotes.iter().map(|q| q.open).collect::<Vec<_>>());
            ordered(lang.line_comments);
            ordered(&lang.block_comments.iter().map(|c| c.0).collect::<Vec<_>>());
            assert!(lang.quotes.len() <= u8::MAX as usize);
            assert!(lang.block_comments.len() <= u8::MAX as usize);
        }
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
            "", " ", "////", "/*", "", "*/", "'''", "\"\"\"", "'", "\"", "`",
            "\\", "\\é", "é☕中文", "'unterminated", "\"unterminated", "/* unterminated",
            "", "r##\"raw", "\"##", "<<<TEXT", "TEXT;", "#[", "[[", "0..é", "0.1.foo", "*/",
        ];
        let mut out = Vec::new();
        for lang in LANGUAGES {
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
    fn scan_clears_and_reuses_the_callers_buffer() {
        let mut out = Vec::with_capacity(16);
        let allocation = out.as_ptr();
        let mut state = State::Code;
        RUST.scan("let x = 1;", &mut state, &mut out);
        assert_eq!(out.len(), 2);
        RUST.scan("", &mut state, &mut out);
        assert!(out.is_empty());
        assert_eq!(out.as_ptr(), allocation);
        assert_eq!(std::mem::size_of::<State>(), 2);
    }

    #[test]
    fn this_source_file_finishes_in_code_state() {
        let mut state = State::Code;
        let mut out = Vec::new();
        for text in include_str!("syntax.rs").lines() {
            RUST.scan(text, &mut state, &mut out);
            valid_spans(text, &out);
        }
        assert_eq!(state, State::default());
    }
}
