//! Compression codecs shared by inputs (files, request bodies), dumps and backups:
//! gzip, zstd, brotli and the LZ4 frame format.
//!
//! gzip and LZ4 are always built. zstd (`zstd` feature, libzstd) and brotli (`brotli`
//! feature, pure Rust) are optional in the library and enabled by the server; without
//! them their inputs fail with [`Error::Unsupported`].

use crate::error::{Budget, BudgetKind, Error, Result};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::Path;

/// A compression format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    #[default]
    None,
    Gzip,
    Zstd,
    Brotli,
    Lz4,
}

/// A codec-specific level, clamped to the codec's range (gzip 0–9, zstd −7–22,
/// brotli 0–11; LZ4 has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level(pub i32);

/// A compressing writer; [`finish`](FinishWrite::finish) writes the frame footer and
/// flushes. Dropping one without finishing may leave a truncated stream.
pub trait FinishWrite: Write {
    fn finish(self: Box<Self>) -> io::Result<()>;
}

impl Codec {
    pub const ALL: [Codec; 5] = [
        Codec::None,
        Codec::Gzip,
        Codec::Zstd,
        Codec::Brotli,
        Codec::Lz4,
    ];

    /// `none`, `gzip`/`gz`, `zstd`/`zst`, `brotli`/`br` or `lz4`.
    pub fn parse(s: &str) -> Result<Codec> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "none" | "identity" => Codec::None,
            "gzip" | "gz" => Codec::Gzip,
            "zstd" | "zst" => Codec::Zstd,
            "brotli" | "br" => Codec::Brotli,
            "lz4" => Codec::Lz4,
            _ => {
                return Err(Error::invalid(format!(
                    "unknown compression {s:?} (none, gzip, zstd, brotli, lz4)"
                )));
            }
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::None => "none",
            Codec::Gzip => "gzip",
            Codec::Zstd => "zstd",
            Codec::Brotli => "brotli",
            Codec::Lz4 => "lz4",
        }
    }

    /// The file extension, with its dot (empty for `None`).
    pub fn extension(self) -> &'static str {
        match self {
            Codec::None => "",
            Codec::Gzip => ".gz",
            Codec::Zstd => ".zst",
            Codec::Brotli => ".br",
            Codec::Lz4 => ".lz4",
        }
    }

    /// The HTTP `Content-Encoding` token (LZ4 has none).
    pub fn content_encoding(self) -> Option<&'static str> {
        match self {
            Codec::Gzip => Some("gzip"),
            Codec::Zstd => Some("zstd"),
            Codec::Brotli => Some("br"),
            Codec::None | Codec::Lz4 => None,
        }
    }

    /// The codec of an HTTP `Content-Encoding` token.
    pub fn from_content_encoding(s: &str) -> Option<Codec> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gzip" | "x-gzip" => Some(Codec::Gzip),
            "zstd" => Some(Codec::Zstd),
            "br" => Some(Codec::Brotli),
            "identity" => Some(Codec::None),
            _ => None,
        }
    }

    /// The codec named by a file's last extension (`data.ttl.zst`), if any.
    pub fn from_extension(path: &Path) -> Option<Codec> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        Codec::ALL
            .into_iter()
            .find(|c| *c != Codec::None && name.ends_with(c.extension()))
    }

    /// `name` without a codec extension.
    pub fn strip_extension(name: &str) -> &str {
        let lower = name.to_ascii_lowercase();
        for c in Codec::ALL {
            if c != Codec::None && lower.ends_with(c.extension()) {
                return &name[..name.len() - c.extension().len()];
            }
        }
        name
    }

    /// The codec of a stream from its first bytes (4 are enough). Brotli has no magic
    /// number, so it is never detected.
    pub fn sniff(prefix: &[u8]) -> Option<Codec> {
        match prefix {
            [0x1f, 0x8b, ..] => Some(Codec::Gzip),
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Some(Codec::Zstd),
            // zstd skippable frame: 0x184D2A5?
            [b0, 0x2a, 0x4d, 0x18, ..] if b0 & 0xf0 == 0x50 => Some(Codec::Zstd),
            [0x04, 0x22, 0x4d, 0x18, ..] => Some(Codec::Lz4),
            _ => None,
        }
    }

    /// The codec of an input: an explicit choice is checked against the magic bytes;
    /// otherwise the magic bytes win over the extension. Returns the codec and, when
    /// the extension disagrees with the data, a warning.
    pub fn detect(
        explicit: Option<Codec>,
        prefix: &[u8],
        name: Option<&Path>,
    ) -> Result<(Codec, Option<String>)> {
        let sniffed = Codec::sniff(prefix);
        let by_name = name.and_then(Codec::from_extension);
        let label = || {
            name.map(|p| p.display().to_string())
                .unwrap_or_else(|| "input".into())
        };
        if let Some(c) = explicit {
            return match sniffed {
                Some(s) if s != c => Err(Error::invalid(format!(
                    "{} is {} data, not {}",
                    label(),
                    s.name(),
                    c.name()
                ))),
                None if c != Codec::None && c != Codec::Brotli => Err(Error::invalid(format!(
                    "{} is not {} data",
                    label(),
                    c.name()
                ))),
                _ => Ok((c, None)),
            };
        }
        match (sniffed, by_name) {
            (Some(s), Some(n)) if s != n => Ok((
                s,
                Some(format!(
                    "{} is named like {} but holds {} data; reading it as {}",
                    label(),
                    n.name(),
                    s.name(),
                    s.name()
                )),
            )),
            (Some(s), _) => Ok((s, None)),
            (None, Some(Codec::Brotli)) => Ok((Codec::Brotli, None)),
            (None, Some(n)) => Ok((
                Codec::None,
                Some(format!(
                    "{} is named like {} but is not; reading it uncompressed",
                    label(),
                    n.name()
                )),
            )),
            (None, None) => Ok((Codec::None, None)),
        }
    }

    /// How many times larger than the compressed size an input usually is (RDF text),
    /// for size estimates.
    pub fn expansion(self) -> u64 {
        match self {
            Codec::None => 1,
            Codec::Gzip => 8,
            Codec::Zstd | Codec::Brotli => 10,
            Codec::Lz4 => 4,
        }
    }

    /// Whether this build can read the codec.
    // without the zstd and brotli features both arms are `false` and clippy asks for
    // `matches!`, which would hide the per-feature mapping
    #[allow(clippy::match_like_matches_macro)]
    pub fn supported(self) -> bool {
        match self {
            Codec::Zstd => cfg!(feature = "zstd"),
            Codec::Brotli => cfg!(feature = "brotli"),
            _ => true,
        }
    }

    /// The default codec of N-Quads dumps and backups (`/$/backup`, `sparkles backup`):
    /// zstd (level 3) when built with the `zstd` feature, else gzip.
    pub fn dump_default() -> Codec {
        if cfg!(feature = "zstd") {
            Codec::Zstd
        } else {
            Codec::Gzip
        }
    }

    fn unsupported(self) -> Error {
        Error::Unsupported(format!("built without {}", self.name()))
    }

    /// A decompressing reader. Concatenated frames are read to the end. With `limit`,
    /// reading past `limit` decompressed bytes fails with
    /// [`BudgetKind::DecompressedBytes`].
    pub fn reader<'a>(self, r: impl Read + 'a, limit: Option<u64>) -> Result<Box<dyn Read + 'a>> {
        let r: Box<dyn Read + 'a> = match self {
            Codec::None => Box::new(r),
            Codec::Gzip => Box::new(flate2::read::MultiGzDecoder::new(io::BufReader::new(r))),
            Codec::Lz4 => Box::new(Lz4Frames(Some(lz4_flex::frame::FrameDecoder::new(
                io::BufReader::new(r),
            )))),
            #[cfg(feature = "zstd")]
            Codec::Zstd => Box::new(zstd::stream::read::Decoder::new(r)?),
            #[cfg(feature = "brotli")]
            Codec::Brotli => Box::new(brotli::Decompressor::new(r, 64 << 10)),
            #[allow(unreachable_patterns)]
            c => return Err(c.unsupported()),
        };
        Ok(match limit {
            Some(limit) => Box::new(LimitedRead {
                inner: r,
                read: 0,
                limit,
            }),
            None => r,
        })
    }

    /// A compressing writer at `level` (the codec's default if `None`); zstd uses
    /// `threads` workers when more than one.
    pub fn writer<'a>(
        self,
        w: impl Write + 'a,
        level: Option<Level>,
        threads: usize,
    ) -> Result<Box<dyn FinishWrite + 'a>> {
        let _ = threads;
        Ok(match self {
            Codec::None => Box::new(Plain(w)),
            Codec::Gzip => {
                let l = level.map_or(6, |l| l.0.clamp(0, 9)) as u32;
                Box::new(flate2::write::GzEncoder::new(
                    w,
                    flate2::Compression::new(l),
                ))
            }
            Codec::Lz4 => Box::new(lz4_flex::frame::FrameEncoder::new(w)),
            #[cfg(feature = "zstd")]
            Codec::Zstd => {
                let l = level.map_or(3, |l| l.0.clamp(-7, 22));
                let mut e = zstd::stream::write::Encoder::new(w, l)?;
                if threads > 1 {
                    e.multithread(threads as u32)?;
                }
                Box::new(e)
            }
            #[cfg(feature = "brotli")]
            Codec::Brotli => {
                let q = level.map_or(5, |l| l.0.clamp(0, 11)) as u32;
                Box::new(brotli::CompressorWriter::new(w, 64 << 10, q, 22))
            }
            #[allow(unreachable_patterns)]
            c => return Err(c.unsupported()),
        })
    }
}

