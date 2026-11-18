//! The delimiters and words to colour in each supported language.

use super::{Lang, Quote};

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
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for",
        "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
        "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while",
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
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal", "not",
        "or", "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    constants: &["True", "False", "None", "self", "cls"],
    ..PLAIN
};

const JS_QUOTES: &[Quote] = &[Quote { open: "`", close: "`", multiline: true, ..DOUBLE }, DOUBLE, SINGLE];
const JS_WORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
    "else", "export", "extends", "finally", "for", "from", "function", "if", "import", "in", "instanceof", "let",
    "new", "of", "return", "static", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while",
    "with", "yield",
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
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
        "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch", "type",
        "var",
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
        "default", "do", "double", "else", "enum", "extends", "final", "finally", "float", "for", "if", "implements",
        "import", "instanceof", "int", "interface", "long", "native", "new", "package", "permits", "private",
        "protected", "public", "record", "return", "sealed", "short", "static", "super", "switch", "synchronized",
        "this", "throw", "throws", "transient", "try", "var", "void", "volatile", "while", "yield",
    ],
    constants: &["true", "false", "null"],
    ..PLAIN
};

const C_WORDS: &[&str] = &[
    "alignas", "alignof", "auto", "bool", "break", "case", "char", "const", "constexpr", "continue", "default", "do",
    "double", "else", "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register", "restrict",
    "return", "short", "signed", "sizeof", "static", "static_assert", "struct", "switch", "thread_local", "typedef",
    "typeof", "typeof_unqual", "union", "unsigned", "void", "volatile", "while", "_Alignas", "_Alignof", "_Atomic",
    "_BitInt", "_Bool", "_Complex", "_Decimal32", "_Decimal64", "_Decimal128", "_Generic", "_Imaginary", "_Noreturn",
    "_Static_assert", "_Thread_local",
];
const CPP_WORDS: &[&str] = &[
    "alignas", "alignof", "auto", "bool", "break", "case", "catch", "char", "class", "const", "constexpr", "continue",
    "decltype", "default", "delete", "do", "double", "else", "enum", "explicit", "extern", "final", "float", "for",
    "friend", "goto", "if", "inline", "int", "long", "mutable", "namespace", "new", "noexcept", "operator", "override",
    "private", "protected", "public", "register", "restrict", "return", "short", "signed", "sizeof", "static",
    "struct", "switch", "template", "this", "throw", "try", "typedef", "typename", "union", "unsigned", "using",
    "virtual", "void", "volatile", "while",
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
        "abstract", "as", "async", "await", "base", "bool", "break", "byte", "case", "catch", "char", "class", "const",
        "continue", "decimal", "default", "delegate", "do", "double", "else", "enum", "event", "explicit", "extern",
        "finally", "fixed", "float", "for", "foreach", "get", "goto", "if", "implicit", "in", "init", "int",
        "interface", "internal", "is", "lock", "long", "namespace", "new", "object", "operator", "out", "override",
        "params", "partial", "private", "protected", "public", "readonly", "record", "ref", "return", "sbyte",
        "sealed", "set", "short", "sizeof", "static", "string", "struct", "switch", "this", "throw", "try", "typeof",
        "uint", "ulong", "unsafe", "ushort", "using", "var", "virtual", "void", "volatile", "when", "where", "while",
        "yield",
    ],
    constants: &["true", "false", "null"],
    ..PLAIN
};

static PHP: Lang = Lang {
    line_comments: &["//", "#"],
    block_comments: C_COMMENTS,
    attributes: true,
    quotes: &[
        // Use the first semicolon as the end, normally the one after the closing label.
        Quote { open: "<<<", close: ";", multiline: true, raw: true },
        Quote { multiline: true, ..DOUBLE },
        Quote { multiline: true, ..SINGLE },
    ],
    keywords: &[
        "abstract", "and", "array", "as", "break", "callable", "case", "catch", "class", "clone", "const", "continue",
        "declare", "default", "do", "echo", "else", "elseif", "empty", "enum", "extends", "final", "finally", "fn",
        "for", "foreach", "function", "global", "if", "implements", "include", "include_once", "instanceof",
        "interface", "isset", "match", "namespace", "new", "or", "print", "private", "protected", "public", "readonly",
        "require", "require_once", "return", "static", "switch", "throw", "trait", "try", "use", "var", "while",
        "yield",
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
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "return",
        "then", "until", "while",
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

/// The file extensions that select each language.
pub(super) const FILE_LANGS: &[(&[&str], &Lang)] = &[
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

/// Find a language by file extension, ignoring ASCII case.
pub fn for_file(name: &str) -> Option<&'static Lang> {
    let (_, ext) = name.rsplit_once('.')?;
    FILE_LANGS
        .iter()
        .find_map(|(extensions, lang)| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)).then_some(*lang))
}

