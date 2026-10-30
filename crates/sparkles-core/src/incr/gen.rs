//! A grammar-driven generator of queries, histories and protections for the
//! differential tests (spec F13 §11). Test only.
//!
//! The queries come from the incremental fragment of spec F13 §4.1 (basic graph
//! patterns, `GRAPH`, `FILTER`, `BIND`, `UNION`, `OPTIONAL`, `MINUS`, `EXISTS`, `VALUES`,
//! `DISTINCT`, `REDUCED`, projection, subqueries and `GROUP BY` with `COUNT`, `SUM` and
//! `AVG`) and from the operators that compile to `recompute` nodes (`MIN`, `MAX`,
//! `HAVING`, `ORDER BY` with `LIMIT`, and paths with `+` and `*`). They read the
//! vocabulary the histories write, so most commits touch most queries. Each query is
//! deterministic: it has one answer per snapshot, so the circuit and the engine can be
//! compared row for row. `SAMPLE`, `GROUP_CONCAT` and an `ORDER BY` that leaves ties
//! under its `LIMIT` are left out, since the engine may answer them either way.
//!
//! The text uses the prefixes `:` (`http://ex/`) and `rdfs:`, which the tests declare.
//! Everything is drawn from one seeded generator, so a seed names a case.

use crate::access::{
    Caller, GraphAccess, GraphRule, Graphs, Limits, Protection, Rule, TripleRules,
};
use rand::RngExt;
use rand::rngs::StdRng;
use std::sync::Arc;

const SUBJECTS: &[&str] = &[":a", ":b", ":c", ":d", ":e"];
const PREDICATES: &[&str] = &[":memberOf", ":knows", ":age", "rdfs:label", ":left", "a"];
const GRAPHS: &[&str] = &[":g1", ":g2"];

/// The objects that `p` takes in the histories.
fn objects(p: &str) -> &'static [&'static str] {
    match p {
        ":memberOf" => &[":t1", ":t2", ":t3"],
        ":knows" => &[":a", ":b", ":c", ":d", ":e"],
        ":age" => &["1", "2", "30", "40", "2.5", "\"x\""],
        "rdfs:label" => &["\"x\"", "\"y\"", "\"x\"@en", "1"],
        ":left" => &["true", "false"],
        "a" => &[":Secret", ":Team"],
        _ => &[":t1", "1", "\"x\""],
    }
}

/// One step of a history.
#[derive(Clone, Debug)]
pub enum Step {
    /// a transaction, as one update request
    Update(String),
    /// a compaction, which switches the generation
    Compact,
    /// a pass of the circuit and a comparison with the oracle
    Check,
}

fn pick<'a, T>(rng: &mut StdRng, xs: &'a [T]) -> &'a T {
    &xs[rng.random_range(0..xs.len())]
}

/// A random quad of the histories' vocabulary, as an `INSERT DATA` or `DELETE DATA`
/// body.
fn quad(rng: &mut StdRng) -> String {
    let s = pick(rng, SUBJECTS);
    let p = pick(rng, PREDICATES);
    let o = pick(rng, objects(p));
    let t = format!("{s} {p} {o}");
    if rng.random_bool(0.25) {
        format!("GRAPH {} {{ {t} }}", pick(rng, GRAPHS))
    } else {
        t
    }
}

