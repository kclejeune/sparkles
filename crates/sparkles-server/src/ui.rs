//! The SvelteKit UI (ui/build), embedded into the binary and served under /ui/.
//!
//! With `SPARKLES_UI_DIR` set to a UI build directory, the UI is read from there
//! instead, at run time. The Nix package sets it to its UI build, so that a change to
//! the UI rebuilds no Rust code.
//!
//! The build writes brotli and gzip siblings of the larger text assets
//! (`ui/scripts/precompress.mjs`); they are served by `Accept-Encoding`, with
//! `Content-Encoding` set so the response compression layer leaves them alone.
//!
//! Pages carry a Content Security Policy ([`page_csp`]): scripts only from the UI itself
//! and the inline scripts of the build (by hash), no framing.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/build"]
struct Assets;

/// The directory the UI is read from instead of the embedded build
/// (`SPARKLES_UI_DIR`), read once.
fn ui_dir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("SPARKLES_UI_DIR")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
    })
    .as_deref()
}

/// The bytes of the UI file at `path`, from [`ui_dir`] or the embedded build.
fn asset(path: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
    match ui_dir() {
        Some(dir) => read_under(dir, path).map(std::borrow::Cow::Owned),
        None => Assets::get(path).map(|f| f.data),
    }
}

/// The file at the relative `path` under `dir`. Only plain names are followed, so no
/// path leads out of the directory.
fn read_under(dir: &std::path::Path, path: &str) -> Option<Vec<u8>> {
    if path
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == ".." || c.contains('\\'))
    {
        return None;
    }
    let p = dir.join(path);
    if !p.is_file() {
        return None;
    }
    std::fs::read(p).ok()
}

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

