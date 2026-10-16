//! The ZIP container of a document, read with bounds on every size.

use crate::{Error, Image, Limits};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// The signature of an OLE compound file, which holds a legacy binary document or a
/// password-protected one.
const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

pub struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    limits: Limits,
    /// bytes unpacked so far
    used: usize,
    images: usize,
}

/// The longest relationship target kept. Real targets are paths and URLs of a few hundred
/// bytes. A target is copied once per reference to it (a hyperlink, a slide id), so a
/// longer one is dropped with its relationship, as if the part did not name it.
pub const MAX_TARGET: usize = 2048;

/// One relationship of a part.
#[derive(Clone, Debug)]
pub struct Rel {
    /// the target as a path in the package, or the URL of an external one
    pub target: String,
    pub external: bool,
    /// the last segment of the relationship type, such as `slide` or `notesSlide`
    pub kind: String,
}

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8], limits: &Limits) -> Result<Package<'a>, Error> {
        if bytes.starts_with(OLE) {
            return Err(Error::Unsupported(
                "the file is a legacy binary Office document or a password-protected one: save it as docx, pptx or xlsx without a password".into(),
            ));
        }
        let zip = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| Error::Corrupt(format!("the file is not a ZIP container: {e}")))?;
        if zip.len() > limits.max_entries {
            return Err(Error::TooLarge(format!(
                "the container has {} entries; at most {} are read",
                zip.len(),
                limits.max_entries
            )));
        }
        Ok(Package {
            zip,
            limits: limits.clone(),
            used: 0,
            images: 0,
        })
    }

    pub fn has(&self, name: &str) -> bool {
        self.zip.index_for_name(name).is_some()
    }

    /// The bytes of an entry, or `None` when the container has none of that name.
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let max = self.limits.max_part;
        self.read_max(name, max)
    }

    fn read_max(&mut self, name: &str, max: usize) -> Result<Option<Vec<u8>>, Error> {
        let Some(i) = self.zip.index_for_name(name) else {
            return Ok(None);
        };
        let left = self.limits.max_total.saturating_sub(self.used);
        let cap = max.min(left);
        let f = self
            .zip
            .by_index(i)
            .map_err(|e| Error::Corrupt(format!("{name}: {e}")))?;
        if f.encrypted() {
            return Err(Error::Encrypted);
        }
        if f.size() > cap as u64 {
            return Err(too_large(name, max, left < max));
        }
        // the header's size is not trusted: the read stops one byte past the cap
        let mut out = Vec::new();
        f.take(cap as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| Error::Corrupt(format!("{name}: {e}")))?;
        if out.len() > cap {
            return Err(too_large(name, max, left < max));
        }
        self.used += out.len();
        Ok(Some(out))
    }

    /// An entry that must exist.
    pub fn need(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        self.read(name)?
            .ok_or_else(|| Error::Corrupt(format!("the document has no {name}")))
    }

    /// The text of a small entry, when it exists and is UTF-8.
    pub fn small_text(&mut self, name: &str, max: usize) -> Option<String> {
        let b = self.read_max(name, max).ok()??;
        String::from_utf8(b).ok()
    }

    /// The relationships of `part` (from `dir/_rels/file.rels`), by id.
    pub fn rels(&mut self, part: &str) -> HashMap<String, Rel> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let path = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let mut out = HashMap::new();
        let Ok(Some(bytes)) = self.read(&path) else {
            return out;
        };
        let Ok(root) = crate::xml::parse(&bytes, &self.limits) else {
            return out;
        };
        for r in root.descendants("Relationship") {
            let (Some(id), Some(target)) = (r.attr("Id"), r.attr("Target")) else {
                continue;
            };
            if target.len() > MAX_TARGET || id.len() > crate::budget::MAX_NAME {
                continue;
            }
            let external = r.attr("TargetMode") == Some("External");
            let kind = r
                .attr("Type")
                .and_then(|t| t.rsplit('/').next())
                .filter(|k| k.len() <= crate::budget::MAX_NAME)
                .unwrap_or("")
                .to_string();
            let target = if external {
                target.to_string()
            } else {
                resolve(dir, target)
            };
            out.insert(
                id.to_string(),
                Rel {
                    target,
                    external,
                    kind,
                },
            );
        }
        out
    }

    /// An embedded picture as the caller may hand it to a model, when the limits and the
    /// format allow it.
    pub fn image(&mut self, path: &str, alt: Option<String>) -> Option<Image> {
        if self.images >= self.limits.max_images {
            return None;
        }
        let media_type = image_type(path)?;
        let max = self.limits.max_image_bytes;
        let bytes = self.read_max(path, max).ok()??;
        if bytes.is_empty() {
            return None;
        }
        self.images += 1;
        Some(Image {
            media_type,
            bytes,
            alt,
        })
    }
}

fn too_large(name: &str, max: usize, total: bool) -> Error {
    if total {
        Error::TooLarge(format!(
            "the document's parts unpack to more than {} bytes",
            max
        ))
    } else {
        Error::TooLarge(format!(
            "{name} unpacks to more than {max} bytes; at most that many are read"
        ))
    }
}

/// The media type of a picture that a model can read, by file extension.
pub fn image_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The path in the package of a relationship target found in the part of directory
/// `dir`.
pub fn resolve(dir: &str, target: &str) -> String {
    let joined = if let Some(t) = target.strip_prefix('/') {
        t.to_string()
    } else if dir.is_empty() {
        target.to_string()
    } else {
        format!("{dir}/{target}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}