/// A random history of `n` transactions, with checks after most of them. It holds
/// small transactions, bulk commits, transactions that add and remove one quad, and
/// compactions.
pub fn history(rng: &mut StdRng, n: usize) -> Vec<Step> {
    let mut out = Vec::new();
    for _ in 0..n {
        let r = rng.random_range(0..20);
        let update = match r {
            // a bulk commit
            0 | 1 => {
                let quads: Vec<String> = (0..rng.random_range(6..20)).map(|_| quad(rng)).collect();
                let verb = if rng.random_bool(0.7) {
                    "INSERT"
                } else {
                    "DELETE"
                };
                format!("{verb} DATA {{ {} }}", quads.join(" . "))
            }
            // one quad added and removed, in either order
            2 | 3 => {
                let q = quad(rng);
                if rng.random_bool(0.5) {
                    format!("INSERT DATA {{ {q} }} ; DELETE DATA {{ {q} }}")
                } else {
                    format!("DELETE DATA {{ {q} }} ; INSERT DATA {{ {q} }}")
                }
            }
            // a delete by pattern
            4 => format!(
                "DELETE WHERE {{ {} {} ?o }}",
                pick(rng, SUBJECTS),
                pick(rng, PREDICATES)
            ),
            _ => {
                let ops: Vec<String> = (0..rng.random_range(1..4))
                    .map(|_| {
                        let verb = if rng.random_bool(0.6) {
                            "INSERT"
                        } else {
                            "DELETE"
                        };
                        format!("{verb} DATA {{ {} }}", quad(rng))
                    })
                    .collect();
                ops.join(" ; ")
            }
        };
        out.push(Step::Update(update));
        if rng.random_bool(0.06) {
            out.push(Step::Compact);
        }
        if rng.random_bool(0.7) {
            out.push(Step::Check);
        }
    }
    out.push(Step::Check);
    out
}

/// Random starting data: a few quads of every predicate.
pub fn data(rng: &mut StdRng) -> String {
    let quads: Vec<String> = (0..rng.random_range(4..16)).map(|_| quad(rng)).collect();
    format!("INSERT DATA {{ {} }}", quads.join(" . "))
}

/// A pattern's text, as the elements of a group, with the variables it may bind.
struct Pat {
    text: String,
    vars: Vec<String>,
}

/// The state of one query's generation.
pub struct QueryGen<'a> {
    rng: &'a mut StdRng,
    next: usize,
}

