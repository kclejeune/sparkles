//! Services, their endpoints and the datasets they serve (spec G08 §5.1–§5.2).

use super::access::{DatasetAccess, EndpointAccess};
use super::convert::{
    Converter, DataFile, DatasetPlan, Mode, Scope, ServerPlan, f, ja, unknown, user_list,
};
use super::endpoints;
use super::graph::{
    ACCESS, ConfigGraph, FUSEKI, GEO, TDB1, TDB2, TEXT, as_subject, boolean, lexical, short, shown,
};
use anyhow::{Result, bail};
use oxrdf::{NamedOrBlankNode, Term};
use std::path::PathBuf;

/// The legacy endpoint properties of a service and their operations.
const LEGACY: [(&str, &str); 9] = [
    ("serviceQuery", "query"),
    ("serviceUpdate", "update"),
    ("serviceUpload", "upload"),
    ("serviceShacl", "shacl"),
    ("serviceReadWriteGraphStore", "gsp-rw"),
    ("serviceReadGraphStore", "gsp-r"),
    ("serviceReadWriteQuads", "gsp-rw"),
    ("serviceReadQuads", "gsp-r"),
    ("serviceUpdateQuads", "update"),
];

/// Graph access entries of a registry: user → graphs.
pub(super) type Registry = Vec<(String, Vec<String>)>;

