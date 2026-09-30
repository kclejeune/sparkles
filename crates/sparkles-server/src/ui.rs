//! The SvelteKit UI (ui/build), embedded into the binary and served under /ui/.
//!
//! The build writes brotli and gzip siblings of the larger text assets
//! (`ui/scripts/precompress.mjs`); they are served by `Accept-Encoding`, with
//! `Content-Encoding` set so the response compression layer leaves them alone.

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/build"]
struct Assets;

/// Whether `Accept-Encoding` allows `coding` (a q-value of 0 refuses it; `*` allows
/// anything not listed).
fn accepts(headers: &HeaderMap, coding: &str) -> bool {
    let Some(v) = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mut star = false;
    for item in v.split(',') {
        let mut parts = item.split(';');
        let name = parts.next().unwrap_or("").trim();
        let q = parts
            .filter_map(|p| p.trim().strip_prefix("q="))
            .find_map(|q| q.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        if name.eq_ignore_ascii_case(coding) {
            return q > 0.0;
        }
        if name == "*" {
            star = q > 0.0;
        }
    }
    star
}

fn file(path: &str, headers: &HeaderMap) -> Option<Response> {
    let f = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut encoded = None;
    let mut varies = false;
    for (coding, ext) in [("br", "br"), ("gzip", "gz")] {
        let Some(c) = Assets::get(&format!("{path}.{ext}")) else {
            continue;
        };
        varies = true;
        if encoded.is_none() && accepts(headers, coding) {
            encoded = Some((coding, c));
        }
    }
    let mut resp = match encoded {
        Some((coding, c)) => (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
                (header::CONTENT_ENCODING, coding.to_string()),
            ],
            c.data.into_owned(),
        )
            .into_response(),
        None => (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            f.data.into_owned(),
        )
            .into_response(),
    };
    if varies {
        resp.headers_mut().insert(
            header::VARY,
            header::HeaderValue::from_static("accept-encoding"),
        );
    }
    Some(resp)
}

pub async fn serve_index(headers: HeaderMap) -> Response {
    file("index.html", &headers)
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built").into_response())
}

/// Static asset, or the SPA fallback for client-side routes.
pub async fn serve(Path(path): Path<String>, headers: HeaderMap) -> Response {
    if let Some(r) = file(&path, &headers) {
        return r;
    }
    if path.starts_with("_app/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    serve_index(headers).await
}

/// An embedded asset with a brotli sibling, if the UI build has one.
#[cfg(test)]
pub(crate) fn precompressed_asset() -> Option<String> {
    Assets::iter()
        .find(|p| Assets::get(&format!("{p}.br")).is_some())
        .map(|p| p.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(header::ACCEPT_ENCODING, v.parse().unwrap());
        m
    }

    #[test]
    fn accept_encoding() {
        assert!(accepts(&h("gzip, br"), "br"));
        assert!(accepts(&h("gzip;q=0.5, BR;q=1"), "br"));
        assert!(!accepts(&h("gzip, br;q=0"), "br"));
        assert!(accepts(&h("*"), "br"));
        assert!(!accepts(&h("*, br;q=0"), "br"));
        assert!(!accepts(&h("gzip"), "br"));
        assert!(!accepts(&HeaderMap::new(), "gzip"));
    }
}
