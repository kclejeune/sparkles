//! `spk:hybridSearch`: a full-text ranking and a vector ranking fused by reciprocal rank
//! fusion (Cormack, Clarke and Büttcher, SIGIR 2009).
//!
//! ```sparql
//! (?s ?score ?textRank ?vectorRank) spk:hybridSearch (
//!     (rdfs:label "brown fox" 100 "lang:en")       # the arguments of text:query
//!     (ex:emb "[0.1, 0.2]"^^spk:vector 100)        # the arguments of spk:vectorSearch
//!     10 "rrf:60" "weights:1,0.5")
//! ```
//!
//! Each list runs as its own property function would, within the active graph, to the
//! depth its limit or k gives (100 by default). Each ranking is then reduced to one entry
//! per subject (per subject and graph under `GRAPH ?g`), its best, and ranked: a
//! subject's rank is one more than the number of subjects with a better score, so tied
//! subjects share a rank. The fused score of a subject is the sum of `w / (k + rank)`
//! over the lists that hold it, and the result is the `limit` best subjects.

use super::ctx::Ctx;
use super::plan::{ActiveGraph, Kind, Node, PathEnd, Planner, TextSpec, VectorSpec};
use super::table::{Table, VarId};
use super::value::Value;
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal};
use rustc_hash::FxHashMap;
use spargebra::term::{TermPattern, TriplePattern};

/// `spk:hybridSearch`
pub const HYBRID_SEARCH: &str = "urn:x-sparkles:hybridSearch";

/// The depth of a list that sets no limit or k.
pub const DEFAULT_DEPTH: usize = 100;
/// The rows a call returns without a limit.
pub const DEFAULT_LIMIT: usize = 10;
/// The constant of reciprocal rank fusion without an `rrf:` option (Cormack et al.).
pub const DEFAULT_RRF_K: f64 = 60.0;

fn bad(m: impl std::fmt::Display) -> Error {
    Error::invalid(format!("spk:hybridSearch: {m}"))
}

/// A `spk:hybridSearch` call planned as a leaf.
#[derive(Clone, Debug)]
pub struct HybridSpec {
    /// the text search (`None`: nothing in scope), and its columns
    pub text: Option<Box<TextSpec>>,
    pub text_vars: Vec<VarId>,
    pub text_s: VarId,
    pub text_score: VarId,
    /// the vector search (`None`: nothing in scope), and its columns
    pub vector: Option<Box<VectorSpec>>,
    pub vector_vars: Vec<VarId>,
    pub vector_s: VarId,
    pub vector_score: VarId,
    /// whether a lower vector score is better (euclidean)
    pub vector_lower_better: bool,
    /// the subjects returned at most
    pub limit: usize,
    pub rrf_k: f64,
    /// the weights of the text and vector rankings
    pub weights: [f64; 2],
    pub subject: PathEnd,
    pub score: Option<VarId>,
    pub text_rank: Option<VarId>,
    pub vector_rank: Option<VarId>,
    /// `GRAPH ?g { … }` around the call: fusion is per subject and graph
    pub graph_var: Option<VarId>,
}

/// Take a call's nested list arguments (the text and vector lists) out of the remaining
/// patterns.
pub fn take_lists(patterns: &mut Vec<TriplePattern>, args: Vec<TermPattern>) -> Result<Vec<Arg>> {
    args.into_iter()
        .map(|a| {
            Ok(
                match super::textpf::take_list(patterns, &a, "spk:hybridSearch")? {
                    Some(items) => Arg::List(items),
                    None => Arg::Term(a),
                },
            )
        })
        .collect()
}

/// An element of the object list: a term, or a nested list.
#[derive(Clone, Debug)]
pub enum Arg {
    Term(TermPattern),
    List(Vec<TermPattern>),
}

