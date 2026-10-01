//! Compilation of a checked schema into the [`crate::ir`]: references resolved and
//! chains collapsed, `&include` expanded per occurrence, triple expressions flattened
//! per shape with their per-(predicate, direction) index and maximum occurrences, shapes
//! classified (flat, deterministic, ambiguous), and pair kinds for labels, START and
//! value expressions.
//!
//! * A declaration that is only a reference (`ex:A @ex:B`) gets no pair kind of its own:
//!   its label names the pair kind at the end of the chain.
//! * Every triple-constraint value expression is a pair kind; a bare reference is the
//!   referenced label's. Value expressions are compiled once per expression of the
//!   schema, not per `&include` occurrence, so the triple-constraint instances an
//!   inclusion makes share their value's pair kind (and a value expression that
//!   includes its own triple expression compiles to a finite IR).
//! * Groups without cardinality or semantic actions inside a group of the same kind are
//!   spliced into it (`(a ; (b ; c))` is `(a ; b ; c)`), and such a group of one
//!   expression is that expression.

use crate::CompiledSchema;
use crate::ast::{Label, Schema, Shape, ShapeExpr, TripleExpr, cardinality};
use crate::check::{Checked, show_label};
use crate::error::SchemaError;
use crate::ir::{
    Dir, Ir, NcId, NcIr, PairKind, PairKindInfo, Se, SeId, ShapeClass, ShapeId, ShapeIr, TcId,
    TcIr, Te, TeId,
};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

/// The most triple-constraint instances a schema may have once its inclusions are
/// expanded (nested inclusions multiply).
pub const MAX_TRIPLE_CONSTRAINTS: usize = 1_000_000;

/// Compile a checked schema.
pub fn compile(schema: &Schema, checked: &Checked) -> Result<CompiledSchema, SchemaError> {
    let mut c = Compiler {
        schema,
        checked,
        ir: Ir {
            strata: checked.num_strata,
            start_acts: schema.start_acts.clone(),
            ..Ir::default()
        },
        decl_kind: vec![None; schema.shapes.len()],
        values: FxHashMap::default(),
        tcs: 0,
    };
    // pair kinds of the declarations that are not bare references
    for (i, d) in schema.shapes.iter().enumerate() {
        if !matches!(d.expr, ShapeExpr::Ref(_)) {
            let k = c.new_kind(Some(d.label.clone()), checked.strata[i]);
            c.decl_kind[i] = Some(k);
        }
    }
    // labels; a bare reference names the pair kind at the end of its chain (acyclic:
    // checked)
    for (i, d) in schema.shapes.iter().enumerate() {
        let k = c.kind_of_decl(i);
        c.ir.labels.insert(d.label.to_shexj(), k);
    }
    for (i, d) in schema.shapes.iter().enumerate() {
        if let Some(k) = c.decl_kind[i] {
            let se = c.se(&d.expr)?;
            c.ir.pairs[k.index()].se = se;
        }
    }
    if let Some(e) = &schema.start {
        let k = match e {
            ShapeExpr::Ref(l) => c.kind_of_label(l),
            _ => {
                let k = c.new_kind(None, checked.start_stratum);
                let se = c.se(e)?;
                c.ir.pairs[k.index()].se = se;
                k
            }
        };
        c.ir.start = Some(k);
    }
    Ok(CompiledSchema::from_ir(
        c.ir,
        schema.prefixes.clone(),
        schema.base.clone(),
    ))
}

struct Compiler<'s, 'c> {
    schema: &'s Schema,
    checked: &'c Checked<'s>,
    ir: Ir,
    /// the pair kind of each declaration that is not a bare reference
    decl_kind: Vec<Option<PairKind>>,
    /// compiled value expressions, by address: the expression and its pair kind
    values: FxHashMap<usize, (SeId, PairKind)>,
    /// triple-constraint instances so far
    tcs: usize,
}

/// A shape being compiled.
#[derive(Default)]
struct ShapeBuilder {
    tcs: Vec<TcIr>,
    te: Vec<Te>,
    max_occ: Vec<Option<u64>>,
}

