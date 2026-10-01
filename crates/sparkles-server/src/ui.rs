//! The SvelteKit UI (ui/build), embedded into the binary and served under /ui/.
//!
//! The build writes brotli and gzip siblings of the larger text assets
//! (`ui/scripts/precompress.mjs`); they are served by `Accept-Encoding`, with
//! `Content-Encoding` set so the response compression layer leaves them alone.
//!
//! Pages carry a Content Security Policy ([`page_csp`]): scripts only from the UI itself
//! and the inline scripts of the build (by hash), no framing.

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
    let csp = (mime.essence_str() == "text/html").then(|| page_csp(&f.data));
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
    if let Some(Ok(csp)) = csp.map(|c| header::HeaderValue::from_str(&c)) {
        let h = resp.headers_mut();
        h.insert(header::CONTENT_SECURITY_POLICY, csp);
        h.insert(
            header::REFERRER_POLICY,
            header::HeaderValue::from_static("same-origin"),
        );
    }
    Some(resp)
}

/// The Content Security Policy of a UI page: everything from the UI's own origin, the
/// page's inline scripts by their SHA-256 (SvelteKit's start-up script and the theme
/// script of `app.html`), WebAssembly compiled from the UI's own modules (the formatter
/// in the browser; `'wasm-unsafe-eval'` allows nothing of JavaScript's `eval`), inline
/// styles (components set `style` attributes), and never in a frame.
pub fn page_csp(html: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let mut scripts = String::new();
    for body in inline_scripts(&String::from_utf8_lossy(html)) {
        let digest = sha2::Sha256::digest(body.as_bytes());
        scripts.push_str(" 'sha256-");
        scripts.push_str(&base64::engine::general_purpose::STANDARD.encode(digest));
        scripts.push('\'');
    }
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{scripts}; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; \
         worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; \
         frame-ancestors 'none'"
    )
}

/// The text of every `<script>` element without a `src` attribute.
fn inline_scripts(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start + "<script".len()..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let attrs = &after[..open_end];
        let body_start = &after[open_end + 1..];
        let Some(close) = body_start.find("</script") else {
            break;
        };
        if !attrs.contains("src=") {
            out.push(&body_start[..close]);
        }
        rest = &body_start[close..];
    }
    out
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
    fn page_csp_hashes_inline_scripts() {
        use base64::Engine as _;
        use sha2::Digest as _;
        let html = "<head><script>let a = 1;</script><script type=\"module\" src=\"/ui/x.js\"></script></head>\
                    <body><div><script>\n  go();\n</script></div></body>";
        assert_eq!(inline_scripts(html), vec!["let a = 1;", "\n  go();\n"]);
        let csp = page_csp(html.as_bytes());
        let hash = |s: &str| {
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(s.as_bytes()))
        };
        assert!(
            csp.contains(&format!(
                "script-src 'self' 'wasm-unsafe-eval' 'sha256-{}' 'sha256-{}';",
                hash("let a = 1;"),
                hash("\n  go();\n")
            )),
            "{csp}"
        );
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(!csp.contains("'unsafe-eval'"), "{csp}");
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
