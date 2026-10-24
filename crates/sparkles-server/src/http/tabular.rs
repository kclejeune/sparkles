//! CSV and TSV tables and mapped files in `POST /{ds}/upload` (specs C05 §7 and F19
//! §2.2), the Graph Store protocol (F19 §2.3) and stored mapping runs. A multipart upload
//! may carry a mapping in a part named `mapping`: CSVW metadata, an R2RML, RML or
//! YARRRML mapping, or a CONSTRUCT template, and a CONSTRUCT query in a part named
//! `template`. `mapping=NAME` names a stored mapping instead. The `base` and `key`
//! parameters set the default mapping's namespace and key, `csvwMode` and `csvwCheck`
//! choose CSVW's standard mode and key checks, and `lookup=true` lets a template read
//! the dataset. Each table, and the files an R2RML, RML or YARRRML mapping reads, are
//! converted into N-Triples or N-Quads files next to the upload's other files before the
//! load.

use super::{ApiResult, BodyBudget, Params, err, err_code};
use crate::state::AppState;
use axum::http::StatusCode;
use serde_json::{Value as J, json};
use sparkles::guard::WriteOptions;
use sparkles::mapping::{self as engine, MappingKind};
use sparkles::tabular::{self, Mapping, Options};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The largest `mapping` or `template` part.
const MAX_PART: usize = 1 << 20;

/// The mapping parts of a multipart upload, or a stored mapping.
#[derive(Default)]
pub(super) struct Mappings {
    pub mapping: Option<String>,
    pub template: Option<String>,
    /// the kind of `mapping`, when a stored mapping or `mappingKind` says it
    pub kind: Option<MappingKind>,
    /// the stored mapping's name and version (`mapping=NAME`)
    pub stored: Option<(String, u64)>,
}

impl Mappings {
    /// Take `mapping=NAME` (and `mappingVersion`) from the parameters: the stored
    /// mapping's text and kind, in place of a part. `mappingKind` names the kind of a
    /// part whose content does not tell.
    pub fn resolve(&mut self, params: &Params, ds: &crate::state::Dataset) -> ApiResult<()> {
        if let Some(k) = params.get("mappingKind") {
            self.kind = Some(MappingKind::parse(k).ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("mappingKind {k:?}: use rml, yarrrml, csvw or template"),
                )
            })?);
        }
        let Some(name) = params.get("mapping") else {
            return Ok(());
        };
        if self.mapping.is_some() {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "give a mapping part or mapping=NAME, not both",
            ));
        }
        let version = params
            .get("mappingVersion")
            .map(|v| {
                v.trim_start_matches('v').parse::<u64>().map_err(|_| {
                    err(
                        StatusCode::BAD_REQUEST,
                        "mappingVersion must be a version number",
                    )
                })
            })
            .transpose()?;
        let s = ds.dataset.mappings().get(name, version).ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                format!("no stored mapping '{name}' in /{}", ds.name),
            )
        })?;
        let kind = s.definition.kind()?;
        self.stored = Some((name.to_string(), s.version.version));
        self.kind = Some(kind);
        self.mapping = Some(s.definition.text);
        Ok(())
    }

    /// The kind of `mapping`: the one given, or its content's, with CSVW for a text
    /// whose content does not tell, as before other kinds existed.
    fn mapping_kind(&self) -> Option<MappingKind> {
        let text = self.mapping.as_deref()?;
        Some(
            self.kind
                .or_else(|| engine::detect(text))
                .unwrap_or(MappingKind::Csvw),
        )
    }
}