impl<'s> Compiler<'s, '_> {
    fn new_kind(&mut self, label: Option<Label>, stratum: u32) -> PairKind {
        let k = PairKind(self.ir.pairs.len() as u32);
        self.ir.pairs.push(PairKindInfo {
            // set once the expression is compiled
            se: SeId(u32::MAX),
            label,
            stratum,
        });
        k
    }

    fn kind_of_decl(&self, mut i: usize) -> PairKind {
        loop {
            if let Some(k) = self.decl_kind[i] {
                return k;
            }
            let ShapeExpr::Ref(l) = &self.schema.shapes[i].expr else {
                unreachable!("a declaration without a pair kind is a reference")
            };
            i = self.checked.decls[l];
        }
    }

    fn kind_of_label(&self, l: &Label) -> PairKind {
        self.kind_of_decl(self.checked.decls[l])
    }

    fn push_se(&mut self, se: Se) -> SeId {
        let id = SeId(self.ir.ses.len() as u32);
        self.ir.ses.push(se);
        id
    }

    /// Compile a shape expression into a new arena entry.
    fn se(&mut self, e: &'s ShapeExpr) -> Result<SeId, SchemaError> {
        let id = self.push_se(Se::External);
        let se = self.build(e)?;
        self.ir.ses[id.index()] = se;
        Ok(id)
    }

    fn build(&mut self, e: &'s ShapeExpr) -> Result<Se, SchemaError> {
        Ok(match e {
            ShapeExpr::Or(v) => Se::Or(v.iter().map(|x| self.se(x)).collect::<Result<_, _>>()?),
            ShapeExpr::And(v) => Se::And(v.iter().map(|x| self.se(x)).collect::<Result<_, _>>()?),
            ShapeExpr::Not(x) => Se::Not(self.se(x)?),
            ShapeExpr::Ref(l) => Se::Ref(self.kind_of_label(l)),
            ShapeExpr::External => Se::External,
            ShapeExpr::Nc(nc) => {
                let regex = match &nc.pattern {
                    None => None,
                    Some(p) => {
                        let flags = nc.flags.as_deref().unwrap_or("");
                        Some(
                            sparkles::sparql::expr::compile_regex(p, flags).map_err(|_| {
                                SchemaError::new(format!("invalid pattern /{p}/{flags}"))
                            })?,
                        )
                    }
                };
                let id = NcId(self.ir.ncs.len() as u32);
                self.ir.ncs.push(NcIr {
                    nc: (**nc).clone(),
                    regex,
                });
                Se::Nc(id)
            }
            ShapeExpr::Shape(s) => Se::Shape(self.shape(s)?),
        })
    }

    /// A triple constraint's value expression: compiled once, with its pair kind.
    fn value(&mut self, e: &'s ShapeExpr) -> Result<(SeId, PairKind), SchemaError> {
        let key = e as *const ShapeExpr as usize;
        if let Some(&v) = self.values.get(&key) {
            return Ok(v);
        }
        let id = self.push_se(Se::External);
        let kind = match e {
            ShapeExpr::Ref(l) => self.kind_of_label(l),
            _ => {
                let k = self.new_kind(None, self.checked.value_stratum(e));
                self.ir.pairs[k.index()].se = id;
                k
            }
        };
        // registered before the body: the body may include the triple expression this
        // value belongs to
        self.values.insert(key, (id, kind));
        let se = self.build(e)?;
        self.ir.ses[id.index()] = se;
        Ok((id, kind))
    }

    fn shape(&mut self, s: &'s Shape) -> Result<ShapeId, SchemaError> {
        let mut b = ShapeBuilder::default();
        let root = match &s.expression {
            None => None,
            Some(t) => Some(self.te(&mut b, t, Some(1))?),
        };
        let mut preds: Vec<(String, Dir, SmallVec<[TcId; 2]>)> = Vec::new();
        let mut index: FxHashMap<(&str, Dir), usize> = FxHashMap::default();
        for (i, tc) in b.tcs.iter().enumerate() {
            let at = *index.entry((&tc.pred, tc.dir)).or_insert_with(|| {
                preds.push((tc.pred.clone(), tc.dir, SmallVec::new()));
                preds.len() - 1
            });
            preds[at].2.push(TcId(i as u32));
        }
        let class = classify(&b, root, &preds);
        let id = ShapeId(self.ir.shapes.len() as u32);
        self.ir.shapes.push(ShapeIr {
            closed: s.is_closed(),
            extra: s.extra.iter().cloned().collect(),
            tcs: b.tcs,
            te: b.te,
            root,
            preds,
            max_occ: b.max_occ,
            class,
            sem_acts: s.sem_acts.clone(),
        });
        Ok(id)
    }

    /// The triple expression an expression stands for (an inclusion's target).
    fn resolve(&self, t: &'s TripleExpr) -> &'s TripleExpr {
        match t {
            TripleExpr::Include(l) => self.resolve(self.checked.tes[l]),
            _ => t,
        }
    }

    /// Compile a triple expression of a shape; `mult` is the product of the maximum
    /// cardinalities of the groups above it (`None`: unbounded).
    fn te(
        &mut self,
        b: &mut ShapeBuilder,
        t: &'s TripleExpr,
        mult: Option<u64>,
    ) -> Result<TeId, SchemaError> {
        let t = self.resolve(t);
        let te = match t {
            TripleExpr::Include(_) => unreachable!("resolved"),
            TripleExpr::Tc(tc) => {
                self.tcs += 1;
                if self.tcs > MAX_TRIPLE_CONSTRAINTS {
                    return Err(SchemaError::new(format!(
                        "the schema has more than {MAX_TRIPLE_CONSTRAINTS} triple constraints \
                         once its inclusions are expanded"
                    )));
                }
                let (min, max) = cardinality(tc.min, tc.max);
                let (value, pair) = match &tc.value_expr {
                    None => (None, None),
                    Some(v) => {
                        let (se, k) = self.value(v)?;
                        (Some(se), Some(k))
                    }
                };
                let id = TcId(b.tcs.len() as u32);
                b.tcs.push(TcIr {
                    pred: tc.predicate.clone(),
                    dir: if tc.is_inverse() { Dir::In } else { Dir::Out },
                    value,
                    pair,
                    min,
                    max,
                    sem_acts: tc.sem_acts.clone(),
                });
                b.max_occ.push(mul(mult, max));
                Te::Tc(id)
            }
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g) => {
                let each = matches!(t, TripleExpr::EachOf(_));
                let mut kids = Vec::new();
                self.splice(each, &g.exprs, &mut kids);
                let (min, max) = cardinality(g.min, g.max);
                if kids.len() == 1 && plain(min, max, &g.sem_acts) {
                    return self.te(b, kids[0], mult);
                }
                let below = mul(mult, max);
                let kids = kids
                    .into_iter()
                    .map(|k| self.te(b, k, below))
                    .collect::<Result<Vec<_>, _>>()?;
                let acts = g.sem_acts.clone();
                if each {
                    Te::EachOf {
                        kids,
                        min,
                        max,
                        acts,
                    }
                } else {
                    Te::OneOf {
                        kids,
                        min,
                        max,
                        acts,
                    }
                }
            }
        };
        let id = TeId(b.te.len() as u32);
        b.te.push(te);
        Ok(id)
    }

    /// The members of a group, with plain groups of the same kind spliced in.
    fn splice(&self, each: bool, exprs: &'s [TripleExpr], out: &mut Vec<&'s TripleExpr>) {
        for x in exprs {
            let x = self.resolve(x);
            match x {
                TripleExpr::EachOf(g) | TripleExpr::OneOf(g)
                    if matches!(x, TripleExpr::EachOf(_)) == each && {
                        let (min, max) = cardinality(g.min, g.max);
                        plain(min, max, &g.sem_acts)
                    } =>
                {
                    self.splice(each, &g.exprs, out)
                }
                _ => out.push(x),
            }
        }
    }
}

