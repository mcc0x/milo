//! Defines the code-colouring rules and selects them by file name or extension.

use super::{Dialect, Escape, Lang, Quote};

const PLAIN: Lang = Lang {
    line_comments: &[],
    block_comments: &[],
    dialect: Dialect::Plain,
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
    caseless: false,
    leading_keywords: false,
};

const DOUBLE: Quote = Quote { open: "\"", close: "\"", multiline: false, escape: Escape::Backslash, extra_quotes: 0 };
const SINGLE: Quote = Quote { open: "'", close: "'", ..DOUBLE };
const C_COMMENTS: &[(&str, &str)] = &[("/*", "*/")];
const C_QUOTES: &[Quote] = &[DOUBLE, SINGLE];
/// Shell quoting kept to one line, for files where a stray apostrophe is ordinary text.
const LINE_QUOTES: &[Quote] = &[DOUBLE, Quote { escape: Escape::None, ..SINGLE }];

static RUST: Lang = Lang {
    chars: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    dialect: Dialect::Rust,
    quotes: &[
        Quote { open: "b\"", multiline: true, ..DOUBLE },
        Quote { open: "c\"", multiline: true, ..DOUBLE },
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
    dialect: Dialect::Javascript,
    quotes: C_QUOTES,
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
    quotes: &[Quote { open: "`", close: "`", multiline: true, escape: Escape::None, ..DOUBLE }, DOUBLE, SINGLE],
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
static CPP: Lang = Lang { keywords: CPP_WORDS, dialect: Dialect::Cpp, ..C };

static CSHARP: Lang = Lang {
    chars: true,
    directives: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    dialect: Dialect::Csharp,
    quotes: &[Quote { open: "@\"", multiline: true, escape: Escape::Doubled, ..DOUBLE }, DOUBLE, SINGLE],
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
    dialect: Dialect::Php,
    quotes: &[Quote { multiline: true, ..DOUBLE }, Quote { multiline: true, ..SINGLE }],
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
    quotes: &[Quote { multiline: true, ..DOUBLE }, Quote { multiline: true, escape: Escape::None, ..SINGLE }],
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "return",
        "then", "until", "while",
    ],
    constants: &["true", "false"],
    ..PLAIN
};

static LUA: Lang = Lang {
    line_comments: &["--"],
    dialect: Dialect::Lua,
    quotes: C_QUOTES,
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local", "not", "or",
        "repeat", "return", "then", "until", "while",
    ],
    constants: &["true", "false", "nil"],
    ..PLAIN
};

static SWIFT: Lang = Lang {
    directives: true,
    line_comments: &["//"],
    block_comments: C_COMMENTS,
    dialect: Dialect::Swift,
    quotes: &[Quote { open: "\"\"\"", close: "\"\"\"", multiline: true, ..DOUBLE }, DOUBLE],
    keywords: &[
        "actor", "any", "as", "associatedtype", "async", "await", "break", "case", "catch", "class", "continue",
        "convenience", "default", "defer", "deinit", "didSet", "do", "dynamic", "else", "enum", "extension",
        "fallthrough", "fileprivate", "final", "for", "func", "get", "guard", "if", "import", "in", "indirect", "init",
        "inout", "internal", "is", "isolated", "lazy", "let", "macro", "mutating", "nonisolated", "nonmutating",
        "open", "operator", "optional", "override", "precedencegroup", "private", "protocol", "public", "repeat",
        "required", "rethrows", "return", "self", "Self", "set", "some", "static", "struct", "subscript", "super",
        "switch", "throw", "throws", "try", "typealias", "unowned", "var", "weak", "where", "while", "willSet",
    ],
    constants: &["true", "false", "nil"],
    ..PLAIN
};

