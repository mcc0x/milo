//! Picks a symbol and colour for each file type and for open and closed folders.
//!
//! The colour belongs to the kind of file and never to its git state: the
//! name beside it carries that. The default set uses Nerd Font glyphs.
//! `STRATA_ICONS=plain` uses ordinary symbols; `off` or `none` hides them.

use std::sync::LazyLock;

enum Set {
    Nerd,
    Plain,
    Off,
}

static SET: LazyLock<Set> = LazyLock::new(|| match std::env::var("STRATA_ICONS").as_deref() {
    _ if cfg!(test) => Set::Plain,
    Ok("plain") => Set::Plain,
    Ok("off" | "none") => Set::Off,
    _ => Set::Nerd,
});

#[derive(Clone, Copy)]
pub struct Icon {
    pub glyph: &'static str,
    /// Index into the 256-colour palette.
    pub colour: u8,
}

const FOLDER: u8 = 75;

pub fn dir(open: bool) -> Icon {
    let glyph = match (&*SET, open) {
        (Set::Nerd, true) => "\u{f07c}",
        (Set::Nerd, false) => "\u{f07b}",
        (Set::Plain, true) => "▾",
        (Set::Plain, false) => "▸",
        (Set::Off, _) => "",
    };
    Icon { glyph, colour: FOLDER }
}

pub fn file(name: &str) -> Icon {
    let (glyph, colour) = kind(name);
    let glyph = match &*SET {
        Set::Nerd => glyph,
        Set::Plain => "•",
        Set::Off => "",
    };
    Icon { glyph, colour }
}

/// The Nerd Font glyph and colour for a file, by its name, then its extension.
fn kind(name: &str) -> (&'static str, u8) {
    match name {
        "Dockerfile" | "docker-compose.yml" | "compose.yaml" | "compose.yml" => return ("\u{f308}", 39),
        "Makefile" | "Justfile" | "justfile" => return ("\u{f489}", 173),
        ".gitignore" | ".gitattributes" | ".gitmodules" => return ("\u{e702}", 202),
        "package.json" | "package-lock.json" => return ("\u{e71e}", 167),
        "LICENSE" | "LICENSE.md" | "LICENSE.txt" => return ("\u{f15c}", 179),
        _ => {}
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "rs" => ("\u{e7a8}", 208),
        "py" => ("\u{e606}", 74),
        "js" | "mjs" | "cjs" => ("\u{e74e}", 220),
        "ts" | "mts" | "cts" => ("\u{e628}", 39),
        "jsx" | "tsx" => ("\u{e7ba}", 80),
        "md" | "mdx" => ("\u{e73e}", 110),
        "json" | "jsonc" => ("\u{e60b}", 185),
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" => ("\u{e615}", 103),
        "html" | "htm" => ("\u{e736}", 202),
        "css" | "scss" | "sass" => ("\u{e749}", 69),
        "go" => ("\u{e627}", 80),
        "lua" => ("\u{e620}", 63),
        "sh" | "bash" | "zsh" | "fish" => ("\u{f489}", 113),
        "lock" => ("\u{f023}", 245),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "avif" => ("\u{f1c5}", 176),
        "c" | "h" => ("\u{e61e}", 68),
        "cpp" | "cc" | "cxx" | "hpp" => ("\u{e61d}", 68),
        "java" => ("\u{e738}", 167),
        "rb" => ("\u{e739}", 160),
        "php" => ("\u{e73d}", 98),
        "swift" => ("\u{e755}", 209),
        "sql" | "db" | "sqlite" => ("\u{f1c0}", 180),
        "pdf" => ("\u{f1c1}", 160),
        "zip" | "gz" | "tar" | "tgz" | "xz" | "zst" => ("\u{f1c6}", 179),
        "txt" | "log" => ("\u{f15c}", 250),
        _ => ("\u{f15b}", 245),
    }
}
