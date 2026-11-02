//! `sparkles ontology`: the OWL 2 ontology of a database's graphs (spec F21) on a local
//! database directory or on a server: its summary, its axioms, its profile checks,
//! edits and OWL 2 EL classification, and the check and classification of an ontology
//! file without a database.

use crate::http::ontology::{
    AxiomQuery, ClassifyQuery, MAX_LIMIT, axiom_page, check_body, classify_body, edit_body,
    mentioned, page_body, summary_body,
};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value as J, json};
use sparkles::handles::ontology::classify::ClassifyRequest;
use sparkles::handles::ontology::{
    self as onto, CheckRequest, EditRequest, OntologyRequest, check_opened,
};
use sparkles::owl::{self, Axiom, ofn::Renderer};
use sparkles::store::StoreOptions;
use sparkles_reasoner::inputs::{GraphRef, ImportMode};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum OntologyCmd {
    /// The header, the counts of entities and axioms, the graphs read and the unmapped
    /// triples
    Summary {
        #[command(flatten)]
        at: Target,
        #[command(flatten)]
        read: Read,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// The axioms, in functional-style syntax or JSON
    Axioms {
        #[command(flatten)]
        at: Target,
        #[command(flatten)]
        read: Read,
        /// Axiom types, comma separated (SubClassOf,EquivalentClasses,…)
        #[arg(long = "type", value_name = "TYPES")]
        types: Option<String>,
        /// Only the axioms that mention this IRI
        #[arg(long)]
        about: Option<String>,
        /// Also the assertions the store holds
        #[arg(long)]
        assertions: bool,
        /// The most axioms listed
        #[arg(long, default_value_t = 1000)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// ofn (a functional-style document) or json
        #[arg(long, default_value = "ofn")]
        format: String,
    },
    /// Check the OWL 2 profiles; exits with status 1 when the ontology is outside one
    Check {
        /// An ontology file to check without a database (Turtle, RDF/XML, OWL
        /// functional-style syntax, N-Triples, TriG, JSON-LD, … by its extension)
        file: Option<PathBuf>,
        #[command(flatten)]
        at: Target,
        #[command(flatten)]
        read: Read,
        /// The profiles: el, ql, rl, dl or all, comma separated
        #[arg(long, default_value = "dl")]
        profile: String,
        /// Check the terminology alone, not the assertions
        #[arg(long)]
        no_assertions: bool,
        /// The most violations each report lists
        #[arg(long, default_value_t = 1000)]
        max_violations: usize,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Classify by OWL 2 EL: the inferred class hierarchy and the unsatisfiable
    /// classes. Exits with status 1 when the ontology is inconsistent or has an
    /// unsatisfiable class
    Classify {
        /// An ontology file to classify without a database (Turtle, RDF/XML, OWL
        /// functional-style syntax, … by its extension)
        file: Option<PathBuf>,
        #[command(flatten)]
        at: Target,
        #[command(flatten)]
        read: Read,
        /// Also classify with the assertions the store holds
        #[arg(long)]
        assertions: bool,
        /// Write the taxonomy to this named graph in one commit
        #[arg(long)]
        target: Option<String>,
        /// The commit message of the write
        #[arg(long)]
        message: Option<String>,
        /// Describe this class: its equivalent classes, direct superclasses and
        /// subclasses, and all superclasses
        #[arg(long)]
        about: Option<String>,
        /// Worker threads (0: the algorithm pool's share for one call)
        #[arg(long, default_value_t = 0)]
        threads: usize,
        /// Exit with status 0 when classes are unsatisfiable but the ontology is
        /// consistent
        #[arg(long)]
        allow_unsatisfiable: bool,
        /// ofn (the taxonomy as a functional-style document), json or text
        #[arg(long, default_value = "ofn")]
        format: String,
    },
    /// Add and remove axioms in one commit
    Edit {
        #[command(flatten)]
        at: Target,
        #[command(flatten)]
        read: Read,
        /// A JSON file with a list of axioms to add (repeatable)
        #[arg(long, value_name = "FILE")]
        add: Vec<PathBuf>,
        /// A JSON file with a list of axioms to remove (repeatable)
        #[arg(long, value_name = "FILE")]
        remove: Vec<PathBuf>,
        /// The graph that takes the added axioms (the first graph read by default)
        #[arg(long)]
        target: Option<String>,
        /// Do not declare the entities the added axioms use
        #[arg(long)]
        no_declare: bool,
        /// The commit message
        #[arg(long)]
        message: Option<String>,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
}

/// Where the ontology is: a database directory or a dataset on a server.
#[derive(Args, Debug, Clone)]
pub struct Target {
    /// Database directory
    #[arg(long, conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// A server to ask instead of a local database (with --dataset)
    #[arg(long, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset on --server
    #[arg(long, requires = "server")]
    pub dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    pub insecure_http: bool,
}

/// What ontology to read.
#[derive(Args, Debug, Clone)]
pub struct Read {
    /// A graph read: an IRI or `default` (repeatable; the default graph when none)
    #[arg(long)]
    pub graph: Vec<String>,
    /// Do not follow owl:imports to graphs of the dataset
    #[arg(long)]
    pub no_imports: bool,
    /// lenient or strict
    #[arg(long, default_value = "lenient")]
    pub mode: String,
}

impl Read {
    fn request(&self) -> Result<OntologyRequest> {
        let mut req = OntologyRequest::default();
        if !self.graph.is_empty() {
            req.graphs = self
                .graph
                .iter()
                .map(|g| {
                    GraphRef::parse(g.trim_start_matches('<').trim_end_matches('>'))
                        .map_err(anyhow::Error::msg)
                })
                .collect::<Result<_>>()?;
        }
        if self.no_imports {
            req.imports = ImportMode::None;
        }
        req.mode = owl::Mode::parse(&self.mode)
            .with_context(|| format!("--mode lenient or strict, not {}", self.mode))?;
        Ok(req)
    }

    /// The members of a request body that name the ontology.
    fn body(&self) -> J {
        let mut j = json!({ "imports": !self.no_imports, "mode": self.mode });
        if !self.graph.is_empty() {
            j["graph"] = json!(self.graph);
        }
        j
    }

    /// The query parameters that name the ontology.
    fn query(&self) -> Vec<(String, String)> {
        let mut q: Vec<(String, String)> = self
            .graph
            .iter()
            .map(|g| ("graph".into(), g.clone()))
            .collect();
        if self.no_imports {
            q.push(("imports".into(), "false".into()));
        }
        q.push(("mode".into(), self.mode.clone()));
        q
    }
}

/// Run a command; the exit status of `check` is 1 when a report does not conform.
pub fn run(cmd: OntologyCmd, opts: StoreOptions) -> Result<()> {
    match cmd {
        OntologyCmd::Summary { at, read, format } => {
            let j = match local(&at)? {
                Some(loc) => {
                    let ds = crate::open_dataset(loc, opts)?;
                    let req = read.request()?;
                    let o = ds.ontology().open(&req, &Default::default())?;
                    summary_body(&name_of(loc), &o, &req, &renderer(&ds))
                }
                None => remote_get(&at, "", &read.query())?,
            };
            print_summary(&j, &format)
        }
        OntologyCmd::Axioms {
            at,
            read,
            types,
            about,
            assertions,
            limit,
            offset,
            format,
        } => {
            if format != "ofn" && format != "json" {
                bail!("unknown --format {format}: ofn or json");
            }
            match local(&at)? {
                Some(loc) => {
                    let ds = crate::open_dataset(loc, opts)?;
                    let req = read.request()?;
                    let o = ds.ontology().open(&req, &Default::default())?;
                    let q = AxiomQuery {
                        types: types.as_deref().map(onto::parse_axiom_types).transpose()?,
                        about: about.clone(),
                        assertions,
                        limit: limit.max(1),
                        offset,
                    };
                    let page = axiom_page(&o, &q, &Default::default())?;
                    if let Some(a) = &about
                        && page.total == 0
                        && !mentioned(&o, a)
                    {
                        bail!("the ontology neither declares nor uses <{a}>");
                    }
                    let r = renderer(&ds);
                    if format == "ofn" {
                        print!(
                            "{}",
                            owl::ofn::document(
                                &r,
                                &o.ontology.header,
                                page.items.iter().map(|(a, _)| a)
                            )
                        );
                    } else {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&page_body(
                                &name_of(loc),
                                &o,
                                &req,
                                &r,
                                &page
                            ))?
                        );
                    }
                }
                None => {
                    let mut q = read.query();
                    if let Some(t) = types {
                        q.push(("type".into(), t));
                    }
                    if let Some(a) = about {
                        q.push(("about".into(), a));
                    }
                    if assertions {
                        q.push(("assertions".into(), "true".into()));
                    }
                    q.push(("limit".into(), limit.to_string()));
                    q.push(("offset".into(), offset.to_string()));
                    q.push(("format".into(), format.clone()));
                    let text = remote_text(&at, "/axioms", &q)?;
                    if format == "json" {
                        let j: J = serde_json::from_str(&text)?;
                        println!("{}", serde_json::to_string_pretty(&j)?);
                    } else {
                        print!("{text}");
                    }
                }
            }
            Ok(())
        }
        OntologyCmd::Check {
            file,
            at,
            read,
            profile,
            no_assertions,
            max_violations,
            format,
        } => {
            let profiles = onto::parse_profiles(&profile)?;
            if profiles.is_empty() {
                bail!("--profile names no profile: el, ql, rl, dl or all");
            }
            let check = CheckRequest {
                profiles: profiles.clone(),
                assertions: !no_assertions,
                max_violations: max_violations.max(1),
            };
            let loc = if file.is_some() && at.server.is_none() {
                at.loc.as_deref()
            } else {
                local(&at)?
            };
            let j = match (file, loc) {
                (Some(_), Some(_)) => bail!("check a FILE or a database (--loc), not both"),
                (Some(_), None) if at.server.is_some() => {
                    bail!("check a FILE or a dataset on --server, not both")
                }
                (Some(f), None) => check_file(&f, &read, &check)?,
                (None, Some(loc)) => {
                    let ds = crate::open_dataset(loc, opts)?;
                    let req = read.request()?;
                    let o = ds.ontology().open(&req, &Default::default())?;
                    let reports = check_opened(&o, &check, &Default::default())?;
                    check_body(
                        &name_of(loc),
                        &o,
                        &req,
                        &renderer(&ds),
                        &reports,
                        check.assertions,
                    )
                }
                (None, None) => {
                    let mut body = read.body();
                    body["profiles"] =
                        json!(profiles.iter().map(|p| p.as_str()).collect::<Vec<_>>());
                    body["assertions"] = json!(!no_assertions);
                    body["maxViolations"] = json!(check.max_violations);
                    remote_post(&at, "/check", &body)?
                }
            };
            print_check(&j, &format)?;
            if !j["conforms"].as_bool().unwrap_or(false) {
                std::process::exit(1);
            }
            Ok(())
        }
        OntologyCmd::Classify {
            file,
            at,
            read,
            assertions,
            target,
            message,
            about,
            threads,
            allow_unsatisfiable,
            format,
        } => {
            if !["ofn", "json", "text"].contains(&format.as_str()) {
                bail!("unknown --format {format}: ofn, json or text");
            }
            let c = ClassifyRequest {
                assertions,
                threads,
                target: target
                    .as_deref()
                    .map(|g| {
                        GraphRef::parse(g.trim_start_matches('<').trim_end_matches('>'))
                            .map_err(anyhow::Error::msg)
                    })
                    .transpose()?,
                message: message.clone(),
                about: about
                    .as_deref()
                    .map(|a| a.trim_start_matches('<').trim_end_matches('>').to_string()),
            };
            let q = ClassifyQuery {
                about: about
                    .as_deref()
                    .map(|a| a.trim_start_matches('<').trim_end_matches('>').to_string()),
                nodes: false,
                limit: MAX_LIMIT,
                offset: 0,
            };
            let loc = if file.is_some() && at.server.is_none() {
                at.loc.as_deref()
            } else {
                local(&at)?
            };
            let local_run = |ds: &sparkles::Dataset, name: &str| -> Result<(J, String)> {
                let req = read.request()?;
                let out = ds.ontology().classify(&req, &c, &Default::default())?;
                let r = renderer(ds);
                let j = classify_body(name, &out, &req, &r, &q)?;
                let doc = owl::ofn::document(
                    &r,
                    &owl::Header::default(),
                    out.classification.taxonomy.axioms().iter(),
                );
                Ok((j, doc))
            };
            let (j, doc) = match (file, loc) {
                (Some(_), Some(_)) => bail!("classify a FILE or a database (--loc), not both"),
                (Some(_), None) if at.server.is_some() => {
                    bail!("classify a FILE or a dataset on --server, not both")
                }
                (Some(_), None) if c.target.is_some() => {
                    bail!("--target writes to a database: name --loc or --server, not a FILE")
                }
                (Some(f), None) => {
                    let ds = sparkles::Dataset::memory();
                    let source = sparkles::io::Source::from_path(&f, None)
                        .with_context(|| format!("cannot read {}", f.display()))?;
                    ds.store()
                        .load(&[source])
                        .with_context(|| format!("cannot read {}", f.display()))?;
                    local_run(&ds, &f.display().to_string())?
                }
                (None, Some(loc)) => {
                    let ds = crate::open_dataset(loc, opts)?;
                    local_run(&ds, &name_of(loc))?
                }
                (None, None) => {
                    let mut body = read.body();
                    body["assertions"] = json!(assertions);
                    body["threads"] = json!(threads);
                    body["limit"] = json!(MAX_LIMIT);
                    if let Some(t) = &target {
                        body["target"] = json!(t);
                    }
                    if let Some(m) = &message {
                        body["message"] = json!(m);
                    }
                    if let Some(a) = &q.about {
                        body["about"] = json!(a);
                    }
                    let j = remote_post(&at, "/classify", &body)?;
                    let doc = if format == "ofn" {
                        // the kept classification answers the second request
                        body.as_object_mut().map(|o| {
                            o.remove("target");
                            o.remove("message");
                        });
                        remote_post_text(&at, "/classify?format=ofn", &body)?
                    } else {
                        String::new()
                    };
                    (j, doc)
                }
            };
            match format.as_str() {
                "ofn" => print!("{doc}"),
                "json" => println!("{}", serde_json::to_string_pretty(&j)?),
                _ => print_classify(&j),
            }
            let consistent = j["consistent"].as_bool().unwrap_or(false);
            let unsat = j["unsatisfiableCount"].as_u64().unwrap_or(0);
            if !consistent || (unsat > 0 && !allow_unsatisfiable) {
                std::process::exit(1);
            }
            Ok(())
        }
        OntologyCmd::Edit {
            at,
            read,
            add,
            remove,
            target,
            no_declare,
            message,
            format,
        } => {
            let add = axioms_of(&add)?;
            let remove = axioms_of(&remove)?;
            if add.is_empty() && remove.is_empty() {
                bail!("name axioms to add (--add FILE) or remove (--remove FILE)");
            }
            let j = match local(&at)? {
                Some(loc) => {
                    let ds = crate::open_dataset(loc, opts)?;
                    let req = read.request()?;
                    let edit = EditRequest {
                        add,
                        remove,
                        declare: Some(!no_declare),
                        graph: target
                            .as_deref()
                            .map(|g| GraphRef::parse(g).map_err(anyhow::Error::msg))
                            .transpose()?,
                        message,
                    };
                    let e = ds.ontology().edit(&req, &edit, &Default::default())?;
                    edit_body(&e, &renderer(&ds))
                }
                None => {
                    let mut body = read.body();
                    body["add"] = J::Array(add.iter().map(owl::json::axiom).collect());
                    body["remove"] = J::Array(remove.iter().map(owl::json::axiom).collect());
                    body["declare"] = json!(!no_declare);
                    if let Some(t) = target {
                        body["target"] = json!(t);
                    }
                    if let Some(m) = message {
                        body["message"] = json!(m);
                    }
                    remote_post(&at, "/axioms", &body)?
                }
            };
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&j)?),
                "text" => {
                    let n = |k: &str| j[k].as_u64().unwrap_or(0);
                    println!(
                        "added {} axioms ({} already present), removed {} ({} absent); {} triples inserted, {} deleted, {} kept",
                        n("added"),
                        n("present"),
                        n("removed"),
                        j["absent"].as_array().map_or(0, Vec::len),
                        n("inserted"),
                        n("deleted"),
                        n("kept"),
                    );
                    for d in j["declared"].as_array().into_iter().flatten() {
                        println!("declared {}", d.as_str().unwrap_or(""));
                    }
                }
                f => bail!("unknown --format {f}: text or json"),
            }
            Ok(())
        }
    }
}

