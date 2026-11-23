//! Picks a symbol and colour for each file type and for open and closed folders.
//!
//! The colour belongs to the kind of file and never to its git state: the
//! name beside it carries that. Outside Rex the default uses ordinary symbols.
//! `MILO_ICONS` can select `nerd`, `plain`, or `off` explicitly.

use crate::preferences::IconMode;

#[derive(Clone, Copy)]
pub struct Icon {
    pub glyph: &'static str,
    /// Index into the 256-colour palette.
    pub colour: u8,
}

const FOLDER: u8 = 75;

pub fn dir(mode: IconMode, open: bool) -> Icon {
    let glyph = match (mode, open) {
        (IconMode::Nerd, true) => "\u{f07c}",
        (IconMode::Nerd, false) => "\u{f07b}",
        (IconMode::Plain, true) => "▾",
        (IconMode::Plain, false) => "▸",
        (IconMode::Off, _) => "",
    };
    Icon { glyph, colour: FOLDER }
}

pub fn file(mode: IconMode, name: &str) -> Icon {
    if mode == IconMode::Off {
        return Icon { glyph: "", colour: 0 };
    }
    let (glyph, colour) = kind(name);
    let glyph = if mode == IconMode::Plain { "•" } else { glyph };
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
    const GENERIC: (&str, u8) = ("\u{f15b}", 245);
    // The longest known extension is "sqlite". Keep folding on the stack.
    let mut folded = [0; 6];
    let Some(ext_bytes) = folded.get_mut(..ext.len()) else { return GENERIC };
    ext_bytes.copy_from_slice(ext.as_bytes());
    ext_bytes.make_ascii_lowercase();
    match &*ext_bytes {
        b"rs" => ("\u{e7a8}", 208),
        b"py" => ("\u{e606}", 74),
        b"js" | b"mjs" | b"cjs" => ("\u{e74e}", 220),
        b"ts" | b"mts" | b"cts" => ("\u{e628}", 39),
        b"jsx" | b"tsx" => ("\u{e7ba}", 80),
        b"md" | b"mdx" => ("\u{e73e}", 110),
        b"json" | b"jsonc" => ("\u{e60b}", 185),
        b"toml" | b"yaml" | b"yml" | b"ini" | b"conf" | b"cfg" | b"env" => ("\u{e615}", 103),
        b"html" | b"htm" => ("\u{e736}", 202),
        b"css" | b"scss" | b"sass" => ("\u{e749}", 69),
        b"go" => ("\u{e627}", 80),
        b"lua" => ("\u{e620}", 63),
        b"sh" | b"bash" | b"zsh" | b"fish" => ("\u{f489}", 113),
        b"lock" => ("\u{f023}", 245),
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"svg" | b"webp" | b"ico" | b"avif" => ("\u{f1c5}", 176),
        b"c" | b"h" => ("\u{e61e}", 68),
        b"cpp" | b"cc" | b"cxx" | b"hpp" => ("\u{e61d}", 68),
        b"java" => ("\u{e738}", 167),
        b"rb" => ("\u{e739}", 160),
        b"php" => ("\u{e73d}", 98),
        b"swift" => ("\u{e755}", 209),
        b"sql" | b"db" | b"sqlite" => ("\u{f1c0}", 180),
        b"pdf" => ("\u{f1c1}", 160),
        b"zip" | b"gz" | b"tar" | b"tgz" | b"xz" | b"zst" => ("\u{f1c6}", 179),
        b"txt" | b"log" => ("\u{f15c}", 250),
        _ => GENERIC,
    }
}
