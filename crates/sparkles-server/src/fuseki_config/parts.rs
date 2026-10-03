//! Text indexes, GeoSPARQL, graph access registries, assembled datasets and inference
//! (spec G08 §5.2–§5.5, §6.3), and the server-wide settings settled from the datasets'.

use super::convert::{Converter, DatasetPlan, Reasoning, SCHEMA_GRAPH, ServerPlan, ja};
use super::graph::{
    ACCESS, ConfigGraph, GEO, TDB1, TDB2, TEXT, as_subject, boolean, iri, lexical, short,
    show_term, shown,
};
use super::service::Registry;
use anyhow::Result;
use oxrdf::{NamedOrBlankNode, Term};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The language tags Sparkles has a stemming analyzer for.
const STEMMED: [&str; 20] = [
    "ar", "da", "de", "el", "en", "es", "fi", "fr", "hu", "it", "nl", "no", "nb", "nn", "pt", "ro",
    "ru", "sv", "ta", "tr",
];

const REASONERS: &str = "http://jena.hpl.hp.com/2003/";

fn t(l: &str) -> String {
    format!("{TEXT}{l}")
}

/// A graph of a TDB database in a slot of a dataset: the slot (none: the default
/// graph), the database's location, whether it is TDB1, and the graph's name.
type TdbSlot = (Option<String>, Option<PathBuf>, bool, Option<String>);

/// What a model of an `ja:RDFDataset` holds.
enum Model {
    /// data files
    Files(Vec<PathBuf>),
    /// a graph of a TDB database: its location, and the graph name (none: the default
    /// graph)
    Tdb {
        location: Option<PathBuf>,
        tdb1: bool,
        graph: Option<String>,
    },
    /// something the converter cannot follow (already reported)
    Unsupported,
}

