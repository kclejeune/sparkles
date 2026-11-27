//! Stream registry replacements through the catalog's atomic rename boundary.
use super::{CONFIG_FILE, DetachedReasoning, Registry, RegistryEntry, SavedRecord};
use crate::Result;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;

fn replace<T>(
    dir: &Path,
    rows: impl IntoIterator<Item = T>,
    mut write_row: impl FnMut(T, &mut BufWriter<&mut File>) -> io::Result<()>,
) -> io::Result<()> {
    let path = dir.join(CONFIG_FILE);
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    sparkles_core::fsutil::replace_with(&path, &tmp, |file| {
        let mut out = BufWriter::new(file);
        write!(
            out,
            "{{\"format\":{},\"datasets\":[",
            crate::formats::CATALOG.writes
        )?;
        for (index, row) in rows.into_iter().enumerate() {
            if index != 0 {
                out.write_all(b",")?;
            }
            write_row(row, &mut out)?;
        }
        out.write_all(b"]}")?;
        out.flush()
    })
}

impl Registry<SavedRecord> {
    pub(super) fn replace(&self, dir: &Path) -> io::Result<()> {
        replace(dir, &self.datasets, |row, out| {
            out.write_all(b"{\"name\":")?;
            serde_json::to_writer(&mut *out, &row.name)?;
            out.write_all(b",\"type\":")?;
            serde_json::to_writer(&mut *out, &row.kind)?;
            if let Some(id) = row.id {
                write!(out, ",\"id\":\"{id}\"")?;
            }
            if let Some(revision) = row.revision {
                write!(out, ",\"revision\":\"{revision}\"")?;
            }
            out.write_all(b",\"reasoning\":")?;
            match &row.reasoning {
                Some(SavedRecord::Open(record)) => serde_json::to_writer(&mut *out, &record.value)?,
                Some(SavedRecord::Closed(DetachedReasoning::Current(record))) => {
                    serde_json::to_writer(&mut *out, &record.as_deref())?
                }
                Some(SavedRecord::Closed(DetachedReasoning::Registry(span))) => {
                    let mut source = span.file.lock();
                    source.seek(SeekFrom::Start(span.offset))?;
                    copy_exact(&mut *source, out, span.len)?;
                }
                None => out.write_all(b"null")?,
            }
            out.write_all(b"}")
        })
    }
}

fn copy_exact(input: &mut impl Read, output: &mut impl Write, len: u64) -> io::Result<()> {
    if io::copy(&mut input.take(len), output)? != len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// Bind one legacy identity in the durable registry. Unrelated rows come from
/// the file the catalog just validated, including their original reasoning bytes.
pub(super) fn bind_row(
    mut file: File,
    dir: &Path,
    index: usize,
    id: Uuid,
    rows: usize,
) -> Result<()> {
    use std::io::BufRead;
    // StreamDeserializer reports the exact length consumed, even for a scalar
    // whose terminating punctuation had to be peeked at.
    fn value<T: serde::de::DeserializeOwned>(input: &mut BufReader<File>) -> io::Result<(T, u64)> {
        let start = input.stream_position()?;
        let mut values = serde_json::Deserializer::from_reader(&mut *input).into_iter::<T>();
        let value = values.next().ok_or(io::ErrorKind::UnexpectedEof)??;
        let len = values.byte_offset() as u64;
        input.seek(SeekFrom::Start(start + len))?;
        Ok((value, len))
    }
    fn punctuation(input: &mut BufReader<File>, expected: u8) -> io::Result<()> {
        loop {
            let bytes = input.fill_buf()?;
            if bytes.is_empty() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let spaces = bytes.iter().take_while(|b| b.is_ascii_whitespace()).count();
            let next = bytes.get(spaces).copied();
            input.consume(spaces + usize::from(next.is_some()));
            if let Some(byte) = next {
                return if byte == expected {
                    Ok(())
                } else {
                    Err(io::Error::other("unexpected registry delimiter"))
                };
            }
        }
    }

    file.rewind()?;
    let mut input = BufReader::new(file);
    punctuation(&mut input, b'{')?;
    loop {
        let (field, _) = value::<String>(&mut input)?;
        punctuation(&mut input, b':')?;
        if field == "datasets" {
            break;
        }
        value::<serde::de::IgnoredAny>(&mut input)?;
        punctuation(&mut input, b',')?;
    }
    punctuation(&mut input, b'[')?;
    replace(dir, 0..rows, |row, out| {
        if row != 0 {
            punctuation(&mut input, b',')?;
        }
        if row == index {
            let (mut bound, _) = value::<RegistryEntry>(&mut input)?;
            bound.id = Some(id);
            serde_json::to_writer(out, &bound)?;
        } else {
            let start = input.stream_position()?;
            let (_, len) = value::<serde::de::IgnoredAny>(&mut input)?;
            input.seek(SeekFrom::Start(start))?;
            copy_exact(&mut input, out, len)?;
        }
        Ok(())
    })?;
    Ok(())
}
