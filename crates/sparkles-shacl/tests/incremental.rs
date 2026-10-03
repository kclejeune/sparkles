//! Incremental write-time validation equals full validation (C10 A15): random shapes
//! over the core constraint components, every path form, `sh:targetWhere` targets and
//! SHACL-SPARQL constraints and components (anchored at the focus node or not), random
//! data and random writes. After every write the guard's decision, counts and listed results are checked
//! against full validations of the states before and after it.

use serde_json::Value as J;
use sparkles_core::Error;
use sparkles_core::guard::{
    GuardMode, GuardStatus, Severity, SeverityCounts, Strategy, WriteOptions,
};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::guard::{
    self, BaselinePolicy, DataGraphSel, SetOutcome, ShaclGuard, ShapesSource, ValidationConfig,
};
use sparkles_shacl::incremental::Tuning;
use sparkles_shacl::{Shapes, ValidationResult};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const EX: &str = "http://ex.org/";
const SH: &str = "http://www.w3.org/ns/shacl#";

/// xorshift64*
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const PREDS: usize = 5;
const CLASSES: usize = 4;
const NODES: usize = 12;

fn node(i: usize) -> String {
    format!("<{EX}n{i}>")
}
fn pred(i: usize) -> String {
    format!("<{EX}p{i}>")
}
fn class(i: usize) -> String {
    format!("<{EX}C{i}>")
}

/// An object: a node, an integer, a string or a language-tagged string.
fn object(r: &mut Rng) -> String {
    match r.below(10) {
        0..=5 => node(r.below(NODES)),
        6 | 7 => format!("{}", r.below(6)),
        8 => format!("\"{}\"", r.pick(&["a", "b", "ab", ""])),
        _ => format!("\"{}\"@{}", r.pick(&["x", "y"]), r.pick(&["en", "fr"])),
    }
}

/// A random data triple (N-Triples, without the final dot); with `lists`, some are list
/// cells.
fn triple(r: &mut Rng, lists: bool) -> String {
    let s = if r.chance(3) {
        format!("<{EX}ghost>")
    } else {
        node(r.below(NODES))
    };
    match r.below(if lists { 12 } else { 10 }) {
        0 | 1 => format!(
            "{s} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> {}",
            class(r.below(CLASSES))
        ),
        // list cells, for the list constraints: well formed or not
        10 => format!(
            "{s} <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> {}",
            node(r.below(NODES))
        ),
        11 => format!(
            "{s} <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> {}",
            if r.chance(40) {
                "<http://www.w3.org/1999/02/22-rdf-syntax-ns#nil>".to_string()
            } else {
                node(r.below(NODES))
            }
        ),
        _ => {
            let o = match object(r) {
                // integers and strings as N-Triples literals
                o if o.chars().all(|c| c.is_ascii_digit()) => {
                    format!("\"{o}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
                }
                o => o,
            };
            format!("{s} {} {o}", pred(r.below(PREDS)))
        }
    }
}

fn subclass(r: &mut Rng) -> String {
    let a = r.below(CLASSES);
    let b = (a + 1 + r.below(CLASSES - 1)) % CLASSES;
    format!(
        "{} <http://www.w3.org/2000/01/rdf-schema#subClassOf> {}",
        class(a),
        class(b)
    )
}

/// Random shapes: every shape and SPARQL constraint has an IRI, so reports of separate
/// parses compare.
struct ShapeGen<'r> {
    r: &'r mut Rng,
    out: String,
    n: usize,
    helpers: usize,
    /// also the SHACL 1.2 list constraints
    lists: bool,
}

