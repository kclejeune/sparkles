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
//! * A label that is abstract or that other shapes extend (ShEx 2.2) has two pair kinds:
//!   the declared one, for the declaration's own expression, and a descendant kind that
//!   references and shape maps use. A node satisfies the descendant kind when it
//!   satisfies the declaration (unless abstract) or a non-abstract declaration through a
//!   shape that extends the label, found by [`Compiler::witness`].
//! * A shape that extends others gets an [`ExtPlan`], resolved once every declaration is
//!   compiled.

use crate::CompiledSchema;
use crate::ast::{Label, Schema, Shape, ShapeExpr, TripleExpr, cardinality};
use crate::check::{Checked, show_label};
use crate::error::SchemaError;
use crate::ir::{
    Dir, ExtPlan, Frame, Ir, NcId, NcIr, PairKind, PairKindInfo, Part, Se, SeId, ShapeClass,
    ShapeId, ShapeIr, TcId, TcIr, Te, TeId,
};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

/// The most triple-constraint instances a schema may have once its inclusions are
/// expanded (nested inclusions multiply).
pub const MAX_TRIPLE_CONSTRAINTS: usize = 1_000_000;

/// The most readings ([`Frame`]s) one extending shape may have.
pub const MAX_FRAMES: usize = 64;

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
        desc_kind: vec![None; schema.shapes.len()],
        desc_decl: FxHashMap::default(),
        shape_ast: Vec::new(),
        ancestors: ancestors(checked),
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
    // checked); a label that is abstract or extended names its descendant kind
    for (i, d) in schema.shapes.iter().enumerate() {
        let k = c.decl_ref_kind(i);
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
            ShapeExpr::Ref(l) => c.ref_kind(l),
            _ => {
                let k = c.new_kind(None, checked.start_stratum);
                let se = c.se(e)?;
                c.ir.pairs[k.index()].se = se;
                k
            }
        };
        c.ir.start = Some(k);
    }
    c.finish_extensions()?;
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
    /// the descendant kind of each declaration that is abstract or extended
    desc_kind: Vec<Option<PairKind>>,
    /// the declaration of each descendant kind
    desc_decl: FxHashMap<PairKind, usize>,
    /// the AST of each compiled shape
    shape_ast: Vec<&'s Shape>,
    /// per declaration, the declarations it extends directly or through others
    ancestors: Vec<FxHashSet<usize>>,
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

    /// Does anything but the declaration's own expression satisfy a reference to it?
    fn has_descendants(&self, i: usize) -> bool {
        self.schema.shapes[i].is_abstract || !self.checked.hierarchy.descendants[i].is_empty()
    }

    /// The pair kind a reference to declaration `i` reads (allocated on first use).
    fn decl_ref_kind(&mut self, i: usize) -> PairKind {
        if !self.has_descendants(i) {
            return self.kind_of_decl(i);
        }
        if let Some(k) = self.desc_kind[i] {
            return k;
        }
        let h = &self.checked.hierarchy;
        let stratum = std::iter::once(i)
            .chain(h.descendants[i].iter().copied())
            .map(|d| self.checked.strata[d])
            .max()
            .unwrap_or(0);
        let k = self.new_kind(Some(self.schema.shapes[i].label.clone()), stratum);
        self.desc_kind[i] = Some(k);
        self.desc_decl.insert(k, i);
        k
    }

    /// The pair kind a reference to `l` reads.
    fn ref_kind(&mut self, l: &Label) -> PairKind {
        self.decl_ref_kind(self.checked.decls[l])
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
            ShapeExpr::Ref(l) => Se::Ref(self.ref_kind(l)),
            ShapeExpr::External => Se::External,
            ShapeExpr::Nc(nc) => {
                let regex = match &nc.pattern {
                    None => None,
                    Some(p) => Some(
                        crate::nc::compile_pattern(p, nc.flags.as_deref())
                            .map_err(SchemaError::new)?,
                    ),
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
            ShapeExpr::Ref(l) => self.ref_kind(l),
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
            ext: None,
        });
        self.shape_ast.push(s);
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

/// Per declaration, the declarations it extends directly or through others.
fn ancestors(checked: &Checked<'_>) -> Vec<FxHashSet<usize>> {
    let parents = &checked.hierarchy.parents;
    (0..parents.len())
        .map(|i| {
            let mut seen = FxHashSet::default();
            let mut stack = parents[i].clone();
            while let Some(p) = stack.pop() {
                if seen.insert(p) {
                    stack.extend(&parents[p]);
                }
            }
            seen
        })
        .collect()
}

/// An extendable reading of a declaration: its own shape and its other conjuncts.
type Variant = (ShapeId, Vec<SeId>);

/// An ancestor under construction.
struct PartBuilder {
    shape: ShapeId,
    constraints: Vec<SeId>,
    /// the parts of the shapes this one extends directly
    direct: Vec<usize>,
}

impl<'s> Compiler<'s, '_> {
    /// The expression of the declared kind behind `k` (a descendant kind stands for its
    /// declaration).
    fn exact_se(&self, k: PairKind) -> SeId {
        match self.desc_decl.get(&k) {
            Some(&d) => self.ir.pairs[self.kind_of_decl(d).index()].se,
            None => self.ir.pairs[k.index()].se,
        }
    }

    /// The declaration's expressions, once every one is compiled: the plans of the
    /// shapes that extend others, then the descendant kinds.
    fn finish_extensions(&mut self) -> Result<(), SchemaError> {
        for sid in 0..self.shape_ast.len() {
            let shape = self.shape_ast[sid];
            if shape.extends.is_empty() {
                continue;
            }
            let frames = self.frames(shape)?;
            self.ir.shapes[sid].ext = Some(ExtPlan { frames });
        }
        let kinds: Vec<(usize, PairKind)> = self
            .desc_kind
            .iter()
            .enumerate()
            .filter_map(|(i, k)| k.map(|k| (i, k)))
            .collect();
        for (i, k) in kinds {
            let mut alts = Vec::new();
            if !self.schema.shapes[i].is_abstract {
                let exact = self.kind_of_decl(i);
                alts.push(self.push_se(Se::Ref(exact)));
            }
            let descendants = self.checked.hierarchy.descendants[i].clone();
            for d in descendants {
                if self.schema.shapes[d].is_abstract {
                    continue;
                }
                let se = self.ir.pairs[self.kind_of_decl(d).index()].se;
                let mut seen = FxHashSet::default();
                if let Some(w) = self.witness(se, i, &mut seen) {
                    alts.push(w);
                }
            }
            let se = self.push_se(Se::Or(alts));
            self.ir.pairs[k.index()].se = se;
        }
        Ok(())
    }

    /// The part of `se` that satisfies a reference to declaration `target` through an
    /// extension: `se` itself when it is a shape that extends `target`, and what the
    /// disjunctions, conjunctions and references of `se` leave of that. `None`: no
    /// shape of `se` extends `target`, so `se` alone does not make a node an instance
    /// of it.
    fn witness(&mut self, se: SeId, target: usize, seen: &mut FxHashSet<SeId>) -> Option<SeId> {
        match self.ir.ses[se.index()].clone() {
            Se::Shape(sid) => {
                let extends = self.shape_ast[sid.index()]
                    .extends
                    .iter()
                    .map(|l| self.checked.decls[l]);
                let mut hit = false;
                for d in extends {
                    hit |= d == target || self.ancestors[d].contains(&target);
                }
                hit.then_some(se)
            }
            Se::Or(xs) => {
                let alts: Vec<SeId> = xs
                    .iter()
                    .filter_map(|&x| self.witness(x, target, seen))
                    .collect();
                match alts.len() {
                    0 => None,
                    1 => Some(alts[0]),
                    _ => Some(self.push_se(Se::Or(alts))),
                }
            }
            Se::And(xs) => {
                let mut alts = Vec::new();
                for i in 0..xs.len() {
                    if let Some(w) = self.witness(xs[i], target, seen) {
                        let mut conj = xs.clone();
                        conj[i] = w;
                        alts.push(self.push_se(Se::And(conj)));
                    }
                }
                match alts.len() {
                    0 => None,
                    1 => Some(alts[0]),
                    _ => Some(self.push_se(Se::Or(alts))),
                }
            }
            Se::Ref(k) => {
                if !seen.insert(se) {
                    return None;
                }
                let inner = self.exact_se(k);
                self.witness(inner, target, seen)
            }
            Se::Not(_) | Se::Nc(_) | Se::External => None,
        }
    }

    /// The readings of the shapes that `shape` extends.
    fn frames(&self, shape: &'s Shape) -> Result<Vec<Frame>, SchemaError> {
        let mut out = Vec::new();
        let pending: Vec<(&'s Label, Option<usize>)> =
            shape.extends.iter().rev().map(|l| (l, None)).collect();
        self.expand(&mut Vec::new(), pending, &mut out)?;
        Ok(out)
    }

    /// Choose a variant for each label still pending, collecting a frame per choice.
    fn expand(
        &self,
        parts: &mut Vec<PartBuilder>,
        mut pending: Vec<(&'s Label, Option<usize>)>,
        out: &mut Vec<Frame>,
    ) -> Result<(), SchemaError> {
        let Some((label, from)) = pending.pop() else {
            if out.len() >= MAX_FRAMES {
                return Err(SchemaError::new(format!(
                    "a shape that extends disjunctions has more than {MAX_FRAMES} readings"
                )));
            }
            out.push(finish_frame(parts));
            return Ok(());
        };
        let variants = self.variants(label)?;
        for (main, constraints) in variants {
            let before = parts.len();
            let mut next = pending.clone();
            let idx = match parts.iter().position(|p| p.shape == main) {
                Some(j) => j,
                None => {
                    parts.push(PartBuilder {
                        shape: main,
                        constraints,
                        direct: Vec::new(),
                    });
                    let idx = parts.len() - 1;
                    next.extend(
                        self.shape_ast[main.index()]
                            .extends
                            .iter()
                            .rev()
                            .map(|l| (l, Some(idx))),
                    );
                    idx
                }
            };
            let linked = match from {
                Some(f) if !parts[f].direct.contains(&idx) => {
                    parts[f].direct.push(idx);
                    true
                }
                _ => false,
            };
            self.expand(parts, next, out)?;
            if linked && let Some(f) = from {
                parts[f].direct.pop();
            }
            parts.truncate(before);
        }
        Ok(())
    }

    /// The readings of an extended label's declaration: its shape and its other
    /// conjuncts, one reading per disjunct of a disjunction.
    fn variants(&self, label: &Label) -> Result<Vec<Variant>, SchemaError> {
        let k = self.kind_of_label(label);
        let mut out = Vec::new();
        self.variants_of(self.ir.pairs[k.index()].se, &mut out);
        if out.is_empty() {
            return Err(SchemaError::new(format!(
                "{} is extended but is neither a shape nor a conjunction with a shape",
                show_label(label, &self.schema.prefixes)
            )));
        }
        Ok(out)
    }

    fn variants_of(&self, se: SeId, out: &mut Vec<Variant>) {
        match &self.ir.ses[se.index()] {
            Se::Shape(sid) => out.push((*sid, Vec::new())),
            Se::And(xs) => {
                if let Some(i) = xs
                    .iter()
                    .position(|x| matches!(self.ir.ses[x.index()], Se::Shape(_)))
                {
                    let Se::Shape(sid) = self.ir.ses[xs[i].index()] else {
                        unreachable!("a shape")
                    };
                    let rest = xs
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != i)
                        .map(|(_, x)| *x)
                        .collect();
                    out.push((sid, rest));
                }
            }
            Se::Or(xs) => {
                for &x in xs {
                    match self.ir.ses[x.index()] {
                        Se::Ref(k) => self.variants_of(self.exact_se(k), out),
                        _ => self.variants_of(x, out),
                    }
                }
            }
            Se::Ref(k) => self.variants_of(self.exact_se(*k), out),
            Se::Not(_) | Se::Nc(_) | Se::External => {}
        }
    }
}

/// The frame of a set of parts: each part's ancestors are the parts it reaches.
fn finish_frame(parts: &[PartBuilder]) -> Frame {
    let below = |i: usize| {
        let mut seen = vec![false; parts.len()];
        let mut stack = parts[i].direct.clone();
        while let Some(j) = stack.pop() {
            if !seen[j] {
                seen[j] = true;
                stack.extend(&parts[j].direct);
            }
        }
        (0..parts.len()).filter(|&j| seen[j]).collect::<Vec<_>>()
    };
    Frame {
        parts: parts
            .iter()
            .enumerate()
            .map(|(i, p)| Part {
                shape: p.shape,
                constraints: p.constraints.clone(),
                below: below(i),
            })
            .collect(),
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
        Some(l) => {
            let shown = || show_label(&Label::from_shexj(&l), &schema.prefixes);
            let k = schema
                .label(&l)
                .ok_or_else(|| SchemaError::new(format!("undefined shape label {}", shown())))?;
            let se = schema.ir.pairs[k.index()].se;
            if matches!(schema.ir.ses[se.index()], Se::External) {
                return Err(SchemaError::new(format!(
                    "external shape {} has no definition",
                    shown()
                )));
            }
            Ok(k)
        }
    }
}

#[cfg(test)]
mod tests;
