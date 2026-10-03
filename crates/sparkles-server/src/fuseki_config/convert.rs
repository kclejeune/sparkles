//! The conversion of Fuseki configuration graphs into a plan: the server's settings, the
//! datasets with their indexes and inference, and the access rules (spec G08 §4–§5).

use super::access::{self, AccessInput, AuthPlan, DatasetAccess};
use super::graph::{
    ConfigGraph, FUSEKI, JA, RDF_TYPE, RDFS, as_subject, boolean, iri, lexical, short, show_term,
};
use super::report::Report;
use super::users::UserFile;
use anyhow::{Result, bail};
use oxrdf::NamedOrBlankNode;
use serde_json::Value;
use std::path::PathBuf;

/// What the plan is for: files to keep, or a server starting now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `config import fuseki`: in-memory datasets with data become persistent ones
    Files,
    /// `serve --fuseki-config`: in-memory datasets load their data at each start
    Serve,
}

/// Everything the converter reads.
pub struct Inputs {
    pub graphs: Vec<ConfigGraph>,
    /// the user file (`shiro.ini` or the password file) and its path
    pub users: Option<(PathBuf, UserFile)>,
    /// whether `users` is a `shiro.ini` (with URL rules) rather than a password file
    pub shiro: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ServerPlan {
    pub timeout: Option<f64>,
    pub update_timeout: Option<f64>,
    pub union_default_graph: bool,
    pub read_only: bool,
    pub gsp_direct_naming: bool,
    pub auto_reason: bool,
    pub metrics_fuseki_names: bool,
    /// `fuseki:realm`
    pub realm: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DataFile {
    pub path: PathBuf,
    /// the named graph to load it into; `None` keeps the file's own graphs
    pub graph: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Reasoning {
    /// `rdfs` or `owl-rl`; `None` with rules
    pub profile: Option<&'static str>,
    /// rule files and inline rules, in order
    pub rule_files: Vec<PathBuf>,
    pub rule_texts: Vec<String>,
    /// the reasoner's `ja:schema` files, loaded into [`SCHEMA_GRAPH`]
    pub schema: Vec<PathBuf>,
    /// GeoSPARQL's schema and RDFS inference
    pub geo_vocab: bool,
    pub default_geometry: bool,
}

/// The graph the files of a reasoner's `ja:schema` are loaded into.
pub const SCHEMA_GRAPH: &str = "urn:x-sparkles:fuseki:schema";

#[derive(Clone, Debug)]
pub struct DatasetPlan {
    pub name: String,
    /// a TDB database to export, when Fuseki kept the data in one
    pub tdb: Option<PathBuf>,
    pub tdb1: bool,
    /// in memory in Fuseki
    pub memory: bool,
    pub data: Vec<DataFile>,
    pub text: Option<Value>,
    pub geo: Option<Value>,
    pub rdfs: Option<PathBuf>,
    pub reasoning: Option<Reasoning>,
    /// the dataset's own union-default-graph setting
    pub union: Option<bool>,
}

impl DatasetPlan {
    /// Kept in a database on disk by Sparkles.
    pub fn persistent(&self, mode: Mode) -> bool {
        !self.memory || (mode == Mode::Files && (!self.data.is_empty() || self.reasoning.is_some()))
    }
}

pub struct Plan {
    pub mode: Mode,
    pub report: Report,
    pub server: ServerPlan,
    pub datasets: Vec<DatasetPlan>,
    pub auth: Option<AuthPlan>,
}

/// A timeout found in a context, in seconds.
pub(super) struct Timeouts {
    pub(super) query: Vec<(String, f64)>,
    pub(super) update: Vec<(String, f64)>,
}

pub(super) struct Converter<'a> {
    pub(super) inputs: &'a Inputs,
    pub(super) mode: Mode,
    pub(super) report: Report,
    pub(super) timeouts: Timeouts,
    pub(super) server_query_timeout: Option<f64>,
    pub(super) server_update_timeout: Option<f64>,
    pub(super) server_union: Option<bool>,
    /// union-default-graph settings found in the contexts read since the last drain
    pub(super) local_union: Vec<(String, bool)>,
}

pub fn convert(inputs: &Inputs, mode: Mode) -> Result<Plan> {
    let mut c = Converter {
        inputs,
        mode,
        report: Report::default(),
        timeouts: Timeouts {
            query: Vec::new(),
            update: Vec::new(),
        },
        server_query_timeout: None,
        server_update_timeout: None,
        server_union: None,
        local_union: Vec::new(),
    };
    c.run()
}

/// The properties a resource may have, and the report of the others.
pub(super) fn unknown<'g>(
    g: &'g ConfigGraph,
    s: &NamedOrBlankNode,
    known: &[&str],
) -> impl Iterator<Item = &'g oxrdf::Triple> + 'g {
    let known: Vec<String> = known.iter().map(|k| k.to_string()).collect();
    g.about(s).filter(move |t| {
        let p = t.predicate.as_str();
        p != RDF_TYPE && !p.starts_with(RDFS) && !known.iter().any(|k| k == p)
    })
}

pub(super) fn f(l: &str) -> String {
    format!("{FUSEKI}{l}")
}
pub(super) fn ja(l: &str) -> String {
    format!("{JA}{l}")
}

impl Converter<'_> {
    fn run(&mut self) -> Result<Plan> {
        let inputs = self.inputs;
        // the server resource: one in all the files
        let mut servers = Vec::new();
        for (gi, g) in inputs.graphs.iter().enumerate() {
            for s in g.of_type(&f("Server")) {
                servers.push((gi, s));
            }
        }
        if servers.len() > 1 {
            bail!(
                "{} fuseki:Server resources found; a configuration has at most one",
                servers.len()
            );
        }
        let server = servers.into_iter().next();
        let mut plan_server = ServerPlan::default();
        let mut server_allowed: Option<Vec<String>> = None;
        let mut listed: Option<(usize, Vec<NamedOrBlankNode>)> = None;
        if let Some((gi, s)) = &server {
            let g = &inputs.graphs[*gi];
            self.server_settings(g, s, &mut plan_server, &mut server_allowed);
            if let Some(list) = g.one(s, &f("services")).ok().flatten() {
                let members = g.list(list).unwrap_or_else(|| vec![list.clone()]);
                listed = Some((*gi, members.iter().filter_map(as_subject).collect()));
            }
        }

        // the services: those the server lists, and every service of the other files
        let mut services: Vec<(usize, NamedOrBlankNode)> = Vec::new();
        for (gi, g) in inputs.graphs.iter().enumerate() {
            match &listed {
                Some((lgi, members)) if *lgi == gi => {
                    services.extend(members.iter().map(|m| (gi, m.clone())));
                    for s in g.of_type(&f("Service")) {
                        if !members.contains(&s) {
                            self.report.ignored(
                                format!("service {}", g.show(&s)),
                                "fuseki:services does not list it, so Fuseki does not serve it",
                            );
                        }
                    }
                }
                _ => services.extend(g.of_type(&f("Service")).into_iter().map(|s| (gi, s))),
            }
        }
        if services.is_empty() {
            bail!("no fuseki:Service found in the configuration");
        }

        let mut datasets: Vec<DatasetPlan> = Vec::new();
        let mut access_ds: Vec<DatasetAccess> = Vec::new();
        // dataset resources already served: (file, node) → service name
        let mut shared: Vec<((usize, NamedOrBlankNode), String)> = Vec::new();
        for (gi, svc) in services {
            let g = &inputs.graphs[gi];
            let Some((ds, acc)) = self.service(g, gi, &svc, &mut shared, &mut plan_server)? else {
                continue;
            };
            if datasets.iter().any(|d| d.name == ds.name) {
                self.report.unsupported(
                    format!("service /{}", ds.name),
                    "a second service has this name; Fuseki refuses it too",
                );
                continue;
            }
            datasets.push(ds);
            access_ds.push(acc);
        }

        self.settle_timeouts(&mut plan_server);
        self.settle_union(&mut plan_server, &mut datasets);

        let auth = access::plan(
            &AccessInput {
                users: inputs.users.as_ref().map(|(p, u)| (p.as_path(), u)),
                shiro: inputs.shiro,
                server_allowed,
                datasets: access_ds,
            },
            &mut self.report,
            &mut plan_server,
        );
        plan_server.auto_reason = datasets.iter().any(|d| d.reasoning.is_some());
        if plan_server.auto_reason {
            self.report.approximated(
                "server",
                if plan_server.read_only {
                    "inference: Jena's inference models answer over the data, and Sparkles \
                     materializes the inferences once, since the server is read-only"
                } else {
                    "inference: Jena's inference models answer over the current data, and \
                     Sparkles materializes inferences, again 5 s after writes stop \
                     (--auto-reason 5)"
                },
            );
        }
        if auth.is_none() {
            self.report.approximated(
                "server",
                "Fuseki listens on every interface and Sparkles on 127.0.0.1; to serve the \
                 network add --host 0.0.0.0, which needs --auth-config or --allow-open-network",
            );
        }
        Ok(Plan {
            mode: self.mode,
            report: std::mem::take(&mut self.report),
            server: plan_server,
            datasets,
            auth,
        })
    }