impl ShapeGen<'_> {
    fn fresh(&mut self, kind: &str) -> String {
        self.n += 1;
        format!("<{EX}{kind}{}>", self.n)
    }

    fn path(&mut self, depth: usize) -> String {
        let r = if depth == 0 { 0 } else { self.r.below(12) };
        match r {
            0..=4 => pred(self.r.below(PREDS)),
            5 | 6 => format!("[ sh:inversePath {} ]", self.path(depth - 1)),
            7 => format!("( {} {} )", self.path(depth - 1), self.path(depth - 1)),
            8 => format!(
                "[ sh:alternativePath ( {} {} ) ]",
                self.path(depth - 1),
                self.path(depth - 1)
            ),
            9 => format!("[ sh:zeroOrMorePath {} ]", self.path(depth - 1)),
            10 => format!("[ sh:oneOrMorePath {} ]", self.path(depth - 1)),
            _ => format!("[ sh:zeroOrOnePath {} ]", self.path(depth - 1)),
        }
    }

    /// A helper shape (no targets) that refers only to helpers after it, so the
    /// references are acyclic.
    fn helper(&mut self, i: usize) -> String {
        format!("<{EX}H{}>", (i + 1 + self.r.below(3)).min(self.helpers + 2))
    }

    /// Constraints of a property shape (or the value constraints of a node shape).
    fn constraints(&mut self, depth: usize, from: usize) -> String {
        let mut cs = Vec::new();
        let mut qualified = false;
        for _ in 0..1 + self.r.below(3) {
            let c = match self.r.below(if self.lists { 28 } else { 24 }) {
                0 => format!("sh:minCount {}", self.r.below(3)),
                1 => format!("sh:maxCount {}", 1 + self.r.below(2)),
                2 | 3 => format!("sh:class {}", class(self.r.below(CLASSES))),
                4 => format!(
                    "sh:datatype {}",
                    self.r
                        .pick(&["xsd:integer", "xsd:string", "rdf:langString"])
                ),
                5 => format!(
                    "sh:nodeKind {}",
                    self.r.pick(&["sh:IRI", "sh:Literal", "sh:BlankNodeOrIRI"])
                ),
                6 => format!("sh:minInclusive {}", self.r.below(4)),
                7 => format!("sh:maxExclusive {}", 1 + self.r.below(4)),
                8 => format!("sh:minLength {}", self.r.below(3)),
                9 => "sh:pattern \"^a\"".into(),
                10 => "sh:languageIn ( \"en\" )".into(),
                11 => "sh:uniqueLang true".into(),
                12 => format!("sh:equals {}", pred(self.r.below(PREDS))),
                13 => format!("sh:disjoint {}", pred(self.r.below(PREDS))),
                14 => format!(
                    "{} {}",
                    self.r.pick(&["sh:lessThan", "sh:lessThanOrEquals"]),
                    pred(self.r.below(PREDS))
                ),
                15 => format!("sh:hasValue {}", node(self.r.below(NODES))),
                16 => format!(
                    "sh:in ( {} {} 1 \"a\" )",
                    node(self.r.below(NODES)),
                    node(self.r.below(NODES))
                ),
                17 | 18 if from <= self.helpers => {
                    format!("sh:node {}", self.helper(from))
                }
                19 if from <= self.helpers => format!("sh:not {}", self.helper(from)),
                20 if from <= self.helpers => format!(
                    "{} ( {} {} )",
                    self.r.pick(&["sh:or", "sh:and", "sh:xone"]),
                    self.helper(from),
                    self.helper(from)
                ),
                // a shape has at most one qualified value shape (SHACL §4.7.3)
                21 if from <= self.helpers && !qualified => {
                    qualified = true;
                    format!(
                        "sh:qualifiedValueShape {} ; sh:{} {}",
                        self.helper(from),
                        self.r.pick(&["qualifiedMinCount", "qualifiedMaxCount"]),
                        self.r.below(2)
                    )
                }
                22 if depth > 0 => format!("sh:property {}", self.property(depth - 1, from)),
                // SHACL 1.2 list constraints
                24 => format!("sh:minListLength {}", 1 + self.r.below(2)),
                25 => format!("sh:maxListLength {}", self.r.below(3)),
                26 => format!("sh:uniqueMembers {}", self.r.pick(&["true", "false"])),
                27 if from <= self.helpers => format!("sh:memberShape {}", self.helper(from)),
                _ => format!("sh:maxCount {}", 2 + self.r.below(2)),
            };
            cs.push(c);
        }
        if self.r.chance(20) {
            cs.push("sh:severity sh:Warning".into());
        }
        cs.join(" ; ")
    }

    /// A property shape; returns its IRI.
    fn property(&mut self, depth: usize, from: usize) -> String {
        let iri = self.fresh("ps");
        let path = self.path(2);
        let cs = self.constraints(depth, from);
        self.out.push_str(&format!(
            "{iri} a sh:PropertyShape ; sh:path {path} ; {cs} .\n"
        ));
        iri
    }

    /// A shape for `sh:targetWhere`: a helper, or one that narrows the nodes that may
    /// conform (by class, value or a required property) or does not (`sh:not`).
    fn where_shape(&mut self) -> String {
        match self.r.below(6) {
            0 => format!("[ sh:class {} ]", class(self.r.below(CLASSES))),
            1 => format!(
                "[ sh:property [ sh:path {} ; sh:minCount 1 ; sh:maxCount {} ] ]",
                pred(self.r.below(PREDS)),
                1 + self.r.below(2)
            ),
            2 => format!("[ sh:not [ sh:class {} ] ]", class(self.r.below(CLASSES))),
            3 => format!("[ sh:hasValue {} ]", node(self.r.below(NODES))),
            4 => format!(
                "[ sh:property [ sh:path [ sh:inversePath {} ] ; sh:minCount 1 ] ; sh:nodeKind sh:IRI ]",
                pred(self.r.below(PREDS))
            ),
            _ => self.helper(0),
        }
    }

    fn targets(&mut self) -> String {
        let mut ts = Vec::new();
        for _ in 0..1 + self.r.below(2) {
            if self.r.chance(15) {
                let w = self.where_shape();
                ts.push(format!("sh:targetWhere {w}"));
                continue;
            }
            ts.push(match self.r.below(5) {
                0 | 1 => format!("sh:targetClass {}", class(self.r.below(CLASSES))),
                2 => format!("sh:targetSubjectsOf {}", pred(self.r.below(PREDS))),
                3 => format!("sh:targetObjectsOf {}", pred(self.r.below(PREDS))),
                _ => format!(
                    "sh:targetNode {}",
                    if self.r.chance(30) {
                        format!("<{EX}ghost>")
                    } else {
                        node(self.r.below(NODES))
                    }
                ),
            });
        }
        ts.join(" ; ")
    }

    fn generate(mut self, sparql: bool, recursive: bool) -> String {
        self.out.push_str(
            "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
             @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
        );
        // helpers H1..H{helpers+2}; the last ones have no references
        let last = self.helpers + 3;
        for i in 1..last {
            let iri = format!("<{EX}H{i}>");
            let cs = if self.r.chance(50) {
                // a value constraint at the node itself
                self.constraints(0, if i <= self.helpers { i } else { usize::MAX })
            } else {
                let from = if i <= self.helpers { i } else { usize::MAX };
                format!("sh:property {}", self.property(1, from))
            };
            self.out
                .push_str(&format!("{iri} a sh:NodeShape ; {cs} .\n"));
        }
        for _ in 0..2 + self.r.below(3) {
            let iri = self.fresh("S");
            let t = self.targets();
            let mut cs = Vec::new();
            for _ in 0..1 + self.r.below(3) {
                cs.push(format!("sh:property {}", self.property(1, 0)));
            }
            if self.r.chance(25) {
                cs.push(self.constraints(0, 0));
            }
            if self.r.chance(15) {
                let p = self.fresh("ps");
                self.out.push_str(&format!(
                    "{p} a sh:PropertyShape ; sh:path {} ; sh:minCount 0 .\n",
                    pred(self.r.below(PREDS))
                ));
                cs.push(format!(
                    "sh:property {p} ; sh:closed true ; sh:ignoredProperties ( rdf:type )"
                ));
            }
            self.out.push_str(&format!(
                "{iri} a sh:NodeShape ; {t} ; {} .\n",
                cs.join(" ; ")
            ));
        }
        if sparql {
            self.out.push_str(&format!(
                "<{EX}HasType> a sh:ConstraintComponent ;\n\
                   sh:parameter [ sh:path <{EX}requiredType> ] ;\n\
                   sh:validator [ a sh:SPARQLAskValidator ;\n\
                     sh:ask \"ASK {{ $value a $requiredType }}\" ] .\n"
            ));
            for _ in 0..1 + self.r.below(2) {
                let iri = self.fresh("Q");
                let t = self.targets();
                let c = self.fresh("sparql");
                let q = self.query();
                self.out.push_str(&format!(
                    "{iri} a sh:NodeShape ; {t} ; sh:sparql {c} .\n{c} sh:select \"{q}\" .\n"
                ));
            }
            if self.r.chance(50) {
                let iri = self.fresh("Q");
                let t = self.targets();
                let path = self.path(1);
                let k = class(self.r.below(CLASSES));
                let ps = self.fresh("ps");
                self.out.push_str(&format!(
                    "{iri} a sh:NodeShape ; {t} ; sh:property {ps} .\n\
                     {ps} sh:path {path} ; <{EX}requiredType> {k} .\n"
                ));
            }
        }
        if recursive {
            self.out.push_str(&format!(
                "<{EX}R> a sh:NodeShape ; sh:targetSubjectsOf {} ; sh:property <{EX}Rp> .\n\
                 <{EX}Rp> sh:path {} ; sh:node <{EX}R> ; sh:maxCount 2 .\n",
                pred(1),
                pred(2)
            ));
        }
        self.out
    }
}

