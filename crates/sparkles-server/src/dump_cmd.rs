//! `sparkles dump`: write a database, or a dataset on a server, in any RDF syntax that
//! Sparkles writes, streaming from one snapshot.
//!
//! N-Quads of the whole dataset take the store's own dump path. Every other output goes
//! through the serializer of `sparkles convert` ([`Output`]), which declares the
//! dataset's prefixes in Turtle, TriG and RDF/XML, and writes named graphs into the
//! default graph with `--merge`.

use crate::CompressArgs;
use crate::tools::convert::{Output, Syntax};
use anyhow::{Context, Result, bail};
use oxrdfio::RdfFormat;
use sparkles::codec::{Codec, FinishWrite};
use sparkles::id::Id;
use sparkles::store::{Store, StoreOptions};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct DumpArgs {
    /// Database directory
    #[arg(long, required_unless_present = "server", conflicts_with = "server")]
    loc: Option<PathBuf>,
    /// A past state: N, commit:N, time:<RFC 3339>, snapshot:NAME
    #[arg(long)]
    at: Option<String>,
    /// Write to this file instead of stdout (its extension picks the syntax and the
    /// compression)
    #[arg(long)]
    out: Option<PathBuf>,
    /// Output syntax: nq, trig, nt, ttl, jsonld, rdfxml, trix, rt (RDF Thrift), rpb (RDF
    /// Protobuf), rj (RDF/JSON), or a media type (default: from --out's extension, else
    /// N-Quads). A triple syntax writes the default graph, or every graph with --merge.
    #[arg(long, value_name = "LANG")]
    format: Option<String>,
    /// Write only this graph (repeatable): an IRI, or `default` for the default graph
    #[arg(long, value_name = "IRI")]
    graph: Vec<String>,
    /// Write the quads of named graphs into the default graph
    #[arg(long)]
    merge: bool,
    #[command(flatten)]
    compress: CompressArgs,
    /// A server to export from instead of a local database (with --dataset)
    #[arg(long, env = "SPARKLES_SERVER")]
    server: Option<String>,
    /// The dataset on --server
    #[arg(long, requires = "server")]
    dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    insecure_http: bool,
}

/// A graph that `--graph` names.
#[derive(Clone, Debug, PartialEq)]
enum GraphSel {
    Default,
    Named(String),
}

fn graph_sel(s: &str) -> Result<GraphSel> {
    Ok(match s {
        "default" | sparkles::sparql::ctx::DEFAULT_GRAPH_IRI => GraphSel::Default,
        iri => {
            oxiri::Iri::parse(iri).map_err(|e| anyhow::anyhow!("--graph {iri}: {e}"))?;
            GraphSel::Named(iri.to_string())
        }
    })
}

/// What a dump writes.
struct Plan {
    syntax: Syntax,
    graphs: Vec<GraphSel>,
    merge: bool,
}

impl Plan {
    fn new(a: &DumpArgs) -> Result<Plan> {
        let syntax = match (&a.format, &a.out) {
            (Some(f), _) => {
                Syntax::named(f).with_context(|| format!("unknown output syntax '{f}'"))?
            }
            (None, Some(p)) => Syntax::of_path(p).unwrap_or(Syntax::Rdf(RdfFormat::NQuads)),
            (None, None) => Syntax::Rdf(RdfFormat::NQuads),
        };
        let mut graphs: Vec<GraphSel> = Vec::new();
        for g in &a.graph {
            let g = graph_sel(g)?;
            if !graphs.contains(&g) {
                graphs.push(g);
            }
        }
        // the graphs named for a triple syntax are written as one graph
        let merge = a.merge || (!syntax.quads() && !graphs.is_empty());
        Ok(Plan {
            syntax,
            graphs,
            merge,
        })
    }

    /// The whole dataset as N-Quads, which the store writes itself.
    fn plain_nquads(&self) -> bool {
        self.syntax == Syntax::Rdf(RdfFormat::NQuads) && self.graphs.is_empty() && !self.merge
    }
}

