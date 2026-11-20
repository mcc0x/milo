//! Only enabled for source development. Never resolve assets against the user's project.
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;

use super::{Asset, Response};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/web/");

impl Asset {
    fn filename(self) -> &'static str {
        match self {
            Self::Index => "index.html",
            Self::Style => "app.css",
            Self::Script => "app.js",
            Self::Mascot => "milo.webp",
        }
    }

    fn path(self) -> std::path::PathBuf {
        std::path::Path::new(ROOT).join(self.filename())
    }
}

fn stamp(asset: Asset) -> io::Result<String> {
    let m = fs::metadata(asset.path())?;
    Ok(format!("{}:{}:{}:{}", m.ino(), m.len(), m.mtime(), m.mtime_nsec()))
}

fn revisions() -> io::Result<String> {
    Ok(format!("{}|{}|{}|{}", stamp(Asset::Index)?, stamp(Asset::Script)?, stamp(Asset::Mascot)?, stamp(Asset::Style)?))
}

pub(super) fn version() -> Response {
    Response::local_asset("text/plain", revisions().map(String::into_bytes))
}

pub(super) fn asset(asset: Asset) -> Response {
    let body = if matches!(asset, Asset::Index) {
        (|| {
            let revisions = revisions()?;
            let mut html = fs::read_to_string(asset.path())?;
            let client = format!("<script defer src=\"/dev.js\" data-revision=\"{revisions}\"></script>");
            html.insert_str(html.rfind("</body>").unwrap_or(html.len()), &client);
            Ok(html.into_bytes())
        })()
    } else {
        fs::read(asset.path())
    };
    Response::local_asset(asset.mime(), body)
}
