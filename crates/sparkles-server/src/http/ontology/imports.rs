//! The import map and the import actions of spec U12's import resolution:
//! `GET` and `PATCH /$/ontology/{ds}/imports` read and change the dataset's import map,
//! `POST /$/ontology/{ds}/imports/catalog` loads an OASIS XML catalog with its files, and
//! `POST /$/ontology/{ds}/imports/fetch` fetches an import into a graph through the
//! outbound policy.
//!
//! The map is kept in the main dataset's `ontology-imports.json`, so every branch shares
//! it. Every write of the map holds the dataset's import-map lock, and a catalog upload
//! or a fetch holds it from before its commit until its map write ends.

use super::*;
use axum::http::HeaderValue;
use oxrdf::{BlankNode, GraphName, GraphNameRef, NamedNode, NamedOrBlankNode, Quad};
use serde::{Deserialize, Serialize};
use sparkles::handles::ontology::ImportTargets;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

/// The file in the main dataset's directory that keeps the map.
pub(crate) const FILE: &str = "ontology-imports.json";
/// The most entries a map holds.
pub(crate) const MAX_ENTRIES: usize = 4096;
/// The longest import IRI or graph name an entry takes, in bytes.
pub(crate) const MAX_IRI_BYTES: usize = 4096;
/// The largest catalog document.
const MAX_CATALOG_BYTES: usize = 1 << 20;
/// The most entries a catalog may have.
const MAX_CATALOG_ENTRIES: usize = 1000;
/// The deepest element nesting of a catalog.
const MAX_CATALOG_DEPTH: usize = 32;
/// What a fetch asks for.
const ACCEPT: &str = "text/turtle, application/rdf+xml;q=0.9, application/n-triples;q=0.8, \
     text/owl-functional;q=0.7, application/ld+json;q=0.6, */*;q=0.1";

/// Where an entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Source {
    Manual,
    Catalog,
    Fetch,
}

/// One entry of the map: the graph an import resolves to, and how the entry was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Entry {
    pub graph: String,
    pub source: Source,
    /// the catalog's path of the file loaded into the graph
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// the URL fetched
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// the branch and commit that the last load or fetch wrote
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// `sha256:` and the hex digest of the bytes loaded
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// when the entry's document was last fetched (RFC 3339)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched: Option<String>,
}

impl Entry {
    fn manual(graph: String) -> Entry {
        Entry {
            graph,
            source: Source::Manual,
            file: None,
            location: None,
            branch: None,
            commit: None,
            etag: None,
            digest: None,
            fetched: None,
        }
    }
}

/// The map as the file keeps it (without its version member).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MapFile {
    #[serde(default)]
    pub map: BTreeMap<String, Entry>,
}

/// Whether `s` may be an import IRI or a graph name of the map.
fn check_iri(what: &str, s: &str) -> Result<(), String> {
    if s.len() > MAX_IRI_BYTES {
        return Err(format!("{what} is longer than {MAX_IRI_BYTES} bytes"));
    }
    NamedNode::new(s).map_err(|e| format!("{what} <{s}> is not an IRI: {e}"))?;
    Ok(())
}

impl MapFile {
    pub fn check(&self) -> Result<(), String> {
        if self.map.len() > MAX_ENTRIES {
            return Err(format!(
                "the import map would hold {} entries, more than the {MAX_ENTRIES} allowed",
                self.map.len()
            ));
        }
        for (iri, e) in &self.map {
            check_iri("an import", iri)?;
            check_iri("a graph", &e.graph)?;
        }
        Ok(())
    }

    /// The entity tag of this revision of the map.
    pub fn etag(&self) -> String {
        crate::settings::etag_of(&serde_json::to_value(self).unwrap_or_default())
    }
}

/// The dataset whose directory keeps the map: the main dataset of a branch.
fn owner(ds: &Arc<Dataset>) -> Arc<Dataset> {
    ds.main().unwrap_or_else(|| ds.clone())
}

/// The dataset's map; empty when it has none.
pub(crate) fn read_map(st: &AppState, ds: &Arc<Dataset>) -> anyhow::Result<MapFile> {
    let ds = owner(ds);
    let Some(mut v) = crate::assist::read_file(st, &ds, FILE)? else {
        return Ok(MapFile::default());
    };
    if let (Some(f), Some(m)) = (sparkles::formats::settings_file(FILE), v.as_object_mut()) {
        m.remove(f.member);
    }
    let m: MapFile =
        serde_json::from_value(v).map_err(|e| anyhow::anyhow!("{FILE} of {}: {e}", ds.name))?;
    m.check()
        .map_err(|e| anyhow::anyhow!("{FILE} of {}: {e}", ds.name))?;
    Ok(m)
}

