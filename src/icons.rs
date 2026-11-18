//! One small symbol per file type.
//!
//! The default set uses Nerd Font glyphs. `STRATA_ICONS=plain` falls back to
//! ordinary Unicode for fonts without them; `STRATA_ICONS=off` shows none.

use std::sync::LazyLock;

enum Set {
    Nerd,
    Plain,
    Off,
}

static SET: LazyLock<Set> = LazyLock::new(|| match std::env::var("STRATA_ICONS").as_deref() {
    Ok("plain") => Set::Plain,
    Ok("off" | "none") => Set::Off,
    _ => Set::Nerd,
});

pub fn dir(open: bool) -> &'static str {
    match (&*SET, open) {
        (Set::Nerd, true) => "\u{f07c}",
        (Set::Nerd, false) => "\u{f07b}",
        (Set::Plain, true) => "▾",
        (Set::Plain, false) => "▸",
        (Set::Off, _) => "",
    }
}

pub fn file(name: &str) -> &'static str {
    match &*SET {
        Set::Nerd => nerd(name),
        Set::Plain => "•",
        Set::Off => "",
    }
}

fn nerd(name: &str) -> &'static str {
    match name {
        "Dockerfile" | "docker-compose.yml" | "compose.yaml" | "compose.yml" => return "\u{f308}",
        "Makefile" | "Justfile" | "justfile" => return "\u{f489}",
        ".gitignore" | ".gitattributes" | ".gitmodules" => return "\u{e702}",
        "package.json" | "package-lock.json" => return "\u{e71e}",
        "LICENSE" | "LICENSE.md" | "LICENSE.txt" => return "\u{f15c}",
        _ => {}
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "\u{e7a8}",
        "py" => "\u{e606}",
        "js" | "mjs" | "cjs" => "\u{e74e}",
        "ts" | "mts" | "cts" => "\u{e628}",
        "jsx" | "tsx" => "\u{e7ba}",
        "md" | "mdx" => "\u{e73e}",
        "json" | "jsonc" => "\u{e60b}",
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" => "\u{e615}",
        "html" | "htm" => "\u{e736}",
        "css" | "scss" | "sass" => "\u{e749}",
        "go" => "\u{e627}",
        "lua" => "\u{e620}",
        "sh" | "bash" | "zsh" | "fish" => "\u{f489}",
        "lock" => "\u{f023}",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "avif" => "\u{f1c5}",
        "c" | "h" => "\u{e61e}",
        "cpp" | "cc" | "cxx" | "hpp" => "\u{e61d}",
        "java" => "\u{e738}",
        "rb" => "\u{e739}",
        "php" => "\u{e73d}",
        "swift" => "\u{e755}",
        "sql" | "db" | "sqlite" => "\u{f1c0}",
        "pdf" => "\u{f1c1}",
        "zip" | "gz" | "tar" | "tgz" | "xz" | "zst" => "\u{f1c6}",
        "txt" | "log" => "\u{f15c}",
        _ => "\u{f15b}",
    }
}
