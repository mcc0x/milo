use super::http::Response;

// The generated, URL-sorted table is the complete list of public files; requests
// never resolve against the filesystem or reinterpret a URL as a path.
type Embedded = (&'static str, &'static str, &'static [u8], &'static str);
include!(concat!(env!("OUT_DIR"), "/dashboard.rs"));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Asset(usize);

impl Asset {
    pub(super) fn at(path: &str) -> Option<Self> {
        ASSETS.binary_search_by(|asset| asset.0.cmp(path)).ok().map(Self)
    }

    pub(super) fn response(self) -> Response {
        let (_, mime, bytes, scripts) = ASSETS[self.0];
        Response::asset(mime, bytes, scripts)
    }
}
