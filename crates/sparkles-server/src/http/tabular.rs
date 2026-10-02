//! CSV and TSV tables in `POST /{ds}/upload` (spec C05 §7). A multipart upload may
//! carry a CSVW metadata document in a part named `mapping` and a CONSTRUCT query in a
//! part named `template`. The `base` and `key` parameters set the default mapping's
//! namespace and key. Each table is converted into an N-Triples file next to the
//! upload's other files before the load.

use super::{ApiResult, BodyBudget, Params, err};
use crate::state::AppState;
use axum::http::StatusCode;
use serde_json::{Value as J, json};
use sparkles::guard::WriteOptions;
use sparkles::tabular::{self, Mapping, Options};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The largest `mapping` or `template` part.
const MAX_PART: usize = 1 << 20;

/// The mapping parts of a multipart upload.
#[derive(Default)]
pub(super) struct Mappings {
    pub mapping: Option<String>,
    pub template: Option<String>,
}

/// Read a `mapping` or `template` part as text.
pub(super) async fn read_part(
    field: &mut axum::extract::multipart::Field<'_>,
    budget: &mut BodyBudget,
    what: &str,
) -> ApiResult<String> {
    let mut v = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| err(e.status(), e.body_text()))?
    {
        budget.read(chunk.len())?;
        v.extend_from_slice(&chunk);
        if v.len() > MAX_PART {
            return Err(err(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the {what} part is larger than 1 MiB"),
            ));
        }
    }
    String::from_utf8(v).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            format!("the {what} part is not UTF-8"),
        )
    })
}

/// The name a client gave an uploaded file (the spooled file is `{n}-{name}`).
fn client_name(p: &Path) -> String {
    let n = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match n.split_once('-') {
        Some((i, rest)) if i.bytes().all(|b| b.is_ascii_digit()) && !i.is_empty() => {
            rest.to_string()
        }
        _ => n,
    }
}

/// A file writer that keeps the server's free-disk reserve, checked every 64 MiB.
struct Reserve<W> {
    inner: W,
    dir: PathBuf,
    reserve: Option<u64>,
    since: u64,
}

const CHECK_EVERY: u64 = 64 << 20;

impl<W: Write> Write for Reserve<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(r) = self.reserve
            && (self.since == 0 || self.since >= CHECK_EVERY)
        {
            sparkles::disk::check_reserve(&self.dir, r, CHECK_EVERY, false)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            self.since = 0;
        }
        let n = self.inner.write(buf)?;
        self.since += n.max(1) as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Convert the tables among `files`, in place, into N-Triples files. Returns a report
/// per table, or an empty list when there is none.
pub(super) fn convert(
    files: &mut [PathBuf],
    m: &Mappings,
    params: &Params,
    st: &AppState,
    wopts: &WriteOptions,
) -> ApiResult<Vec<J>> {
    let tables = files
        .iter()
        .filter(|f| tabular::tabular_kind(f).is_some())
        .count();
    if tables == 0 {
        if m.mapping.is_some() || m.template.is_some() {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "a mapping or template was given, but no file named .csv or .tsv",
            ));
        }
        return Ok(Vec::new());
    }
    let base = params.get("base").map(str::to_string);
    let key = params.get("key").map(str::to_string);
    let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
    let metadata = match &m.mapping {
        Some(text) => Some(Arc::new(
            tabular::csvw::parse(text, None).map_err(|e| bad(format!("mapping: {e}")))?,
        )),
        None => None,
    };
    if key.is_some() && (metadata.is_some() || m.template.is_some()) {
        return Err(bad(
            "key applies to the default mapping only, not with a mapping or template".into(),
        ));
    }
    let mapping = match (&m.template, metadata) {
        (Some(t), metadata) => Mapping::Template {
            template: Arc::new(
                tabular::Template::parse(t, base.as_deref())
                    .map_err(|e| bad(format!("template: {e}")))?,
            ),
            metadata,
        },
        (None, Some(md)) => Mapping::Csvw(md),
        (None, None) => Mapping::Default { key },
    };
    let (cancel, deadline) = (wopts.cancel.clone(), wopts.deadline);
    let check: tabular::Check = Arc::new(move || {
        WriteOptions {
            cancel: cancel.clone(),
            deadline,
            ..Default::default()
        }
        .check()
    });
    let mut reports = Vec::new();
    for (i, f) in files.iter_mut().enumerate() {
        let Some(kind) = tabular::tabular_kind(f) else {
            continue;
        };
        let name = client_name(f);
        let mut o = Options::new(mapping.clone(), name.clone());
        o.base = base.clone();
        o.tsv = kind == tabular::TabularKind::Tsv;
        o.part = i;
        o.limits.max_output_bytes = st.limits.max_decompressed_bytes;
        o.query.max_memory_bytes = st.limits.query_memory_bytes;
        o.query.max_rows_produced = st.limits.max_rows_produced;
        o.query.max_rows = Some(st.limits.max_rows);
        o.check = Some(check.clone());
        let out = f.with_extension("nt");
        let io = |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        let input = tabular::open(f, st.limits.max_decompressed_bytes)?;
        let w = Reserve {
            inner: std::io::BufWriter::with_capacity(
                1 << 20,
                std::fs::File::create(&out).map_err(io)?,
            ),
            dir: out.parent().map(Path::to_path_buf).unwrap_or_default(),
            reserve: st.limits.min_free_disk_bytes,
            since: 0,
        };
        let stats = match tabular::write(input, &o, w, oxrdfio::RdfFormat::NTriples, None) {
            Err(sparkles::Error::Io(e)) if e.to_string().contains("not enough free disk space") => {
                return Err(sparkles::Error::StorageFull(e.to_string()).into());
            }
            r => r?,
        };
        reports.push(json!({
            "file": name,
            "rows": stats.rows,
            "triples": stats.triples,
            "warnings": stats.warnings,
        }));
        *f = out;
    }
    Ok(reports)
}