impl ShapeGen<'_> {
    /// A SHACL-SPARQL query: most are anchored at `$this`, some are not.
    fn query(&mut self) -> String {
        let a = pred(self.r.below(PREDS));
        let b = pred(self.r.below(PREDS));
        let c = pred(self.r.below(PREDS));
        let k = class(self.r.below(CLASSES));
        match self.r.below(9) {
            0 => format!(
                "SELECT $this ?value WHERE {{ $this {a} ?value . FILTER NOT EXISTS {{ ?value {b} ?x }} }}"
            ),
            // uniqueness: another node with the same value
            1 => format!(
                "SELECT $this ?value WHERE {{ $this {a} ?value . ?other {a} ?value . FILTER(?other != $this) }}"
            ),
            2 => format!(
                "SELECT $this WHERE {{ $this {a} ?x OPTIONAL {{ ?x {b} ?y }} FILTER(!BOUND(?y)) }}"
            ),
            3 => format!(
                "SELECT $this ?value WHERE {{ {{ $this {a} ?value }} UNION {{ ?value {b} $this }} FILTER NOT EXISTS {{ ?value a {k} }} }}"
            ),
            4 => format!(
                "SELECT $this WHERE {{ $this {a}/{b}* ?v . ?v {c} ?w . FILTER(isLiteral(?w)) }}"
            ),
            5 => format!(
                "SELECT $this ?value WHERE {{ $this ?p ?value . FILTER(?p = {a} && isIRI(?value)) FILTER EXISTS {{ ?value ?q $this }} }}"
            ),
            6 => format!(
                "SELECT $this WHERE {{ $this {a} ?x }} GROUP BY $this HAVING (COUNT(?x) > 1)"
            ),
            // not anchored: these read the whole data graph
            7 => {
                format!("SELECT $this WHERE {{ $this {a} ?x . FILTER NOT EXISTS {{ ?y a {k} }} }}")
            }
            _ => format!(
                "SELECT $this WHERE {{ ?x {a} $this . ?y {b} ?x . FILTER EXISTS {{ ?z {c} ?y }} FILTER NOT EXISTS {{ ?q {c} ?q }} }}"
            ),
        }
    }
}