fn file(path: &str, headers: &HeaderMap, map_origin: Option<&str>) -> Option<Response> {
    let f = asset(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let csp = if mime.essence_str() == "text/html" {
        Some(page_csp(&f, map_origin))
    } else if path.starts_with("_app/immutable/workers/") {
        Some(worker_csp(map_origin))
    } else {
        None
    };
    let cache = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut encoded = None;
    let mut varies = false;
    for (coding, ext) in [("br", "br"), ("gzip", "gz")] {
        let Some(c) = asset(&format!("{path}.{ext}")) else {
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
            c.into_owned(),
        )
            .into_response(),
        None => (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            f.into_owned(),
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
/// styles (components set `style` attributes), and never in a frame. `map_origin` (the
/// origin of `serve --map-style-url`) may also be fetched from and shown as images: the
/// maps' style, tiles, glyphs and sprites. Scripts stay the UI's own whatever the style.
pub fn page_csp(html: &[u8], map_origin: Option<&str>) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let mut scripts = String::new();
    for body in inline_scripts(&String::from_utf8_lossy(html)) {
        let digest = sha2::Sha256::digest(body.as_bytes());
        scripts.push_str(" 'sha256-");
        scripts.push_str(&base64::engine::general_purpose::STANDARD.encode(digest));
        scripts.push('\'');
    }
    let map = map_origin.map(|o| format!(" {o}")).unwrap_or_default();
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'{scripts}; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob:{map}; font-src 'self' data:; connect-src 'self'{map}; \
         worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; \
         frame-ancestors 'none'"
    )
}

/// The Content Security Policy of a web worker script of the UI (the maps' MapLibre
/// worker). A worker runs under the policy of its own script, not the page's: it may
/// fetch from the UI's origin and the map style's (the basemap, tiles, glyphs), and
/// nothing else.
pub fn worker_csp(map_origin: Option<&str>) -> String {
    let map = map_origin.map(|o| format!(" {o}")).unwrap_or_default();
    format!(
        "default-src 'none'; script-src 'self'; connect-src 'self'{map}; \
         img-src 'self' data: blob:{map}; frame-ancestors 'none'"
    )
}

/// The origin (`scheme://host[:port]`, lower case) of a `serve --map-style-url`, which
/// the pages' policy allows ([`page_csp`]): MapLibre fetches the style there, and the
/// tiles, glyphs and sprites the style names must come from the same origin. An error
/// for anything but an absolute `http` or `https` URL without credentials.
pub fn map_style_origin(url: &str) -> anyhow::Result<String> {
    let bad = || anyhow::anyhow!("--map-style-url: {url:?} is not an absolute http(s) URL");
    let uri: Uri = url.parse().map_err(|_| bad())?;
    let scheme = uri.scheme_str().ok_or_else(bad)?.to_ascii_lowercase();
    let authority = uri.authority().ok_or_else(bad)?;
    if !matches!(scheme.as_str(), "http" | "https")
        || authority.as_str().contains('@')
        || authority.host().is_empty()
    {
        return Err(bad());
    }
    Ok(format!(
        "{scheme}://{}",
        authority.as_str().to_ascii_lowercase()
    ))
}

/// The origin the pages' policy allows for the maps, when the server has a style URL.
fn map_origin(st: &AppState) -> Option<String> {
    st.map_style_url
        .as_deref()
        .and_then(|u| map_style_origin(u).ok())
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

pub async fn serve_index(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    file("index.html", &headers, map_origin(&st).as_deref())
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built").into_response())
}

/// Static asset, or the SPA fallback for client-side routes.
pub async fn serve(
    State(st): State<Arc<AppState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = file(&path, &headers, map_origin(&st).as_deref()) {
        return r;
    }
    if path.starts_with("_app/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    serve_index(State(st), headers).await
}

/// An embedded web worker script, if the UI build has one.
#[cfg(test)]
pub(crate) fn worker_asset() -> Option<String> {
    Assets::iter()
        .find(|p| p.starts_with("_app/immutable/workers/") && p.ends_with(".js"))
        .map(|p| p.into_owned())
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
        let csp = page_csp(html.as_bytes(), None);
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
        assert!(csp.contains("connect-src 'self';"), "{csp}");
    }

    #[test]
    fn map_style_origin_in_the_policy() {
        let o = map_style_origin("https://Tiles.Example.com:8443/styles/basic/style.json?key=1")
            .unwrap();
        assert_eq!(o, "https://tiles.example.com:8443");
        let csp = page_csp(b"<p>", Some(&o));
        assert!(
            csp.contains("img-src 'self' data: blob: https://tiles.example.com:8443;"),
            "{csp}"
        );
        assert!(
            csp.contains("connect-src 'self' https://tiles.example.com:8443;"),
            "{csp}"
        );
        // scripts and workers stay the UI's own
        assert!(
            csp.contains("script-src 'self' 'wasm-unsafe-eval';"),
            "{csp}"
        );
        assert!(csp.contains("worker-src 'self' blob:;"), "{csp}");
        let worker = worker_csp(Some(&o));
        assert!(
            worker.contains("connect-src 'self' https://tiles.example.com:8443;"),
            "{worker}"
        );
        assert_eq!(
            worker_csp(None),
            "default-src 'none'; script-src 'self'; connect-src 'self'; \
             img-src 'self' data: blob:; frame-ancestors 'none'"
        );
        assert_eq!(
            map_style_origin("http://localhost:8080/style.json").unwrap(),
            "http://localhost:8080"
        );
        for bad in [
            "style.json",
            "/styles/style.json",
            "ftp://example.com/style.json",
            "https://user:pw@example.com/style.json",
            "javascript:alert(1)",
            "https://exa mple.com/",
        ] {
            assert!(map_style_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_ui_directory_serves_plain_paths_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("_app")).unwrap();
        std::fs::write(dir.path().join("_app/x.js"), b"go()").unwrap();
        std::fs::write(dir.path().join("index.html"), b"<p>").unwrap();
        assert_eq!(read_under(dir.path(), "_app/x.js").unwrap(), b"go()");
        assert_eq!(read_under(dir.path(), "index.html").unwrap(), b"<p>");
        for bad in [
            "",
            "_app",
            "_app/",
            "../index.html",
            "_app/../index.html",
            "./index.html",
            "_app\\x.js",
            "missing.js",
        ] {
            assert!(read_under(dir.path(), bad).is_none(), "{bad}");
        }
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
