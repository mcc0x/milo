use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

const BUNDLE: &str = "dashboard/bundle";
const FORMAT: &str = "milo-dashboard-bundle-v1";

fn digest(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn script_digest(hash: &str) -> bool {
    hash.strip_suffix('=').is_some_and(|bytes| {
        bytes.len() == 43 && bytes.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
    })
}

fn mime(file: &str) -> Option<&'static str> {
    if file == "index.html" {
        return Some("text/html; charset=utf-8");
    }
    if !matches!(file, "favicon.png" | "milo.webp" | "rex.png") {
        let next = file.strip_prefix("_next/static/")?;
        if !next.split('/').all(|part| {
            part.as_bytes().first().is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        }) {
            return None;
        }
    }
    Some(match file.rsplit_once('.')?.1 {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

/// Ship the helper inside Milo, but link AppKit and WebKit only into the
/// helper: a normal pane does not load either framework at startup.
fn build_preview() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    println!("cargo:rerun-if-changed=macos/Preview");
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        _ => panic!("unsupported macOS preview architecture"),
    };
    let out = Path::new(&env::var_os("OUT_DIR").unwrap()).join("Milo Preview");
    let compiled = std::process::Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "clang", "-target", &format!("{arch}-apple-macos13.0"), "-fobjc-arc", "-Os"])
        .args(["macos/Preview/main.m", "macos/Preview/Anchor.m", "macos/Preview/Browser.m"])
        .args(["-framework", "AppKit", "-framework", "WebKit", "-framework", "Security"])
        .args(["-framework", "CoreGraphics", "-Wl,-sectcreate,__TEXT,__info_plist,macos/Preview/Info.plist", "-o"])
        .arg(out)
        .status()
        .expect("install Apple's Command Line Tools to build the WebKit preview");
    assert!(compiled.success(), "could not compile the macOS WebKit preview");
}

fn main() {
    build_preview();
    // Cargo source archives carry the production bundle. No Node, network or generated
    // framework intermediates are needed here. CI checks the source and asset digests.
    println!("cargo:rerun-if-changed={BUNDLE}");
    let manifest = fs::read_to_string(format!("{BUNDLE}/manifest"))
        .unwrap_or_else(|error| panic!("could not read the dashboard bundle: {error}; run bun run build in dashboard/"));
    let mut lines = manifest.lines();
    assert_eq!(lines.next(), Some(FORMAT), "unsupported dashboard bundle manifest version");
    assert!(lines.next().and_then(|line| line.strip_prefix("source ")).is_some_and(digest), "missing dashboard source digest");

    let mut scripts = String::new();
    let mut assets = BTreeMap::new();
    for line in lines {
        if let Some(hash) = line.strip_prefix("script ").filter(|hash| script_digest(hash)) {
            write!(scripts, " 'sha256-{hash}'").unwrap();
        } else if let Some((file, hash)) = line.strip_prefix("asset ").and_then(|row| row.split_once(' ')) {
            let mime = mime(file).filter(|_| digest(hash)).unwrap_or_else(|| panic!("invalid dashboard asset: {line}"));
            let url = if file == "index.html" { "/".to_owned() } else { format!("/{file}") };
            assert!(assets.insert(url, (file, mime)).is_none(), "duplicate dashboard asset: {file}");
        } else {
            panic!("invalid dashboard bundle record: {line}");
        }
    }
    assert!(!scripts.is_empty(), "missing dashboard inline script digests");
    for url in ["/", "/favicon.png", "/milo.webp", "/rex.png"] {
        assert!(assets.contains_key(url), "missing dashboard bundle asset {url}");
    }
    assert!(assets.keys().any(|url| url.ends_with(".js")), "missing dashboard JavaScript chunks");

    let mut rust = String::from("static ASSETS: &[Embedded] = &[\n");
    for (url, (file, mime)) in assets {
        let scripts = if url == "/" { scripts.as_str() } else { "" };
        writeln!(
            rust,
            "    ({url:?}, {mime:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{BUNDLE}/{file}\")), {scripts:?}),"
        )
        .unwrap();
    }
    rust.push_str("];\n");
    fs::write(Path::new(&env::var_os("OUT_DIR").unwrap()).join("dashboard.rs"), rust).unwrap();
}