#[cfg(test)]
mod tests {
    use super::super::{
        Class, State,
        tests::{one, pieces},
    };
    use super::*;

    #[test]
    fn rust_colours_keywords_types_calls_numbers_and_comments() {
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
    fn c_directives_are_keywords() {
        assert_eq!(one(&C, "#include <stdio.h>")[0], ("#include", Class::Keyword));
    }

    #[test]
    fn data_files_colour_their_field_names() {
        assert_eq!(
            one(&JSON, r#"  "name": "strata", "n": 3, "ok": true"#).iter().map(|p| p.1).collect::<Vec<_>>(),
            [Class::Key, Class::Str, Class::Key, Class::Number, Class::Key, Class::Number]
        );
        assert_eq!(
            one(&TOML, "opt-level = 3  # fast"),
            [("opt-level", Class::Key), ("3", Class::Number), ("# fast", Class::Comment),]
        );
        assert_eq!(one(&YAML, "  - name: build"), [("name", Class::Key)]);
        assert_eq!(one(&YAML, "url: http://x"), [("url", Class::Key)]);
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
    fn rust_raw_and_byte_strings_keep_their_delimiters() {
        let text = "r##\"Use \"#\" ... and \" for inches.\"##;";
        assert_eq!(one(&RUST, text), [(&text[..text.len() - 1], Class::Str)]);
        assert_eq!(one(&RUST, "br#\"x\"y\"#"), [("br#\"x\"y\"#", Class::Str)]);
        for (open, close) in [
            ("r###\"", "\"###"),
            ("r##\"", "\"##"),
            ("r#\"", "\"#"),
            ("r\"", "\""),
            ("br###\"", "\"###"),
            ("br##\"", "\"##"),
            ("br#\"", "\"#"),
            ("br\"", "\""),
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
    fn typescript_keywords_are_not_javascript_keywords() {
        let ts = "type number string any declare interface namespace readonly keyof satisfies \
                  unknown never abstract implements private protected public enum is as boolean get set";
        for ext in ["js", "jsx", "mjs", "cjs"] {
            assert_eq!(one(for_file(&format!("file.{ext}")).unwrap(), ts), []);
        }
        for ext in ["ts", "tsx", "mts", "cts"] {
            let spans = one(for_file(&format!("file.{ext}")).unwrap(), ts);
            assert!(spans.iter().all(|p| p.1 == Class::Keyword));
            assert_eq!(spans.len(), ts.split_whitespace().count());
        }
    }

    #[test]
    fn c_and_cpp_have_distinct_keywords() {
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
        assert_eq!(
            one(&YAML, "enabled: [yes, no, on, off]"),
            [
                ("enabled", Class::Key),
                ("yes", Class::Number),
                ("no", Class::Number),
                ("on", Class::Number),
                ("off", Class::Number),
            ]
        );
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
            assert_eq!(
                pieces(&PHP, "TEXT; echo 1;", &mut state),
                [("TEXT;", Class::Str), ("echo", Class::Keyword), ("1", Class::Number),]
            );
            assert_eq!(state, State::Code);
        }
    }

    #[test]
    fn cpp_raw_strings_can_contain_quotes_and_comments() {
        let mut state = State::Code;
        assert_eq!(
            pieces(&CPP, "auto s = R\"(a \"quote\"", &mut state),
            [("auto", Class::Keyword), ("R\"(a \"quote\"", Class::Str),]
        );
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
    fn json_allows_comments() {
        assert_eq!(
            one(&JSON, "/* tsconfig */ {\"ok\": true}"),
            [("/* tsconfig */", Class::Comment), ("\"ok\"", Class::Key), ("true", Class::Number),]
        );
    }

    #[test]
    fn toml_colours_raw_strings_and_table_headers() {
        let mut state = State::Code;
        assert_eq!(pieces(&TOML, "text = '''literal", &mut state), [("text", Class::Key), ("'''literal", Class::Str)]);
        assert_eq!(pieces(&TOML, "\\raw # text'''", &mut state), [("\\raw # text'''", Class::Str)]);
        assert_eq!(state, State::Code);
        assert_eq!(one(&TOML, " [table] # name"), [("[table]", Class::Key), ("# name", Class::Comment)]);
        assert_eq!(one(&TOML, "[[array.table]]"), [("[[array.table]]", Class::Key)]);
    }

    #[test]
    fn csharp_colours_raw_strings_and_contextual_keywords() {
        let mut state = State::Code;
        assert_eq!(
            pieces(&CSHARP, "var s = \"\"\"a \" quote", &mut state),
            [("var", Class::Keyword), ("\"\"\"a \" quote", Class::Str),]
        );
        assert_eq!(pieces(&CSHARP, "/* raw */\"\"\";", &mut state), [("/* raw */\"\"\"", Class::Str)]);
        assert_eq!(state, State::Code);
        let words = "partial where when goto unsafe init ushort sbyte";
        assert_eq!(one(&CSHARP, words).len(), words.split_whitespace().count());
        assert!(one(&CSHARP, words).iter().all(|p| p.1 == Class::Keyword));
    }

    #[test]
    fn java_colours_sealed_and_permits_as_keywords() {
        assert_eq!(one(&JAVA, "sealed permits"), [("sealed", Class::Keyword), ("permits", Class::Keyword)]);
    }

    #[test]
    fn php_colours_attributes_and_include_keywords() {
        assert_eq!(one(&PHP, "#[Route('/')]"), [("Route", Class::Function), ("'/'", Class::Str)]);
        assert_eq!(
            one(&PHP, "require_once include_once"),
            [("require_once", Class::Keyword), ("include_once", Class::Keyword),]
        );
    }

    #[test]
    fn declarations_and_control_flow_keep_their_language_colours() {
        assert_eq!(
            one(&GO, "func main() { fmt.Println(`hi`); return }"),
            [
                ("func", Class::Keyword),
                ("main", Class::Function),
                ("Println", Class::Function),
                ("`hi`", Class::Str),
                ("return", Class::Keyword),
            ]
        );
        assert_eq!(
            one(&JAVA, "public static void main(String[] args) { return; }"),
            [
                ("public", Class::Keyword),
                ("static", Class::Keyword),
                ("void", Class::Keyword),
                ("main", Class::Function),
                ("String", Class::Type),
                ("return", Class::Keyword),
            ]
        );
        assert_eq!(
            one(&CSHARP, "public string Name { get; init; }"),
            [
                ("public", Class::Keyword),
                ("string", Class::Keyword),
                ("Name", Class::Type),
                ("get", Class::Keyword),
                ("init", Class::Keyword),
            ]
        );
        assert_eq!(
            one(&PHP, "foreach ($items as $item) { echo \"hi\"; }"),
            [("foreach", Class::Keyword), ("as", Class::Keyword), ("echo", Class::Keyword), ("\"hi\"", Class::Str),]
        );
        assert_eq!(
            one(&SHELL, "for file in *.rs; do echo \"$file\"; done # done"),
            [
                ("for", Class::Keyword),
                ("in", Class::Keyword),
                ("do", Class::Keyword),
                ("\"$file\"", Class::Str),
                ("done", Class::Keyword),
                ("# done", Class::Comment),
            ]
        );
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
        for (_, lang) in FILE_LANGS {
            ordered(&lang.quotes.iter().map(|q| q.open).collect::<Vec<_>>());
            ordered(lang.line_comments);
            ordered(&lang.block_comments.iter().map(|c| c.0).collect::<Vec<_>>());
            assert!(lang.quotes.len() <= usize::from(u8::MAX));
            assert!(lang.block_comments.len() <= usize::from(u8::MAX));
        }
    }
}