/// The words most dialects share. Strings stay on one line because dialects disagree about backslashes.
static SQL: Lang = Lang {
    names: false,
    caseless: true,
    line_comments: &["--"],
    block_comments: C_COMMENTS,
    quotes: &[Quote { escape: Escape::Doubled, ..SINGLE }, Quote { escape: Escape::Doubled, ..DOUBLE }],
    keywords: &[
        "add", "all", "alter", "and", "any", "as", "asc", "begin", "between", "by", "cascade", "case", "cast", "check",
        "column", "commit", "conflict", "constraint", "create", "cross", "database", "declare", "default", "delete",
        "desc", "distinct", "do", "drop", "else", "end", "except", "exists", "explain", "for", "foreign", "from",
        "full", "function", "grant", "group", "having", "if", "in", "index", "inner", "insert", "intersect", "into",
        "is", "join", "key", "left", "like", "limit", "natural", "not", "nothing", "offset", "on", "or", "order",
        "outer", "over", "partition", "primary", "procedure", "recursive", "references", "rename", "replace", "return",
        "returning", "returns", "revoke", "right", "rollback", "schema", "select", "set", "table", "temp", "temporary",
        "then", "to", "transaction", "trigger", "truncate", "union", "unique", "update", "use", "using", "values",
        "view", "when", "where", "window", "with",
    ],
    constants: &["true", "false", "null"],
    ..PLAIN
};

/// Instructions are conventionally upper case, which keeps them apart from the shell words in `RUN`.
static DOCKERFILE: Lang = Lang {
    names: false,
    spaced_comments: true,
    line_comments: &["#"],
    quotes: LINE_QUOTES,
    keywords: &[
        "ADD", "ARG", "AS", "CMD", "COPY", "ENTRYPOINT", "ENV", "EXPOSE", "FROM", "HEALTHCHECK", "LABEL", "MAINTAINER",
        "ONBUILD", "RUN", "SHELL", "STOPSIGNAL", "USER", "VOLUME", "WORKDIR",
    ],
    ..PLAIN
};

/// A recipe's tab is expanded before the scanner sees it, so a shell word such as
/// `else` or `export` that starts a recipe line is coloured like the directive.
static MAKEFILE: Lang = Lang {
    names: false,
    spaced_comments: true,
    leading_keywords: true,
    line_comments: &["#"],
    quotes: LINE_QUOTES,
    keywords: &[
        "define", "else", "endef", "endif", "export", "ifdef", "ifeq", "ifndef", "ifneq", "include", "override",
        "private", "sinclude", "undefine", "unexport", "vpath",
    ],
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
    quotes: &[Quote { multiline: true, ..DOUBLE }, Quote { multiline: true, escape: Escape::Doubled, ..SINGLE }],
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
        Quote { open: "\"\"\"", close: "\"\"\"", multiline: true, extra_quotes: 2, ..DOUBLE },
        Quote { open: "'''", close: "'''", multiline: true, escape: Escape::None, extra_quotes: 2 },
        DOUBLE,
        Quote { escape: Escape::None, ..SINGLE },
    ],
    constants: &["true", "false"],
    names: false,
    key_end: Some(b'='),
    table_headers: true,
    ..PLAIN
};

/// Extensions are listed together when they use the same colouring rules.
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
    (&["lua"], &LUA),
    (&["swift"], &SWIFT),
    (&["sql"], &SQL),
    (&["dockerfile"], &DOCKERFILE),
    (&["mk"], &MAKEFILE),
];

/// Files known by their whole name.
const NAMED_LANGS: &[(&[&str], &Lang)] = &[(&["Dockerfile"], &DOCKERFILE), (&["Makefile", "GNUmakefile"], &MAKEFILE)];