impl Converter<'_> {
    /// One service: its name, endpoints and dataset.
    pub(super) fn service(
        &mut self,
        g: &ConfigGraph,
        gi: usize,
        svc: &NamedOrBlankNode,
        shared: &mut Vec<((usize, NamedOrBlankNode), String)>,
        server: &mut ServerPlan,
    ) -> Result<Option<(DatasetPlan, DatasetAccess)>> {
        let name = match g.one(svc, &f("name")) {
            Ok(Some(Term::Literal(l))) => l.value().trim().trim_start_matches('/').to_string(),
            _ => {
                self.report.unsupported(
                    format!("service {}", g.show(svc)),
                    "the service has no fuseki:name string, so Fuseki skips it",
                );
                return Ok(None);
            }
        };
        let place = format!("service /{name}");
        if !crate::state::valid_name(&name) {
            self.report.unsupported(
                &place,
                "the name is not a valid Sparkles dataset name (letters, digits, '.', '_' and \
                 '-', no '/')",
            );
            return Ok(None);
        }
        let mut known: Vec<String> =
            vec![f("name"), f("dataset"), f("endpoint"), f("allowedUsers")];
        known.extend(LEGACY.iter().map(|(p, _)| f(p)));
        let known: Vec<&str> = known.iter().map(String::as_str).collect();
        for t in unknown(g, svc, &known) {
            self.report.ignored(
                &place,
                format!(
                    "{} is not a service setting Sparkles knows",
                    short(t.predicate.as_str())
                ),
            );
        }

        let (eps, union_from_endpoints) = self.endpoints(g, svc, &place);
        self.check_endpoints(&name, &place, &eps, server);
        let service_allowed =
            (!g.objects(svc, &f("allowedUsers")).is_empty()).then(|| user_list(g, svc));

        let Some(ds_node) = g
            .one(svc, &f("dataset"))
            .ok()
            .flatten()
            .and_then(as_subject)
        else {
            self.report
                .unsupported(&place, "the service has no fuseki:dataset");
            return Ok(None);
        };
        // the dataset and the datasets it wraps
        let mut chain = vec![ds_node.clone()];
        let wrapped = [
            format!("{TEXT}dataset"),
            format!("{GEO}dataset"),
            format!("{ACCESS}dataset"),
            ja("dataset"),
        ];
        while chain.len() < 16 {
            let last = chain.last().expect("not empty");
            let next = wrapped
                .iter()
                .find_map(|p| g.one(last, p).ok().flatten().and_then(as_subject));
            match next {
                Some(n) if !chain.contains(&n) => chain.push(n),
                _ => break,
            }
        }
        if let Some((_, first)) = shared
            .iter()
            .find(|(k, _)| k.0 == gi && chain.contains(&k.1))
        {
            self.report.unsupported(
                &place,
                format!(
                    "the service shares its dataset with /{first}; Sparkles has no second name \
                     for a dataset, so the data is served at /{first} only"
                ),
            );
            return Ok(None);
        }
        shared.extend(chain.into_iter().map(|n| ((gi, n), name.clone())));
        let mut plan = DatasetPlan {
            name: name.clone(),
            tdb: None,
            tdb1: false,
            memory: true,
            data: Vec::new(),
            text: None,
            geo: None,
            rdfs: None,
            reasoning: None,
            union: union_from_endpoints,
        };
        let mut graphs = None;
        let dplace = format!("dataset /{name}");
        self.dataset(g, &ds_node, &mut plan, &mut graphs, &dplace, 0)?;
        let acc = DatasetAccess {
            name,
            service_allowed,
            endpoints: eps,
            graphs,
        };
        Ok(Some((plan, acc)))
    }

    /// The endpoints of a service, and a union-default-graph setting in their contexts.
    fn endpoints(
        &mut self,
        g: &ConfigGraph,
        svc: &NamedOrBlankNode,
        place: &str,
    ) -> (Vec<EndpointAccess>, Option<bool>) {
        let mut eps: Vec<EndpointAccess> = Vec::new();
        let mut union = None;
        for (p, op) in LEGACY {
            for o in g.objects(svc, &f(p)) {
                match o {
                    Term::Literal(l) => eps.push(EndpointAccess {
                        op: op.into(),
                        name: l.value().to_string(),
                        allowed: None,
                    }),
                    o => {
                        let Some(e) = as_subject(o) else { continue };
                        let ename = g
                            .one(&e, &f("name"))
                            .ok()
                            .flatten()
                            .and_then(lexical)
                            .unwrap_or("")
                            .to_string();
                        let allowed = (!g.objects(&e, &f("allowedUsers")).is_empty())
                            .then(|| user_list(g, &e));
                        eps.push(EndpointAccess {
                            op: op.into(),
                            name: ename,
                            allowed,
                        });
                    }
                }
            }
        }
        for e in g.objects(svc, &f("endpoint")) {
            let Some(e) = as_subject(e) else {
                self.report.unsupported(
                    place,
                    "a fuseki:endpoint is a literal, which Fuseki refuses",
                );
                continue;
            };
            for t in unknown(
                g,
                &e,
                &[
                    &f("operation"),
                    &f("name"),
                    &f("allowedUsers"),
                    &ja("context"),
                    &f("implementation"),
                ],
            ) {
                self.report.ignored(
                    place,
                    format!(
                        "{} on an endpoint has no Sparkles equivalent",
                        short(t.predicate.as_str())
                    ),
                );
            }
            let ename = g
                .one(&e, &f("name"))
                .ok()
                .flatten()
                .and_then(lexical)
                .unwrap_or("")
                .to_string();
            let eplace = if ename.is_empty() {
                place.to_string()
            } else {
                format!("{place}/{ename}")
            };
            if !g.objects(&e, &f("implementation")).is_empty() {
                self.report.unsupported(
                    &eplace,
                    "fuseki:implementation: Sparkles cannot run custom Java operations",
                );
                continue;
            }
            let op = match g.one(&e, &f("operation")).ok().flatten() {
                Some(Term::NamedNode(n)) => match n.as_str().strip_prefix(FUSEKI) {
                    Some(op) => endpoints::canonical(op).to_string(),
                    None => {
                        self.report.unsupported(
                            &eplace,
                            format!("the operation {} is not one of Fuseki's", short(n.as_str())),
                        );
                        continue;
                    }
                },
                _ => {
                    self.report
                        .unsupported(&eplace, "the endpoint has no fuseki:operation");
                    continue;
                }
            };
            self.contexts(g, &e, &eplace, Scope::Local);
            for (p, on) in std::mem::take(&mut self.local_union) {
                self.report.approximated(
                    p,
                    format!(
                        "unionDefaultGraph {on} on one endpoint applies to the whole dataset \
                         in Sparkles"
                    ),
                );
                union = Some(on);
            }
            let allowed = (!g.objects(&e, &f("allowedUsers")).is_empty()).then(|| user_list(g, &e));
            eps.push(EndpointAccess {
                op,
                name: ename,
                allowed,
            });
        }
        if eps.is_empty() {
            self.report.approximated(
                place,
                "the service declares no endpoints; Sparkles serves the usual ones",
            );
            for op in ["query", "update", "gsp-rw", "patch"] {
                eps.push(EndpointAccess {
                    op: op.into(),
                    name: String::new(),
                    allowed: None,
                });
            }
        }
        (eps, union)
    }

    /// Report each endpoint: served at its name, or not.
    fn check_endpoints(
        &mut self,
        name: &str,
        place: &str,
        eps: &[EndpointAccess],
        server: &mut ServerPlan,
    ) {
        let mut served = Vec::new();
        for e in eps {
            let shown = if e.name.is_empty() {
                format!("/{name}")
            } else {
                format!("/{name}/{}", e.name)
            };
            match endpoints::served_names(&e.op) {
                None => self.report.unsupported(
                    place,
                    format!(
                        "the operation fuseki:{} at {shown} has no Sparkles equivalent",
                        e.op
                    ),
                ),
                Some(names) if names.contains(&e.name.as_str()) => {
                    if e.op.starts_with("gsp-direct") {
                        server.gsp_direct_naming = true;
                        self.report.approximated(
                            place,
                            format!(
                                "fuseki:{}: --gsp-direct-naming, which applies to every dataset",
                                e.op
                            ),
                        );
                    } else if e.op != "no-op" {
                        served.push(format!("{} at {shown}", e.op));
                    }
                }
                Some(names) => {
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
                    self.report.unsupported(
                        place,
                        format!(
                            "fuseki:{} at {shown}: Sparkles serves it at {} only, so clients of \
                             {shown} find nothing there (a reverse proxy can rewrite the path)",
                            e.op,
                            at.join(", ")
                        ),
                    );
                }
            }
        }
        if !served.is_empty() {
            self.report
                .converted(place, format!("endpoints: {}", served.join(", ")));
        }
    }

    /// A dataset description, through its wrappers.
    pub(super) fn dataset(
        &mut self,
        g: &ConfigGraph,
        d: &NamedOrBlankNode,
        plan: &mut DatasetPlan,
        graphs: &mut Option<Registry>,
        place: &str,
        depth: usize,
    ) -> Result<()> {
        if depth > 16 {
            bail!("{place}: the dataset descriptions nest too deeply");
        }
        self.contexts(g, d, place, Scope::Local);
        for (_, on) in self.local_union.drain(..) {
            plan.union = Some(on);
        }
        let types = g.types(d);
        let is = |t: String| types.contains(&t);
        if is(format!("{TDB2}DatasetTDB2"))
            || is(format!("{TDB2}DatasetTDB"))
            || is(format!("{TDB1}DatasetTDB"))
        {
            return self.tdb(g, d, plan, place, is(format!("{TDB1}DatasetTDB")));
        }
        if is(ja("MemoryDataset")) || is(ja("DatasetTxnMem")) || is(ja("DatasetMemory")) {
            self.only(g, d, place, &[&ja("data")]);
            for t in g.objects(d, &ja("data")) {
                if let Some(p) = g.file_ref(t) {
                    self.data_file(plan, place, p, None);
                }
            }
            if plan.data.is_empty() {
                self.report.converted(place, "an in-memory dataset (--mem)");
            }
            return Ok(());
        }
        if is(format!("{TEXT}TextDataset")) {
            let dsp = format!("{TEXT}dataset");
            let index = format!("{TEXT}index");
            let producer = format!("{TEXT}textDocProducer");
            self.only(g, d, place, &[&dsp, &index, &producer]);
            if !g.objects(d, &producer).is_empty() {
                self.report.approximated(
                    place,
                    "text:textDocProducer: Sparkles indexes every literal of the mapped predicates",
                );
            }
            match g.one(d, &index).ok().flatten().and_then(as_subject) {
                Some(i) => plan.text = self.text_index(g, &i, place),
                None => self
                    .report
                    .unsupported(place, "the text dataset has no text:index"),
            }
            return self.inner(g, d, &dsp, plan, graphs, place, depth);
        }
        if is(format!("{GEO}GeosparqlDataset")) || is(format!("{GEO}geosparqlDataset")) {
            self.geosparql(g, d, plan, place);
            return self.inner(g, d, &format!("{GEO}dataset"), plan, graphs, place, depth);
        }
        if is(ja("DatasetRDFS")) {
            self.only(g, d, place, &[&ja("rdfsSchema"), &ja("dataset")]);
            match g
                .one(d, &ja("rdfsSchema"))
                .ok()
                .flatten()
                .and_then(|t| g.file_ref(t))
            {
                Some(p) => {
                    self.report.converted(
                        place,
                        format!(
                            "ja:DatasetRDFS: RDFS on read with the schema {} (--rdfs)",
                            shown(&p)
                        ),
                    );
                    plan.rdfs = Some(p);
                }
                None => self
                    .report
                    .unsupported(place, "ja:DatasetRDFS has no ja:rdfsSchema file"),
            }
            return self.inner(g, d, &ja("dataset"), plan, graphs, place, depth);
        }
        if is(format!("{ACCESS}AccessControlledDataset")) {
            let reg = format!("{ACCESS}registry");
            let dsp = format!("{ACCESS}dataset");
            self.only(g, d, place, &[&reg, &dsp]);
            match g.one(d, &reg).ok().flatten().and_then(as_subject) {
                Some(r) => *graphs = Some(self.registry(g, &r, place)),
                None => self.report.unsupported(
                    place,
                    "the access-controlled dataset has no access:registry",
                ),
            }
            return self.inner(g, d, &dsp, plan, graphs, place, depth);
        }
        if is(ja("RDFDataset")) {
            return self.rdf_dataset(g, d, plan, place);
        }
        if types.is_empty() {
            self.report
                .unsupported(place, "the dataset has no rdf:type, which Fuseki refuses");
        } else {
            let names: Vec<String> = types.iter().map(|t| short(t)).collect();
            self.report.unsupported(
                place,
                format!(
                    "the dataset type {} has no Sparkles equivalent",
                    names.join(", ")
                ),
            );
        }
        Ok(())
    }

    fn tdb(
        &mut self,
        g: &ConfigGraph,
        d: &NamedOrBlankNode,
        plan: &mut DatasetPlan,
        place: &str,
        tdb1: bool,
    ) -> Result<()> {
        let ns = if tdb1 { TDB1 } else { TDB2 };
        let loc_p = format!("{ns}location");
        let union_p = format!("{ns}unionDefaultGraph");
        self.only(g, d, place, &[&loc_p, &union_p]);
        if let Ok(Some(u)) = g.one(d, &union_p) {
            plan.union = Some(boolean(u).unwrap_or(false));
        }
        match g.one(d, &loc_p).ok().flatten() {
            Some(t) if lexical(t) == Some("--mem--") => {
                self.report
                    .converted(place, "an in-memory TDB dataset (--mem)");
            }
            Some(t) => {
                let loc = g.file_ref(t).unwrap_or_default();
                plan.memory = false;
                plan.tdb = Some(loc.clone());
                plan.tdb1 = tdb1;
                self.report.manual(
                    place,
                    format!(
                        "the {} database at {} becomes a Sparkles database; load.sh exports it \
                         with {} and loads the dump",
                        if tdb1 { "TDB1" } else { "TDB2" },
                        shown(&loc),
                        if tdb1 { "tdbdump" } else { "tdb2.tdbdump" }
                    ),
                );
            }
            None => self
                .report
                .unsupported(place, "the TDB dataset has no location"),
        }
        Ok(())
    }

    /// The dataset a wrapper wraps.
    #[allow(clippy::too_many_arguments)]
    fn inner(
        &mut self,
        g: &ConfigGraph,
        d: &NamedOrBlankNode,
        p: &str,
        plan: &mut DatasetPlan,
        graphs: &mut Option<Registry>,
        place: &str,
        depth: usize,
    ) -> Result<()> {
        match g.one(d, p).ok().flatten().and_then(as_subject) {
            Some(inner) => self.dataset(g, &inner, plan, graphs, place, depth + 1),
            None => {
                self.report
                    .unsupported(place, format!("{} names no dataset", short(p)));
                Ok(())
            }
        }
    }

    /// Report the properties of a description that the converter does not read.
    pub(super) fn only(
        &mut self,
        g: &ConfigGraph,
        d: &NamedOrBlankNode,
        place: &str,
        known: &[&str],
    ) {
        let ctx = ja("context");
        let mut k: Vec<&str> = known.to_vec();
        k.push(&ctx);
        for t in unknown(g, d, &k) {
            let p = t.predicate.as_str();
            self.report.ignored(
                place,
                format!("{} is not a setting Sparkles knows", short(p)),
            );
        }
    }

    pub(super) fn data_file(
        &mut self,
        plan: &mut DatasetPlan,
        place: &str,
        path: PathBuf,
        graph: Option<String>,
    ) {
        let into = match &graph {
            Some(g) => format!(" into <{g}>"),
            None => String::new(),
        };
        match self.mode {
            Mode::Files => self.report.approximated(
                place,
                format!(
                    "{}{into}: Fuseki reads it into memory at each start and forgets writes, \
                     and load.sh loads it once into a persistent database",
                    shown(&path)
                ),
            ),
            Mode::Serve => self.report.converted(
                place,
                format!("{}{into}: loaded into memory at each start", shown(&path)),
            ),
        }
        if !path.is_file() {
            self.report.manual(
                place,
                format!(
                    "{} does not exist here; correct its path in load.sh",
                    shown(&path)
                ),
            );
        }
        plan.data.push(DataFile { path, graph });
    }
}