impl std::fmt::Display for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Codec {
    type Err = Error;
    fn from_str(s: &str) -> Result<Codec> {
        Codec::parse(s)
    }
}

struct Plain<W>(W);

impl<W: Write> Write for Plain<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write> FinishWrite for Plain<W> {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write> FinishWrite for flate2::write::GzEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish()?.flush()
    }
}

impl<W: Write> FinishWrite for lz4_flex::frame::FrameEncoder<W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish().map_err(io::Error::other)?.flush()
    }
}

#[cfg(feature = "zstd")]
impl<W: Write> FinishWrite for zstd::stream::write::Encoder<'_, W> {
    fn finish(self: Box<Self>) -> io::Result<()> {
        (*self).finish()?.flush()
    }
}

#[cfg(feature = "brotli")]
impl<W: Write> FinishWrite for brotli::CompressorWriter<W> {
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.flush()?;
        // `into_inner` writes the final meta-block
        self.into_inner().flush()
    }
}

/// Reads LZ4 frames one after another (the frame decoder stops after one).
struct Lz4Frames<R: io::BufRead>(Option<lz4_flex::frame::FrameDecoder<R>>);

impl<R: io::BufRead> Read for Lz4Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let Some(d) = &mut self.0 else { return Ok(0) };
            let n = d.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            // the end of a frame: another one follows unless the input is done
            let mut r = self.0.take().expect("decoder").into_inner();
            if r.fill_buf()?.is_empty() {
                return Ok(0);
            }
            self.0 = Some(lz4_flex::frame::FrameDecoder::new(r));
        }
    }
}