pub fn run(a: DumpArgs, opts: StoreOptions) -> Result<()> {
    let plan = Plan::new(&a)?;
    let codec = a.compress.codec(
        a.out
            .as_deref()
            .and_then(Codec::from_extension)
            .unwrap_or_default(),
    )?;
    let sink: Box<dyn std::io::Write> = match &a.out {
        Some(p) => {
            Box::new(std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?)
        }
        None => Box::new(std::io::stdout().lock()),
    };
    let w = codec.writer(
        std::io::BufWriter::with_capacity(1 << 16, sink),
        a.compress.level(),
        a.compress.threads(),
    )?;
    match &a.loc {
        Some(loc) => local(loc, &a, &plan, w, opts),
        None => remote(&a, &plan, w),
    }
}

fn local(
    loc: &std::path::Path,
    a: &DumpArgs,
    plan: &Plan,
    mut w: Box<dyn FinishWrite>,
    opts: StoreOptions,
) -> Result<()> {
    let store = Store::open(loc, opts)?;
    let at =
        a.at.as_deref()
            .map(str::parse::<sparkles::history::At>)
            .transpose()?;
    if plan.plain_nquads() {
        match &at {
            Some(at) => {
                let r = store.resolve(at)?;
                eprintln!("at commit {} ({})", r.commit.seq, r.commit.timestamp());
                store.dump_nquads_at(at, &mut w)?;
            }
            None => {
                store.dump_nquads(&mut w)?;
            }
        }
        w.finish()?;
        return Ok(());
    }
    let snap = match &at {
        Some(at) => {
            let (snap, r) = store.snapshot_at(at, &Default::default())?;
            eprintln!("at commit {} ({})", r.commit.seq, r.commit.timestamp());
            snap
        }
        None => store.snapshot(),
    };
    // a dump reads every block once: keep the blocks queries use cached
    let snap = snap.without_cache_fill();
    let mut out = Output::new(plan.syntax, w, plan.merge);
    out.start(store.prefixes());
    let mut failed: Option<anyhow::Error> = None;
    let mut write = |k: &[Id; 4]| -> sparkles::Result<()> {
        if let Some(q) = snap.quad_to_terms(k)
            && let Err(e) = out.write(q)
        {
            failed = Some(e);
            return Err(sparkles::Error::invalid("the output failed"));
        }
        Ok(())
    };
    let r = if plan.graphs.is_empty() {
        snap.for_each_quad(&mut write)
    } else {
        let mut r = Ok(());
        for g in &plan.graphs {
            let id = match g {
                GraphSel::Default => Some(Id::DEFAULT_GRAPH),
                GraphSel::Named(iri) => snap.lookup_iri(iri),
            };
            let Some(id) = id else {
                if let GraphSel::Named(iri) = g {
                    eprintln!("warning: no graph <{iri}>");
                }
                continue;
            };
            r = snap.for_each_quad_in(&[id.0], &mut write);
            if r.is_err() {
                break;
            }
        }
        r
    };
    if let Some(e) = failed {
        return Err(e);
    }
    r?;
    out.finish()
}