    fn server_settings(
        &mut self,
        g: &ConfigGraph,
        s: &NamedOrBlankNode,
        plan: &mut ServerPlan,
        allowed: &mut Option<Vec<String>>,
    ) {
        let place = "server";
        for t in unknown(
            g,
            s,
            &[
                &f("services"),
                &ja("context"),
                &f("passwd"),
                &f("realm"),
                &f("auth"),
                &f("allowedUsers"),
                &f("contextPath"),
                &f("pingEP"),
                &f("statsEP"),
                &f("metricsEP"),
                &f("compactEP"),
                &ja("loadClass"),
            ],
        ) {
            self.report.ignored(
                place,
                format!(
                    "{} is not a Fuseki server setting Sparkles knows",
                    short(t.predicate.as_str())
                ),
            );
        }
        self.contexts(g, s, place, Scope::Server);
        if let Ok(Some(t)) = g.one(s, &f("contextPath")) {
            let p = lexical(t).unwrap_or("");
            if !matches!(p, "" | "/") {
                self.report.unsupported(
                    place,
                    format!(
                        "fuseki:contextPath \"{p}\": Sparkles serves datasets at the root, so a \
                         reverse proxy has to strip the prefix"
                    ),
                );
            }
        }
        for ep in ["pingEP", "statsEP", "metricsEP", "compactEP"] {
            if let Ok(Some(t)) = g.one(s, &f(ep)) {
                let on = boolean(t).unwrap_or(false);
                if ep == "metricsEP" && on {
                    plan.metrics_fuseki_names = true;
                    self.report.converted(
                        place,
                        "fuseki:metricsEP: /$/metrics, with Fuseki's metric names (--metrics-fuseki-names)",
                    );
                } else {
                    self.report.converted(
                        place,
                        format!("fuseki:{ep} {on}: Sparkles always serves /$/ping, /$/stats, /$/metrics and /$/compact"),
                    );
                }
            }
        }
        if let Ok(Some(t)) = g.one(s, &f("auth")) {
            match lexical(t)
                .or(iri(t))
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "basic" => self
                    .report
                    .converted(place, "fuseki:auth \"basic\": HTTP Basic"),
                "digest" => self.report.approximated(
                    place,
                    "fuseki:auth \"digest\": Sparkles authenticates users with HTTP Basic, which \
                     Jena's clients also answer",
                ),
                other => self.report.unsupported(
                    place,
                    format!("fuseki:auth \"{other}\" is not an HTTP scheme Sparkles has"),
                ),
            }
        }
        if !g.objects(s, &f("allowedUsers")).is_empty() {
            *allowed = Some(user_list(g, s));
        }
        for t in g.objects(s, &ja("loadClass")) {
            self.report.unsupported(
                place,
                format!(
                    "ja:loadClass {}: Sparkles cannot load Java code",
                    show_term(t)
                ),
            );
        }
        if let Ok(Some(t)) = g.one(s, &f("realm")) {
            plan.realm = lexical(t).map(String::from);
            self.report.converted(
                place,
                format!(
                    "fuseki:realm \"{}\": the realm of auth.toml",
                    lexical(t).unwrap_or("")
                ),
            );
        }
    }

    /// `ja:context [ ja:cxtName …; ja:cxtValue … ]` on `s`.
    pub(super) fn contexts(
        &mut self,
        g: &ConfigGraph,
        s: &NamedOrBlankNode,
        place: &str,
        scope: Scope,
    ) {
        for c in g.objects(s, &ja("context")) {
            let Some(c) = as_subject(c) else {
                continue;
            };
            let name = g
                .one(&c, &ja("cxtName"))
                .ok()
                .flatten()
                .and_then(|t| lexical(t).or(iri(t)))
                .unwrap_or("")
                .to_string();
            let value = g
                .one(&c, &ja("cxtValue"))
                .ok()
                .flatten()
                .and_then(|t| lexical(t).or(iri(t)))
                .unwrap_or("")
                .to_string();
            // `arq:queryTimeout`, or the symbol's full IRI
            let local = name
                .rsplit_once(['#', ':'])
                .map(|(_, l)| l)
                .unwrap_or(&name)
                .to_string();
            match local.as_str() {
                "queryTimeout" | "timeout" => match parse_timeout(&value) {
                    Some((secs, first)) => {
                        if first {
                            self.report.approximated(
                                place,
                                format!(
                                    "{name} \"{value}\": Sparkles has no limit on the time to the \
                                     first result, so only the overall {secs} s applies"
                                ),
                            );
                        }
                        match scope {
                            Scope::Server => self.server_query_timeout = Some(secs),
                            Scope::Local => self.timeouts.query.push((place.to_string(), secs)),
                        }
                    }
                    None => self.report.unsupported(
                        place,
                        format!("{name} \"{value}\" is not a timeout in milliseconds"),
                    ),
                },
                "updateTimeout" => match parse_timeout(&value) {
                    Some((secs, _)) => match scope {
                        Scope::Server => self.server_update_timeout = Some(secs),
                        Scope::Local => self.timeouts.update.push((place.to_string(), secs)),
                    },
                    None => self.report.unsupported(
                        place,
                        format!("{name} \"{value}\" is not a timeout in milliseconds"),
                    ),
                },
                "unionDefaultGraph" => {
                    let on = matches!(value.trim(), "true" | "1");
                    match scope {
                        Scope::Server => {
                            self.server_union = Some(on);
                            self.report.converted(place, format!("{name} {on}"));
                        }
                        Scope::Local => {
                            // recorded by the caller through `local_union`
                            self.local_union.push((place.to_string(), on));
                        }
                    }
                }
                _ => self.report.ignored(
                    place,
                    format!("context setting {name} has no Sparkles equivalent"),
                ),
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Server,
    Local,
}

/// `"10000"` or `"1000,60000"` (first result, overall) in milliseconds → seconds, and
/// whether a first-result part was given.
fn parse_timeout(v: &str) -> Option<(f64, bool)> {
    let parts: Vec<&str> = v.split(',').map(str::trim).collect();
    let ms: f64 = parts.last()?.parse().ok()?;
    if !(ms.is_finite() && ms >= 0.0) || parts.len() > 2 {
        return None;
    }
    Some((ms / 1000.0, parts.len() == 2))
}

/// The user names of `fuseki:allowedUsers` (one or a list, several times).
pub(super) fn user_list(g: &ConfigGraph, s: &NamedOrBlankNode) -> Vec<String> {
    g.values(s, &f("allowedUsers"))
        .iter()
        .map(|t| lexical(t).map(String::from).unwrap_or_else(|| show_term(t)))
        .collect()
}