/// The database directory, or `None` for a server; one of them is needed.
fn local(at: &Target) -> Result<Option<&Path>> {
    match (&at.loc, &at.server) {
        (Some(l), _) => Ok(Some(l.as_path())),
        (None, Some(_)) => Ok(None),
        (None, None) => {
            bail!("name a database directory (--loc) or a server (--server with --dataset)")
        }
    }
}

fn name_of(loc: &Path) -> String {
    loc.file_name().map_or_else(
        || loc.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn renderer(ds: &sparkles::Dataset) -> Renderer {
    Renderer::new(ds.prefixes())
}

/// The axioms of JSON files, each a list of axioms or one axiom.
fn axioms_of(files: &[PathBuf]) -> Result<Vec<Axiom>> {
    let mut out = Vec::new();
    for f in files {
        let text =
            std::fs::read_to_string(f).with_context(|| format!("cannot read {}", f.display()))?;
        let j: J = serde_json::from_str(&text)
            .with_context(|| format!("{}: invalid JSON", f.display()))?;
        let items = match j {
            J::Array(a) => a,
            j => vec![j],
        };
        for (i, v) in items.iter().enumerate() {
            out.push(
                owl::json::read_axiom(v)
                    .map_err(|e| anyhow::anyhow!("{}: axiom {i}: {e}", f.display()))?,
            );
        }
    }
    Ok(out)
}

/// Check an ontology file: read it into a database in memory and check its graphs.
/// The imports of a file are followed only to the graphs of the file.
fn check_file(f: &Path, read: &Read, check: &CheckRequest) -> Result<J> {
    let ds = sparkles::Dataset::memory();
    let source = sparkles::io::Source::from_path(f, None)
        .with_context(|| format!("cannot read {}", f.display()))?;
    ds.store()
        .load(&[source])
        .with_context(|| format!("cannot read {}", f.display()))?;
    let req = read.request()?;
    let o = ds.ontology().open(&req, &Default::default())?;
    let reports = check_opened(&o, check, &Default::default())?;
    Ok(check_body(
        &f.display().to_string(),
        &o,
        &req,
        &renderer(&ds),
        &reports,
        check.assertions,
    ))
}

#[cfg(feature = "auth")]
fn remote(at: &Target) -> Result<(crate::remote::Remote, String)> {
    let Some(ds) = at.dataset.as_deref() else {
        bail!("--dataset NAME is required with --server");
    };
    Ok((
        crate::remote::Remote::open(at.server.as_deref(), at.insecure_http)?,
        ds.to_string(),
    ))
}

#[cfg(feature = "auth")]
fn remote_text(at: &Target, path: &str, query: &[(String, String)]) -> Result<String> {
    let (r, ds) = remote(at)?;
    let enc = |v: &str| {
        percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    let q: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    let req = r.req(
        reqwest::Method::GET,
        &format!("/$/ontology/{ds}{path}?{}", q.join("&")),
    );
    Ok(r.check(req.send(), Some(&ds))?.text()?)
}

#[cfg(feature = "auth")]
fn remote_get(at: &Target, path: &str, query: &[(String, String)]) -> Result<J> {
    Ok(serde_json::from_str(&remote_text(at, path, query)?)?)
}

#[cfg(feature = "auth")]
fn remote_post(at: &Target, path: &str, body: &J) -> Result<J> {
    use crate::remote::JsonBody;
    let (r, ds) = remote(at)?;
    let req = r
        .req(reqwest::Method::POST, &format!("/$/ontology/{ds}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string());
    r.check(req.send(), Some(&ds))?.json_value()
}

#[cfg(feature = "auth")]
fn remote_post_text(at: &Target, path: &str, body: &J) -> Result<String> {
    let (r, ds) = remote(at)?;
    let req = r
        .req(reqwest::Method::POST, &format!("/$/ontology/{ds}{path}"))
        .header("content-type", "application/json")
        .body(body.to_string());
    Ok(r.check(req.send(), Some(&ds))?.text()?)
}

#[cfg(not(feature = "auth"))]
fn remote_post_text(_: &Target, _: &str, _: &J) -> Result<String> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

#[cfg(not(feature = "auth"))]
fn remote_text(_: &Target, _: &str, _: &[(String, String)]) -> Result<String> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

#[cfg(not(feature = "auth"))]
fn remote_get(_: &Target, _: &str, _: &[(String, String)]) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

#[cfg(not(feature = "auth"))]
fn remote_post(_: &Target, _: &str, _: &J) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

fn print_summary(j: &J, format: &str) -> Result<()> {
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(j)?),
        "text" => {
            let h = &j["header"];
            println!("ontology:  {}", h["iri"].as_str().unwrap_or("(anonymous)"));
            if let Some(v) = h["versionIri"].as_str() {
                println!("version:   {v}");
            }
            for i in h["imports"].as_array().into_iter().flatten() {
                println!("imports:   {}", i.as_str().unwrap_or(""));
            }
            let graphs: Vec<&str> = j["graphs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(J::as_str)
                .collect();
            println!("graphs:    {}", graphs.join(" "));
            println!("axioms:    {}", j["axiomCount"]);
            for (t, n) in j["axioms"].as_object().into_iter().flatten() {
                println!("  {t:<32} {n}");
            }
            println!("entities:");
            for (k, n) in j["entities"].as_object().into_iter().flatten() {
                println!("  {k:<32} {n}");
            }
            println!(
                "triples:   {} read, {} unmapped",
                j["triples"], j["unmapped"]
            );
            let d = &j["diagnostics"];
            println!(
                "notes:     {} warnings, {} errors",
                d["warnings"], d["errors"]
            );
            for n in d["list"].as_array().into_iter().flatten() {
                println!(
                    "  {} {}: {}",
                    n["severity"].as_str().unwrap_or(""),
                    n["code"].as_str().unwrap_or(""),
                    n["message"].as_str().unwrap_or("")
                );
            }
            for w in j["warnings"].as_array().into_iter().flatten() {
                println!("warning:   {}", w.as_str().unwrap_or(""));
            }
        }
        f => bail!("unknown --format {f}: text or json"),
    }
    Ok(())
}

fn print_check(j: &J, format: &str) -> Result<()> {
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(j)?),
        "text" => {
            for r in j["reports"].as_array().into_iter().flatten() {
                let title = r["title"].as_str().unwrap_or("");
                if r["conforms"].as_bool() == Some(true) {
                    println!("{title}: conforms ({} axioms checked)", r["checked"]);
                    continue;
                }
                println!("{title}: {} violations", r["total"]);
                for v in r["violations"].as_array().into_iter().flatten() {
                    match v["axiom"].as_str() {
                        Some(a) => println!(
                            "  {}: {}\n    {a}",
                            v["rule"].as_str().unwrap_or(""),
                            v["message"].as_str().unwrap_or("")
                        ),
                        None => println!(
                            "  {}: {}",
                            v["rule"].as_str().unwrap_or(""),
                            v["message"].as_str().unwrap_or("")
                        ),
                    }
                }
                if r["truncated"].as_bool() == Some(true) {
                    println!("  (more violations not listed)");
                }
            }
        }
        f => bail!("unknown --format {f}: text or json"),
    }
    Ok(())
}

fn print_classify(j: &J) {
    let n = |k: &str| j[k].as_u64().unwrap_or(0);
    let consistent = j["consistent"].as_bool().unwrap_or(false);
    println!(
        "{}: {} classes in {} nodes, {} unsatisfiable",
        if consistent {
            "consistent"
        } else {
            "inconsistent"
        },
        n("classes"),
        n("nodeCount"),
        n("unsatisfiableCount"),
    );
    for c in j["unsatisfiable"].as_array().into_iter().flatten() {
        println!("  unsatisfiable {}", c.as_str().unwrap_or(""));
    }
    if j["complete"].as_bool() != Some(true) {
        println!(
            "incomplete: {} axioms left out or used in part",
            n("excludedCount")
        );
        for e in j["excluded"].as_array().into_iter().flatten() {
            let why = e["partial"]
                .as_str()
                .map(|p| format!("partly used ({p})"))
                .unwrap_or_else(|| {
                    format!(
                        "{}: {}",
                        e["rule"].as_str().unwrap_or(""),
                        e["message"].as_str().unwrap_or("")
                    )
                });
            println!("  {why}\n    {}", e["ofn"].as_str().unwrap_or(""));
        }
    }
    let s = &j["stats"];
    println!(
        "{} run on {} threads: index {:.1} ms, saturation {:.1} ms, taxonomy {:.1} ms",
        if s["incremental"].as_bool() == Some(true) {
            "incremental"
        } else {
            "full"
        },
        s["threads"],
        s["indexMs"].as_f64().unwrap_or(0.0),
        s["saturationMs"].as_f64().unwrap_or(0.0),
        s["taxonomyMs"].as_f64().unwrap_or(0.0),
    );
    let c = &j["class"];
    if c.is_object() {
        let list = |k: &str| -> String {
            c[k].as_array()
                .into_iter()
                .flatten()
                .filter_map(J::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!("class {}", c["iri"].as_str().unwrap_or(""));
        println!("  equivalent   {}", list("classes"));
        println!("  parents      {}", list("parentClasses"));
        println!("  children     {}", list("childClasses"));
        println!("  superclasses {}", list("superClasses"));
    }
    if let Some(w) = j["written"].as_object() {
        println!(
            "wrote the taxonomy: {} axioms added, {} removed",
            w.get("added").and_then(J::as_u64).unwrap_or(0),
            w.get("removed").and_then(J::as_u64).unwrap_or(0)
        );
    }
}
