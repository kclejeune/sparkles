//! Assembler bodies on `POST /$/datasets`: the part of a Fuseki service description
//! (`config.ttl`) that maps to a Sparkles dataset. A description names one
//! `fuseki:Service` with a `fuseki:name` and a `fuseki:dataset`. The dataset is TDB2 or
//! TDB1 (a persistent dataset) or an in-memory one (`ja:MemoryDataset`,
//! `ja:DatasetTxnMem`, `ja:RDFDataset`, or TDB at `--mem--`). The endpoints may be any
//! of the operations Sparkles serves, at the names it serves them at.
//!
//! Everything else is refused with a `400` that names it: other dataset types (text
//! indexes, inference, GeoSPARQL), data to load (`ja:data`), access control in the
//! description, contexts, RDF Patch, custom endpoint names, and services without a
//! write endpoint. `tdb2:location` is ignored, as Sparkles keeps its databases under its
//! data directory. `tdb2:unionDefaultGraph` must match `--union-default-graph`.

use crate::state::DbType;
use oxrdf::{NamedOrBlankNode, Term, Triple};

const FUSEKI: &str = "http://jena.apache.org/fuseki#";
const JA: &str = "http://jena.hpl.hp.com/2005/11/Assembler#";
const TDB2: &str = "http://jena.apache.org/2016/tdb#";
const TDB1: &str = "http://jena.hpl.hp.com/2008/tdb#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// What a description asks for.
#[derive(Debug, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    pub kind: DbType,
}

/// The server settings a description is checked against.
pub struct Server {
    pub union_default_graph: bool,
    pub gsp_direct_naming: bool,
}

/// Read a description in `format`.
pub fn parse(body: &[u8], format: oxrdfio::RdfFormat, server: &Server) -> Result<Spec, String> {
    let mut triples = Vec::new();
    for q in oxrdfio::RdfParser::from_format(format)
        .with_base_iri("http://base/")
        .expect("a valid base IRI")
        .for_slice(body)
    {
        let q = q.map_err(|e| format!("Failed to read configuration: {e}"))?;
        triples.push(Triple::from(q));
    }
    Description { triples }.spec(server)
}

struct Description {
    triples: Vec<Triple>,
}

fn short(iri: &str) -> String {
    for (p, ns) in [
        ("fuseki:", FUSEKI),
        ("ja:", JA),
        ("tdb2:", TDB2),
        ("tdb:", TDB1),
        ("rdfs:", RDFS),
    ] {
        if let Some(l) = iri.strip_prefix(ns) {
            return format!("{p}{l}");
        }
    }
    if iri == RDF_TYPE {
        return "rdf:type".into();
    }
    format!("<{iri}>")
}

fn term_str(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => short(n.as_str()),
        t => t.to_string(),
    }
}

fn as_subject(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(n.clone().into()),
        Term::BlankNode(b) => Some(b.clone().into()),
        _ => None,
    }
}

/// The operations Sparkles serves and the endpoint names it serves them at (`""` is the
/// dataset URL itself).
fn served(op: &str, server: &Server) -> Result<&'static [&'static str], String> {
    Ok(match op {
        "query" => &["", "sparql", "query"],
        "update" => &["", "update"],
        "gsp-rw" => &["", "data"],
        "gsp-r" => &["", "get", "data"],
        "upload" => &["upload"],
        "prefixes-r" | "prefixes-rw" => &["prefixes"],
        "shacl" if cfg!(feature = "shacl") => &["shacl"],
        "no-op" => &[
            "", "sparql", "query", "update", "data", "get", "upload", "shacl", "prefixes",
        ],
        "gsp-direct-rw" | "gsp-direct-r" if server.gsp_direct_naming => &[""],
        "gsp-direct-rw" | "gsp-direct-r" => {
            return Err(format!(
                "fuseki:{op} needs a server started with --gsp-direct-naming"
            ));
        }
        "patch" => {
            return Err("fuseki:patch is not supported: Sparkles cannot apply RDF Patch".into());
        }
        other => return Err(format!("the operation fuseki:{other} is not supported")),
    })
}