fn write_map(st: &AppState, ds: &Arc<Dataset>, m: &MapFile) -> anyhow::Result<()> {
    let ds = owner(ds);
    if m.map.is_empty() {
        crate::assist::remove_file(st, &ds, FILE)
    } else {
        crate::assist::write_file(st, &ds, FILE, &serde_json::to_value(m)?)
    }
}

/// The import-map lock of a dataset (by its main dataset's name).
fn lock(ds: &Arc<Dataset>) -> Arc<parking_lot::Mutex<()>> {
    static LOCKS: LazyLock<parking_lot::Mutex<HashMap<String, Arc<parking_lot::Mutex<()>>>>> =
        LazyLock::new(Default::default);
    LOCKS
        .lock()
        .entry(owner(ds).name.clone())
        .or_default()
        .clone()
}

/// Where the imports of a read of `ds` may resolve besides the graphs their IRIs name:
/// the map's entries and the graphs of the vocabulary resources in this dataset, by
/// namespace (and by namespace without its final `#` or `/`), in the order of the
/// resources' ids. A map or catalog that cannot be read is logged and left out.
pub(crate) fn targets(st: &AppState, ds: &Arc<Dataset>) -> ImportTargets {
    let mut t = ImportTargets::default();
    match read_map(st, ds) {
        Ok(m) => t.map = m.map.into_iter().map(|(k, e)| (k, e.graph)).collect(),
        Err(e) => tracing::warn!(dataset = %ds.name, "{e:#}"),
    }
    let name = owner(ds).name.clone();
    match st.resources.read() {
        Ok(c) => {
            let mut vocabularies: Vec<_> = c
                .resources
                .iter()
                .filter(|r| r.input.kind == crate::resources::Kind::Vocabulary)
                .collect();
            vocabularies.sort_by(|a, b| a.id.cmp(&b.id));
            for r in vocabularies {
                let graphs: Vec<&String> = r
                    .input
                    .graphs
                    .iter()
                    .filter(|l| l.dataset == name)
                    .map(|l| &l.graph)
                    .collect();
                if graphs.is_empty() {
                    continue;
                }
                for ns in &r.input.namespaces {
                    let mut keys = vec![ns.iri.as_str()];
                    if let Some(k) = ns
                        .iri
                        .strip_suffix('#')
                        .or_else(|| ns.iri.strip_suffix('/'))
                    {
                        keys.push(k);
                    }
                    for k in keys {
                        let v = t.resources.entry(k.to_string()).or_default();
                        for g in &graphs {
                            if !v.contains(g) {
                                v.push((*g).clone());
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(dataset = %ds.name, "the resource catalog: {}", crate::resources::message(&e))
        }
    }
    t
}

fn internal(e: anyhow::Error) -> ApiError {
    err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

fn entry_json(iri: &str, e: &Entry) -> J {
    let mut j = serde_json::to_value(e).unwrap_or_default();
    j["iri"] = json!(iri);
    j
}

/// The map as a caller sees it: an administrator every entry, anyone else the entries
/// whose graph the caller can read.
fn map_body(name: &str, m: &MapFile, visible: &dyn Fn(&str) -> bool, admin: bool) -> J {
    json!({
        "dataset": name,
        "revision": m.etag(),
        "admin": admin,
        "maxEntries": MAX_ENTRIES,
        "entries": m.map.iter()
            .filter(|(_, e)| admin || visible(&e.graph))
            .map(|(k, e)| entry_json(k, e))
            .collect::<Vec<_>>(),
    })
}

fn with_tag(mut r: Response, tag: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(tag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r
}

/// Whether the caller may see every entry, and whether a graph holds triples that the
/// caller's masked view of the branch shows (a graph whose triples are all hidden counts
/// as absent).
fn viewer(
    st: &AppState,
    ds: &Arc<Dataset>,
    p: &Principal,
    params: &Params,
) -> (bool, impl Fn(&str) -> bool + use<>) {
    let admin = p.can(&owner(ds).name, crate::auth::Level::Admin);
    let opts = CallerContext::for_caller(st, ds, p).options(params, Endpoint::Query);
    let snap = ds.dataset.snapshot();
    let access = opts.graphs.as_ref().map(|a| a.bound(&snap));
    let view = match &access {
        Some(a) => a.masked(&snap).ok(),
        None => Some(snap.clone()),
    };
    let visible = move |g: &str| match (&access, &view) {
        (None, _) => true,
        (Some(a), Some(v)) => sparkles_reasoner::inputs::GraphRef::Named(g.to_string())
            .id(v)
            .is_some_and(|id| a.readable_id(&snap, id)),
        (Some(_), None) => false,
    };
    (admin, visible)
}

/// `GET /$/ontology/{ds}/imports`: the import map with its revision as the entity tag.
pub(in crate::http) async fn get_map(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    blocking(move || {
        let m = read_map(&st, &ds).map_err(internal)?;
        let (admin, visible) = viewer(&st, &ds, &p, &params);
        let tag = m.etag();
        Ok(with_tag(
            Json(map_body(&ds.name, &m, &visible, admin)).into_response(),
            &tag,
        ))
    })
    .await
}

/// Refuse a write whose `If-Match` names another revision of the map.
fn check_revision(headers: &HeaderMap, m: &MapFile) -> ApiResult<()> {
    let Some(v) = headers.get(header::IF_MATCH) else {
        return Ok(());
    };
    let v = v.to_str().unwrap_or("").trim();
    let tag = m.etag();
    let ok = v == "*"
        || v.split(',').any(|t| {
            let t = t.trim();
            t.strip_prefix("W/").unwrap_or(t) == tag
        });
    if ok {
        Ok(())
    } else {
        Err(err_code(
            StatusCode::PRECONDITION_FAILED,
            "precondition-failed",
            "If-Match: the import map has changed",
        ))
    }
}

/// `PATCH /$/ontology/{ds}/imports`: `{"set": {iri: graph}, "remove": [iri]}` with
/// `admin`. Set entries are manual, and an `If-Match` names the map's revision.
pub(in crate::http) async fn patch_map(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let o = body_object(&bytes, &["set", "remove"])?;
    let mut set: Vec<(String, String)> = Vec::new();
    match o.get("set") {
        None | Some(J::Null) => {}
        Some(J::Object(m)) => {
            for (k, v) in m {
                let Some(g) = v.as_str() else {
                    return Err(bad(format!("set.{k}: the graph, as a string")));
                };
                set.push((k.clone(), g.to_string()));
            }
        }
        Some(_) => return Err(bad("set: an object from import IRI to graph")),
    }
    let remove: Vec<String> = match o.get("remove") {
        None | Some(J::Null) => Vec::new(),
        Some(J::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad("remove: a list of import IRIs"))
            })
            .collect::<ApiResult<_>>()?,
        Some(_) => return Err(bad("remove: a list of import IRIs")),
    };
    if set.is_empty() && remove.is_empty() {
        return Err(bad("name entries to set or remove"));
    }
    blocking(move || {
        let l = lock(&ds);
        let _held = l.lock();
        let mut m = read_map(&st, &ds).map_err(internal)?;
        check_revision(&headers, &m)?;
        for iri in &remove {
            m.map.remove(iri);
        }
        for (iri, g) in set {
            m.map.insert(iri, Entry::manual(g));
        }
        m.check().map_err(bad)?;
        write_map(&st, &ds, &m).map_err(internal)?;
        let (admin, visible) = viewer(&st, &ds, &p, &params);
        Ok(with_tag(
            Json(map_body(&ds.name, &m, &visible, admin)).into_response(),
            &m.etag(),
        ))
    })
    .await
}

// ------------------------------------------------------------------ catalogs ------

/// One `uri` entry of a catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogEntry {
    pub name: String,
    pub uri: String,
}

/// What a catalog holds: its `uri` entries, and the entries it skipped with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Catalog {
    pub entries: Vec<CatalogEntry>,
    pub skipped: Vec<(String, String)>,
}

/// Read an OASIS XML catalog. Nothing outside `bytes` is read: a document type or entity
/// declaration is refused, and so is nesting deeper than 32 elements.
pub(crate) fn parse_catalog(bytes: &[u8]) -> Result<Catalog, String> {
    use quick_xml::events::{BytesStart, Event};
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(format!(
            "the catalog is larger than {MAX_CATALOG_BYTES} bytes"
        ));
    }
    let mut reader = quick_xml::Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut out = Catalog::default();
    let mut buf = Vec::new();
    // per open element: whether it is under an xml:base
    let mut stack: Vec<bool> = Vec::new();
    let mut root = false;
    let attr = |e: &BytesStart<'_>, key: &[u8]| -> Result<Option<String>, String> {
        for a in e.attributes() {
            let a = a.map_err(|e| format!("the catalog is not well-formed XML: {e}"))?;
            if a.key.as_ref() == key {
                return a
                    .unescape_value()
                    .map(|v| Some(v.into_owned()))
                    .map_err(|e| format!("the catalog is not well-formed XML: {e}"));
            }
        }
        Ok(None)
    };
    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| format!("the catalog is not well-formed XML: {e}"))?;
        let (e, empty) = match &ev {
            Event::DocType(_) => {
                return Err("the catalog has a document type declaration, which is refused".into());
            }
            Event::Start(e) => (e.clone().into_owned(), false),
            Event::Empty(e) => (e.clone().into_owned(), true),
            Event::End(_) => {
                stack.pop();
                buf.clear();
                continue;
            }
            Event::Eof => break,
            _ => {
                buf.clear();
                continue;
            }
        };
        let local = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
        let based = stack.last().copied().unwrap_or(false) || attr(&e, b"xml:base")?.is_some();
        if stack.is_empty() {
            if root {
                return Err("the catalog has more than one root element".into());
            }
            if local != "catalog" {
                return Err(format!("the root element is <{local}>, not <catalog>"));
            }
            root = true;
        } else {
            match local.as_str() {
                "group" => {}
                "uri" => {
                    let name = attr(&e, b"name")?;
                    let uri = attr(&e, b"uri")?;
                    match (name, uri) {
                        _ if based => out.skipped.push((
                            name_of(&e),
                            "an entry under an xml:base is not supported".into(),
                        )),
                        (Some(name), Some(uri)) => {
                            if out.entries.len() >= MAX_CATALOG_ENTRIES {
                                return Err(format!(
                                    "the catalog has more than {MAX_CATALOG_ENTRIES} entries"
                                ));
                            }
                            out.entries.push(CatalogEntry { name, uri });
                        }
                        _ => out
                            .skipped
                            .push((name_of(&e), "a uri entry needs name and uri".into())),
                    }
                }
                other if stack.len() <= 2 => out
                    .skipped
                    .push((name_of(&e), format!("<{other}> entries are not supported"))),
                _ => {}
            }
        }
        if !empty {
            if stack.len() >= MAX_CATALOG_DEPTH {
                return Err(format!(
                    "the catalog nests elements deeper than {MAX_CATALOG_DEPTH} levels"
                ));
            }
            stack.push(based);
        }
        buf.clear();
    }
    if !root {
        return Err("the catalog has no <catalog> element".into());
    }
    Ok(out)
}