impl Converter<'_> {
    /// `text:TextIndexLucene` → `text.json`.
    pub(super) fn text_index(
        &mut self,
        g: &ConfigGraph,
        i: &NamedOrBlankNode,
        place: &str,
    ) -> Option<Value> {
        if !g.has_type(i, &t("TextIndexLucene")) {
            let types: Vec<String> = g.types(i).iter().map(|x| short(x)).collect();
            self.report.unsupported(
                place,
                format!(
                    "the text index {} is not a Lucene index, which is the only kind Sparkles \
                     converts",
                    types.join(", ")
                ),
            );
            return None;
        }
        let mut languages: Vec<String> = Vec::new();
        let mut all_languages = false;
        for (p, v) in g
            .about(i)
            .map(|tr| (tr.predicate.as_str().to_string(), tr.object.clone()))
        {
            let local = p.strip_prefix(TEXT).unwrap_or("");
            match local {
                "entityMap" | "" => {}
                "directory" => self.report.ignored(
                    place,
                    format!(
                        "text:directory {}: Sparkles keeps the index in the database",
                        show_term(&v)
                    ),
                ),
                "storeValues" => self.report.ignored(
                    place,
                    "text:storeValues: Sparkles can always return and highlight the literal",
                ),
                "multilingualSupport" => {
                    if boolean(&v) == Some(true) {
                        all_languages = true;
                        self.report.converted(
                            place,
                            "text:multilingualSupport: every language Sparkles stems (\"languages\": \"all\")",
                        );
                    }
                }
                "analyzer" | "queryAnalyzer" | "defineAnalyzers" => {
                    if let Some(a) = as_subject(&v)
                        && let Some(lang) = self.localized(g, &a)
                    {
                        languages.push(lang);
                    }
                    self.report.approximated(
                        place,
                        format!(
                            "text:{local} {}: Sparkles splits words, lowercases and folds to \
                             ASCII, and stems only the languages in \"languages\"",
                            self.analyzer_name(g, &v)
                        ),
                    );
                }
                "propLists" => self.report.approximated(
                    place,
                    "text:propLists: a search names one or more predicates instead",
                ),
                "ignoreIndexErrors" | "maxBasicQueries" | "cacheQueries" | "queryParser" => self
                    .report
                    .ignored(place, format!("text:{local} has no Sparkles equivalent")),
                _ if p.starts_with("http://www.w3.org/") => {}
                _ => self.report.ignored(
                    place,
                    format!(
                        "{} on the text index is not a setting Sparkles knows",
                        short(&p)
                    ),
                ),
            }
        }
        let Some(em) = g
            .one(i, &t("entityMap"))
            .ok()
            .flatten()
            .and_then(as_subject)
        else {
            self.report
                .unsupported(place, "the text index has no text:entityMap");
            return None;
        };
        for f in ["entityField", "uidField", "graphField", "langField"] {
            if let Ok(Some(v)) = g.one(&em, &t(f)) {
                self.report.ignored(
                    place,
                    format!(
                        "text:{f} {}: Sparkles always records subjects, graphs and languages",
                        show_term(v)
                    ),
                );
            }
        }
        let default_field = g
            .one(&em, &t("defaultField"))
            .ok()
            .flatten()
            .and_then(lexical)
            .map(String::from);
        let mut predicates: Vec<String> = Vec::new();
        let mut fields: Vec<(String, Vec<String>)> = Vec::new();
        for entry in g.values(&em, &t("map")) {
            let Some(e) = as_subject(&entry) else {
                continue;
            };
            let field = g
                .one(&e, &t("field"))
                .ok()
                .flatten()
                .and_then(lexical)
                .unwrap_or("")
                .to_string();
            let preds: Vec<String> = g
                .values(&e, &t("predicate"))
                .iter()
                .filter_map(|p| iri(p).map(String::from))
                .collect();
            if g.one(&e, &t("noIndex")).ok().flatten().and_then(boolean) == Some(true) {
                self.report.ignored(
                    place,
                    format!("text:noIndex field \"{field}\": only stored, not searched"),
                );
                continue;
            }
            if let Some(a) = g.one(&e, &t("analyzer")).ok().flatten() {
                if let Some(s) = as_subject(a)
                    && let Some(lang) = self.localized(g, &s)
                {
                    languages.push(lang);
                }
                self.report.approximated(
                    place,
                    format!(
                        "the analyzer {} of field \"{field}\": Sparkles splits words, lowercases \
                         and folds to ASCII, and stems only the languages in \"languages\"",
                        self.analyzer_name(g, a)
                    ),
                );
            }
            for p in &preds {
                if !predicates.contains(p) {
                    predicates.push(p.clone());
                }
            }
            match fields.iter_mut().find(|(f, _)| *f == field) {
                Some((_, v)) => v.extend(preds),
                None => fields.push((field, preds)),
            }
        }
        if predicates.is_empty() {
            self.report.unsupported(
                place,
                "the entity map maps no predicate, so there is nothing to index",
            );
            return None;
        }
        let listed: Vec<String> = fields
            .iter()
            .map(|(f, ps)| {
                let ps: Vec<String> = ps.iter().map(|p| short(p)).collect();
                format!("\"{f}\" = {}", ps.join(" "))
            })
            .collect();
        self.report.converted(
            place,
            format!(
                "text index: the fields {} become the indexed predicates (--text)",
                listed.join(", ")
            ),
        );
        if fields.len() > 1 {
            let df = default_field.unwrap_or_default();
            self.report.approximated(
                place,
                format!(
                    "a text:query without a property searches every indexed predicate in \
                     Sparkles, and only the default field \"{df}\" in Jena"
                ),
            );
        }
        let mut cfg = json!({ "predicates": predicates });
        if all_languages {
            cfg["languages"] = json!("all");
        } else if !languages.is_empty() {
            languages.sort();
            languages.dedup();
            cfg["languages"] = json!(languages);
        }
        Some(cfg)
    }

    /// The language of a `text:LocalizedAnalyzer` that Sparkles stems.
    fn localized(&mut self, g: &ConfigGraph, a: &NamedOrBlankNode) -> Option<String> {
        if !g.has_type(a, &t("LocalizedAnalyzer")) {
            return None;
        }
        let lang = g
            .one(a, &t("language"))
            .ok()
            .flatten()
            .and_then(lexical)?
            .to_ascii_lowercase();
        let primary = lang.split('-').next().unwrap_or("").to_string();
        STEMMED.contains(&primary.as_str()).then_some(primary)
    }

    fn analyzer_name(&self, g: &ConfigGraph, a: &Term) -> String {
        match as_subject(a) {
            Some(s) => {
                let types: Vec<String> = g.types(&s).iter().map(|x| short(x)).collect();
                if types.is_empty() {
                    show_term(a)
                } else {
                    types.join(" ")
                }
            }
            None => show_term(a),
        }
    }

    /// `geosparql:GeosparqlDataset` → `geo.json` and inference.
    pub(super) fn geosparql(
        &mut self,
        g: &ConfigGraph,
        d: &NamedOrBlankNode,
        plan: &mut DatasetPlan,
        place: &str,
    ) {
        let p = |l: &str| format!("{GEO}{l}");
        let flag = |l: &str, dflt: bool| -> bool {
            g.one(d, &p(l))
                .ok()
                .flatten()
                .and_then(boolean)
                .unwrap_or(dflt)
        };
        let rewrite = flag("queryRewrite", true);
        let inference = flag("inference", true);
        let default_geometry = flag("applyDefaultGeometry", false);
        plan.geo = Some(json!({ "queryRewrite": rewrite }));
        self.report.converted(
            place,
            format!("GeoSPARQL dataset: a spatial index (--geo) with queryRewrite {rewrite}"),
        );
        if inference || default_geometry {
            let r = plan.reasoning.get_or_insert_with(Reasoning::default);
            r.geo_vocab |= inference;
            r.default_geometry |= default_geometry;
            if r.profile.is_none() && r.rule_files.is_empty() && r.rule_texts.is_empty() {
                r.profile = Some("rdfs");
            }
            let what = match (inference, default_geometry) {
                (true, true) => "the GeoSPARQL schema's RDFS inference and geo:hasDefaultGeometry",
                (true, false) => "the GeoSPARQL schema's RDFS inference",
                _ => "geo:hasDefaultGeometry",
            };
            self.report.converted(
                place,
                format!("{what}: sparkles infer --vocab geosparql in load.sh"),
            );
        }
        for (l, why) in [
            (
                "spatialIndexFile",
                "Sparkles keeps the spatial index in the database",
            ),
            ("indexEnabled", "it sizes Jena's caches"),
            ("indexSizes", "it sizes Jena's caches"),
            ("indexExpiries", "it sizes Jena's caches"),
            ("indexExpires", "it sizes Jena's caches"),
            (
                "spatialIndexPerGraph",
                "Sparkles' index always records graphs",
            ),
        ] {
            if !g.objects(d, &p(l)).is_empty() {
                self.report.ignored(place, format!("geosparql:{l}: {why}"));
            }
        }
        if let Ok(Some(v)) = g.one(d, &p("srsUri")) {
            self.report.approximated(
                place,
                format!(
                    "geosparql:srsUri {}: Sparkles' index works on WGS 84 whatever the preferred \
                     CRS, and functions convert between CRSs",
                    show_term(v)
                ),
            );
        }
    }

    /// `access:SecurityRegistry` → user → graphs.
    pub(super) fn registry(
        &mut self,
        g: &ConfigGraph,
        r: &NamedOrBlankNode,
        place: &str,
    ) -> Registry {
        let mut out: Registry = Vec::new();
        let mut add =
            |user: String, graphs: Vec<String>| match out.iter_mut().find(|(u, _)| *u == user) {
                Some((_, v)) => {
                    for x in graphs {
                        if !v.contains(&x) {
                            v.push(x);
                        }
                    }
                }
                None => out.push((user, graphs)),
            };
        let name = |t: &Term| -> Option<String> {
            match t {
                Term::NamedNode(n) => Some(n.as_str().to_string()),
                Term::Literal(l) => Some(l.value().to_string()),
                _ => None,
            }
        };
        for e in g.objects(r, &format!("{ACCESS}entry")) {
            match g.list(e) {
                Some(members) => {
                    let mut it = members.iter();
                    let Some(user) = it.next().and_then(lexical) else {
                        continue;
                    };
                    add(user.to_string(), it.filter_map(name).collect());
                }
                None => {
                    let Some(s) = as_subject(e) else { continue };
                    let Some(user) = g
                        .one(&s, &format!("{ACCESS}user"))
                        .ok()
                        .flatten()
                        .and_then(lexical)
                    else {
                        continue;
                    };
                    let graphs = g
                        .values(&s, &format!("{ACCESS}graphs"))
                        .iter()
                        .filter_map(name)
                        .collect();
                    add(user.to_string(), graphs);
                }
            }
        }
        let users: Vec<String> = out
            .iter()
            .map(|(u, gs)| format!("{u} ({})", gs.len()))
            .collect();
        self.report.converted(
            place,
            format!(
                "graph access control: read grants limited to the graphs of each entry, for {}",
                users.join(", ")
            ),
        );
        out
    }

    /// `ja:RDFDataset` with a default graph and named graphs.
    pub(super) fn rdf_dataset(
        &mut self,
        g: &ConfigGraph,
        d: &NamedOrBlankNode,
        plan: &mut DatasetPlan,
        place: &str,
    ) -> Result<()> {
        self.only(
            g,
            d,
            place,
            &[&ja("defaultGraph"), &ja("namedGraph"), &ja("graph")],
        );
        let mut slots: Vec<(Option<String>, NamedOrBlankNode)> = Vec::new();
        for p in ["defaultGraph", "graph"] {
            if let Some(m) = g.one(d, &ja(p)).ok().flatten().and_then(as_subject) {
                slots.push((None, m));
            }
        }
        for ng in g.objects(d, &ja("namedGraph")) {
            let Some(ng) = as_subject(ng) else { continue };
            let name = g
                .one(&ng, &ja("graphName"))
                .ok()
                .flatten()
                .and_then(|t| iri(t).or(lexical(t)))
                .map(String::from);
            let model = g.one(&ng, &ja("graph")).ok().flatten().and_then(as_subject);
            match (name, model) {
                (Some(n), Some(m)) => slots.push((Some(n), m)),
                _ => self
                    .report
                    .unsupported(place, "a ja:namedGraph without ja:graphName and ja:graph"),
            }
        }
        // (slot, location, TDB1, graph name)
        let mut tdb: Vec<TdbSlot> = Vec::new();
        for (slot, m) in &slots {
            match self.model(g, m, plan, slot.as_deref(), place, 0) {
                Model::Files(files) => {
                    for p in files {
                        self.data_file(plan, place, p, slot.clone());
                    }
                }
                Model::Tdb {
                    location,
                    tdb1,
                    graph,
                } => tdb.push((slot.clone(), location, tdb1, graph)),
                Model::Unsupported => {}
            }
        }
        if tdb.is_empty() {
            return Ok(());
        }
        // graphs of one TDB database: only its whole default graph (or union graph) maps
        // to a Sparkles dataset
        let locations: Vec<&Option<PathBuf>> = tdb.iter().map(|(_, l, _, _)| l).collect();
        let one_db = locations.windows(2).all(|w| w[0] == w[1]);
        let renamed: Vec<String> = tdb
            .iter()
            .filter(|(slot, _, _, graph)| match slot {
                None => !matches!(graph.as_deref(), None | Some("urn:x-arq:UnionGraph")),
                Some(_) => true,
            })
            .map(|(slot, _, _, graph)| {
                format!(
                    "{} ← {}",
                    slot.as_deref().unwrap_or("default graph"),
                    graph.as_deref().unwrap_or("default graph")
                )
            })
            .collect();
        if !one_db || !renamed.is_empty() {
            self.report.unsupported(
                place,
                format!(
                    "the dataset selects or renames graphs of TDB databases ({}); Sparkles \
                     serves whole datasets, and a clone can copy some graphs into a new one \
                     (POST /$/datasets/{{ds}}/clone)",
                    if renamed.is_empty() {
                        "several databases".to_string()
                    } else {
                        renamed.join(", ")
                    }
                ),
            );
            return Ok(());
        }
        let (_, location, tdb1, graph) = tdb.remove(0);
        if graph.as_deref() == Some("urn:x-arq:UnionGraph") {
            plan.union = Some(true);
        }
        match location {
            Some(loc) => {
                plan.memory = false;
                self.report.manual(
                    place,
                    format!(
                        "the {} database at {} becomes a Sparkles database; load.sh exports it \
                         and loads the dump",
                        if tdb1 { "TDB1" } else { "TDB2" },
                        shown(&loc)
                    ),
                );
                plan.tdb = Some(loc);
                plan.tdb1 = tdb1;
            }
            None => self
                .report
                .converted(place, "an in-memory TDB dataset (--mem)"),
        }
        self.report.approximated(
            place,
            "Fuseki served only the default graph of the TDB database here, and Sparkles \
             serves its named graphs too",
        );
        Ok(())
    }

    /// A model of an `ja:RDFDataset`.
    fn model(
        &mut self,
        g: &ConfigGraph,
        m: &NamedOrBlankNode,
        plan: &mut DatasetPlan,
        slot: Option<&str>,
        place: &str,
        depth: usize,
    ) -> Model {
        if depth > 8 {
            self.report.unsupported(place, "the models nest too deeply");
            return Model::Unsupported;
        }
        let types = g.types(m);
        let is = |x: String| types.contains(&x);
        if is(format!("{TDB2}GraphTDB2"))
            || is(format!("{TDB2}GraphTDB"))
            || is(format!("{TDB1}GraphTDB"))
        {
            let tdb1 = is(format!("{TDB1}GraphTDB"));
            let ns = if tdb1 { TDB1 } else { TDB2 };
            let graph = g
                .one(m, &format!("{ns}graphName"))
                .ok()
                .flatten()
                .and_then(|t| iri(t).or(lexical(t)))
                .map(String::from);
            let location = match g
                .one(m, &format!("{ns}dataset"))
                .ok()
                .flatten()
                .and_then(as_subject)
            {
                Some(ds) => g.one(&ds, &format!("{ns}location")).ok().flatten().cloned(),
                None => g.one(m, &format!("{ns}location")).ok().flatten().cloned(),
            };
            let location = match location {
                Some(t) if lexical(&t) == Some("--mem--") => None,
                Some(t) => g.file_ref(&t),
                None => None,
            };
            return Model::Tdb {
                location,
                tdb1,
                graph,
            };
        }
        if is(ja("InfModel")) {
            if slot.is_some() {
                self.report.unsupported(
                    place,
                    "an inference model as a named graph: Sparkles materializes inferences of \
                     the default graph",
                );
                return Model::Unsupported;
            }
            self.only(g, m, place, &[&ja("baseModel"), &ja("reasoner")]);
            if let Some(r) = g
                .one(m, &ja("reasoner"))
                .ok()
                .flatten()
                .and_then(as_subject)
            {
                self.reasoner(g, &r, plan, place);
            } else {
                self.report
                    .unsupported(place, "the ja:InfModel has no ja:reasoner");
            }
            return match g
                .one(m, &ja("baseModel"))
                .ok()
                .flatten()
                .and_then(as_subject)
            {
                Some(b) => self.model(g, &b, plan, slot, place, depth + 1),
                None => Model::Files(Vec::new()),
            };
        }
        if is(ja("UnionModel")) {
            self.report.unsupported(
                place,
                "ja:UnionModel: Sparkles has no graph made of other graphs, other than the \
                 union of all named graphs (--union-default-graph)",
            );
            return Model::Unsupported;
        }
        if is(ja("MemoryModel")) || is(ja("DefaultModel")) || is(ja("Model")) || types.is_empty() {
            return Model::Files(self.content(g, m, place));
        }
        let names: Vec<String> = types.iter().map(|x| short(x)).collect();
        self.report.unsupported(
            place,
            format!(
                "the model type {} has no Sparkles equivalent",
                names.join(", ")
            ),
        );
        Model::Unsupported
    }

    /// The files of `ja:content [ ja:externalContent <file> ]` (and `ja:initialContent`).
    fn content(&mut self, g: &ConfigGraph, m: &NamedOrBlankNode, place: &str) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for p in ["content", "initialContent"] {
            for c in g.objects(m, &ja(p)) {
                let Some(c) = as_subject(c) else { continue };
                for e in g.objects(&c, &ja("externalContent")) {
                    if let Some(f) = g.file_ref(e) {
                        files.push(f);
                    }
                }
                if !g.objects(&c, &ja("literalContent")).is_empty() {
                    self.report.unsupported(
                        place,
                        "ja:literalContent: put the data in a file and load it",
                    );
                }
            }
        }
        for e in g.objects(m, &ja("externalContent")) {
            if let Some(f) = g.file_ref(e) {
                files.push(f);
            }
        }
        files
    }

    /// `ja:reasoner [ ja:reasonerURL …; ja:schema …; ja:rulesFrom …; ja:rule … ]`.
    fn reasoner(
        &mut self,
        g: &ConfigGraph,
        r: &NamedOrBlankNode,
        plan: &mut DatasetPlan,
        place: &str,
    ) {
        let url = g
            .one(r, &ja("reasonerURL"))
            .ok()
            .flatten()
            .and_then(iri)
            .unwrap_or("")
            .to_string();
        let local = url.strip_prefix(REASONERS).unwrap_or(&url).to_string();
        let mut reasoning = plan.reasoning.take().unwrap_or_default();
        let rule_files: Vec<PathBuf> = g
            .objects(r, &ja("rulesFrom"))
            .into_iter()
            .filter_map(|t| g.file_ref(t))
            .collect();
        let rule_texts: Vec<String> = g
            .objects(r, &ja("rule"))
            .into_iter()
            .filter_map(lexical)
            .map(String::from)
            .collect();
        match local.as_str() {
            "RDFSExptRuleReasoner" | "RDFSRuleReasoner" => {
                reasoning.profile = Some("rdfs");
                self.report.converted(
                    place,
                    format!("the {local}: RDFS materialized (--profile rdfs)"),
                );
            }
            "TransitiveReasoner" => {
                reasoning.profile = Some("rdfs");
                self.report.approximated(
                    place,
                    "the TransitiveReasoner: RDFS materialized, which includes the transitive \
                     closure of rdfs:subClassOf and rdfs:subPropertyOf",
                );
            }
            "OWLFBRuleReasoner"
            | "OWLMiniFBRuleReasoner"
            | "OWLMicroFBRuleReasoner"
            | "OWLReasoner" => {
                reasoning.profile = Some("owl-rl");
                self.report.approximated(
                    place,
                    format!(
                        "the {local}: OWL 2 RL materialized (--profile owl-rl), which differs \
                         from Jena's OWL rule sets in coverage"
                    ),
                );
            }
            "GenericRuleReasoner" => {
                if rule_files.is_empty() && rule_texts.is_empty() {
                    self.report.unsupported(
                        place,
                        "the GenericRuleReasoner has no ja:rulesFrom or ja:rule",
                    );
                } else {
                    reasoning.profile = None;
                    self.report.converted(
                        place,
                        "the GenericRuleReasoner's rules: materialized with sparkles infer --rules \
                         (backward rules are not materialized)",
                    );
                }
                if !g.objects(r, &ja("mode")).is_empty() {
                    self.report
                        .approximated(place, "ja:mode: the rules are materialized forward");
                }
            }
            "" => self
                .report
                .unsupported(place, "the reasoner has no ja:reasonerURL"),
            other => self.report.unsupported(
                place,
                format!("the reasoner <{other}> has no Sparkles equivalent"),
            ),
        }
        reasoning.rule_files.extend(rule_files);
        reasoning.rule_texts.extend(rule_texts);
        for s in g.objects(r, &ja("schema")) {
            match as_subject(s) {
                Some(sm) => {
                    let files = self.content(g, &sm, place);
                    if files.is_empty() {
                        self.report
                            .unsupported(place, "ja:schema names no file content");
                    }
                    for fpath in files {
                        self.report.converted(
                            place,
                            format!(
                                "ja:schema {}: loaded into <{SCHEMA_GRAPH}> and read as the \
                                 ontology",
                                shown(&fpath)
                            ),
                        );
                        reasoning.schema.push(fpath);
                    }
                }
                None => self.report.unsupported(place, "ja:schema is not a model"),
            }
        }
        plan.reasoning = Some(reasoning);
    }

    /// The server's timeouts from all the contexts found.
    pub(super) fn settle_timeouts(&mut self, server: &mut ServerPlan) {
        let settle = |c: &mut Self,
                      server_value: Option<f64>,
                      local: Vec<(String, f64)>,
                      what: &str,
                      flag: &str|
         -> Option<f64> {
            let chosen = match server_value {
                Some(v) => {
                    c.report
                        .converted("server", format!("{what} timeout {v} s ({flag} {v})"));
                    Some(v)
                }
                None => local.iter().map(|(_, v)| *v).reduce(f64::max),
            };
            if let Some(v) = chosen {
                for (place, x) in &local {
                    if *x == v && server_value.is_none() {
                        c.report.approximated(
                            place,
                            format!("{what} timeout {x} s: Sparkles' timeouts apply to every dataset ({flag} {v})"),
                        );
                    } else {
                        c.report.approximated(
                            place,
                            format!("{what} timeout {x} s: Sparkles' timeouts apply to every dataset, so this one gets {v} s ({flag})"),
                        );
                    }
                }
            }
            chosen
        };
        let q = std::mem::take(&mut self.timeouts.query);
        let u = std::mem::take(&mut self.timeouts.update);
        let sq = self.server_query_timeout;
        let su = self.server_update_timeout;
        server.timeout = settle(self, sq, q, "query", "--timeout");
        server.update_timeout = settle(self, su, u, "update", "--update-timeout");
    }

    /// `--union-default-graph` when every dataset asks for it.
    pub(super) fn settle_union(&mut self, server: &mut ServerPlan, datasets: &mut [DatasetPlan]) {
        let dflt = self.server_union.unwrap_or(false);
        let eff: Vec<bool> = datasets.iter().map(|d| d.union.unwrap_or(dflt)).collect();
        if eff.is_empty() {
            return;
        }
        if eff.iter().all(|u| *u) {
            server.union_default_graph = true;
            if self.server_union != Some(true) {
                self.report.converted(
                    "server",
                    "every dataset's default graph is the union of its named graphs (--union-default-graph)",
                );
            }
        } else if eff.iter().any(|u| *u) {
            for (d, u) in datasets.iter().zip(&eff) {
                if *u {
                    self.report.unsupported(
                        format!("dataset /{}", d.name),
                        "its default graph is the union of its named graphs, which Sparkles \
                         sets for the whole server (--union-default-graph) and other datasets \
                         do not use; query GRAPH <urn:x-arq:UnionGraph> instead",
                    );
                }
            }
        }
    }
}
