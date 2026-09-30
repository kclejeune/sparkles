//! HTTP compression: response encodings and levels (`serve --http-compression…`), and
//! compressed request bodies (`Content-Encoding: gzip, br, zstd, deflate`) with a cap on
//! their decompressed size (`--max-decompressed-mb`).

use axum::extract::Request;
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use tower_http::CompressionLevel;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{And, DefaultPredicate, Predicate, SizeAbove};

/// Response compression of `serve`.
#[derive(Clone, Debug)]
pub struct HttpCompression {
    pub enabled: bool,
    pub level: CompressionLevel,
    pub zstd: bool,
    pub br: bool,
    pub gzip: bool,
    pub deflate: bool,
}

impl Default for HttpCompression {
    fn default() -> HttpCompression {
        HttpCompression {
            enabled: true,
            level: CompressionLevel::Default,
            zstd: true,
            br: true,
            gzip: true,
            deflate: true,
        }
    }
}

impl HttpCompression {
    /// From the `serve` flags: `auto|off`, `fastest|default|best|N` and a comma-separated
    /// list of `zstd`, `br`, `gzip`, `deflate`.
    pub fn parse(mode: &str, level: &str, algorithms: &str) -> anyhow::Result<HttpCompression> {
        let enabled = match mode {
            "auto" | "on" => true,
            "off" | "none" => false,
            _ => anyhow::bail!("--http-compression must be auto or off, not {mode:?}"),
        };
        let level = match level {
            "fastest" => CompressionLevel::Fastest,
            "default" => CompressionLevel::Default,
            "best" => CompressionLevel::Best,
            n => CompressionLevel::Precise(n.parse().map_err(|_| {
                anyhow::anyhow!(
                    "--http-compression-level must be fastest, default, best or a number, not {n:?}"
                )
            })?),
        };
        let mut c = HttpCompression {
            enabled,
            level,
            zstd: false,
            br: false,
            gzip: false,
            deflate: false,
        };
        for a in algorithms
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            match a {
                "zstd" => c.zstd = true,
                "br" | "brotli" => c.br = true,
                "gzip" => c.gzip = true,
                "deflate" => c.deflate = true,
                _ => anyhow::bail!(
                    "unknown --http-compression-algorithms entry {a:?} (zstd, br, gzip, deflate)"
                ),
            }
        }
        if enabled && !(c.zstd || c.br || c.gzip || c.deflate) {
            anyhow::bail!("--http-compression-algorithms is empty; use --http-compression off");
        }
        Ok(c)
    }

    /// The response compression layer. Bodies under 256 bytes, images, and responses
    /// that already have a `Content-Encoding` (precompressed UI assets) are sent as is.
    pub fn layer(&self) -> CompressionLayer<And<DefaultPredicate, SizeAbove>> {
        let on = self.enabled;
        CompressionLayer::new()
            .quality(self.level)
            .zstd(on && self.zstd)
            .br(on && self.br)
            .gzip(on && self.gzip)
            .deflate(on && self.deflate)
            .compress_when(DefaultPredicate::new().and(SizeAbove::new(256)))
    }
}

/// Marks requests that arrive with a `Content-Encoding` (outside the decompression
/// layer, which removes the header).
#[derive(Clone, Copy)]
pub struct Encoded;

pub async fn mark_encoded(mut req: Request, next: Next) -> Response {
    if req
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        req.extensions_mut().insert(Encoded);
    }
    next.run(req).await
}

/// Caps the decompressed size of a compressed request body (inside the decompression
/// layer). Reading past the cap fails with `LengthLimitError`, which extractors and
/// the spooling reader turn into 413.
pub async fn limit_decompressed(
    axum::extract::State(limit): axum::extract::State<Option<u64>>,
    req: Request,
    next: Next,
) -> Response {
    match limit {
        Some(limit) if req.extensions().get::<Encoded>().is_some() => {
            let req = req.map(|b| {
                axum::body::Body::new(http_body_util::Limited::new(
                    b,
                    usize::try_from(limit).unwrap_or(usize::MAX),
                ))
            });
            next.run(req).await
        }
        _ => next.run(req).await,
    }
}

/// Whether a body read error is the decompressed-size cap.
pub fn is_length_limit(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur = Some(e);
    while let Some(e) = cur {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        cur = e.source();
    }
    false
}