/// A short description of a catalog element for the skipped list.
fn name_of(e: &quick_xml::events::BytesStart<'_>) -> String {
    let local = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
    let key = e
        .attributes()
        .flatten()
        .find(|a| {
            matches!(
                a.key.as_ref(),
                b"name" | b"uriStartString" | b"catalog" | b"systemId" | b"publicId"
            )
        })
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
    match key {
        Some(k) => format!("{local} {k}"),
        None => local,
    }
}

/// A relative catalog path as the body's parts are keyed: without `./` steps and
/// leading slashes. `None` for a path that leaves its directory.
fn part_key(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => return None,
            s => out.push(s),
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

/// Whether a catalog `uri` is an absolute IRI rather than a relative path.
fn absolute(uri: &str) -> bool {
    uri.split_once(':').is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.len() > 1
    }) && NamedNode::new(uri).is_ok()
}

/// The syntax of a document from its media type, then its name, else RDF/XML (the
/// syntax of most `.owl` files).
fn syntax_of(content_type: Option<&str>, name: &str) -> sparkles::io::RdfSyntax {
    content_type
        .filter(|c| {
            !matches!(
                c.split(';').next().unwrap_or("").trim(),
                "" | "text/plain" | "application/octet-stream" | "application/xml" | "text/xml"
            )
        })
        .and_then(sparkles::io::RdfSyntax::from_name)
        .or_else(|| {
            let path = name.split(['?', '#']).next().unwrap_or(name);
            path.rsplit_once('.')
                .and_then(|(_, ext)| sparkles::io::RdfSyntax::from_extension(ext))
        })
        .unwrap_or(sparkles::io::RdfSyntax::Rdf(
            sparkles::io::RdfFormat::RdfXml,
        ))
}