#[cfg(feature = "auth")]
fn remote(a: &DumpArgs, plan: &Plan, mut w: Box<dyn FinishWrite>) -> Result<()> {
    use crate::remote::{JsonBody, Remote};
    let Some(ds) = a.dataset.as_deref() else {
        bail!("--dataset NAME is required with --server");
    };
    let r = Remote::open(a.server.as_deref(), a.insecure_http)?;
    let path = format!("/{ds}/data");
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(at) = &a.at {
        query.push(("at", at.clone()));
    }
    // The export endpoint writes the whole dataset in a quad syntax, and one graph (the
    // default graph without --graph) in a triple syntax. Anything else is read as
    // N-Quads and written here.
    let direct = if plan.syntax.quads() {
        plan.graphs.is_empty() && !plan.merge
    } else {
        match plan.graphs.as_slice() {
            [] if !plan.merge => {
                query.push(("default", String::new()));
                true
            }
            [GraphSel::Default] => {
                query.push(("default", String::new()));
                true
            }
            [GraphSel::Named(iri)] => {
                query.push(("graph", iri.clone()));
                true
            }
            _ => false,
        }
    };
    let media = plan.syntax.media_type();
    // N3 has no media type the server offers
    let media = media.split(';').next().unwrap_or(media).trim();
    if direct && media != "text/plain" {
        let mut resp = r.check(
            r.req(reqwest::Method::GET, &with_query(&path, &query))
                .header("accept", media)
                .send(),
            Some(ds),
        )?;
        let got = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if got != media.to_ascii_lowercase() {
            bail!("the server answered {got} instead of {media}");
        }
        std::io::copy(&mut resp, &mut w).context("writing the output")?;
        w.finish()?;
        return Ok(());
    }
    let prefixes = r
        .check(
            r.req(reqwest::Method::GET, &format!("/{ds}/prefixes"))
                .send(),
            Some(ds),
        )?
        .json_value()?;
    let prefixes: Vec<(String, String)> = prefixes["prefixes"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let resp = r.check(
        r.req(reqwest::Method::GET, &with_query(&path, &query))
            .header("accept", "application/n-quads")
            .send(),
        Some(ds),
    )?;
    let mut out = Output::new(plan.syntax, w, plan.merge);
    out.start(prefixes);
    let wanted = |g: &oxrdf::GraphName| {
        plan.graphs.is_empty()
            || plan.graphs.iter().any(|s| match (s, g) {
                (GraphSel::Default, oxrdf::GraphName::DefaultGraph) => true,
                (GraphSel::Named(iri), oxrdf::GraphName::NamedNode(n)) => n.as_str() == iri,
                _ => false,
            })
    };
    let reader = std::io::BufReader::with_capacity(1 << 16, resp);
    for q in oxrdfio::RdfParser::from_format(RdfFormat::NQuads).for_reader(reader) {
        let q = q.context("reading the server's N-Quads")?;
        if wanted(&q.graph_name) {
            out.write(q)?;
        }
    }
    out.finish()
}

/// `path?k=v&…`, the values percent-encoded (an empty value: the key alone).
#[cfg(feature = "auth")]
fn with_query(path: &str, query: &[(&str, String)]) -> String {
    let mut out = path.to_string();
    for (i, (k, v)) in query.iter().enumerate() {
        out.push(if i == 0 { '?' } else { '&' });
        out.push_str(k);
        if !v.is_empty() {
            out.push('=');
            out.extend(percent_encoding::utf8_percent_encode(
                v,
                percent_encoding::NON_ALPHANUMERIC,
            ));
        }
    }
    out
}

#[cfg(not(feature = "auth"))]
fn remote(_: &DumpArgs, _: &Plan, _: Box<dyn FinishWrite>) -> Result<()> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::jena_formats::JenaFormat;
    use clap::Parser;

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        dump: DumpArgs,
    }

    fn args(a: &[&str]) -> DumpArgs {
        Cli::try_parse_from(std::iter::once("dump").chain(a.iter().copied()))
            .unwrap()
            .dump
    }

    fn db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("db");
        let store = Store::open(&loc, StoreOptions::default()).unwrap();
        store.set_prefix("ex", "http://e/").unwrap();
        sparkles::sparql::update::update(
            &store,
            "INSERT DATA { <http://e/a> <http://e/p> 1 . \
             GRAPH <http://e/g> { <http://e/b> <http://e/p> 2 } \
             GRAPH <http://e/h> { <http://e/c> <http://e/p> 3 } }",
            &Default::default(),
        )
        .unwrap();
        drop(store);
        (dir, loc)
    }

    fn dump(loc: &std::path::Path, extra: &[&str], out: &str) -> String {
        let dir = loc.parent().unwrap();
        let file = dir.join(out);
        let mut a = vec![
            "--loc",
            loc.to_str().unwrap(),
            "--out",
            file.to_str().unwrap(),
        ];
        a.extend_from_slice(extra);
        run(args(&a), StoreOptions::default()).unwrap();
        let bytes = std::fs::read(&file).unwrap();
        let mut r = Codec::from_extension(&file)
            .unwrap_or_default()
            .reader(std::io::Cursor::new(bytes), None)
            .unwrap();
        let mut s = Vec::new();
        std::io::Read::read_to_end(&mut r, &mut s).unwrap();
        String::from_utf8_lossy(&s).into_owned()
    }

    #[test]
    fn formats_from_the_flag_and_the_extension() {
        let a = args(&["--loc", "x", "--out", "d.ttl.gz"]);
        assert_eq!(
            Plan::new(&a).unwrap().syntax,
            Syntax::Rdf(RdfFormat::Turtle)
        );
        let a = args(&["--loc", "x", "--out", "d.trix", "--format", "rt"]);
        assert_eq!(
            Plan::new(&a).unwrap().syntax,
            Syntax::Jena(JenaFormat::Thrift)
        );
        let a = args(&["--loc", "x", "--out", "d.backup"]);
        assert!(Plan::new(&a).unwrap().plain_nquads());
        let a = args(&["--loc", "x", "--format", "nt", "--graph", "http://e/g"]);
        assert!(
            Plan::new(&a).unwrap().merge,
            "a named graph in a triple syntax"
        );
        assert!(Plan::new(&args(&["--loc", "x", "--format", "nope"])).is_err());
    }

    #[test]
    fn dumps_in_each_syntax() {
        let (_dir, loc) = db();
        let nq = dump(&loc, &[], "all.nq.zst");
        assert_eq!(nq.lines().count(), 3, "{nq}");
        let ttl = dump(&loc, &[], "d.ttl");
        assert!(ttl.contains("@prefix ex: <http://e/>"), "{ttl}");
        assert!(ttl.contains("ex:a ex:p 1"), "{ttl}");
        assert!(!ttl.contains("ex:b"), "named graphs are left out: {ttl}");
        let merged = dump(&loc, &["--merge"], "m.nt.gz");
        assert_eq!(merged.lines().count(), 3, "{merged}");
        let g = dump(&loc, &["--graph", "http://e/g"], "g.nt");
        assert_eq!(
            g.trim(),
            "<http://e/b> <http://e/p> \"2\"^^<http://www.w3.org/2001/XMLSchema#integer> ."
        );
        let two = dump(
            &loc,
            &["--graph", "http://e/h", "--graph", "default"],
            "two.nq",
        );
        assert_eq!(two.lines().count(), 2, "{two}");
        assert!(two.contains("<http://e/h>"), "{two}");
        let trig = dump(&loc, &[], "d.trig");
        assert!(trig.contains("ex:g {"), "{trig}");
        let xml = dump(&loc, &[], "d.rdf");
        assert!(xml.contains("xmlns:ex=\"http://e/\""), "{xml}");
        let trix = dump(&loc, &[], "d.trix");
        assert_eq!(trix.matches("<graph>").count(), 3, "{trix}");
        let rj = dump(&loc, &[], "d.rj");
        assert!(rj.contains("\"http://e/a\""), "{rj}");
        let jsonld = dump(&loc, &[], "d.jsonld");
        assert!(jsonld.contains("http://e/g"), "{jsonld}");
        // the binary syntaxes read back to the same quads
        for ext in ["rt", "rpb"] {
            let file = loc.parent().unwrap().join(format!("d.{ext}"));
            dump(&loc, &[], &format!("d.{ext}"));
            let j = JenaFormat::from_path(&file).unwrap();
            let mut nq = Vec::new();
            crate::http::jena_formats::transcode_with_base(
                j,
                std::fs::File::open(&file).unwrap(),
                true,
                &mut nq,
                None,
            )
            .unwrap();
            assert_eq!(String::from_utf8(nq).unwrap().lines().count(), 3, "{ext}");
        }
    }
}