fn shapes_text(r: &mut Rng, sparql: bool, recursive: bool, lists: bool) -> String {
    let helpers = 1 + r.below(3);
    ShapeGen {
        r,
        out: String::new(),
        n: 0,
        helpers,
        lists,
    }
    .generate(sparql, recursive)
}

/// The results of a full validation of `data` (as JSON, sorted) and their counts.
fn full(data: &BTreeSet<String>, shapes: &Shapes) -> (Vec<String>, SeverityCounts) {
    let s = Store::in_memory(StoreOptions::default());
    if !data.is_empty() {
        let nt: String = data.iter().map(|t| format!("{t} .\n")).collect();
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    }
    let report = sparkles_shacl::validate(&s.snapshot(), shapes, &Default::default()).unwrap();
    let mut c = SeverityCounts::default();
    for r in &report.results {
        match r.severity.as_str().strip_prefix(SH) {
            Some("Warning") => c.warning += 1,
            Some("Info") => c.info += 1,
            _ => c.violation += 1,
        }
    }
    (sorted(&report.results), c)
}

fn sorted(rs: &[ValidationResult]) -> Vec<String> {
    let mut v: Vec<String> = rs
        .iter()
        .map(|r| identity(&sparkles_shacl::report::result_json(r)))
        .collect();
    v.sort();
    v
}