/// Parse a document into the triples of graph `graph`, its blank nodes renamed with
/// `tag` so that two documents of one transaction never share one.
fn parse_document(
    bytes: &[u8],
    syntax: sparkles::io::RdfSyntax,
    base: Option<&str>,
    graph: &str,
    tag: &str,
) -> Result<Vec<Quad>, String> {
    let g = NamedNode::new(graph).map_err(|e| e.to_string())?;
    let mut src = sparkles::io::Source::from_bytes(bytes.to_vec(), syntax, Some(g.clone()));
    src.base = base.map(str::to_string);
    let (quads, _) = sparkles::io::parse_to_vec(&src).map_err(|e| e.to_string())?;
    let rename = |b: &BlankNode| BlankNode::new_unchecked(format!("{tag}{}", b.as_str()));
    Ok(quads
        .into_iter()
        .map(|q| Quad {
            subject: match q.subject {
                NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(rename(&b)),
                s => s,
            },
            predicate: q.predicate,
            object: match q.object {
                oxrdf::Term::BlankNode(b) => oxrdf::Term::BlankNode(rename(&b)),
                o => o,
            },
            graph_name: GraphName::NamedNode(g.clone()),
        })
        .collect())
}

fn has_blank(q: &Quad) -> bool {
    matches!(q.subject, NamedOrBlankNode::BlankNode(_))
        || matches!(q.object, oxrdf::Term::BlankNode(_))
}