impl<'a> QueryGen<'a> {
    pub fn new(rng: &'a mut StdRng) -> QueryGen<'a> {
        QueryGen { rng, next: 0 }
    }

    fn fresh(&mut self) -> String {
        let v = format!("?v{}", self.next);
        self.next += 1;
        v
    }

    fn chance(&mut self, p: f64) -> bool {
        self.rng.random_bool(p)
    }

    /// A variable of `scope` most of the time, else a fresh one.
    fn var(&mut self, scope: &[String], reuse: f64) -> String {
        if !scope.is_empty() && self.chance(reuse) {
            pick(self.rng, scope).clone()
        } else {
            self.fresh()
        }
    }

    fn predicate(&mut self) -> String {
        let r = self.rng.random_range(0..40);
        match r {
            0 => ":knows/:memberOf".into(),
            1 => ":knows|:memberOf".into(),
            2 => ":knows?".into(),
            3 => "^:knows".into(),
            4 => ":knows+".into(),
            5 => ":knows*".into(),
            _ => pick(self.rng, PREDICATES).to_string(),
        }
    }

    fn triple(&mut self, scope: &[String]) -> Pat {
        let p = self.predicate();
        let mut vars = Vec::new();
        let s = if self.chance(0.85) {
            let v = self.var(scope, 0.6);
            vars.push(v.clone());
            v
        } else {
            pick(self.rng, SUBJECTS).to_string()
        };
        let o = if self.chance(0.75) {
            let mut sc: Vec<String> = scope.to_vec();
            sc.extend(vars.iter().cloned());
            let v = self.var(&sc, 0.3);
            vars.push(v.clone());
            v
        } else {
            let base = p.trim_end_matches(['+', '*', '?']);
            pick(self.rng, objects(base)).to_string()
        };
        vars.dedup();
        Pat {
            text: format!("{s} {p} {o} ."),
            vars,
        }
    }

    /// A deterministic boolean expression over `vars`.
    fn condition(&mut self, vars: &[String]) -> String {
        if vars.is_empty() {
            return "true".into();
        }
        let x = pick(self.rng, vars).clone();
        let y = pick(self.rng, vars).clone();
        let r = self.rng.random_range(0..16);
        match r {
            0 => format!("{x} > 1"),
            1 => format!("{x} = :t1"),
            2 => format!("{x} != \"x\""),
            3 => format!("BOUND({x})"),
            4 => format!("!BOUND({x})"),
            5 => format!("isIRI({x})"),
            6 => format!("isLiteral({x})"),
            7 => format!("STR({x}) < \"http://ex/c\""),
            8 => format!("({x} > 1 || {y} = :a)"),
            9 => format!("{x} + 1 > 2"),
            10 => format!("{x} IN (1, :t1, \"x\")"),
            11 => format!("sameTerm({x}, {y})"),
            12 => format!("{x} = {y}"),
            13 => format!("REGEX(STR({x}), \"t\")"),
            14 => format!("COALESCE({x}, 0) >= 1"),
            _ => format!("({x} < 40 && BOUND({y}))"),
        }
    }

    /// A deterministic expression over `vars` for a `BIND`.
    fn expression(&mut self, vars: &[String]) -> String {
        if vars.is_empty() {
            return "1".into();
        }
        let x = pick(self.rng, vars).clone();
        let r = self.rng.random_range(0..9);
        match r {
            0 => format!("{x} + 1"),
            1 => format!("STR({x})"),
            2 => format!("CONCAT(STR({x}), \"-\")"),
            3 => format!("IF(BOUND({x}), 1, 0)"),
            4 => format!("COALESCE({x}, 0)"),
            5 => format!("UCASE(STR({x}))"),
            6 => format!("STRLEN(STR({x}))"),
            7 => format!("isIRI({x})"),
            _ => "\"k\"".into(),
        }
    }

    /// A constant of the histories' vocabulary, or `UNDEF`.
    fn constant(&mut self) -> String {
        let all = [
            ":a", ":b", ":t1", ":t2", "1", "30", "\"x\"", "true", "UNDEF",
        ];
        pick(self.rng, &all).to_string()
    }

    fn pattern(&mut self, depth: usize, scope: &[String]) -> Pat {
        if depth == 0 {
            return self.triple(scope);
        }
        let r = self.rng.random_range(0..26);
        let d = depth - 1;
        match r {
            // a join of two patterns
            0..=5 => {
                let a = self.pattern(d, scope);
                let mut sc = scope.to_vec();
                sc.extend(a.vars.iter().cloned());
                let b = self.pattern(d, &sc);
                Pat {
                    text: format!("{} {}", a.text, b.text),
                    vars: union(&a.vars, &b.vars),
                }
            }
            6 | 7 => {
                let a = self.pattern(d, scope);
                let b = self.pattern(d, &with(scope, &a.vars));
                Pat {
                    text: format!("{} OPTIONAL {{ {} }}", a.text, b.text),
                    vars: union(&a.vars, &b.vars),
                }
            }
            8 | 9 => {
                let a = self.pattern(d, scope);
                let b = self.pattern(d, scope);
                Pat {
                    text: format!("{{ {} }} UNION {{ {} }}", a.text, b.text),
                    vars: union(&a.vars, &b.vars),
                }
            }
            10 => {
                let a = self.pattern(d, scope);
                let b = self.pattern(d, &a.vars);
                Pat {
                    text: format!("{} MINUS {{ {} }}", a.text, b.text),
                    vars: a.vars,
                }
            }
            11 | 12 => {
                let a = self.pattern(d, scope);
                let b = self.pattern(d, &a.vars);
                let not = if self.chance(0.5) { "NOT " } else { "" };
                Pat {
                    text: format!("{} FILTER {not}EXISTS {{ {} }}", a.text, b.text),
                    vars: a.vars,
                }
            }
            13..=15 => {
                let a = self.pattern(d, scope);
                let c = self.condition(&a.vars);
                Pat {
                    text: format!("{} FILTER({c})", a.text),
                    vars: a.vars,
                }
            }
            16 | 17 => {
                let a = self.pattern(d, scope);
                let e = self.expression(&a.vars);
                let v = self.fresh();
                Pat {
                    text: format!("{} BIND({e} AS {v})", a.text),
                    vars: union(&a.vars, std::slice::from_ref(&v)),
                }
            }
            18 => {
                let v = self.var(scope, 0.7);
                let n = self.rng.random_range(1..4);
                let vals: Vec<String> = (0..n).map(|_| self.constant()).collect();
                let a = self.pattern(d, &with(scope, std::slice::from_ref(&v)));
                Pat {
                    text: format!("VALUES {v} {{ {} }} {}", vals.join(" "), a.text),
                    vars: union(std::slice::from_ref(&v), &a.vars),
                }
            }
            19 => {
                let a = self.pattern(d, scope);
                let g = pick(self.rng, GRAPHS);
                Pat {
                    text: format!("GRAPH {g} {{ {} }}", a.text),
                    vars: a.vars,
                }
            }
            20 => {
                let g = self.fresh();
                let a = self.pattern(d, scope);
                Pat {
                    text: format!("GRAPH {g} {{ {} }}", a.text),
                    vars: union(std::slice::from_ref(&g), &a.vars),
                }
            }
            21 | 22 => {
                // a subquery projecting some of its variables
                let a = self.pattern(d, &[]);
                if a.vars.is_empty() {
                    return a;
                }
                let keep: Vec<String> = a
                    .vars
                    .iter()
                    .filter(|_| self.rng.random_bool(0.6))
                    .cloned()
                    .collect();
                let keep = if keep.is_empty() {
                    vec![a.vars[0].clone()]
                } else {
                    keep
                };
                let distinct = if self.chance(0.4) { "DISTINCT " } else { "" };
                Pat {
                    text: format!(
                        "{{ SELECT {distinct}{} WHERE {{ {} }} }}",
                        keep.join(" "),
                        a.text
                    ),
                    vars: keep,
                }
            }
            23 => {
                // a grouped subquery
                let a = self.pattern(d, &[]);
                if a.vars.is_empty() {
                    return a;
                }
                let k = pick(self.rng, &a.vars).clone();
                let n = self.fresh();
                Pat {
                    text: format!(
                        "{{ SELECT {k} (COUNT(*) AS {n}) WHERE {{ {} }} GROUP BY {k} }}",
                        a.text
                    ),
                    vars: vec![k, n],
                }
            }
            _ => self.triple(scope),
        }
    }

    /// A query of the fragment: SELECT in most cases, then ASK and CONSTRUCT.
    pub fn query(&mut self) -> String {
        let depth = self.rng.random_range(1..4);
        let p = self.pattern(depth, &[]);
        let vars = p.vars.clone();
        let body = p.text;
        let r = self.rng.random_range(0..20);
        if vars.is_empty() || r == 0 {
            return format!("ASK {{ {body} }}");
        }
        match r {
            1 | 2 => {
                // a template of the pattern's variables, with a blank node at times
                let s = pick(self.rng, &vars).clone();
                let o = pick(self.rng, &vars).clone();
                let extra = if self.chance(0.3) {
                    format!(" {s} :tagged [ :with {o} ] .")
                } else {
                    String::new()
                };
                format!("CONSTRUCT {{ {s} :out {o} .{extra} }} WHERE {{ {body} }}")
            }
            3..=6 => {
                // grouped, with the aggregates of §4.1 and a few recomputed ones
                let k = pick(self.rng, &vars).clone();
                let x = pick(self.rng, &vars).clone();
                let mut aggs = vec!["(COUNT(*) AS ?n)".to_string()];
                let choices = [
                    format!("(SUM({x}) AS ?sum)"),
                    format!("(AVG({x}) AS ?avg)"),
                    format!("(COUNT({x}) AS ?cnt)"),
                    format!("(COUNT(DISTINCT {x}) AS ?dc)"),
                    format!("(MIN({x}) AS ?min)"),
                    format!("(MAX({x}) AS ?max)"),
                ];
                for c in &choices {
                    if self.chance(0.3) {
                        aggs.push(c.clone());
                    }
                }
                let having = if self.chance(0.2) {
                    " HAVING (COUNT(*) > 1)"
                } else {
                    ""
                };
                if self.chance(0.2) {
                    format!("SELECT {} WHERE {{ {body} }}", aggs.join(" "))
                } else {
                    format!(
                        "SELECT {k} {} WHERE {{ {body} }} GROUP BY {k}{having}",
                        aggs.join(" ")
                    )
                }
            }
            7 => {
                // an ordered window: the order covers every column, so it has no ties
                // other than equal rows
                let cols: Vec<String> = vars.iter().take(3).cloned().collect();
                let limit = self.rng.random_range(1..4);
                let offset = if self.chance(0.3) { " OFFSET 1" } else { "" };
                format!(
                    "SELECT DISTINCT {c} WHERE {{ {body} }} ORDER BY {c} LIMIT {limit}{offset}",
                    c = cols.join(" ")
                )
            }
            8..=11 => format!("SELECT * WHERE {{ {body} }}"),
            _ => {
                let keep: Vec<String> = vars
                    .iter()
                    .filter(|_| self.rng.random_bool(0.6))
                    .cloned()
                    .collect();
                let keep = if keep.is_empty() {
                    vec![vars[0].clone()]
                } else {
                    keep
                };
                let m = match self.rng.random_range(0..4) {
                    0 => "DISTINCT ",
                    1 => "REDUCED ",
                    _ => "",
                };
                format!("SELECT {m}{} WHERE {{ {body} }}", keep.join(" "))
            }
        }
    }
}

fn union(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = a.to_vec();
    for v in b {
        if !out.contains(v) {
            out.push(v.clone());
        }
    }
    out
}

fn with(scope: &[String], more: &[String]) -> Vec<String> {
    union(scope, more)
}

/// A random reader: every graph or some of them, with a protection by predicate (which
/// filters the changes) or by subject class (which depends on the data), or none. The
/// description names it in failures.
pub fn access(rng: &mut StdRng) -> (Arc<GraphAccess>, String) {
    let (read, gdesc) = match rng.random_range(0..4) {
        0 => (Graphs::All, "all graphs"),
        1 => (
            Graphs::Only(GraphRule::new(["default"], &[])),
            "the default graph",
        ),
        2 => (
            Graphs::Only(GraphRule::new(["default", "http://ex/g1"], &[])),
            "the default graph and :g1",
        ),
        _ => (
            Graphs::Only(GraphRule::new(["http://ex/g1", "http://ex/g2"], &[])),
            ":g1 and :g2",
        ),
    };
    let preds = ["http://ex/age", "http://ex/memberOf", "http://ex/knows"];
    let pred = preds[rng.random_range(0..preds.len())];
    let (protection, pdesc) = match rng.random_range(0..3) {
        0 => (None, "no protection".to_string()),
        1 => (
            Some(Protection {
                name: "hide".into(),
                predicates: Some(vec![pred.into()]),
                classes: None,
                subclasses: false,
                graphs: None,
                pattern: None,
                prefixes: Default::default(),
                hide_inferences: false,
            }),
            format!("{pred} hidden"),
        ),
        _ => (
            Some(Protection {
                name: "hide".into(),
                predicates: Some(vec![pred.into()]),
                classes: Some(vec!["http://ex/Secret".into()]),
                subclasses: false,
                graphs: None,
                pattern: None,
                prefixes: Default::default(),
                hide_inferences: false,
            }),
            format!("{pred} hidden on :Secret subjects"),
        ),
    };
    let access = match protection {
        None => GraphAccess::graphs(read, Graphs::none()),
        Some(p) => GraphAccess::with_triples(
            read,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(p),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ),
    };
    (Arc::new(access), format!("{gdesc}, {pdesc}"))
}

/// How many generated cases a test runs: `default`, or `SPARKLES_INCR_GEN_CASES` for a
/// longer run.
pub fn cases(default: usize) -> usize {
    std::env::var("SPARKLES_INCR_GEN_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The first seed of a run: 0, or `SPARKLES_INCR_GEN_SEED`.
pub fn first_seed() -> u64 {
    std::env::var("SPARKLES_INCR_GEN_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}