/// A result without its messages (they may list a shape's predicates in another
/// order after another parse of the same shapes) and details, as the guard matches
/// results.
fn identity(r: &J) -> String {
    let mut r = r.clone();
    r.as_object_mut().unwrap().remove("messages");
    r.as_object_mut().unwrap().remove("details");
    r.to_string()
}

/// `a − b` as multisets of sorted lists.
fn minus(a: &[String], b: &[String]) -> Vec<String> {
    let mut left: BTreeMap<&String, usize> = BTreeMap::new();
    for x in b {
        *left.entry(x).or_default() += 1;
    }
    a.iter()
        .filter(|x| match left.get_mut(x) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

fn blocking(c: &SeverityCounts) -> u64 {
    c.violation
}

fn is_blocking(json: &str) -> bool {
    !(json.contains("shacl#Warning") || json.contains("shacl#Info"))
}

fn config(mode: GuardMode, baseline: BaselinePolicy, shapes: &str) -> ValidationConfig {
    ValidationConfig {
        format: 2,
        language: None,
        mode,
        shapes: ShapesSource {
            inline: Some(shapes.to_string()),
            ..Default::default()
        },
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
        baseline,
        timeout_seconds: 60.0,
        report_limit: 10_000,
        updated: None,
    }
}

#[derive(Default, Debug)]
struct Tally {
    /// validated incrementally though they changed `rdfs:subClassOf`
    subclass_incremental: usize,
    strategies: BTreeMap<String, usize>,
    fallbacks: BTreeMap<String, usize>,
    rejected: usize,
    writes: usize,
}

/// Run one random scenario; returns what it saw.
fn scenario(seed: u64, mode: GuardMode, policy: BaselinePolicy, lists: bool, tally: &mut Tally) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let sparql = r.chance(30);
    let recursive = r.chance(20);
    let text = shapes_text(&mut r, sparql, recursive, lists);
    let shapes = Shapes::parse(&text, RdfFormat::Turtle, None)
        .unwrap_or_else(|e| panic!("seed {seed}: {e:#}\n{text}"));
    let store = Store::in_memory(StoreOptions::default());
    let mut data: BTreeSet<String> = BTreeSet::new();
    // strict reject starts from no data (which conforms unless a target node is missing
    // a value); the others from random data
    if !(mode == GuardMode::Reject && policy == BaselinePolicy::Strict) {
        for _ in 0..40 + r.below(40) {
            data.insert(triple(&mut r, lists));
        }
        if r.chance(60) {
            data.insert(subclass(&mut r));
        }
        let nt: String = data.iter().map(|t| format!("{t} .\n")).collect();
        store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let g: Arc<ShaclGuard> = match guard::set_config(&store, Some(config(mode, policy, &text))) {
        Ok(SetOutcome::Installed(g, _)) => g,
        Ok(SetOutcome::NotConforming(_)) => return,
        Ok(SetOutcome::Removed) => unreachable!(),
        Err(e) => panic!("seed {seed}: {e:#}"),
    };
    g.set_tuning(Tuning {
        max_focus: 30,
        max_share: 1.0,
        share_floor: usize::MAX,
        max_visit: 400,
    });
    let (mut before, _) = full(&data, &shapes);
    for step in 0..steps() {
        // the write: some deletes of present triples, some inserts
        let mut del = Vec::new();
        let mut ins = Vec::new();
        let big = r.chance(3);
        let n = if big { 25 } else { 1 + r.below(3) };
        for _ in 0..n {
            if r.chance(35) && !data.is_empty() {
                let i = r.below(data.len());
                del.push(data.iter().nth(i).unwrap().clone());
            } else if r.chance(4) {
                ins.push(subclass(&mut r));
            } else {
                ins.push(triple(&mut r, lists));
            }
        }
        let bypass = r.chance(2);
        let mut after_data = data.clone();
        for t in &del {
            after_data.remove(t);
        }
        for t in &ins {
            after_data.insert(t.clone());
        }
        let mut u = String::new();
        if !del.is_empty() {
            u.push_str(&format!("DELETE DATA {{ {} }} ;\n", del.join(" . ")));
        }
        u.push_str(&format!("INSERT DATA {{ {} }}", ins.join(" . ")));
        let opts = QueryOptions {
            write: WriteOptions {
                bypass_validation: bypass,
                ..Default::default()
            },
            ..Default::default()
        };
        let (after, after_c) = full(&after_data, &shapes);
        let data_text = data.iter().cloned().collect::<Vec<_>>().join(" .\n");
        let ctx = || {
            format!("seed {seed} step {step} {mode:?} {policy:?}\n{u}\n{text}\nDATA\n{data_text}")
        };
        let res = update(&store, &u, &opts);
        tally.writes += 1;
        let introduced = minus(&after, &before);
        let new_blocking = introduced.iter().filter(|j| is_blocking(j)).count() as u64;
        let should_reject = !bypass
            && after_data != data
            && mode == GuardMode::Reject
            && match policy {
                BaselinePolicy::Strict => blocking(&after_c) > 0,
                BaselinePolicy::Grandfather => new_blocking > 0,
            };
        let summary = match res {
            Err(Error::Rejected(rej)) => {
                assert!(
                    should_reject,
                    "rejected but full validation passes: {}",
                    ctx()
                );
                tally.rejected += 1;
                Some(rej.summary)
            }
            Err(e) => panic!("{e}: {}", ctx()),
            Ok(st) => {
                let v = st.commit.and_then(|c| c.validation).map(|v| (*v).clone());
                assert!(
                    !should_reject,
                    "accepted but full validation rejects: {:?}\n{}",
                    v.as_ref()
                        .map(|v| (v.status, v.strategy, &v.fallback, v.blocking)),
                    ctx()
                );
                v
            }
        };
        let committed = !should_reject;
        if let Some(s) = &summary {
            *tally
                .strategies
                .entry(format!("{:?}", s.strategy))
                .or_default() += 1;
            if let Some(f) = &s.fallback {
                *tally.fallbacks.entry(f.clone()).or_default() += 1;
            }
            if s.strategy == Strategy::Incremental
                && ins.iter().chain(&del).any(|t| t.contains("subClassOf"))
            {
                tally.subclass_incremental += 1;
            }
            match s.status {
                GuardStatus::Bypassed => {}
                GuardStatus::Skipped => {
                    // a skipped write cannot change the results
                    assert_eq!(
                        after,
                        before,
                        "skipped write changed the results: {}",
                        ctx()
                    );
                }
                _ => {
                    assert_eq!(
                        s.by_severity,
                        after_c,
                        "counts ({:?} {:?} focus {:?}, new {:?}, gone {:?}): {}",
                        s.strategy,
                        s.fallback,
                        s.focus_nodes,
                        introduced,
                        minus(&before, &after),
                        ctx()
                    );
                    assert_eq!(s.total as usize, after.len(), "total: {}", ctx());
                    assert_eq!(s.blocking, blocking(&after_c), "blocking: {}", ctx());
                    let listed: Vec<String> = {
                        let mut v: Vec<String> = s.results.iter().map(identity).collect();
                        v.sort();
                        v
                    };
                    assert!(
                        minus(&listed, &after).is_empty(),
                        "listed results the state does not have: {:?}\nstate: {:?}\n{:?} {:?}\n{}",
                        minus(&listed, &after),
                        minus(&after, &listed),
                        s.strategy,
                        s.fallback,
                        ctx()
                    );
                    assert!(
                        minus(&introduced, &listed).is_empty(),
                        "new results not listed: {:?}\n{}",
                        minus(&introduced, &listed),
                        ctx()
                    );
                    if policy == BaselinePolicy::Grandfather {
                        assert_eq!(s.introduced, Some(new_blocking), "introduced: {}", ctx());
                    }
                    if s.strategy == Strategy::Full {
                        assert_eq!(listed, after, "full listing: {}", ctx());
                    }
                }
            }
        }
        if committed {
            data = after_data;
            before = after;
            // the guard's baseline is the exact state when it knows it
            if let Some(b) = g.status().baseline
                && b.conforms.is_some()
            {
                assert_eq!(b.by_severity, after_c, "baseline: {}", ctx());
            }
        }
    }
}

/// Scenarios per test (`SPARKLES_DIFF_SEEDS`, default 6).
fn seeds() -> u64 {
    std::env::var("SPARKLES_DIFF_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
}

/// Writes per scenario (`SPARKLES_DIFF_STEPS`, default 60).
fn steps() -> usize {
    std::env::var("SPARKLES_DIFF_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
}

fn run(mode: GuardMode, policy: BaselinePolicy, first: u64) -> Tally {
    run_with(mode, policy, first, false)
}

fn run_with(mode: GuardMode, policy: BaselinePolicy, first: u64, lists: bool) -> Tally {
    let mut t = Tally::default();
    // `SPARKLES_DIFF_SEED=n` runs one scenario of every test
    let only: Option<u64> = std::env::var("SPARKLES_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok());
    let range = match only {
        Some(n) => n..n + 1,
        None => first..first + seeds(),
    };
    for seed in range {
        scenario(seed, mode, policy, lists, &mut t);
    }
    eprintln!("{mode:?} {policy:?}: {t:?}");
    t
}

#[test]
fn warn_mode_counts_and_results_equal_full_validation() {
    let t = run(GuardMode::Warn, BaselinePolicy::Strict, 0);
    assert!(
        t.strategies.get("Incremental").copied().unwrap_or(0) > t.writes / 4,
        "{t:?}"
    );
    for f in ["baseline", "sparql", "recursive", "budget"] {
        assert!(t.fallbacks.contains_key(f), "fallback {f} not hit: {t:?}");
    }
    assert!(t.subclass_incremental > 0, "{t:?}");
}

#[test]
fn strict_reject_decisions_equal_full_validation() {
    let t = run(GuardMode::Reject, BaselinePolicy::Strict, 100);
    assert!(t.rejected > 0, "{t:?}");
    assert!(
        t.strategies.get("Incremental").copied().unwrap_or(0) > 0,
        "{t:?}"
    );
}

#[test]
fn grandfather_rejects_only_new_blocking_results() {
    let t = run(GuardMode::Reject, BaselinePolicy::Grandfather, 200);
    assert!(t.rejected > 0, "{t:?}");
    assert!(
        t.strategies.get("Incremental").copied().unwrap_or(0) > t.writes / 4,
        "{t:?}"
    );
}

/// Shapes with the SHACL 1.2 list constraints over data with list cells, well formed or
/// not: the incremental results equal full validation's.
#[test]
fn list_constraints_equal_full_validation() {
    let t = run_with(GuardMode::Warn, BaselinePolicy::Strict, 300, true);
    assert!(
        t.strategies.get("Incremental").copied().unwrap_or(0) > t.writes / 4,
        "{t:?}"
    );
    let t = run_with(GuardMode::Reject, BaselinePolicy::Grandfather, 400, true);
    assert!(t.rejected > 0, "{t:?}");
}