/// Make graph `graph` hold exactly `quads` within `tx`. Without blank nodes on either
/// side only the differences are written, so an unchanged document changes nothing;
/// otherwise the graph is cleared and loaded again.
fn replace_graph(
    tx: &mut sparkles::Transaction<'_>,
    graph: &str,
    quads: &[Quad],
) -> sparkles::Result<()> {
    let g = NamedNode::new(graph).map_err(|e| Error::invalid(e.to_string()))?;
    let old = tx.find(Some(GraphNameRef::NamedNode(g.as_ref())), None, None, None)?;
    if old.iter().any(has_blank) || quads.iter().any(has_blank) {
        for q in &old {
            tx.remove(q.as_ref())?;
        }
        for q in quads {
            tx.insert(q.as_ref())?;
        }
        return Ok(());
    }
    let new: BTreeSet<String> = quads.iter().map(|q| q.to_string()).collect();
    let had: BTreeSet<String> = old.iter().map(|q| q.to_string()).collect();
    for q in &old {
        if !new.contains(&q.to_string()) {
            tx.remove(q.as_ref())?;
        }
    }
    for q in quads {
        if !had.contains(&q.to_string()) {
            tx.insert(q.as_ref())?;
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(bytes);
    let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

fn branch_name() -> String {
    branches::current().unwrap_or_else(|| sparkles::branch::MAIN.to_string())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The parts of a catalog upload: the catalog and the files by key.
struct Upload {
    catalog: Vec<u8>,
    files: BTreeMap<String, (Vec<u8>, Option<String>)>,
}

async fn read_upload(request: axum::extract::Request, st: &AppState) -> ApiResult<Upload> {
    use axum::extract::FromRequest;
    let mut budget = BodyBudget::new(&st.limits);
    let mp_err = |e: axum::extract::multipart::MultipartError| err(e.status(), e.body_text());
    let mut mp = Multipart::from_request(request, &())
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    let mut catalog = None;
    let mut files = BTreeMap::new();
    while let Some(mut field) = mp.next_field().await.map_err(mp_err)? {
        let name = field
            .name()
            .map(str::to_string)
            .filter(|n| !n.is_empty())
            .or_else(|| field.file_name().map(str::to_string))
            .ok_or_else(|| bad("a part has neither a name nor a file name"))?;
        let ct = field.content_type().map(str::to_string);
        let mut bytes = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(mp_err)? {
            budget.read(chunk.len())?;
            if name == "catalog" && bytes.len() + chunk.len() > MAX_CATALOG_BYTES {
                return Err(bad(format!(
                    "the catalog is larger than {MAX_CATALOG_BYTES} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        if name == "catalog" {
            if catalog.is_some() {
                return Err(bad("the body has more than one catalog part"));
            }
            catalog = Some(bytes);
        } else {
            let key = part_key(&name)
                .ok_or_else(|| bad(format!("part {name:?}: a path that leaves its directory")))?;
            if files.insert(key, (bytes, ct)).is_some() {
                return Err(bad(format!("the body has the file {name:?} twice")));
            }
        }
    }
    Ok(Upload {
        catalog: catalog.ok_or_else(|| bad("the body has no catalog part"))?,
        files,
    })
}

/// What a catalog upload does with each entry.
struct Planned {
    iri: String,
    graph: String,
    /// the file loaded into the graph, by its catalog path
    file: Option<String>,
}

fn plan_catalog(
    c: &Catalog,
    files: &BTreeMap<String, (Vec<u8>, Option<String>)>,
) -> (Vec<Planned>, Vec<(String, String)>) {
    let mut planned: Vec<Planned> = Vec::new();
    let mut skipped = c.skipped.clone();
    for e in &c.entries {
        let label = format!("uri {}", e.name);
        if let Err(m) = check_iri("the name", &e.name) {
            skipped.push((label, m));
            continue;
        }
        if planned.iter().any(|p| p.iri == e.name) {
            skipped.push((label, "the catalog names this import twice".into()));
            continue;
        }
        if absolute(&e.uri) {
            if let Err(m) = check_iri("the uri", &e.uri) {
                skipped.push((label, m));
                continue;
            }
            planned.push(Planned {
                iri: e.name.clone(),
                graph: e.uri.clone(),
                file: None,
            });
            continue;
        }
        match part_key(&e.uri) {
            Some(k) if files.contains_key(&k) => planned.push(Planned {
                iri: e.name.clone(),
                graph: e.name.clone(),
                file: Some(k),
            }),
            Some(k) => skipped.push((label, format!("the file {k} is not in the body"))),
            None => skipped.push((label, format!("the path {} leaves its directory", e.uri))),
        }
    }
    (planned, skipped)
}

/// The receipt-carrying error of a map write that failed after the commit.
fn map_failed(e: anyhow::Error, receipt: &sparkles::commit::Receipt, entries: Vec<J>) -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({
            "error": format!("the graphs were committed, but the import map was not written: {e:#}; PATCH the map with these entries to finish"),
            "code": "import-map-unwritten",
            "commit": receipt.commit.seq,
            "entries": entries,
        }),
    )
}

/// `POST /$/ontology/{ds}/imports/catalog`: load an OASIS XML catalog and its files.
pub(in crate::http) async fn catalog(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    history::reject_at(&params)?;
    if content_type(&headers) != "multipart/form-data" {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send the catalog and its files as multipart/form-data",
        ));
    }
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    if wopts.opts.precondition.is_none() {
        wopts.opts.precondition =
            conditional::write_precondition(&headers, ds.store.dataset_id(), &Target::Dataset);
    }
    let upload = read_upload(request, &st).await?;
    let cat = parse_catalog(&upload.catalog).map_err(bad)?;
    let (planned, skipped) = plan_catalog(&cat, &upload.files);
    if planned.is_empty() {
        return Err(bad(format!(
            "the catalog has no entry to load or map ({} skipped)",
            skipped.len()
        )));
    }
    // parse every file before anything changes
    let mut loads: Vec<(String, String, Vec<Quad>, String)> = Vec::new();
    for (i, pl) in planned.iter().enumerate() {
        if let Some(f) = &pl.file {
            let (bytes, ct) = &upload.files[f];
            let quads = parse_document(
                bytes,
                syntax_of(ct.as_deref(), f),
                None,
                &pl.graph,
                &format!("c{i}x"),
            )
            .map_err(|e| bad(format!("{f}: {e}")))?;
            loads.push((pl.graph.clone(), f.clone(), quads, sha256(bytes)));
        }
    }
    let (mut wopts, timeout) = wopts.start();
    if wopts.message.is_none() {
        wopts.message = Some("Load an XML catalog's imports".into());
    }
    blocking(move || {
        let l = lock(&ds);
        let _held = l.lock();
        let mut m = read_map(&st, &ds).map_err(internal)?;
        let branch = branch_name();
        let mut next = m.clone();
        for pl in &planned {
            let load = loads.iter().find(|x| x.0 == pl.graph && pl.file.is_some());
            next.map.insert(
                pl.iri.clone(),
                Entry {
                    graph: pl.graph.clone(),
                    source: Source::Catalog,
                    file: pl.file.clone(),
                    location: None,
                    branch: load.map(|_| branch.clone()),
                    commit: None,
                    etag: None,
                    digest: load.map(|x| x.3.clone()),
                    fetched: None,
                },
            );
        }
        next.check().map_err(bad)?;
        let (_, receipt) = ds
            .dataset
            .transaction_receipt_with(wopts, |tx| {
                for (graph, _, quads, _) in &loads {
                    replace_graph(tx, graph, quads)?;
                }
                Ok(())
            })
            .map_err(|e| write_error(e, false))?;
        for pl in &planned {
            if pl.file.is_some()
                && let Some(e) = next.map.get_mut(&pl.iri)
            {
                e.commit = Some(receipt.commit.seq);
            }
        }
        let entries: Vec<J> = planned
            .iter()
            .filter_map(|pl| next.map.get(&pl.iri).map(|e| entry_json(&pl.iri, e)))
            .collect();
        write_map(&st, &ds, &next).map_err(|e| map_failed(e, &receipt, entries.clone()))?;
        m = next;
        let body = json!({
            "loaded": loads.iter().map(|(g, f, q, _)| json!({"graph": g, "file": f, "triples": q.len()})).collect::<Vec<_>>(),
            "entries": entries,
            "skipped": skipped.iter().map(|(e, r)| json!({"entry": e, "reason": r})).collect::<Vec<_>>(),
            "changed": receipt.committed,
            "revision": m.etag(),
        });
        Ok(write_response(&ds, StatusCode::OK, Some(body), &receipt, wanted, false))
    })
    .await
    .map_err(|e| with_timeout(e, timeout))
}

/// `POST /$/ontology/{ds}/imports/fetch`: `{"iri", "location"?, "graph"?}`. Fetch the
/// import's document through the outbound policy into its graph, and record the entry.
pub(in crate::http) async fn fetch(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    history::reject_at(&params)?;
    let o = body_object(&bytes, &["iri", "location", "graph"])?;
    let text = |k: &str| -> ApiResult<Option<String>> {
        match o.get(k) {
            None | Some(J::Null) => Ok(None),
            Some(J::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(bad(format!("{k}: a string"))),
        }
    };
    let iri = text("iri")?.ok_or_else(|| bad("iri: the import to fetch"))?;
    check_iri("the import", &iri).map_err(bad)?;
    let (location, graph) = (text("location")?, text("graph")?);
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    if wopts.opts.precondition.is_none() {
        wopts.opts.precondition =
            conditional::write_precondition(&headers, ds.store.dataset_id(), &Target::Dataset);
    }
    let qopts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Update);
    let (mut wopts, timeout) = wopts.start();
    if wopts.message.is_none() {
        wopts.message = Some(format!("Fetch the import <{iri}>").into());
    }
    blocking(move || {
        let l = lock(&ds);
        let _held = l.lock();
        let mut m = read_map(&st, &ds).map_err(internal)?;
        let old = m.map.get(&iri).cloned();
        let location = location
            .or_else(|| old.as_ref().and_then(|e| e.location.clone()))
            .unwrap_or_else(|| iri.clone());
        if !(location.starts_with("http://") || location.starts_with("https://"))
            || NamedNode::new(&location).is_err()
        {
            return Err(bad(format!(
                "the location <{location}> is not an http or https URL"
            )));
        }
        let graph = graph
            .or_else(|| old.as_ref().map(|e| e.graph.clone()))
            .unwrap_or_else(|| iri.clone());
        check_iri("the graph", &graph).map_err(bad)?;
        let mut entry = Entry {
            graph: graph.clone(),
            source: Source::Fetch,
            file: None,
            location: Some(location.clone()),
            branch: Some(branch_name()),
            commit: None,
            etag: None,
            digest: None,
            fetched: Some(now()),
        };
        let mut next = m.clone();
        next.map.insert(iri.clone(), entry.clone());
        next.check().map_err(bad)?;
        let policy = &qopts.outbound;
        let budget = sparkles::outbound::RequestBudget::new(policy);
        let doc = sparkles::outbound::fetch_document(policy, &budget, &location, ACCEPT)
            .map_err(ApiError::from)?;
        let quads = parse_document(
            &doc.bytes,
            syntax_of(Some(&doc.content_type), &location),
            Some(&location),
            &graph,
            "f",
        )
        .map_err(|e| {
            err_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "import-unparsed",
                format!("<{location}> does not parse: {e}"),
            )
        })?;
        let (_, receipt) = ds
            .dataset
            .transaction_receipt_with(wopts, |tx| replace_graph(tx, &graph, &quads))
            .map_err(|e| write_error(e, false))?;
        entry.etag = doc.etag.clone();
        entry.digest = Some(sha256(&doc.bytes));
        entry.commit = Some(receipt.commit.seq);
        next.map.insert(iri.clone(), entry.clone());
        write_map(&st, &ds, &next)
            .map_err(|e| map_failed(e, &receipt, vec![entry_json(&iri, &entry)]))?;
        m = next;
        let body = json!({
            "entry": entry_json(&iri, &entry),
            "triples": quads.len(),
            "changed": receipt.committed,
            "revision": m.etag(),
        });
        Ok(write_response(
            &ds,
            StatusCode::OK,
            Some(body),
            &receipt,
            wanted,
            false,
        ))
    })
    .await
    .map_err(|e| with_timeout(e, timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs() {
        let c = parse_catalog(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<catalog prefer="public" xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
  <uri id="User Entered Import Resolution" name="http://ex.org/a" uri="a.owl"/>
  <group id="Folder Repository, directory=, recursive=true" prefer="public" xml:base="">
    <uri name="http://ex.org/b" uri="./sub/b.ttl"/>
  </group>
  <group prefer="public">
    <uri name="http://ex.org/c" uri="http://mirror.org/c"/>
    <rewriteURI uriStartString="http://ex.org/r/" rewritePrefix="r/"/>
  </group>
  <nextCatalog catalog="other.xml"/>
  <uri name="http://ex.org/d"/>
</catalog>"#,
        )
        .unwrap();
        assert_eq!(
            c.entries,
            [
                CatalogEntry {
                    name: "http://ex.org/a".into(),
                    uri: "a.owl".into()
                },
                CatalogEntry {
                    name: "http://ex.org/c".into(),
                    uri: "http://mirror.org/c".into()
                },
            ]
        );
        let reasons: Vec<&str> = c.skipped.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "uri http://ex.org/b",
                "rewriteURI http://ex.org/r/",
                "nextCatalog other.xml",
                "uri http://ex.org/d"
            ]
        );
    }

    #[test]
    fn unsafe_catalogs_are_refused() {
        let entity = br#"<?xml version="1.0"?>
<!DOCTYPE catalog [ <!ENTITY x SYSTEM "file:///etc/passwd"> ]>
<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog"><uri name="&x;" uri="a"/></catalog>"#;
        assert!(parse_catalog(entity).unwrap_err().contains("document type"));
        let undeclared = br#"<catalog><uri name="&x;" uri="a"/></catalog>"#;
        assert!(parse_catalog(undeclared).is_err());
        let deep = format!(
            "<catalog>{}{}</catalog>",
            "<group>".repeat(40),
            "</group>".repeat(40)
        );
        assert!(
            parse_catalog(deep.as_bytes())
                .unwrap_err()
                .contains("deeper")
        );
        assert!(
            parse_catalog(b"<other/>")
                .unwrap_err()
                .contains("not <catalog>")
        );
        assert!(parse_catalog(b"not xml <").is_err());
    }

    #[test]
    fn plans_and_paths() {
        assert_eq!(part_key("./sub//b.ttl").as_deref(), Some("sub/b.ttl"));
        assert_eq!(part_key("../etc/passwd"), None);
        assert!(absolute("http://ex.org/x") && absolute("urn:x:y"));
        assert!(!absolute("a.owl") && !absolute("C:/x.owl") && !absolute("sub/b.ttl"));
        let c = Catalog {
            entries: vec![
                CatalogEntry {
                    name: "http://ex.org/a".into(),
                    uri: "a.owl".into(),
                },
                CatalogEntry {
                    name: "http://ex.org/a".into(),
                    uri: "b.owl".into(),
                },
                CatalogEntry {
                    name: "http://ex.org/m".into(),
                    uri: "missing.owl".into(),
                },
                CatalogEntry {
                    name: "http://ex.org/c".into(),
                    uri: "http://mirror.org/c".into(),
                },
                CatalogEntry {
                    name: "not an iri".into(),
                    uri: "a.owl".into(),
                },
            ],
            skipped: Vec::new(),
        };
        let files = BTreeMap::from([("a.owl".to_string(), (Vec::new(), None))]);
        let (planned, skipped) = plan_catalog(&c, &files);
        let p: Vec<(&str, &str, Option<&str>)> = planned
            .iter()
            .map(|p| (p.iri.as_str(), p.graph.as_str(), p.file.as_deref()))
            .collect();
        assert_eq!(
            p,
            [
                ("http://ex.org/a", "http://ex.org/a", Some("a.owl")),
                ("http://ex.org/c", "http://mirror.org/c", None)
            ]
        );
        assert_eq!(skipped.len(), 3);
    }

    #[test]
    fn map_bounds_and_revisions() {
        let mut m = MapFile::default();
        let tag = m.etag();
        m.map
            .insert("http://ex.org/a".into(), Entry::manual("urn:g".into()));
        assert!(m.check().is_ok());
        assert_ne!(m.etag(), tag);
        m.map
            .insert("http://ex.org/b".into(), Entry::manual("not an iri".into()));
        assert!(m.check().is_err());
        m.map.remove("http://ex.org/b");
        for i in 0..MAX_ENTRIES {
            m.map
                .insert(format!("urn:i:{i}"), Entry::manual("urn:g".into()));
        }
        assert!(m.check().unwrap_err().contains("more than"));
        assert!(serde_json::from_value::<MapFile>(json!({"map": {}, "other": 1})).is_err());
        assert_eq!(
            syntax_of(Some("text/turtle; charset=utf-8"), "x"),
            sparkles::io::RdfSyntax::Rdf(sparkles::io::RdfFormat::Turtle)
        );
        assert_eq!(
            syntax_of(Some("application/octet-stream"), "http://x/o.owl"),
            sparkles::io::RdfSyntax::Rdf(sparkles::io::RdfFormat::RdfXml)
        );
    }
}