impl Description {
    fn objects<'a>(&'a self, s: &NamedOrBlankNode, p: &str) -> Vec<&'a Term> {
        self.triples
            .iter()
            .filter(|t| &t.subject == s && t.predicate.as_str() == p)
            .map(|t| &t.object)
            .collect()
    }

    fn one<'a>(&'a self, s: &NamedOrBlankNode, p: &str) -> Result<Option<&'a Term>, String> {
        let mut it = self.objects(s, p).into_iter();
        let first = it.next();
        if it.next().is_some() {
            return Err(format!("{s} has more than one {}", short(p)));
        }
        Ok(first)
    }

    fn types(&self, s: &NamedOrBlankNode) -> Vec<String> {
        self.objects(s, RDF_TYPE)
            .into_iter()
            .filter_map(|t| match t {
                Term::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Refuse any property of `s` outside `allowed`.
    fn only(&self, s: &NamedOrBlankNode, what: &str, allowed: &[&str]) -> Result<(), String> {
        for t in self.triples.iter().filter(|t| &t.subject == s) {
            let p = t.predicate.as_str();
            if !allowed.contains(&p) && !p.starts_with(RDFS) {
                return Err(format!(
                    "{} on the {what} is not supported{}",
                    short(p),
                    hint(p)
                ));
            }
        }
        Ok(())
    }

    /// The service: the one subject with `fuseki:name`, else the one `fuseki:Service`.
    fn service(&self) -> Result<NamedOrBlankNode, String> {
        let name = format!("{FUSEKI}name");
        let named: Vec<&Triple> = self
            .triples
            .iter()
            .filter(|t| t.predicate.as_str() == name && self.is_service(&t.subject))
            .collect();
        if let [t] = named.as_slice() {
            return Ok(t.subject.clone());
        }
        let services: Vec<&NamedOrBlankNode> = self
            .triples
            .iter()
            .filter(|t| {
                t.predicate.as_str() == RDF_TYPE
                    && matches!(&t.object, Term::NamedNode(n) if n.as_str() == format!("{FUSEKI}Service"))
            })
            .map(|t| &t.subject)
            .collect();
        match services.as_slice() {
            [] if named.is_empty() => Err("No triple rdf:type fuseki:Service found".into()),
            [s] => Ok((*s).clone()),
            _ => Err("Multiple Fuseki service descriptions".into()),
        }
    }

    /// A subject that is a service: typed `fuseki:Service`, or untyped (Fuseki finds a
    /// service by its unique `fuseki:name` alone).
    fn is_service(&self, s: &NamedOrBlankNode) -> bool {
        let t = self.types(s);
        t.is_empty() || t.iter().any(|t| *t == format!("{FUSEKI}Service"))
    }

    fn spec(&self, server: &Server) -> Result<Spec, String> {
        let svc = self.service()?;
        let f = |l: &str| format!("{FUSEKI}{l}");
        let legacy = [
            ("serviceQuery", "query"),
            ("serviceUpdate", "update"),
            ("serviceUpload", "upload"),
            ("serviceReadWriteGraphStore", "gsp-rw"),
            ("serviceReadGraphStore", "gsp-r"),
            ("serviceReadWriteQuads", "gsp-rw"),
            ("serviceReadQuads", "gsp-r"),
        ];
        let mut allowed: Vec<String> =
            vec![RDF_TYPE.into(), f("name"), f("dataset"), f("endpoint")];
        allowed.extend(legacy.iter().map(|(p, _)| f(p)));
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        self.only(&svc, "service", &allowed)?;

        let name = match self.one(&svc, &f("name"))? {
            Some(Term::Literal(l)) => l.value().trim().trim_start_matches('/').to_string(),
            Some(t) => {
                return Err(format!(
                    "Found {} : Service names are strings, which are then used to build the external URI",
                    term_str(t)
                ));
            }
            None => return Err("No name given in description of Fuseki service".into()),
        };
        if name.is_empty() {
            return Err("Empty dataset name".into());
        }

        // endpoints: (operation, name)
        let mut endpoints: Vec<(String, String)> = Vec::new();
        for (p, op) in legacy {
            for o in self.objects(&svc, &f(p)) {
                match o {
                    Term::Literal(l) => endpoints.push((op.into(), l.value().to_string())),
                    t => return Err(format!("fuseki:{p} {} is not a string", term_str(t))),
                }
            }
        }
        for e in self.objects(&svc, &f("endpoint")) {
            let e = as_subject(e).ok_or("a fuseki:endpoint is not a resource")?;
            self.only(&e, "endpoint", &[&f("operation"), &f("name")])?;
            let op = match self.one(&e, &f("operation"))? {
                Some(Term::NamedNode(n)) => match n.as_str().strip_prefix(FUSEKI) {
                    Some(op) => op.to_string(),
                    None => {
                        return Err(format!(
                            "the operation {} is not supported",
                            short(n.as_str())
                        ));
                    }
                },
                Some(t) => return Err(format!("the operation {} is not supported", term_str(t))),
                None => return Err("an endpoint without fuseki:operation".into()),
            };
            let ename = match self.one(&e, &f("name"))? {
                Some(Term::Literal(l)) => l.value().to_string(),
                Some(t) => return Err(format!("endpoint name {} is not a string", term_str(t))),
                None => String::new(),
            };
            endpoints.push((op, ename));
        }
        for (op, ename) in &endpoints {
            let names = served(op, server)?;
            if !names.contains(&ename.as_str()) {
                let at: Vec<String> = names
                    .iter()
                    .map(|n| {
                        if n.is_empty() {
                            format!("/{name}")
                        } else {
                            format!("/{name}/{n}")
                        }
                    })
                    .collect();
                return Err(format!(
                    "the endpoint name \u{201c}{ename}\u{201d} of fuseki:{op} is not supported: Sparkles serves it at {}",
                    at.join(", ")
                ));
            }
        }
        let writes = endpoints.iter().any(|(op, _)| {
            matches!(
                op.as_str(),
                "update" | "gsp-rw" | "upload" | "gsp-direct-rw"
            )
        });
        if !endpoints.is_empty() && !writes {
            return Err(
                "a read-only service is not supported: Sparkles serves every endpoint of a \
                 dataset (grant read access per dataset with --auth-config instead)"
                    .into(),
            );
        }

        let ds = match self.one(&svc, &f("dataset"))? {
            Some(t) => as_subject(t).ok_or("fuseki:dataset is not a resource")?,
            None => {
                return Err(format!(
                    "the service \u{201c}{name}\u{201d} has no fuseki:dataset"
                ));
            }
        };
        let kind = self.dataset_kind(&ds, server)?;
        Ok(Spec { name, kind })
    }

    fn dataset_kind(&self, ds: &NamedOrBlankNode, server: &Server) -> Result<DbType, String> {
        let types = self.types(ds);
        let known = [
            format!("{TDB2}DatasetTDB2"),
            format!("{TDB2}DatasetTDB"),
            format!("{TDB1}DatasetTDB"),
            format!("{JA}MemoryDataset"),
            format!("{JA}DatasetTxnMem"),
            format!("{JA}RDFDataset"),
            format!("{JA}Object"),
        ];
        if let Some(t) = types.iter().find(|t| !known.contains(t)) {
            return Err(format!(
                "the dataset type {} is not supported{}",
                short(t),
                hint(t)
            ));
        }
        let tdb2 = types
            .iter()
            .any(|t| *t == format!("{TDB2}DatasetTDB2") || *t == format!("{TDB2}DatasetTDB"));
        let tdb1 = types.iter().any(|t| *t == format!("{TDB1}DatasetTDB"));
        let mem = types.iter().any(|t| {
            *t == format!("{JA}MemoryDataset")
                || *t == format!("{JA}DatasetTxnMem")
                || *t == format!("{JA}RDFDataset")
        });
        if !(tdb2 || tdb1 || mem) {
            return Err("the dataset has no supported rdf:type (tdb2:DatasetTDB2, tdb:DatasetTDB, ja:MemoryDataset or ja:DatasetTxnMem)".into());
        }
        let ns = if tdb1 { TDB1 } else { TDB2 };
        if tdb2 || tdb1 {
            self.only(
                ds,
                "dataset",
                &[
                    RDF_TYPE,
                    &format!("{ns}location"),
                    &format!("{ns}unionDefaultGraph"),
                ],
            )?;
            if let Some(u) = self.one(ds, &format!("{ns}unionDefaultGraph"))? {
                let on = matches!(u, Term::Literal(l) if l.value() == "true" || l.value() == "1");
                if on != server.union_default_graph {
                    return Err(format!(
                        "{}unionDefaultGraph {on} does not match the server, which runs {} --union-default-graph",
                        if tdb1 { "tdb:" } else { "tdb2:" },
                        if server.union_default_graph {
                            "with"
                        } else {
                            "without"
                        }
                    ));
                }
            }
            let in_memory = matches!(
                self.one(ds, &format!("{ns}location"))?,
                Some(Term::Literal(l)) if l.value() == "--mem--"
            );
            Ok(if in_memory {
                DbType::Mem
            } else {
                DbType::Persistent
            })
        } else {
            self.only(ds, "dataset", &[RDF_TYPE])?;
            Ok(DbType::Mem)
        }
    }
}

/// What to do instead, for what is refused often.
fn hint(iri: &str) -> &'static str {
    match iri {
        _ if iri == format!("{JA}data")
            || iri == format!("{JA}defaultGraph")
            || iri == format!("{JA}namedGraph") =>
        {
            ": create the dataset, then load data with the Graph Store Protocol or /upload"
        }
        _ if iri.starts_with("http://jena.apache.org/text#") => {
            ": create the dataset, then enable full-text search with PUT /$/text/{ds}"
        }
        _ if iri.starts_with("http://jena.apache.org/spatial#")
            || iri.starts_with("http://jena.apache.org/geosparql#") =>
        {
            ": create the dataset with \"geo\": true, or PUT /$/geo/{ds}"
        }
        _ if iri == format!("{JA}InfModel")
            || iri.contains("DatasetRDFS")
            || iri.contains("Reasoner") =>
        {
            ": create the dataset, then materialize inferences with POST /$/reason/{ds}"
        }
        _ if iri.ends_with("allowedUsers") || iri.ends_with("#unionGraph") => {
            ": configure access per dataset with --auth-config"
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Server = Server {
        union_default_graph: false,
        gsp_direct_naming: false,
    };

    fn spec(ttl: &str) -> Result<Spec, String> {
        let doc = format!(
            "@prefix fuseki: <{FUSEKI}> .\n@prefix ja: <{JA}> .\n@prefix tdb2: <{TDB2}> .\n\
             @prefix tdb: <{TDB1}> .\n@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n{ttl}"
        );
        parse(doc.as_bytes(), oxrdfio::RdfFormat::Turtle, &SERVER)
    }

    #[test]
    fn tdb2_service_with_endpoints() {
        let s = spec(
            r#"<#s> a fuseki:Service ; fuseki:name "ds" ;
                 fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "sparql" ] ;
                 fuseki:endpoint [ fuseki:operation fuseki:update ] ;
                 fuseki:endpoint [ fuseki:operation fuseki:gsp-rw ; fuseki:name "data" ] ;
                 fuseki:dataset <#d> .
               <#d> a tdb2:DatasetTDB2 ; tdb2:location "DB2" ."#,
        )
        .unwrap();
        assert_eq!(
            s,
            Spec {
                name: "ds".into(),
                kind: DbType::Persistent
            }
        );
    }

    #[test]
    fn legacy_properties_and_memory_datasets() {
        let s = spec(
            r#"[] a fuseki:Service ; fuseki:name "/mem" ; fuseki:serviceQuery "query" ;
                 fuseki:serviceUpdate "update" ; fuseki:serviceReadWriteGraphStore "data" ;
                 fuseki:dataset [ a ja:DatasetTxnMem ] ."#,
        )
        .unwrap();
        assert_eq!(s.name, "mem");
        assert_eq!(s.kind, DbType::Mem);
        let s = spec(
            r#"[] a fuseki:Service ; fuseki:name "m" ; fuseki:dataset [ a tdb2:DatasetTDB2 ; tdb2:location "--mem--" ] ."#,
        )
        .unwrap();
        assert_eq!(s.kind, DbType::Mem);
    }

    #[test]
    fn refusals_name_what_is_refused() {
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:dataset [ a <http://jena.apache.org/text#TextDataset> ] ."#).unwrap_err();
        assert!(
            e.contains("<http://jena.apache.org/text#TextDataset>") && e.contains("/$/text"),
            "{e}"
        );
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:dataset [ a ja:MemoryDataset ; ja:data "a.ttl" ] ."#).unwrap_err();
        assert!(e.contains("ja:data"), "{e}");
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:endpoint [ fuseki:operation fuseki:query ; fuseki:name "q" ] ; fuseki:endpoint [ fuseki:operation fuseki:update ] ; fuseki:dataset [ a ja:MemoryDataset ] ."#).unwrap_err();
        assert!(e.contains("\u{201c}q\u{201d}"), "{e}");
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:endpoint [ fuseki:operation fuseki:query ] ; fuseki:dataset [ a ja:MemoryDataset ] ."#).unwrap_err();
        assert!(e.contains("read-only"), "{e}");
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:endpoint [ fuseki:operation fuseki:patch ] ; fuseki:dataset [ a ja:MemoryDataset ] ."#).unwrap_err();
        assert!(e.contains("RDF Patch"), "{e}");
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:dataset [ a tdb2:DatasetTDB2 ; tdb2:unionDefaultGraph true ] ."#).unwrap_err();
        assert!(e.contains("--union-default-graph"), "{e}");
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "x" ; fuseki:allowedUsers "u" ; fuseki:dataset [ a ja:MemoryDataset ] ."#).unwrap_err();
        assert!(e.contains("--auth-config"), "{e}");
        assert!(
            spec("<#a> <#b> <#c> .")
                .unwrap_err()
                .contains("No triple rdf:type fuseki:Service")
        );
        let e = spec(r#"[] a fuseki:Service ; fuseki:name "a" ; fuseki:dataset [ a ja:MemoryDataset ] . [] a fuseki:Service ; fuseki:name "b" ; fuseki:dataset [ a ja:MemoryDataset ] ."#).unwrap_err();
        assert!(e.contains("Multiple"), "{e}");
    }
}