/// What a conversion may read and how: the caller's write options and admitted grant,
/// a snapshot for template lookups, and the one graph of a Graph Store request.
pub(super) struct Ctx<'a> {
    pub st: &'a AppState,
    pub wopts: &'a WriteOptions,
    pub grant: Option<&'a Arc<sparkles::sparql::pool::PoolGrant>>,
    pub lookup: Option<tabular::template::Lookup>,
    pub only_graph: Option<oxrdf::NamedNode>,
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
pub(super) fn client_name(p: &Path) -> String {
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

/// Convert the tables among `files`, and the files an R2RML, RML or YARRRML mapping
/// reads, in place, into N-Triples and N-Quads files. Returns a report per table and per
/// mapping run, or an empty list when there is nothing to convert.
pub(super) fn convert(
    files: &mut Vec<PathBuf>,
    m: &Mappings,
    params: &Params,
    cx: &Ctx<'_>,
) -> ApiResult<Vec<J>> {
    let st = cx.st;
    let wopts = cx.wopts;
    let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
    let kind = m.mapping_kind();
    let standard = match params.get("csvwMode") {
        None | Some("minimal") => false,
        Some("standard") => true,
        Some(v) => return Err(bad(format!("csvwMode {v:?}: use minimal or standard"))),
    };
    let keys = match params.get("csvwCheck") {
        None => standard,
        Some("keys") => true,
        Some("none") => false,
        Some(v) => return Err(bad(format!("csvwCheck {v:?}: use keys or none"))),
    };
    if (standard || params.get("csvwCheck") == Some("keys")) && kind != Some(MappingKind::Csvw) {
        return Err(bad(
            "csvwMode and csvwCheck apply to a CSVW metadata mapping".into(),
        ));
    }
    if cx.lookup.is_some() && kind != Some(MappingKind::Template) && m.template.is_none() {
        return Err(bad("lookup applies to a CONSTRUCT template".into()));
    }
    let (cancel, deadline) = (wopts.cancel.clone(), wopts.deadline);
    let check = move || {
        WriteOptions {
            cancel: cancel.clone(),
            deadline,
            ..Default::default()
        }
        .check()
    };
    if let Some(k @ (MappingKind::Rml | MappingKind::Yarrrml)) = kind {
        if m.template.is_some() || params.get("key").is_some() {
            return Err(bad(
                "template and key do not apply to an R2RML, RML or YARRRML mapping".into(),
            ));
        }
        let text = m.mapping.as_deref().expect("a mapping");
        let report = run_engine(files, text, k, m, params, cx, Arc::new(check))?;
        return Ok(vec![report]);
    }
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
    let (metadata, template_text) = match (kind, &m.mapping) {
        (Some(MappingKind::Template), Some(t)) => {
            if m.template.is_some() {
                return Err(bad(
                    "the mapping is a CONSTRUCT template; give one template".into(),
                ));
            }
            (None, Some(t.clone()))
        }
        (_, Some(text)) => (
            Some(Arc::new(
                tabular::csvw::parse(text, None).map_err(|e| bad(format!("mapping: {e}")))?,
            )),
            m.template.clone(),
        ),
        (_, None) => (None, m.template.clone()),
    };
    if key.is_some() && (metadata.is_some() || template_text.is_some()) {
        return Err(bad(
            "key applies to the default mapping only, not with a mapping or template".into(),
        ));
    }
    let mapping = match (&template_text, metadata) {
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
    let check: tabular::Check = Arc::new(check);
    let options = |i: usize, f: &Path, kind: tabular::TabularKind| {
        let name = client_name(f);
        let mut o = Options::new(mapping.clone(), name);
        o.base = base.clone();
        o.tsv = kind == tabular::TabularKind::Tsv;
        o.part = i;
        o.standard = standard;
        o.limits.max_output_bytes = st.limits.max_decompressed_bytes;
        o.query.max_memory_bytes = st.limits.query_memory_bytes;
        o.query.max_rows_produced = st.limits.max_rows_produced;
        o.query.max_rows = Some(st.limits.max_rows);
        // the upload's admitted grant covers its templates' CONSTRUCT queries
        o.query.grant = cx.grant.cloned();
        o.check = Some(check.clone());
        o.lookup = cx.lookup.clone();
        o
    };
    // the tables to convert, in the order of their foreign keys when keys are checked
    let mut order: Vec<usize> = (0..files.len())
        .filter(|i| tabular::tabular_kind(&files[*i]).is_some())
        .collect();
    let mut registry = None;
    if keys {
        let Mapping::Csvw(meta) = &mapping else {
            unreachable!("keys need CSVW metadata, checked above")
        };
        let mut indexed = Vec::new();
        for &i in &order {
            let k = tabular::tabular_kind(&files[i]).expect("a table");
            let t = tabular::table_index(meta, &options(i, &files[i], k))
                .map_err(|e| bad(e.to_string()))?;
            indexed.push((t, i));
        }
        let tables: Vec<usize> = indexed.iter().map(|(t, _)| *t).collect();
        let by_key = tabular::key_order(meta, &tables).map_err(|e| bad(e.to_string()))?;
        order = by_key
            .into_iter()
            .filter_map(|t| indexed.iter().find(|(x, _)| *x == t).map(|(_, i)| *i))
            .collect();
        registry = Some(Arc::new(tabular::KeyRegistry::new(
            st.limits.query_memory_bytes,
        )));
    }
    let mut reports = Vec::new();
    for i in order {
        let f = &mut files[i];
        let kind = tabular::tabular_kind(f).expect("a table");
        let mut o = options(i, f, kind);
        o.keys = registry.clone();
        let name = o.name.clone();
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
        let mut r = json!({
            "file": name,
            "rows": stats.rows,
            "triples": stats.triples,
            "warnings": stats.warnings,
        });
        if let Some(c) = stats.lookup_commit {
            r["lookupCommit"] = c.into();
        }
        if let Some((n, v)) = &m.stored {
            r["mapping"] = json!({ "name": n, "version": v });
        }
        reports.push(r);
        *f = out;
    }
    Ok(reports)
}

/// Run an R2RML, RML or YARRRML mapping over the uploaded files that its logical
/// sources name, and the server's connections. The files it reads are replaced by one
/// N-Quads file of its output, and the others stay to be loaded as they are.
fn run_engine(
    files: &mut Vec<PathBuf>,
    text: &str,
    kind: MappingKind,
    m: &Mappings,
    params: &Params,
    cx: &Ctx<'_>,
    check: engine::Check,
) -> ApiResult<J> {
    let st = cx.st;
    let mapping = engine::parse(text, kind, None)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("mapping: {e}")))?;
    let last = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
    let given: Vec<(String, PathBuf)> = files.iter().map(|f| (client_name(f), f.clone())).collect();
    let mut used = Vec::new();
    for s in &mapping.sources {
        if let Some(file) = s.kind.file() {
            let want = last(file);
            match given.iter().find(|(n, _)| *n == want || *n == file) {
                Some((_, p)) => used.push(p.clone()),
                None => {
                    return Err(err_code(
                        StatusCode::BAD_REQUEST,
                        "mapping-source-missing",
                        format!(
                            "the mapping reads the file {file:?}, and the upload has no file part of that name"
                        ),
                    ));
                }
            }
        }
    }
    let inputs = engine::Inputs {
        files: given,
        dir: None,
        connections: crate::settings::connections::effective(st),
    };
    let opts = engine::RunOptions {
        check: Some(check),
        max_decompressed: st.limits.max_decompressed_bytes,
        max_json_bytes: engine::DEFAULT_MAX_JSON_BYTES,
        memory_budget: st.limits.query_memory_bytes,
        base: params.get("base").map(str::to_string),
        max_output_bytes: st.limits.max_decompressed_bytes,
        only_graph: cx.only_graph.clone(),
    };
    let dir = files
        .first()
        .and_then(|f| f.parent())
        .map(Path::to_path_buf);
    let (path, stats) =
        engine::to_nquads_file(&mapping, &inputs, &opts, dir.as_deref()).map_err(mapping_error)?;
    let kept = path
        .keep()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    files.retain(|f| !used.contains(f));
    // a metadata file of a CSV source is read with it, not loaded
    files.retain(|f| !client_name(f).ends_with("-metadata.json"));
    files.push(kept);
    let mut r = json!({
        "kind": kind.name(),
        "rows": stats.rows,
        "quads": stats.quads,
        "warnings": stats.warnings,
    });
    if let Some((n, v)) = &m.stored {
        r["mapping"] = json!({ "name": n, "version": v });
    }
    Ok(r)
}

/// The engine's errors with the codes of spec F19: a missing source and a graph map
/// that a Graph Store request cannot honour.
pub(super) fn mapping_error(e: sparkles::Error) -> super::ApiError {
    if let sparkles::Error::Invalid(m) = &e {
        for code in ["mapping-source-missing", "mapping-graph-conflict"] {
            if m.contains(code) {
                let text = m.replacen(&format!("{code}: "), "", 1);
                return err_code(StatusCode::BAD_REQUEST, code, text);
            }
        }
    }
    e.into()
}