/// Plan a `spk:hybridSearch` call.
pub(super) fn hybrid_leaf(
    p: &Planner<'_>,
    subjects: Vec<TermPattern>,
    args: Vec<Arg>,
    g: &ActiveGraph,
) -> Result<Node> {
    let shape = || bad("expected ((text arguments) (vector arguments) [limit] [options])");
    if subjects.is_empty() || subjects.len() > 4 {
        return Err(bad(
            "the subject is ?s or (?s ?score ?textRank ?vectorRank), each slot after ?s optional",
        ));
    }
    let var = |t: &TermPattern, name: &str| -> Result<VarId> {
        match t {
            TermPattern::Variable(_) | TermPattern::BlankNode(_) => match p.term_pattern(t) {
                super::plan::PT::V(v) => Ok(v),
                super::plan::PT::C(_) => unreachable!("a variable"),
            },
            _ => Err(bad(format!("{name} must be a variable"))),
        }
    };
    let subject = match p.term_pattern(&subjects[0]) {
        super::plan::PT::V(v) => PathEnd::Var(v),
        super::plan::PT::C(id) => PathEnd::Const(id),
    };
    let score = subjects.get(1).map(|t| var(t, "the score")).transpose()?;
    let text_rank = subjects
        .get(2)
        .map(|t| var(t, "the text rank"))
        .transpose()?;
    let vector_rank = subjects
        .get(3)
        .map(|t| var(t, "the vector rank"))
        .transpose()?;

    let mut args = args.into_iter();
    let text_args = match args.next() {
        Some(Arg::List(items)) => items,
        Some(Arg::Term(t @ TermPattern::Literal(_))) => vec![t],
        _ => return Err(shape()),
    };
    let Some(Arg::List(mut vector_args)) = args.next() else {
        return Err(shape());
    };
    let mut limit = DEFAULT_LIMIT;
    let mut rrf_k = DEFAULT_RRF_K;
    let mut weights = [1.0, 1.0];
    let mut seen = [false; 3];
    for a in args {
        let Arg::Term(TermPattern::Literal(l)) = a else {
            return Err(shape());
        };
        let mut once = |i: usize| {
            if std::mem::replace(&mut seen[i], true) {
                Err(bad(format!("{l} is given twice")))
            } else {
                Ok(())
            }
        };
        if l.datatype() != xsd::STRING {
            once(0)?;
            limit = l
                .value()
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=crate::vector::MAX_K).contains(n))
                .ok_or_else(|| bad(format!("the limit must be 1..={}", crate::vector::MAX_K)))?;
        } else if let Some(k) = l.value().strip_prefix("rrf:") {
            once(1)?;
            rrf_k = k
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|k| k.is_finite() && *k >= 0.0)
                .ok_or_else(|| bad(format!("rrf: needs a number of at least 0, got {k:?}")))?;
        } else if let Some(w) = l.value().strip_prefix("weights:") {
            once(2)?;
            let ws: Vec<f64> = w
                .split(',')
                .map(|x| x.trim().parse::<f64>())
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| {
                    bad(format!(
                        "weights: needs two numbers, as in weights:1,0.5, got {w:?}"
                    ))
                })?;
            match ws.as_slice() {
                [t, v] if [t, v].iter().all(|x| x.is_finite() && **x >= 0.0) && t + v > 0.0 => {
                    weights = [*t, *v]
                }
                _ => {
                    return Err(bad(format!(
                        "weights: needs two numbers of at least 0, not both 0, got {w:?}"
                    )));
                }
            }
        } else {
            return Err(bad(format!("unexpected argument {l}")));
        }
    }

    // the two searches, planned as their own calls with outputs of their own
    let n = p.ctx.fresh_var();
    let hidden = |name: &str| {
        TermPattern::BlankNode(BlankNode::new_unchecked(format!("spkhybrid{n}{name}")))
    };
    let (ts, tscore, vs, vscore) = (hidden("ts"), hidden("tsc"), hidden("vs"), hidden("vsc"));
    let mut call = super::textpf::decode(vec![ts.clone(), tscore.clone()], text_args)?;
    if call.query_var.is_some() {
        return Err(bad("arguments must be constants"));
    }
    if call.highlight.is_some() {
        return Err(bad("the text list takes no highlight: option"));
    }
    call.limit = call.limit.or(Some(DEFAULT_DEPTH));
    let text_node = p.text_leaf(call, g)?;
    // the vector list's k, or the default depth
    if !vector_args
        .iter()
        .skip(2)
        .any(|a| matches!(a, TermPattern::Literal(l) if l.datatype() != xsd::STRING))
    {
        if vector_args.len() < 2 {
            return Err(bad("the vector list is (predicate query [k] [options])"));
        }
        vector_args.insert(
            2,
            TermPattern::Literal(Literal::new_typed_literal(
                DEFAULT_DEPTH.to_string(),
                xsd::INTEGER,
            )),
        );
    }
    let vector_node = p.vector_leaf(vec![vs.clone(), vscore.clone()], vector_args, g)?;
    let v = |t: &TermPattern| var(t, "").expect("a hidden variable");
    let text_vars = text_node.vars.clone();
    let text = match text_node.kind {
        Kind::TextSearch(spec) => Some(spec),
        _ => None,
    };
    let vector_vars = vector_node.vars.clone();
    let vector = match vector_node.kind {
        Kind::VectorSearch(spec) => {
            if spec.needs_input() {
                return Err(bad(
                    "the vector query must be a vector literal or an entity, and candidates:join is not supported",
                ));
            }
            Some(spec)
        }
        _ => None,
    };
    let graph_var = p.graph_filter(g).and_then(|(_, gv)| gv);
    let mut vars = Vec::new();
    for x in [
        match subject {
            PathEnd::Var(v) => Some(v),
            PathEnd::Const(_) => None,
        },
        score,
        text_rank,
        vector_rank,
        graph_var,
    ]
    .into_iter()
    .flatten()
    {
        if !vars.contains(&x) {
            vars.push(x);
        }
    }
    let desc = format!(
        "{} ← text [{}] + vector [{}] limit {limit} rrf {rrf_k} weights {},{}",
        vars.iter()
            .map(|v| format!("?{}", p.ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(" "),
        // the searches without their hidden outputs
        text_node.desc.split_once("← ").map_or("", |d| d.1),
        vector_node.desc.split_once("← ").map_or("", |d| d.1),
        weights[0],
        weights[1],
    );
    let spec = HybridSpec {
        vector_lower_better: vector
            .as_ref()
            .is_some_and(|v| !v.metric.higher_is_better()),
        text,
        text_vars,
        text_s: v(&ts),
        text_score: v(&tscore),
        vector,
        vector_vars,
        vector_s: v(&vs),
        vector_score: v(&vscore),
        limit,
        rrf_k,
        weights,
        subject,
        score,
        text_rank,
        vector_rank,
        graph_var,
    };
    let mut node = Node::leaf(Kind::HybridSearch(Box::new(spec)), vars, limit as f64, desc);
    node.cost = text_node.cost + vector_node.cost + limit as f64;
    Ok(node)
}

/// A ranked entry: the subject, and its graph under `GRAPH ?g` (else undefined).
type Key = (Id, Id);

/// One entry per key, its best score, ranked: one more than the number of keys with a
/// better score.
fn ranks(
    ctx: &Ctx,
    t: &Table,
    s: VarId,
    score: VarId,
    graph: Option<VarId>,
    lower_better: bool,
) -> Result<FxHashMap<Key, usize>> {
    let (Some(cs), Some(cscore)) = (t.col_of(s), t.col_of(score)) else {
        return Ok(FxHashMap::default());
    };
    let cg = graph.and_then(|g| t.col_of(g));
    let mut values: FxHashMap<Id, f64> = FxHashMap::default();
    let mut best: FxHashMap<Key, f64> = FxHashMap::default();
    for r in 0..t.len() {
        if r % 4096 == 4095 {
            ctx.check()?;
        }
        let id = t.get(r, cscore);
        let v = *values.entry(id).or_insert_with(|| match ctx.value(id) {
            Some(Value::Float(f)) => f64::from(f32::from(f)),
            Some(Value::Double(d)) => f64::from(d),
            _ => f64::NAN,
        });
        if v.is_nan() {
            continue;
        }
        let key = (t.get(r, cs), cg.map_or(Id::UNDEF, |c| t.get(r, c)));
        let better = |a: f64, b: f64| if lower_better { a < b } else { a > b };
        best.entry(key)
            .and_modify(|b| {
                if better(v, *b) {
                    *b = v
                }
            })
            .or_insert(v);
    }
    let mut order: Vec<(Key, f64)> = best.into_iter().collect();
    order.sort_by(|a, b| {
        let by = if lower_better {
            a.1.total_cmp(&b.1)
        } else {
            b.1.total_cmp(&a.1)
        };
        by.then(a.0.cmp(&b.0))
    });
    let mut out = FxHashMap::default();
    let mut rank = 0;
    for (i, (key, v)) in order.iter().enumerate() {
        if i == 0 || *v != order[i - 1].1 {
            rank = i + 1;
        }
        out.insert(*key, rank);
    }
    Ok(out)
}

/// Evaluate a `spk:hybridSearch` call.
pub(super) fn search(
    ctx: &Ctx,
    spec: &HybridSpec,
    vars: &[VarId],
) -> Result<(Table, serde_json::Map<String, serde_json::Value>)> {
    let mut counters = serde_json::Map::new();
    let text = match &spec.text {
        Some(t) => {
            let table = crate::text::search(ctx, t, &spec.text_vars)?;
            counters.insert("textHits".into(), table.len().into());
            ranks(
                ctx,
                &table,
                spec.text_s,
                spec.text_score,
                spec.graph_var,
                false,
            )?
        }
        None => FxHashMap::default(),
    };
    ctx.check()?;
    let vector = match &spec.vector {
        Some(v) => {
            let (table, c) = super::exec::vector_search(ctx, v, None, &spec.vector_vars)?;
            counters.insert("vectorHits".into(), table.len().into());
            if let Some(m) = c.get("method") {
                counters.insert("vectorMethod".into(), m.clone());
            }
            ranks(
                ctx,
                &table,
                spec.vector_s,
                spec.vector_score,
                spec.graph_var,
                spec.vector_lower_better,
            )?
        }
        None => FxHashMap::default(),
    };
    ctx.check()?;
    let [wt, wv] = spec.weights;
    let rrf = |w: f64, r: Option<&usize>| r.map_or(0.0, |&r| w / (spec.rrf_k + r as f64));
    let mut fused: Vec<(Key, f64, Option<usize>, Option<usize>)> = text
        .keys()
        .chain(vector.keys().filter(|k| !text.contains_key(k)))
        .filter(|k| match spec.subject {
            PathEnd::Const(s) => k.0 == s,
            PathEnd::Var(_) => true,
        })
        .map(|k| {
            let (rt, rv) = (text.get(k), vector.get(k));
            (*k, rrf(wt, rt) + rrf(wv, rv), rt.copied(), rv.copied())
        })
        .collect();
    counters.insert("fused".into(), fused.len().into());
    // best first; ties break by term id, as in spk:vectorSearch
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    fused.truncate(spec.limit);
    ctx.check_output(fused.len(), vars.len())?;
    let col = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
    let cs = match spec.subject {
        PathEnd::Var(v) => col(Some(v)),
        PathEnd::Const(_) => None,
    };
    let (cscore, ctr, cvr, cg) = (
        col(spec.score),
        col(spec.text_rank),
        col(spec.vector_rank),
        col(spec.graph_var),
    );
    let int = |r: Option<usize>| r.map_or(Id::UNDEF, |r| Id::from_i64(r as i64).expect("a rank"));
    let mut t = Table::new(vars.to_vec());
    let mut row = vec![Id::UNDEF; vars.len()];
    for ((s, g), score, rt, rv) in fused {
        row.fill(Id::UNDEF);
        // the same variable in two slots must bind one value
        let mut ok = true;
        let mut set = |c: Option<usize>, id: Id| {
            if let Some(c) = c {
                if row[c] != Id::UNDEF && row[c] != id {
                    ok = false;
                }
                row[c] = id;
            }
        };
        set(cs, s);
        set(
            cscore,
            Id::from_f64(score).unwrap_or_else(|| ctx.intern_value(&Value::Double(score.into()))),
        );
        if rt.is_some() {
            set(ctr, int(rt));
        }
        if rv.is_some() {
            set(cvr, int(rv));
        }
        if spec.graph_var.is_some() {
            set(cg, g);
        }
        if ok {
            t.push_row(&row);
        }
    }
    Ok((t, counters))
}