/// A group that matches exactly once and has no semantic actions.
fn plain(min: u32, max: Option<u32>, acts: &[crate::ast::SemAct]) -> bool {
    min == 1 && max == Some(1) && acts.is_empty()
}

/// The product of maximum cardinalities (`None`: unbounded; 0 times unbounded is 0).
fn mul(a: Option<u64>, b: Option<u32>) -> Option<u64> {
    match (a, b) {
        (Some(0), _) | (_, Some(0)) => Some(0),
        (Some(a), Some(b)) => Some(a.saturating_mul(u64::from(b))),
        _ => None,
    }
}

fn classify(
    b: &ShapeBuilder,
    root: Option<TeId>,
    preds: &[(String, Dir, SmallVec<[TcId; 2]>)],
) -> ShapeClass {
    if preds.iter().any(|(_, _, tcs)| tcs.len() > 1) {
        return ShapeClass::Ambiguous;
    }
    let flat = match root.map(|r| &b.te[r.index()]) {
        None | Some(Te::Tc(_)) => true,
        Some(Te::EachOf {
            kids,
            min,
            max,
            acts,
        }) => plain(*min, *max, acts) && kids.iter().all(|k| matches!(b.te[k.index()], Te::Tc(_))),
        Some(Te::OneOf { .. }) => false,
    };
    if flat {
        ShapeClass::Flat
    } else {
        ShapeClass::Deterministic
    }
}

/// The pair kind of a shape-map label: a declared label, or START.
pub fn shape_label(
    schema: &CompiledSchema,
    label: &crate::ShapeLabel,
) -> Result<PairKind, SchemaError> {
    match label.as_shexj() {
        None => schema
            .ir
            .start
            .ok_or_else(|| SchemaError::new("the schema has no start shape")),
        Some(l) => schema.label(&l).ok_or_else(|| {
            let shown = show_label(&Label::from_shexj(&l), &schema.prefixes);
            SchemaError::new(format!("undefined shape label {shown}"))
        }),
    }
}

#[cfg(test)]
mod tests;