/// Fails once more than `limit` bytes were read.
struct LimitedRead<R> {
    inner: R,
    read: u64,
    limit: u64,
}

impl<R: Read> Read for LimitedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.limit {
            return Err(io::Error::other(Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                limit: self.limit,
                requested: self.read,
            })));
        }
        Ok(n)
    }
}

/// The engine error inside an I/O error from a [`Codec::reader`] (a budget), or the
/// I/O error itself.
pub fn io_error(e: io::Error) -> Error {
    if e.get_ref().is_some_and(|i| i.is::<Error>()) {
        match e.into_inner().map(|i| i.downcast::<Error>()) {
            Some(Ok(e)) => *e,
            Some(Err(i)) => Error::Io(io::Error::other(i)),
            None => unreachable!(),
        }
    } else {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(c: Codec) {
        let data: Vec<u8> = (0..200_000u32)
            .flat_map(|i| format!("<s{i}> <p> \"o {}\" .\n", i % 97).into_bytes())
            .collect();
        let mut out = Vec::new();
        let mut w = c.writer(&mut out, None, 2).unwrap();
        w.write_all(&data).unwrap();
        w.finish().unwrap();
        if c != Codec::None {
            assert!(out.len() < data.len() / 3, "{c}: {}", out.len());
        }
        if c != Codec::Brotli {
            assert_eq!(Codec::sniff(&out), (c != Codec::None).then_some(c));
        }
        // two concatenated frames decode to both (brotli has no frames)
        let mut two = out.clone();
        if c != Codec::Brotli {
            two.extend_from_slice(&out);
        }
        let copies = if c == Codec::Brotli { 1 } else { 2 };
        let mut back = Vec::new();
        c.reader(&two[..], None)
            .unwrap()
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back.len(), data.len() * copies, "{c}");
        assert_eq!(&back[..data.len()], &data[..]);
        // a limit below the decompressed size is a budget error
        let e = c
            .reader(&out[..], Some(1000))
            .unwrap()
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert!(
            matches!(
                io_error(e),
                Error::BudgetExceeded(Budget {
                    kind: BudgetKind::DecompressedBytes,
                    ..
                })
            ),
            "{c}"
        );
    }

    #[test]
    fn roundtrips() {
        for c in Codec::ALL {
            if c.supported() {
                roundtrip(c);
            }
        }
    }

    #[test]
    fn names_and_detection() {
        let p = Path::new;
        assert_eq!(Codec::from_extension(p("a.ttl.ZST")), Some(Codec::Zstd));
        assert_eq!(Codec::from_extension(p("a.nq.gz")), Some(Codec::Gzip));
        assert_eq!(Codec::from_extension(p("a.ttl")), None);
        assert_eq!(Codec::strip_extension("a.ttl.br"), "a.ttl");
        assert_eq!(Codec::parse("zst").unwrap(), Codec::Zstd);
        assert!(Codec::parse("compress").is_err());
        let gz = [0x1f, 0x8b, 8, 0];
        // magic wins over the name, with a warning
        let (c, warn) = Codec::detect(None, &gz, Some(p("x.ttl.zst"))).unwrap();
        assert_eq!(c, Codec::Gzip);
        assert!(warn.unwrap().contains("gzip"));
        // an explicit codec that disagrees with the data is an error
        assert!(Codec::detect(Some(Codec::Brotli), &gz, None).is_err());
        assert!(Codec::detect(Some(Codec::Zstd), b"<a> ", None).is_err());
        // brotli only by name or choice
        assert_eq!(
            Codec::detect(None, b"\x0b\x02", Some(p("x.nt.br")))
                .unwrap()
                .0,
            Codec::Brotli
        );
        assert_eq!(
            Codec::detect(None, b"<a> <b>", None).unwrap(),
            (Codec::None, None)
        );
    }
}
