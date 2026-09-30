//! The SvelteKit UI (ui/build), embedded into the binary and served under /ui/.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/build"]
struct Assets;

fn file(path: &str) -> Option<Response> {
    let f = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        (
            [(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())],
            f.data.into_owned(),
        )
            .into_response(),
    )
}

pub async fn serve_index() -> Response {
    file("index.html").unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built").into_response())
}

/// Static asset, or the SPA fallback for client-side routes.
pub async fn serve(Path(path): Path<String>) -> Response {
    if let Some(r) = file(&path) {
        return r;
    }
    if path.starts_with("_app/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    serve_index().await
}
