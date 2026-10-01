//! The per-query geometry memo (a bounded LRU of parsed literals) and the geometry
//! arguments of functions.

use super::geom::Geom;
use super::{Fnv, GeomRef};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row, Val, eval};
use crate::sparql::value::{EvalResult, TypeError, Value};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes of parsed geometries one query keeps.
pub const MEMO_BYTES: usize = 64 << 20;
/// Largest geometry functions accept when the dataset has no spatial index (whose
/// configuration sets it otherwise).
pub const DEFAULT_MAX_VERTICES: u32 = 1_000_000;
/// Largest sum of input vertices of one operation, unless the store sets another.
pub const DEFAULT_OP_VERTICES: u64 = 2_000_000;

/// What a geometry is remembered by: its term id, or a hash of `(datatype, lexical
/// form)` for computed values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemoKey {
    Id(Id),
    Lex(u64),
}

impl MemoKey {
    pub fn lex(dt: &str, lex: &str) -> MemoKey {
        let mut h = Fnv::new();
        h.field(dt.as_bytes());
        h.field(lex.as_bytes());
        MemoKey::Lex(h.finish())
    }
}

/// Parsed geometries of one query, keyed by term id or by a hash of `(datatype, lexical
/// form)`, least recently used first out once they hold more than the budget. Ill-typed
/// literals are remembered too, so a bad literal on many rows is parsed once.
pub struct GeoMemo {
    lru: Mutex<Lru>,
    budget: usize,
    op_vertices: AtomicU64,
}

impl Default for GeoMemo {
    fn default() -> Self {
        GeoMemo::with_budget(MEMO_BYTES)
    }
}

impl GeoMemo {
    pub fn with_budget(bytes: usize) -> GeoMemo {
        GeoMemo {
            lru: Mutex::new(Lru::default()),
            budget: bytes,
            op_vertices: AtomicU64::new(DEFAULT_OP_VERTICES),
        }
    }

    /// The geometry under `key`, parsing it with `parse` on a miss (`None`: not a valid
    /// geometry).
    pub fn get_or_parse(
        &self,
        key: MemoKey,
        parse: impl FnOnce() -> Option<GeomRef>,
    ) -> Option<GeomRef> {
        if let Some(hit) = self.lru.lock().get(key) {
            return hit;
        }
        // parse without the lock; two threads may both parse a literal once
        let g = parse();
        let bytes = g.as_ref().map_or(64, |g| g.mem_size());
        self.lru.lock().insert(key, g.clone(), bytes, self.budget);
        g
    }

    /// Geometries held.
    pub fn len(&self) -> usize {
        self.lru.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Estimated bytes held.
    pub fn bytes(&self) -> usize {
        self.lru.lock().bytes
    }

    /// Set the largest sum of input vertices of one operation (the store's
    /// `geo_op_vertices`).
    pub fn set_op_vertices(&self, n: u64) {
        self.op_vertices.store(n, Ordering::Relaxed);
    }

    pub fn op_vertices(&self) -> u64 {
        self.op_vertices.load(Ordering::Relaxed)
    }
}

const NIL: usize = usize::MAX;

struct Slot {
    key: MemoKey,
    val: Option<GeomRef>,
    bytes: usize,
    prev: usize,
    next: usize,
}

/// A doubly linked list over a slab, most recent at `head`.
struct Lru {
    map: FxHashMap<MemoKey, usize>,
    slots: Vec<Slot>,
    free: Vec<usize>,
    head: usize,
    tail: usize,
    bytes: usize,
}

impl Default for Lru {
    fn default() -> Self {
        Lru {
            map: FxHashMap::default(),
            slots: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
            bytes: 0,
        }
    }
}

impl Lru {
    fn unlink(&mut self, i: usize) {
        let (prev, next) = (self.slots[i].prev, self.slots[i].next);
        match prev {
            NIL => self.head = next,
            p => self.slots[p].next = next,
        }
        match next {
            NIL => self.tail = prev,
            n => self.slots[n].prev = prev,
        }
    }