fn listed_lang(langs: &[(&[&str], &'static Lang)], word: &str) -> Option<&'static Lang> {
    langs.iter().find_map(|(words, lang)| words.iter().any(|w| word.eq_ignore_ascii_case(w)).then_some(*lang))
}

/// Find a language by the name of a path's last component or by its extension, ignoring ASCII case.
pub fn for_file(path: &str) -> Option<&'static Lang> {
    let name = path.rsplit('/').next()?;
    let by_extension = || listed_lang(FILE_LANGS, name.rsplit_once('.')?.1);
    listed_lang(NAMED_LANGS, name).or_else(by_extension)
}

impl Lang {
    /// Only colour a script when its explicit interpreter agrees with the extension.
    pub fn agrees_with_shebang(&self, first: &str) -> bool {
        let Some(command) = first.trim_start_matches('\u{feff}').strip_prefix("#!") else { return true };
        let mut words = command.split_ascii_whitespace();
        let Some(program) = words.next().and_then(|path| path.strip_prefix('/')) else { return true };
        let program = program.rsplit('/').next().unwrap_or("");
        let program = if program == "env" { words.find(|word| !word.starts_with('-')).unwrap_or("") } else { program };
        let languages: &[_] = match program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') {
            "ash" | "bash" | "dash" | "ksh" | "sh" | "zsh" => &[&SHELL],
            "python" | "pypy" | "uv" => &[&PYTHON],
            "bun" | "deno" | "node" | "nodejs" | "ts-node" | "tsx" | "zx" => &[&JAVASCRIPT, &TYPESCRIPT, &JSX, &TSX],
            "php" => &[&PHP],
            "cargo" | "rust-script" => &[&RUST],
            "lua" | "luajit" => &[&LUA],
            "swift" => &[&SWIFT],
            "make" | "gmake" => &[&MAKEFILE],
            _ => return false,
        };
        languages.iter().any(|&lang| std::ptr::eq(self, lang))
    }
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
            one(&JSON, r#"  "name": "milo", "n": 3, "ok": true"#).iter().map(|p| p.1).collect::<Vec<_>>(),
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
        for (name, lang) in [
            ("init.lua", &LUA),
            ("App.swift", &SWIFT),
            ("db/001_users.SQL", &SQL),
            ("Dockerfile", &DOCKERFILE),
            ("dockerfile", &DOCKERFILE),
            ("ops.d/Dockerfile", &DOCKERFILE),
            ("api.dockerfile", &DOCKERFILE),
            ("Makefile", &MAKEFILE),
            ("makefile", &MAKEFILE),
            ("GNUmakefile", &MAKEFILE),
            ("build/rules.mk", &MAKEFILE),
            ("Makefile.toml", &TOML),
        ] {
            assert!(for_file(name).is_some_and(|found| std::ptr::eq(found, lang)), "{name}");
        }
        // A name matches whole, and a dot in a folder's name does not start an extension.
        let unknown = ["README.md", "LICENSE", "Dockerfiles", "Dockerfile.md", "Makefile.win", "my.Makefile"];
        for name in unknown.into_iter().chain(["v1.rs/README", "Makefile/notes"]) {
            assert!(for_file(name).is_none(), "{name}");
        }
    }

    #[test]
    fn lua_long_brackets_close_only_at_their_own_level() {
        let mut state = State::Code;
        assert_eq!(
            pieces(&LUA, "local s = [==[one ]] two", &mut state),
            [("local", Class::Keyword), ("[==[one ]] two", Class::Str)]
        );
        assert_eq!(pieces(&LUA, "-- ]=] still text", &mut state), [("-- ]=] still text", Class::Str)]);
        assert_eq!(
            pieces(&LUA, "]]==] .. name -- done", &mut state),
            [("]]==]", Class::Str), ("-- done", Class::Comment)]
        );
        assert_eq!(state, State::Code);

        assert_eq!(pieces(&LUA, "x = 1 --[[ note", &mut state), [("1", Class::Number), ("--[[ note", Class::Comment)]);
        let hidden = "local hidden = \"]=]\"";
        assert_eq!(pieces(&LUA, hidden, &mut state), [(hidden, Class::Comment)]);
        assert_eq!(
            pieces(&LUA, "]] return t[a[1]], [[x]]", &mut state),
            [("]]", Class::Comment), ("return", Class::Keyword), ("1", Class::Number), ("[[x]]", Class::Str)]
        );
        assert_eq!(state, State::Code);
    }

    #[test]
    fn lua_colours_one_line_long_comments_and_ordinary_code() {
        assert_eq!(
            one(&LUA, "--[==[ a ]] b ]==] local n = nil"),
            [("--[==[ a ]] b ]==]", Class::Comment), ("local", Class::Keyword), ("nil", Class::Number)]
        );
        assert_eq!(one(&LUA, "--[= not long ]]"), [("--[= not long ]]", Class::Comment)]);
        assert_eq!(
            one(&LUA, "if a ~= b then print(\"hi\") end"),
            [
                ("if", Class::Keyword),
                ("then", Class::Keyword),
                ("print", Class::Function),
                ("\"hi\"", Class::Str),
                ("end", Class::Keyword),
            ]
        );
        // More `=` than a level can count is not a long bracket, and the line stays code.
        let mut state = State::Code;
        let wide = format!("[{}[", "=".repeat(256));
        assert_eq!(pieces(&LUA, &wide, &mut state), []);
        assert_eq!(state, State::Code);
    }

    #[test]
    fn swift_raw_strings_need_their_number_signs_to_close() {
        assert_eq!(
            one(&SWIFT, r###"let s = #"a "quoted" \(x)"# + ##"one "# two"##"###),
            [
                ("let", Class::Keyword),
                (r##"#"a "quoted" \(x)"#"##, Class::Str),
                (r###"##"one "# two"##"###, Class::Str),
            ]
        );
        assert_eq!(one(&SWIFT, "  #\"raw\"# + #\"\"#"), [("#\"raw\"#", Class::Str), ("#\"\"#", Class::Str)]);

        let mut state = State::Code;
        assert_eq!(
            pieces(&SWIFT, "let text = #\"\"\"", &mut state),
            [("let", Class::Keyword), ("#\"\"\"", Class::Str)]
        );
        let body = "  \"\"\" // still \"# text";
        assert_eq!(pieces(&SWIFT, body, &mut state), [(body, Class::Str)]);
        assert_eq!(
            pieces(&SWIFT, "  \"\"\"#; return nil", &mut state),
            [("  \"\"\"#", Class::Str), ("return", Class::Keyword), ("nil", Class::Number)]
        );
        assert_eq!(state, State::Code);

        // An unclosed one-line raw string ends with its line, and one that starts with two quotes is one line.
        pieces(&SWIFT, "let open = #\"unclosed", &mut state);
        assert_eq!(state, State::Code);
        assert_eq!(
            pieces(&SWIFT, "let q = #\"\"\"\"# + 1", &mut state)[1..],
            [("#\"\"\"\"#", Class::Str), ("1", Class::Number)]
        );
        assert_eq!(state, State::Code);
    }

    #[test]
    fn a_long_run_of_swift_number_signs_is_not_read_again_from_each_one() {
        // Each `#` used to count every `#` after it before finding no quote.
        let text = format!("x{}", "#".repeat(1 << 18));
        let mut state = State::Code;
        assert!(pieces(&SWIFT, &text, &mut state).is_empty());
        assert_eq!(state, State::Code);
    }

    #[test]
    fn swift_colours_multiline_strings_nested_comments_and_directives() {
        let mut state = State::Code;
        assert_eq!(pieces(&SWIFT, "let s = \"\"\"", &mut state), [("let", Class::Keyword), ("\"\"\"", Class::Str)]);
        let body = "/* text */ \"quoted\"";
        assert_eq!(pieces(&SWIFT, body, &mut state), [(body, Class::Str)]);
        assert_eq!(pieces(&SWIFT, "\"\"\"", &mut state), [("\"\"\"", Class::Str)]);
        assert_eq!(state, State::Code);

        let outer = "/* outer /* inner */ still";
        assert_eq!(pieces(&SWIFT, outer, &mut state), [(outer, Class::Comment)]);
        assert_eq!(
            pieces(&SWIFT, "*/ guard let x else { return }", &mut state),
            [
                ("*/", Class::Comment),
                ("guard", Class::Keyword),
                ("let", Class::Keyword),
                ("else", Class::Keyword),
                ("return", Class::Keyword),
            ]
        );
        assert_eq!(one(&SWIFT, "#if os(iOS)"), [("#if", Class::Keyword), ("os", Class::Function)]);
        assert_eq!(
            one(&SWIFT, "func greet(_ name: String) -> Int? { nil } // done"),
            [
                ("func", Class::Keyword),
                ("greet", Class::Function),
                ("String", Class::Type),
                ("Int", Class::Type),
                ("nil", Class::Number),
                ("// done", Class::Comment),
            ]
        );
    }

    #[test]
    fn sql_keywords_ignore_case_and_strings_double_their_quotes() {
        assert_eq!(
            one(&SQL, "SELECT name FROM users WHERE note = 'it''s' AND ok IS NOT NULL; -- done"),
            [
                ("SELECT", Class::Keyword),
                ("FROM", Class::Keyword),
                ("WHERE", Class::Keyword),
                ("'it''s'", Class::Str),
                ("AND", Class::Keyword),
                ("IS", Class::Keyword),
                ("NOT", Class::Keyword),
                ("NULL", Class::Number),
                ("-- done", Class::Comment),
            ]
        );
        assert_eq!(
            one(&SQL, "create table \"order\" (id integer primary key, n int default 42) /* v2 */"),
            [
                ("create", Class::Keyword),
                ("table", Class::Keyword),
                ("\"order\"", Class::Str),
                ("primary", Class::Keyword),
                ("key", Class::Keyword),
                ("default", Class::Keyword),
                ("42", Class::Number),
                ("/* v2 */", Class::Comment),
            ]
        );
        // A column named like a keyword is a member, and an open string ends with its line.
        assert_eq!(one(&SQL, "select o.key from o"), [("select", Class::Keyword), ("from", Class::Keyword)]);
        let mut state = State::Code;
        pieces(&SQL, "insert into t values ('open", &mut state);
        assert_eq!(state, State::Code);
    }

    #[test]
    fn dockerfile_colours_instructions_but_not_shell_words() {
        assert_eq!(
            one(&DOCKERFILE, "FROM alpine:3.20 AS build # stage"),
            [("FROM", Class::Keyword), ("3.20", Class::Number), ("AS", Class::Keyword), ("# stage", Class::Comment)]
        );
        assert_eq!(
            one(&DOCKERFILE, "RUN add-apt-repository ppa && echo \"it's $USER\" 'a\\' # note"),
            [
                ("RUN", Class::Keyword),
                ("\"it's $USER\"", Class::Str),
                ("'a\\'", Class::Str),
                ("# note", Class::Comment)
            ]
        );
        assert_eq!(one(&DOCKERFILE, "    run from copy $USER"), []);
        let mut state = State::Code;
        pieces(&DOCKERFILE, "LABEL note=don't", &mut state);
        assert_eq!(state, State::Code);
    }

    #[test]
    fn makefile_directives_are_keywords_only_at_the_start_of_a_line() {
        assert_eq!(
            one(&MAKEFILE, "include config.mk # local"),
            [("include", Class::Keyword), ("# local", Class::Comment)]
        );
        assert_eq!(one(&MAKEFILE, "-include local.mk"), [("include", Class::Keyword)]);
        assert_eq!(one(&MAKEFILE, "  ifeq ($(MODE),release)"), [("ifeq", Class::Keyword)]);
        assert_eq!(one(&MAKEFILE, "main.o: main.c include/define.h"), []);
        // A directive ends at a blank; these are targets.
        for target in ["include/version.h: version.txt", "export-pdf: doc.tex", "define.o: define.c"] {
            assert_eq!(one(&MAKEFILE, target), [], "{target}");
        }
        assert_eq!(
            one(&MAKEFILE, "\t$(CC) -O2 -o $@ $< \"don't\" # link"),
            [("\"don't\"", Class::Str), ("# link", Class::Comment)]
        );
        let mut state = State::Code;
        pieces(&MAKEFILE, "\t@echo don't stop", &mut state);
        assert_eq!(state, State::Code);
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
    fn rust_raw_c_strings_need_all_the_hashes_to_close() {
        let marks = "#".repeat(255);
        let start = format!("cr{marks}\"start");
        let close = format!("\"{marks}");
        let end = format!("{close}; let n = 1;");
        let mut state = State::Code;
        assert_eq!(pieces(&RUST, &start, &mut state), [(start.as_str(), Class::Str)]);
        assert_eq!(pieces(&RUST, "\"# // still raw", &mut state), [("\"# // still raw", Class::Str)]);
        assert_eq!(pieces(&RUST, &end, &mut state)[..2], [(close.as_str(), Class::Str), ("let", Class::Keyword)]);
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
    fn yaml_doubled_apostrophes_do_not_end_single_quoted_strings() {
        assert_eq!(
            one(&YAML, "text: 'don''t' # comment"),
            [("text", Class::Key), ("'don''t'", Class::Str), ("# comment", Class::Comment)]
        );
    }

    #[test]
    fn yaml_quoted_values_can_extend_across_lines() {
        let mut state = State::Code;
        pieces(&YAML, "text: 'one", &mut state);
        assert_eq!(
            pieces(&YAML, "  two'' # text' # comment", &mut state),
            [("  two'' # text'", Class::Str), ("# comment", Class::Comment)]
        );
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
        for opener in ["<<<TEXT", "<<<'TEXT'", "<<< \t\"TEXT\" \t"] {
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
    fn php_heredocs_end_at_the_label_not_at_a_semicolon_inside() {
        let mut state = State::Code;
        pieces(&PHP, "echo f(<<<'END'", &mut state);
        for line in ["a; /* body", "END_MORE", "text ending with END"] {
            assert_eq!(pieces(&PHP, line, &mut state), [(line, Class::Str)]);
        }
        assert_eq!(pieces(&PHP, "  END, 42);", &mut state), [("  END", Class::Str), ("42", Class::Number)]);
        assert_eq!(state, State::Code);
    }

    #[test]
    fn malformed_php_heredoc_candidates_leave_long_lines_in_code_state() {
        // Overlapping openers used to scan the same whitespace suffix for every '<'.
        const N: usize = 32_768;
        let mut text = String::with_capacity(2 * N + 3);
        text.extend(std::iter::repeat_n('<', N + 3));
        text.extend(std::iter::repeat_n(' ', N));

        let mut state = State::Code;
        assert!(pieces(&PHP, &text, &mut state).is_empty());
        assert_eq!(state, State::Code);
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
    fn cpp_raw_strings_match_the_custom_delimiter() {
        let mut state = State::Code;
        let start = "u8R\"tag(initial )\" still raw";
        assert_eq!(pieces(&CPP, start, &mut state), [(start, Class::Str)]);
        assert_eq!(pieces(&CPP, "/* raw */", &mut state), [("/* raw */", Class::Str)]);
        assert_eq!(pieces(&CPP, ")tag\"; return;", &mut state), [(")tag\"", Class::Str), ("return", Class::Keyword)]);
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
    fn nested_javascript_templates_keep_the_outer_string_open() {
        let mut state = State::Code;
        let first = "const tpl = `outer ${f(`nested ${";
        let next = "  { key: '`}' }.key /* } `";
        let last = " */} end`) } tail`";
        assert_eq!(pieces(&JAVASCRIPT, first, &mut state)[1..], [(&first[12..], Class::Str)]);
        assert_eq!(pieces(&JAVASCRIPT, next, &mut state), [(next, Class::Str)]);
        assert_eq!(
            pieces(&JAVASCRIPT, &format!("{last}; return;"), &mut state),
            [(last, Class::Str), ("return", Class::Keyword)]
        );
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
    fn toml_multiline_strings_keep_extra_closing_quotes_inside_the_span() {
        for close in ["\"\"\"\"", "\"\"\"\"\"", "''''", "'''''"] {
            let open = &close[..3];
            let body = format!("quoted text{close}");
            let comment = "# comment";
            let line = format!("{body} {comment}");
            let mut state = State::Code;
            pieces(&TOML, &format!("text = {open}"), &mut state);
            assert_eq!(pieces(&TOML, &line, &mut state), [(body.as_str(), Class::Str), (comment, Class::Comment)]);
            assert_eq!(state, State::Code);

            let value = format!("{open}{body}");
            assert_eq!(
                one(&TOML, &format!("text = {value} {comment}")),
                [("text", Class::Key), (value.as_str(), Class::Str), (comment, Class::Comment)]
            );
        }
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
    fn csharp_raw_strings_use_the_length_of_the_opening_quote_run() {
        let mut state = State::Code;
        pieces(&CSHARP, "var s = \"\"\"\"", &mut state);
        let body = "  \"\"\" // still raw";
        assert_eq!(pieces(&CSHARP, body, &mut state), [(body, Class::Str)]);
        assert_eq!(
            pieces(&CSHARP, "  \"\"\"\"; return;", &mut state),
            [("  \"\"\"\"", Class::Str), ("return", Class::Keyword)]
        );
    }

    #[test]
    fn csharp_doubled_quotes_do_not_end_multiline_verbatim_strings() {
        let mut state = State::Code;
        let first = r#"@"This has ""quoted"" text"#;
        assert_eq!(pieces(&CSHARP, first, &mut state), [(first, Class::Str)]);
        assert_eq!(
            pieces(&CSHARP, "on the next line\"; return;", &mut state)[..2],
            [("on the next line\"", Class::Str), ("return", Class::Keyword)]
        );
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
