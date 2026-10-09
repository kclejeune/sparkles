//! The compression of the loader's temporary files.
//!
//! Builds with the `zstd` feature use zstd at level 1, and other builds use LZ4. The
//! files are written and read by the same build, so they do not record the codec.
//!
//! The partial vocabularies hold sorted and front-coded keys, which still repeat a lot
//! within a block, such as the paths of IRIs, language tags and datatypes. On DBpedia,
//! zstd makes their blocks about a quarter smaller than LZ4 does. The columns of the
//! batch quads, the sorted runs and the regrouping spills are delta-coded varints, and
//! zstd makes them smaller as well. zstd takes about twice as long as LZ4 to compress
//! and four times as long to decompress. The loader writes these files while it waits
//! for the device, and reads them back in threads that decode ahead of the merges.

use crate::error::{Error, Result};

/// A compressor that keeps its context from one block to the next.
pub(super) struct BlockCodec {
    #[cfg(feature = "zstd")]
    z: zstd::bulk::Compressor<'static>,
}

impl BlockCodec {
    pub(super) fn new() -> Result<BlockCodec> {
        Ok(BlockCodec {
            #[cfg(feature = "zstd")]
            z: zstd::bulk::Compressor::new(1)?,
        })
    }

    pub(super) fn compress(&mut self, raw: &[u8]) -> Result<Vec<u8>> {
        #[cfg(feature = "zstd")]
        return Ok(self.z.compress(raw)?);
        #[cfg(not(feature = "zstd"))]
        return Ok(lz4_flex::block::compress(raw));
    }
}

/// Compress `raw` with the codec of the calling thread.
pub(super) fn compress(raw: &[u8]) -> Result<Vec<u8>> {
    thread_local! {
        static CODEC: std::cell::RefCell<Option<BlockCodec>> =
            const { std::cell::RefCell::new(None) };
    }
    CODEC.with_borrow_mut(|c| match c {
        Some(c) => c.compress(raw),
        None => c.insert(BlockCodec::new()?).compress(raw),
    })
}

/// Decompress a block of `len` bytes into `out`. Each thread keeps one zstd context.
/// `what` names the file in an error.
pub(super) fn decompress(comp: &[u8], len: usize, out: &mut Vec<u8>, what: &str) -> Result<()> {
    let bad = |e: &dyn std::fmt::Display| Error::Corrupt(format!("{what} block: {e}"));
    out.clear();
    #[cfg(feature = "zstd")]
    {
        thread_local! {
            static DECODER: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> =
                const { std::cell::RefCell::new(None) };
        }
        out.reserve(len);
        DECODER.with_borrow_mut(|d| -> Result<()> {
            let d = match d {
                Some(d) => d,
                None => d.insert(zstd::bulk::Decompressor::new()?),
            };
            d.decompress_to_buffer(comp, out).map_err(|e| bad(&e))?;
            Ok(())
        })?;
    }
    #[cfg(not(feature = "zstd"))]
    {
        out.resize(len, 0);
        let n = lz4_flex::block::decompress_into(comp, out).map_err(|e| bad(&e))?;
        out.truncate(n);
    }
    if out.len() != len {
        return Err(bad(&format!("{} bytes instead of {len}", out.len())));
    }
    Ok(())
}