    fn push_front(&mut self, i: usize) {
        self.slots[i].prev = NIL;
        self.slots[i].next = self.head;
        match self.head {
            NIL => self.tail = i,
            h => self.slots[h].prev = i,
        }
        self.head = i;
    }

    /// `Some(entry)` on a hit (moved to the front).
    fn get(&mut self, key: MemoKey) -> Option<Option<GeomRef>> {
        let i = *self.map.get(&key)?;
        if self.head != i {
            self.unlink(i);
            self.push_front(i);
        }
        Some(self.slots[i].val.clone())
    }

    fn insert(&mut self, key: MemoKey, val: Option<GeomRef>, bytes: usize, budget: usize) {
        if self.map.contains_key(&key) {
            return;
        }
        let slot = Slot {
            key,
            val,
            bytes,
            prev: NIL,
            next: NIL,
        };
        let i = match self.free.pop() {
            Some(i) => {
                self.slots[i] = slot;
                i
            }
            None => {
                self.slots.push(slot);
                self.slots.len() - 1
            }
        };
        self.map.insert(key, i);
        self.push_front(i);
        self.bytes += bytes;
        // the newest entry stays even when it alone is over the budget
        while self.bytes > budget && self.tail != i {
            let t = self.tail;
            self.unlink(t);
            self.map.remove(&self.slots[t].key);
            self.bytes -= self.slots[t].bytes;
            self.slots[t].val = None;
            self.free.push(t);
        }
    }
}

/// The largest geometry functions accept in this query (`maxVertices`).
pub fn max_vertices(ctx: &Ctx) -> u32 {
    ctx.snap
        .geo
        .as_ref()
        .map_or(DEFAULT_MAX_VERTICES, |v| v.config.max_vertices)
}

/// Check the `maxOpVertices` budget of one operation over `inputs` (a type error when
/// their vertices add up to more).
pub fn check_op_vertices(ctx: &Ctx, inputs: &[&Geom]) -> EvalResult<()> {
    let total: u64 = inputs.iter().map(|g| u64::from(g.vertices)).sum();
    if total > ctx.geo.op_vertices() {
        Err(TypeError)
    } else {
        Ok(())
    }
}

/// A geometry from a decoded value (`None`: not a geometry literal, or ill-typed).
pub fn parse_value(v: &Value, max_vertices: u32) -> Option<GeomRef> {
    match v {
        Value::Other { lex, dt } if super::parse::is_geometry_datatype(dt) => {
            super::parse::parse_limited(lex, dt, max_vertices)
                .ok()
                .map(Arc::new)
        }
        _ => None,
    }
}

/// The geometry of a term id: the generation's geometry column for the stored
/// literals it holds, else the memo.
pub(crate) fn by_id(ctx: &Ctx, id: Id, decoded: Option<&Value>) -> EvalResult<GeomRef> {
    if id.is_undef() {
        return Err(TypeError);
    }
    if let Some(g) = from_column(ctx, id) {
        return Ok(g);
    }
    let max = max_vertices(ctx);
    ctx.geo
        .get_or_parse(MemoKey::Id(id), || match decoded {
            Some(v) => parse_value(v, max),
            None => parse_value(&ctx.value(id)?, max),
        })
        .ok_or(TypeError)
}

/// The geometry column's parse of a stored literal, when the dataset's spatial index
/// has one for `id`.
fn from_column(ctx: &Ctx, id: Id) -> Option<GeomRef> {
    if !matches!(id.tag(), crate::id::Tag::Vocab | crate::id::Tag::Delta) {
        return None;
    }
    // a transaction's view keeps its base's column: its ids are those of the generation
    let base = ctx.snap.geo.as_deref()?.base.as_ref()?;
    if base.generation != ctx.snap.generation.uid {
        return None;
    }
    match base.column.get(id.0)? {
        super::column::Slot::Geom(e) => e.geom(&ctx.snap).ok(),
        _ => None,
    }
}

/// Argument `i` of a function call as a geometry: the generation's geometry column for
/// stored literals, else the memo (parsing on a miss). Not a geometry: a type error.
#[allow(dead_code)] // the functions call it
pub(crate) fn geom_arg(args: &[Expr], i: usize, row: &Row<'_>, ctx: &Ctx) -> EvalResult<GeomRef> {
    let e = args.get(i).ok_or(TypeError)?;
    match e {
        Expr::Var(v) => by_id(ctx, row.get(*v), None),
        Expr::Const(id) => by_id(ctx, *id, None),
        Expr::Lit(id, v) => by_id(ctx, *id, Some(v)),
        _ => match eval(e, row, ctx)? {
            Val::Id(id) => by_id(ctx, id, None),
            Val::Dec(id, v) => by_id(ctx, id, Some(&v)),
            Val::V(v) => {
                let Value::Other { lex, dt } = &v else {
                    return Err(TypeError);
                };
                let max = max_vertices(ctx);
                ctx.geo
                    .get_or_parse(MemoKey::lex(dt, lex), || parse_value(&v, max))
                    .ok_or(TypeError)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::CrsRef;
    use crate::geo::vocab::{GEOJSON_LITERAL, WKT_LITERAL};
    use crate::sparql::table::Table;
    use crate::store::{Store, StoreOptions};

    fn point(n: u32) -> GeomRef {
        Arc::new(Geom::from_geometry(
            CrsRef::Known(crate::geo::crs::CRS84),
            georust::Point::new(f64::from(n), 0.0).into(),
        ))
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        // a point is 176 bytes: room for three
        let memo = GeoMemo::with_budget(3 * 176);
        let key = |n: u64| MemoKey::Lex(n);
        for n in 0..3 {
            memo.get_or_parse(key(u64::from(n)), || Some(point(n)));
        }
        assert_eq!((memo.len(), memo.bytes()), (3, 3 * 176));
        // touch 0, so 1 is the oldest
        assert!(
            memo.get_or_parse(key(0), || panic!("hit expected"))
                .is_some()
        );
        memo.get_or_parse(key(3), || Some(point(3)));
        assert_eq!(memo.len(), 3);
        let mut parsed = false;
        memo.get_or_parse(key(1), || {
            parsed = true;
            Some(point(1))
        });
        assert!(parsed, "1 was evicted");
        assert!(memo.get_or_parse(key(0), || panic!("0 was kept")).is_some());
        // ill-typed literals are remembered
        assert!(memo.get_or_parse(key(9), || None).is_none());
        assert!(memo.get_or_parse(key(9), || panic!("remembered")).is_none());
        // an entry larger than the whole budget is kept alone
        let big = GeoMemo::with_budget(10);
        big.get_or_parse(key(0), || Some(point(0)));
        big.get_or_parse(key(1), || Some(point(1)));
        assert_eq!(big.len(), 1);
        assert!(big.get_or_parse(key(1), || panic!("kept")).is_some());
    }

    #[test]
    fn keys() {
        assert_eq!(
            MemoKey::lex(WKT_LITERAL, "POINT(1 2)"),
            MemoKey::lex(WKT_LITERAL, "POINT(1 2)")
        );
        assert_ne!(
            MemoKey::lex(WKT_LITERAL, "POINT(1 2)"),
            MemoKey::lex(GEOJSON_LITERAL, "POINT(1 2)")
        );
    }

    fn lit(lex: &str, dt: &str) -> Value {
        Value::Other {
            lex: lex.into(),
            dt: dt.into(),
        }
    }

    #[test]
    fn geometry_arguments() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let table = Table::unit();
        let row = Row {
            table: &table,
            i: 0,
            map: &[],
            dec: None,
        };
        let wkt = lit("POINT(1 2)", WKT_LITERAL);
        let id = ctx.intern_value(&wkt);
        let args = vec![
            Expr::Const(id),
            Expr::Lit(id, wkt.clone()),
            Expr::Const(ctx.intern_value(&lit("POINT(1", WKT_LITERAL))),
            Expr::Const(ctx.intern_value(&Value::Str("POINT(1 2)".into()))),
            Expr::Const(ctx.intern_value(&lit(
                r#"{"type":"Point","coordinates":[1,2]}"#,
                GEOJSON_LITERAL,
            ))),
            Expr::Var(0),
        ];
        let a = geom_arg(&args, 0, &row, &ctx).unwrap();
        let b = geom_arg(&args, 1, &row, &ctx).unwrap();
        // one parse for the id
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.g, georust::Point::new(1.0, 2.0).into());
        assert!(geom_arg(&args, 2, &row, &ctx).is_err());
        assert!(geom_arg(&args, 3, &row, &ctx).is_err());
        assert_eq!(
            geom_arg(&args, 4, &row, &ctx).unwrap().g,
            georust::Point::new(1.0, 2.0).into()
        );
        // unbound, and no such argument
        assert!(geom_arg(&args, 5, &row, &ctx).is_err());
        assert!(geom_arg(&args, 6, &row, &ctx).is_err());
        assert_eq!(ctx.geo.len(), 4);
    }

    #[test]
    fn stored_literals_come_from_the_column() {
        const AS_WKT: &str = "<http://www.opengis.net/ont/geosparql#asWKT>";
        let triple = |s: &str, lex: &str| {
            format!("<http://example.org/{s}> {AS_WKT} \"{lex}\"^^<{WKT_LITERAL}> .")
        };
        let ds = crate::dataset::Dataset::memory();
        ds.load_str(&triple("a", "POINT(1 2)"), crate::io::RdfFormat::NTriples)
            .unwrap();
        let literal = |lex: &str| {
            oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
                lex,
                oxrdf::NamedNode::new_unchecked(WKT_LITERAL),
            ))
        };
        let table = Table::unit();
        let row = Row {
            table: &table,
            i: 0,
            map: &[],
            dec: None,
        };
        let arg = |ctx: &Ctx, lex: &str| {
            let id = ctx.snap.lookup_term(&literal(lex)).unwrap();
            geom_arg(&[Expr::Const(id)], 0, &row, ctx).unwrap()
        };
        // without the index, a stored literal is parsed into the memo
        let ctx = Ctx::new(ds.snapshot());
        arg(&ctx, "POINT(1 2)");
        assert_eq!(ctx.geo.len(), 1);
        ds.store()
            .enable_geo(crate::geo::GeoConfig::default())
            .unwrap();
        ds.update(&format!("INSERT DATA {{ {} }}", triple("b", "POINT(3 4)")))
            .unwrap();
        // with it, base and delta literals come from the column: the memo misses nothing
        let ctx = Ctx::new(ds.snapshot());
        let a = arg(&ctx, "POINT(1 2)");
        let b = arg(&ctx, "POINT(3 4)");
        assert_eq!(ctx.geo.len(), 0);
        assert_eq!(b.g, georust::Point::new(3.0, 4.0).into());
        let view = ctx.snap.geo.clone().unwrap();
        let base = view.base.as_ref().unwrap();
        let id = ctx.snap.lookup_term(&literal("POINT(1 2)")).unwrap();
        let Some(crate::geo::column::Slot::Geom(e)) = base.column.get(id.0) else {
            panic!("indexed");
        };
        assert!(Arc::ptr_eq(&a, &e.geom(&ctx.snap).unwrap()));
    }

    #[test]
    fn vertex_budgets() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        assert_eq!(max_vertices(&ctx), DEFAULT_MAX_VERTICES);
        let a = point(1);
        assert!(check_op_vertices(&ctx, &[&a, &a]).is_ok());
        ctx.geo.set_op_vertices(1);
        assert_eq!(check_op_vertices(&ctx, &[&a, &a]), Err(TypeError));
        assert!(check_op_vertices(&ctx, &[&a]).is_ok());
    }
}
