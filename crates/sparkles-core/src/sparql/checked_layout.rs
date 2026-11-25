//! Owned allocation layouts for copying a completed checked plan.

use super::expr::{Expr, Func};
use super::plan::{Agg, GraphFilter, Kind, Node, ScanSpec, TextSpec, VectorSpec};
use super::table::Table;
use super::walk::{Algebra, visit_algebra, visit_algebra_ordinary};
use crate::access::Preparation;
use crate::error::Result;
use oxrdf::{Literal, Term};
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern,
    PropertyPathExpression,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

pub(super) struct Layout<'a> {
    preparation: Option<&'a Preparation>,
    bytes: u64,
    retained: bool,
}

impl<'a> Layout<'a> {
    pub(super) fn new(preparation: &'a Preparation) -> Self {
        Self {
            preparation: Some(preparation),
            bytes: 0,
            retained: false,
        }
    }

    pub(super) fn retained(preparation: &'a Preparation) -> Self {
        Self {
            retained: true,
            ..Self::new(preparation)
        }
    }

    pub(super) fn unchecked(retained: bool) -> Layout<'static> {
        Layout {
            preparation: None,
            bytes: 0,
            retained,
        }
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn add(&mut self, bytes: usize) -> Result<()> {
        if let Some(preparation) = self.preparation {
            preparation.check()?;
            self.bytes = self
                .bytes
                .checked_add(bytes as u64)
                .ok_or_else(|| preparation.exhausted())?;
        } else {
            self.bytes = self.bytes.saturating_add(bytes as u64);
        }
        Ok(())
    }

    pub(super) fn slots<T>(&mut self, count: usize) -> Result<()> {
        if let Some(preparation) = self.preparation {
            self.add(
                count
                    .checked_mul(size_of::<T>())
                    .ok_or_else(|| preparation.exhausted())?,
            )
        } else {
            self.bytes = self
                .bytes
                .saturating_add((count as u64).saturating_mul(size_of::<T>() as u64));
            Ok(())
        }
    }

    pub(super) fn slice<T>(&mut self, values: &[T]) -> Result<()> {
        self.slots::<T>(values.len())
    }

    pub(super) fn vector<T>(&mut self, values: &Vec<T>) -> Result<()> {
        let slots = if self.retained {
            values.capacity()
        } else {
            values.len()
        };
        self.slots::<T>(slots)
    }

    /// The larger of the layouts `a` and `b` add, for storage of two steps that is never
    /// live at once.
    pub(super) fn larger(
        &mut self,
        a: impl FnOnce(&mut Self) -> Result<()>,
        b: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let start = self.bytes;
        a(self)?;
        let first = self.bytes - start;
        self.bytes = start;
        b(self)?;
        self.bytes = self.bytes.max(start + first);
        Ok(())
    }

    fn optional_vector<T>(&mut self, values: Option<&Vec<T>>) -> Result<()> {
        if let Some(values) = values {
            self.vector(values)?;
        }
        Ok(())
    }

    fn string(&mut self, value: &String) -> Result<()> {
        self.add(if self.retained {
            value.capacity()
        } else {
            value.len()
        })
    }

    /// A raw hash table stores each key/value pair and a control byte per
    /// bucket. Its usable capacity is at least half the bucket count.
    /// Round up, including the trailing control group and alignment.
    pub(super) fn map<K, V>(&mut self, capacity: usize) -> Result<()> {
        if capacity == 0 {
            return Ok(());
        }
        let buckets = if let Some(preparation) = self.preparation {
            capacity
                .checked_next_power_of_two()
                .and_then(|count| count.max(2).checked_mul(2))
                .ok_or_else(|| preparation.exhausted())?
        } else {
            capacity
                .checked_next_power_of_two()
                .and_then(|count| count.max(2).checked_mul(2))
                .unwrap_or(usize::MAX)
        };
        self.slots::<(K, V)>(buckets)?;
        self.add(buckets)?;
        self.add(align_of::<(K, V)>().max(64))
    }

    fn texts(&mut self, values: &Vec<String>) -> Result<()> {
        self.vector(values)?;
        for value in values {
            self.string(value)?;
        }
        Ok(())
    }

    fn literal(&mut self, literal: &Literal) -> Result<()> {
        self.add(literal.value().len())?;
        if let Some(language) = literal.language() {
            self.add(language.len())?;
        } else if literal.datatype() != oxrdf::vocab::xsd::STRING {
            self.add(literal.datatype().as_str().len())?;
        }
        Ok(())
    }

    fn term_pattern(&mut self, term: &TermPattern) -> Result<()> {
        match term {
            TermPattern::NamedNode(node) => self.add(node.as_str().len()),
            TermPattern::BlankNode(node) => self.add(node.as_str().len()),
            TermPattern::Variable(var) => self.add(var.as_str().len()),
            TermPattern::Literal(literal) => self.literal(literal),
            TermPattern::Triple(triple) => {
                self.slots::<TriplePattern>(1)?;
                self.triple(triple)
            }
        }
    }

    fn named_pattern(&mut self, pattern: &NamedNodePattern) -> Result<()> {
        self.add(match pattern {
            NamedNodePattern::NamedNode(node) => node.as_str().len(),
            NamedNodePattern::Variable(var) => var.as_str().len(),
        })
    }

    fn triple(&mut self, triple: &TriplePattern) -> Result<()> {
        self.term_pattern(&triple.subject)?;
        self.named_pattern(&triple.predicate)?;
        self.term_pattern(&triple.object)
    }

    pub(super) fn ground(&mut self, term: &GroundTerm) -> Result<()> {
        match term {
            GroundTerm::NamedNode(node) => self.add(node.as_str().len()),
            GroundTerm::Literal(literal) => self.literal(literal),
            GroundTerm::Triple(triple) => {
                self.slots::<spargebra::term::GroundTriple>(1)?;
                self.add(triple.subject.as_str().len())?;
                self.add(triple.predicate.as_str().len())?;
                self.ground(&triple.object)
            }
        }
    }

    fn term(&mut self, term: &Term) -> Result<()> {
        match term {
            Term::NamedNode(node) => self.add(node.as_str().len()),
            Term::BlankNode(node) => self.add(node.as_str().len()),
            Term::Literal(literal) => self.literal(literal),
            Term::Triple(triple) => {
                self.slots::<oxrdf::Triple>(1)?;
                self.add(match &triple.subject {
                    oxrdf::NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                    oxrdf::NamedOrBlankNode::BlankNode(node) => node.as_str().len(),
                })?;
                self.add(triple.predicate.as_str().len())?;
                self.term(&triple.object)
            }
        }
    }

    pub(super) fn path(&mut self, path: &PropertyPathExpression) -> Result<()> {
        use PropertyPathExpression as P;
        match path {
            P::NamedNode(node) => self.add(node.as_str().len()),
            P::Reverse(inner)
            | P::ZeroOrMore(inner)
            | P::OneOrMore(inner)
            | P::ZeroOrOne(inner)
            | P::Range { path: inner, .. }
            | P::Distinct(inner)
            | P::Multi(inner)
            | P::Shortest(inner) => {
                self.slots::<P>(1)?;
                self.path(inner)
            }
            P::Sequence(left, right) | P::Alternative(left, right) => {
                self.slots::<P>(2)?;
                self.path(left)?;
                self.path(right)
            }
            P::NegatedPropertySet(nodes) => {
                self.slice(nodes)?;
                for node in nodes {
                    self.add(node.as_str().len())?;
                }
                Ok(())
            }
        }
    }

    fn aggregate_name(&mut self, name: &AggregateFunction) -> Result<()> {
        match name {
            AggregateFunction::Custom(node) => self.add(node.as_str().len()),
            AggregateFunction::GroupConcat { separator } => {
                self.add(separator.as_ref().map_or(0, String::len))
            }
            AggregateFunction::Count
            | AggregateFunction::Sum
            | AggregateFunction::Avg
            | AggregateFunction::Min
            | AggregateFunction::Max
            | AggregateFunction::Sample => Ok(()),
        }
    }

    pub(super) fn syntax(&mut self, root: Algebra<'_>) -> Result<()> {
        let preparation = self.preparation;
        let visit = |item| match item {
            Algebra::Pattern(pattern) => self.pattern_item(pattern),
            Algebra::Expr(expression) => self.expression_item(expression),
            Algebra::Aggregate(aggregate) => match aggregate {
                AggregateExpression::CountSolutions { .. } => Ok(()),
                AggregateExpression::FunctionCall { name, .. } => self.aggregate_name(name),
                AggregateExpression::Fold { order, .. } => self.slice(order),
            },
        };
        match preparation {
            Some(preparation) => visit_algebra(root, preparation, visit),
            None => visit_algebra_ordinary(root, visit),
        }
    }

    fn pattern_item(&mut self, pattern: &GraphPattern) -> Result<()> {
        use GraphPattern as P;
        match pattern {
            P::Bgp { patterns } => {
                self.slice(patterns)?;
                for pattern in patterns {
                    self.triple(pattern)?;
                }
                Ok(())
            }
            P::Path {
                subject,
                path,
                object,
            } => {
                self.term_pattern(subject)?;
                self.path(path)?;
                self.term_pattern(object)
            }
            P::Join { .. }
            | P::LeftJoin { .. }
            | P::Lateral { .. }
            | P::Union { .. }
            | P::Minus { .. }
            | P::SemiJoin { .. }
            | P::AntiJoin { .. } => self.slots::<P>(2),
            P::Filter { .. } | P::Distinct { .. } | P::Reduced { .. } | P::Slice { .. } => {
                self.slots::<P>(1)
            }
            P::Graph { name, .. } | P::Service { name, .. } => {
                self.slots::<P>(1)?;
                self.named_pattern(name)
            }
            P::Extend { variable, .. } | P::Assign { variable, .. } => {
                self.slots::<P>(1)?;
                self.add(variable.as_str().len())
            }
            P::Unfold {
                variable, second, ..
            } => {
                self.slots::<P>(1)?;
                self.add(variable.as_str().len())?;
                self.add(second.as_ref().map_or(0, |v| v.as_str().len()))
            }
            P::OrderBy { expression, .. } => {
                self.slots::<P>(1)?;
                self.slice(expression)
            }
            P::Project { variables, .. } => {
                self.slots::<P>(1)?;
                self.variables(variables)
            }
            P::Group {
                variables,
                aggregates,
                ..
            } => {
                self.slots::<P>(1)?;
                self.variables(variables)?;
                self.slice(aggregates)?;
                for (var, _) in aggregates {
                    self.add(var.as_str().len())?;
                }
                Ok(())
            }
            P::Values {
                variables,
                bindings,
            } => {
                self.variables(variables)?;
                self.slice(bindings)?;
                for row in bindings {
                    self.slice(row)?;
                    for term in row.iter().flatten() {
                        self.ground(term)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn variables(&mut self, variables: &[oxrdf::Variable]) -> Result<()> {
        self.slice(variables)?;
        for var in variables {
            self.add(var.as_str().len())?;
        }
        Ok(())
    }

    fn expression_item(&mut self, expression: &Expression) -> Result<()> {
        use Expression as E;
        match expression {
            E::NamedNode(node) => self.add(node.as_str().len()),
            E::Literal(literal) => self.literal(literal),
            E::Variable(var) | E::Bound(var) => self.add(var.as_str().len()),
            E::Or(..)
            | E::And(..)
            | E::Equal(..)
            | E::SameTerm(..)
            | E::Greater(..)
            | E::GreaterOrEqual(..)
            | E::Less(..)
            | E::LessOrEqual(..)
            | E::Add(..)
            | E::Subtract(..)
            | E::Multiply(..)
            | E::Divide(..) => self.slots::<E>(2),
            E::Not(_) | E::UnaryPlus(_) | E::UnaryMinus(_) => self.slots::<E>(1),
            E::If(..) => self.slots::<E>(3),
            E::Exists(_) => self.slots::<GraphPattern>(1),
            E::In(_, args) => {
                self.slots::<E>(1)?;
                self.slice(args)
            }
            E::Coalesce(args) => self.slice(args),
            E::FunctionCall(function, args) => {
                self.function(function)?;
                self.slice(args)
            }
        }
    }

    fn function(&mut self, function: &Function) -> Result<()> {
        if let Function::Custom(node) = function {
            self.add(node.as_str().len())?;
        }
        Ok(())
    }

    pub(super) fn expressions(&mut self, expressions: &Vec<Expr>) -> Result<()> {
        self.vector(expressions)?;
        for expression in expressions {
            self.expr(expression)?;
        }
        Ok(())
    }

    /// The storage that a copy of `expression` owns beyond its own slot.
    pub(super) fn expression(&mut self, expression: &Expr) -> Result<()> {
        self.expr(expression)
    }

    fn expr_box(&mut self, expr: &Expr) -> Result<()> {
        self.slots::<Expr>(1)?;
        self.expr(expr)
    }

    fn expr(&mut self, expression: &Expr) -> Result<()> {
        if let Some(preparation) = self.preparation {
            preparation.check()?;
        }
        match expression {
            // Value's heap variants and ExistsSpec are Arc-backed.
            Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) | Expr::Exists(_) => {
                Ok(())
            }
            Expr::Or(a, b)
            | Expr::And(a, b)
            | Expr::Eq(a, b)
            | Expr::SameTerm(a, b)
            | Expr::Cmp(a, b, _)
            | Expr::Arith(a, b, _) => {
                self.expr_box(a)?;
                self.expr_box(b)
            }
            Expr::Not(expr) | Expr::Pos(expr) | Expr::Neg(expr) => self.expr_box(expr),
            Expr::If(a, b, c) => {
                self.expr_box(a)?;
                self.expr_box(b)?;
                self.expr_box(c)
            }
            Expr::In(expr, args) => {
                self.expr_box(expr)?;
                self.expressions(args)
            }
            Expr::Coalesce(args) => self.expressions(args),
            Expr::Call(func, args) => {
                match func {
                    Func::Builtin(function) => self.function(function)?,
                    Func::Cast(node) => self.add(node.as_str().len())?,
                    Func::Ext(text) => self.string(text)?,
                    Func::Registered(_) => {}
                }
                self.expressions(args)
            }
        }
    }

    fn expr_option(&mut self, expression: Option<&Expr>) -> Result<()> {
        if let Some(expression) = expression {
            self.expr(expression)?;
        }
        Ok(())
    }

    fn order(&mut self, order: &Vec<(Expr, bool)>) -> Result<()> {
        self.vector(order)?;
        for (expr, _) in order {
            self.expr(expr)?;
        }
        Ok(())
    }

    fn agg(&mut self, agg: &Agg) -> Result<()> {
        self.aggregate_name(&agg.func)?;
        self.expr_option(agg.expr.as_ref())?;
        if let Some(fold) = &agg.fold {
            self.slots::<super::plan::Fold>(1)?;
            self.expr_option(fold.value.as_ref())?;
            self.order(&fold.order)?;
        }
        Ok(())
    }

    pub(super) fn table(&mut self, table: &Table) -> Result<()> {
        self.vector(&table.vars)?;
        self.vector(&table.sorted)?;
        self.vector(&table.cols)?;
        for column in &table.cols {
            self.vector(column)?;
        }
        Ok(())
    }

    pub(super) fn graph(&mut self, graph: &GraphFilter) -> Result<()> {
        match graph {
            GraphFilter::Set(ids) => self.vector(ids),
            GraphFilter::All
            | GraphFilter::Default
            | GraphFilter::Named
            | GraphFilter::One(_)
            | GraphFilter::PreparedSet(_) => Ok(()),
        }
    }

    pub(super) fn scan(&mut self, scan: &ScanSpec) -> Result<()> {
        self.vector(&scan.prefix)?;
        self.vector(&scan.cols)?;
        self.vector(&scan.eqs)?;
        self.graph(&scan.graph)
    }

    fn text(&mut self, spec: &TextSpec) -> Result<()> {
        self.slots::<TextSpec>(1)?;
        self.string(&spec.query)?;
        self.texts(&spec.predicates)?;
        if let Some(lang) = &spec.lang {
            self.string(lang)?;
        }
        self.graph(&spec.graph)?;
        if let Some(highlight) = &spec.highlight {
            self.string(&highlight.start)?;
            self.string(&highlight.end)?;
            self.string(&highlight.frag_sep)?;
        }
        Ok(())
    }

    fn vector_search(&mut self, spec: &VectorSpec) -> Result<()> {
        self.slots::<VectorSpec>(1)?;
        self.graph(&spec.graph)
    }

    pub(super) fn metadata(&mut self, node: &Node) -> Result<()> {
        self.vector(&node.vars)?;
        self.vector(&node.certain)?;
        self.vector(&node.sorted)?;
        self.map::<super::table::VarId, f64>(node.dist.capacity())?;
        self.string(&node.desc)
    }

    /// A node's own storage: its metadata, children vector and kind, but not its children.
    pub(super) fn shallow(&mut self, node: &Node) -> Result<()> {
        self.metadata(node)?;
        self.vector(&node.children)?;
        self.kind(&node.kind)
    }

    pub(super) fn node(&mut self, node: &Node) -> Result<()> {
        self.metadata(node)?;
        self.vector(&node.children)?;
        for child in &node.children {
            self.node(child)?;
        }
        self.kind(&node.kind)
    }

    fn kind(&mut self, kind: &Kind) -> Result<()> {
        match kind {
            Kind::Empty
            | Kind::GraphNames(_)
            | Kind::GraphBranches(_)
            | Kind::Minus
            | Kind::HalfJoin { .. }
            | Kind::Union
            | Kind::Distinct
            | Kind::Slice { .. }
            | Kind::Unpack { .. }
            | Kind::CountJoinRuns { .. } => Ok(()),
            Kind::Scan(scan)
            | Kind::CountScan { spec: scan, .. }
            | Kind::CountDistinctScan { spec: scan, .. } => self.scan(scan),
            Kind::RangeScan(scan, range) => {
                self.scan(scan)?;
                self.vector(&range.ranges)?;
                self.expressions(&range.filter)
            }
            Kind::OrderedTopK(spec) => {
                self.slots::<super::plan::OrderedTopK>(1)?;
                self.scan(&spec.scan)?;
                self.vector(&spec.pieces)?;
                self.expressions(&spec.filter)?;
                self.expressions(&spec.range_filter)?;
                self.vector(&spec.tie_order)?;
                self.slots::<Node>(1)?;
                self.node(&spec.fallback)
            }
            Kind::Values(table) => self.table(table),
            Kind::Join { keys, .. }
            | Kind::CountJoin { keys, .. }
            | Kind::Sort(keys)
            | Kind::Project(keys) => self.vector(keys),
            Kind::LeftJoin { expr } => self.expr_option(expr.as_ref()),
            Kind::KeyedJoin { expr, .. }
            | Kind::KeyedLeftJoin { expr, .. }
            | Kind::Extend(_, expr)
            | Kind::Assign(_, expr)
            | Kind::Unfold { expr, .. } => self.expr(expr),
            Kind::Filter(exprs) => self.expressions(exprs),
            Kind::OrderBy { keys, .. } => self.order(keys),
            Kind::Group { keys, aggs } => {
                self.vector(keys)?;
                self.vector(aggs)?;
                for (_, agg) in aggs {
                    self.agg(agg)?;
                }
                Ok(())
            }
            Kind::GroupCountScan { spec, counts, .. } => {
                self.scan(spec)?;
                self.vector(counts)
            }
            Kind::CountFilterScan { spec, filter, .. } => {
                self.scan(spec)?;
                self.expressions(filter)
            }
            Kind::Path { spec, .. } => {
                self.slots::<super::plan::PathSpec>(1)?;
                self.optional_vector(spec.preds.as_ref())?;
                self.graph(&spec.graph)
            }
            Kind::Service { query, .. } => self.string(query),
            Kind::TextSearch(spec) => self.text(spec),
            Kind::VectorSearch(spec) => self.vector_search(spec),
            Kind::HybridSearch(spec) => {
                self.slots::<super::hybrid::HybridSpec>(1)?;
                if let Some(text) = &spec.text {
                    self.text(text)?;
                }
                if let Some(vector) = &spec.vector {
                    self.vector_search(vector)?;
                }
                self.vector(&spec.text_vars)?;
                self.vector(&spec.vector_vars)
            }
            Kind::SpatialScan(spec) => {
                self.slots::<super::geopf::SpatialScanSpec>(1)?;
                self.scan(&spec.scan)?;
                self.vector(&spec.windows)?;
                self.vector(&spec.tests)?;
                self.expressions(&spec.filter)
            }
            Kind::SpatialPf(spec) => {
                self.slots::<super::geopf::SpatialPfSpec>(1)?;
                self.graph(&spec.graph)
            }
            Kind::IndexJoin(spec) => {
                self.slots::<super::indexjoin::IndexJoinSpec>(1)?;
                self.vector(&spec.probes)?;
                for probe in &spec.probes {
                    self.scan(&probe.scan)?;
                    self.expressions(&probe.filter)?;
                    if let Some(star) = &probe.star {
                        self.graph(&star.graph)?;
                    }
                }
                Ok(())
            }
            Kind::SpatialJoin(spec) => {
                self.slots::<super::geojoin::SpatialJoinSpec>(1)?;
                for side in [&spec.left, &spec.right] {
                    if let super::geojoin::JoinSide::Index { scan, .. } = side {
                        self.scan(scan)?;
                    }
                }
                self.expressions(&spec.filter)
            }
            Kind::SpatialKnn(spec) => {
                self.slots::<super::geojoin::SpatialKnnSpec>(1)?;
                self.scan(&spec.scan)?;
                self.vector(&spec.placeholder)?;
                self.expr(&spec.order)
            }
            Kind::SpatialRelate(spec) => {
                self.slots::<super::georewrite::SpatialRelateSpec>(1)?;
                self.graph(&spec.graph)
            }
            Kind::Lateral(spec) => {
                self.slots::<super::lateral::LateralSpec>(1)?;
                self.syntax(Algebra::Pattern(&spec.pattern))?;
                self.vector(&spec.keys)?;
                self.vector(&spec.outer)?;
                self.vector(&spec.outer_scoped)?;
                if let Some(Some(expr)) = &spec.optional {
                    self.syntax(Algebra::Expr(expr))?;
                }
                if let Some(service) = &spec.service {
                    self.slots::<super::enhancer::RemoteLoop>(1)?;
                    self.add(service.endpoint.as_str().len())?;
                    self.string(&service.idx)?;
                }
                Ok(())
            }
            Kind::PropertyFn(spec) => {
                self.slots::<super::arqpf::PfSpec>(1)?;
                self.vector(&spec.slots)?;
                self.vector(&spec.inputs)?;
                self.graph(&spec.graph)
            }
            Kind::RegisteredProperty(spec) => spec.copy_layout(self),
            Kind::PathSearch(spec) => {
                self.slots::<super::pathsearch::PathSearchSpec>(1)?;
                self.optional_vector(spec.predicates.as_ref())?;
                self.vector(&spec.directions)?;
                self.vector(&spec.exclude)?;
                self.graph(&spec.graph)
            }
            Kind::HistoryChanges(spec) => {
                self.slots::<super::history_svc::HistorySpec>(1)?;
                for slot in [&spec.s, &spec.p, &spec.o, &spec.g, &spec.op] {
                    if let super::history_svc::Slot::Const(term) = slot {
                        self.term(term)?;
                    }
                }
                for bound in [&spec.from, &spec.to] {
                    if let Some(crate::store::HistoryBound::At(crate::history::At::Snapshot(
                        name,
                    ))) = bound
                    {
                        self.string(name)?;
                    }
                }
                self.vector(&spec.inputs)
            }
            Kind::ViewRead(spec) => {
                self.slots::<super::viewsvc::ViewSpec>(1)?;
                self.string(&spec.name)?;
                self.vector(&spec.bind)?;
                for (name, _) in &spec.bind {
                    self.string(name)?;
                }
                Ok(())
            }
            Kind::AlgoCall(spec) => {
                self.slots::<super::algosvc::AlgoSpec>(1)?;
                self.optional_vector(spec.proj.predicates.as_ref())?;
                self.vector(&spec.proj.node_classes)?;
                self.texts(&spec.proj.node_class_names)?;
                self.graph(&spec.proj.graph)?;
                self.optional_vector(spec.iteration_weights.as_ref())?;
                self.vector(&spec.sources)?;
                // A map node holds a fixed number of keys and links. Charging
                // one node's full capacity per item bounds any valid fill level.
                let item = size_of::<(String, super::algosvc::Num)>() + size_of::<usize>();
                let node_bytes = 2 * size_of::<usize>() + 16 * item;
                for key in spec.nums.keys() {
                    self.add(node_bytes)?;
                    self.string(key)?;
                }
                Ok(())
            }
            Kind::PatternCount(spec) => {
                self.slots::<super::patternplan::PatternCountSpec>(1)?;
                self.vector(&spec.counts)
            }
            Kind::PatternScan(_) => self.slots::<super::patternplan::PatternScanSpec>(1),
        }
    }
}

/// Owned heap bytes of a cloned [`Node`] (including its root slot, metadata, children,
/// expressions, `VALUES` tables, and `OrderedTopK` fallback), excluding `Arc`-shared
/// `ExistsSpec`, `PreparedGraphNames`, `PreparedGraphBranches`, and `Value` backing.
pub(super) fn node_copy_bytes(node: &Node) -> u64 {
    let mut layout = Layout::unchecked(false);
    let _ = layout.slots::<Node>(1);
    let _ = layout.node(node);
    layout.bytes()
}

/// Owned retained heap bytes of [`Node`] using vector/string capacities (mirrors
/// [`Layout::retained`] without requiring a checked [`Preparation`]).
pub(super) fn node_retained_bytes(node: &Node) -> u64 {
    let mut layout = Layout::unchecked(true);
    let _ = layout.slots::<Node>(1);
    let _ = layout.node(node);
    layout.bytes()
}

/// Heap bytes owned by `Arc`-backed [`super::value::Value`] variants inside `Expr::Lit`.
pub(super) fn value_heap_bytes(value: &super::value::Value) -> u64 {
    use super::value::Value;
    let arc = (2 * size_of::<usize>()) as u64;
    match value {
        Value::Iri(s) | Value::BNode(s) | Value::Str(s) => arc.saturating_add(s.len() as u64),
        Value::Lang(s, l) | Value::LangDir(s, l, _) => arc
            .saturating_mul(2)
            .saturating_add(s.len() as u64)
            .saturating_add(l.len() as u64),
        Value::Other { lex, dt } => arc
            .saturating_mul(2)
            .saturating_add(lex.len() as u64)
            .saturating_add(dt.len() as u64),
        Value::Triple(_) => arc.saturating_add(size_of::<oxrdf::Triple>() as u64),
        _ => 0,
    }
}

/// Immediate heap bytes that `compile_expression` will allocate for `expr` (its boxed
/// children, pre-sized argument vector, literal `Value` strings, custom function IRI,
/// and description rendering slack). Does not recurse into sub-expressions or nested
/// `EXISTS` patterns.
pub(super) fn expression_compile_bytes(expr: &Expression) -> u64 {
    use Expression as E;
    let boxed: u64 = match expr {
        E::Or(..)
        | E::And(..)
        | E::Equal(..)
        | E::SameTerm(..)
        | E::Greater(..)
        | E::GreaterOrEqual(..)
        | E::Less(..)
        | E::LessOrEqual(..)
        | E::Add(..)
        | E::Subtract(..)
        | E::Multiply(..)
        | E::Divide(..) => 2,
        E::UnaryPlus(_) | E::UnaryMinus(_) | E::Not(_) | E::In(..) => 1,
        E::If(..) => 3,
        E::NamedNode(_)
        | E::Literal(_)
        | E::Variable(_)
        | E::Exists(_)
        | E::Bound(_)
        | E::Coalesce(_)
        | E::FunctionCall(..) => 0,
    };
    let args: u64 = match expr {
        E::In(_, args) | E::Coalesce(args) | E::FunctionCall(_, args) => args.len() as u64,
        _ => 0,
    };
    let slots = boxed
        .saturating_add(args)
        .saturating_mul(size_of::<Expr>() as u64);
    let extra: u64 = match expr {
        E::NamedNode(node) => node.as_str().len() as u64,
        E::Variable(var) | E::Bound(var) => (var.as_str().len() as u64).saturating_add(4),
        E::Literal(literal) => (literal.value().len() as u64)
            .saturating_add(literal.datatype().as_str().len() as u64)
            .saturating_add(literal.language().map_or(0, |l| l.len() as u64))
            .saturating_add((4 * size_of::<usize>()) as u64),
        E::FunctionCall(Function::Custom(iri), _) => iri.as_str().len() as u64,
        _ => 0,
    };
    slots.saturating_add(extra)
}

/// Owned heap bytes of a cloned [`GraphPattern`] beyond its root slot (mirrors
/// [`Layout::syntax`] without requiring a checked [`Preparation`]).
pub(super) fn pattern_bytes(pattern: &GraphPattern) -> u64 {
    fn literal_bytes(literal: &Literal) -> u64 {
        let mut bytes = literal.value().len() as u64;
        if let Some(language) = literal.language() {
            bytes = bytes.saturating_add(language.len() as u64);
        } else if literal.datatype() != oxrdf::vocab::xsd::STRING {
            bytes = bytes.saturating_add(literal.datatype().as_str().len() as u64);
        }
        bytes
    }

    fn named_pattern_bytes(pattern: &NamedNodePattern) -> u64 {
        match pattern {
            NamedNodePattern::NamedNode(node) => node.as_str().len() as u64,
            NamedNodePattern::Variable(var) => var.as_str().len() as u64,
        }
    }

    fn term_pattern_bytes(term: &TermPattern) -> u64 {
        match term {
            TermPattern::NamedNode(node) => node.as_str().len() as u64,
            TermPattern::BlankNode(node) => node.as_str().len() as u64,
            TermPattern::Variable(var) => var.as_str().len() as u64,
            TermPattern::Literal(literal) => literal_bytes(literal),
            TermPattern::Triple(triple) => {
                (size_of::<TriplePattern>() as u64).saturating_add(triple_bytes(triple))
            }
        }
    }

    fn triple_bytes(triple: &TriplePattern) -> u64 {
        term_pattern_bytes(&triple.subject)
            .saturating_add(named_pattern_bytes(&triple.predicate))
            .saturating_add(term_pattern_bytes(&triple.object))
    }

    fn ground_bytes(term: &GroundTerm) -> u64 {
        match term {
            GroundTerm::NamedNode(node) => node.as_str().len() as u64,
            GroundTerm::Literal(literal) => literal_bytes(literal),
            GroundTerm::Triple(triple) => (size_of::<spargebra::term::GroundTriple>() as u64)
                .saturating_add(triple.subject.as_str().len() as u64)
                .saturating_add(triple.predicate.as_str().len() as u64)
                .saturating_add(ground_bytes(&triple.object)),
        }
    }

    fn path_bytes(path: &PropertyPathExpression) -> u64 {
        use PropertyPathExpression as P;
        match path {
            P::NamedNode(node) => node.as_str().len() as u64,
            P::Reverse(inner)
            | P::ZeroOrMore(inner)
            | P::OneOrMore(inner)
            | P::ZeroOrOne(inner)
            | P::Range { path: inner, .. }
            | P::Distinct(inner)
            | P::Multi(inner)
            | P::Shortest(inner) => (size_of::<P>() as u64).saturating_add(path_bytes(inner)),
            P::Sequence(left, right) | P::Alternative(left, right) => (size_of::<P>() as u64 * 2)
                .saturating_add(path_bytes(left))
                .saturating_add(path_bytes(right)),
            P::NegatedPropertySet(nodes) => nodes.iter().fold(
                (nodes.len().saturating_mul(size_of::<oxrdf::NamedNode>())) as u64,
                |acc, node| acc.saturating_add(node.as_str().len() as u64),
            ),
        }
    }

    fn variables_bytes(variables: &[oxrdf::Variable]) -> u64 {
        variables.iter().fold(
            (variables.len().saturating_mul(size_of::<oxrdf::Variable>())) as u64,
            |acc, var| acc.saturating_add(var.as_str().len() as u64),
        )
    }

    fn aggregate_name_bytes(name: &AggregateFunction) -> u64 {
        match name {
            AggregateFunction::Custom(node) => node.as_str().len() as u64,
            AggregateFunction::GroupConcat { separator } => {
                separator.as_ref().map_or(0, |s| s.len() as u64)
            }
            AggregateFunction::Count
            | AggregateFunction::Sum
            | AggregateFunction::Avg
            | AggregateFunction::Min
            | AggregateFunction::Max
            | AggregateFunction::Sample => 0,
        }
    }

    let mut bytes = 0u64;
    let Ok(()) =
        visit_algebra_ordinary::<std::convert::Infallible>(Algebra::Pattern(pattern), |item| {
            let add = match item {
                Algebra::Pattern(gp) => {
                    use GraphPattern as P;
                    match gp {
                        P::Bgp { patterns } => patterns.iter().fold(
                            (patterns.len().saturating_mul(size_of::<TriplePattern>())) as u64,
                            |acc, tp| acc.saturating_add(triple_bytes(tp)),
                        ),
                        P::Path {
                            subject,
                            path,
                            object,
                        } => term_pattern_bytes(subject)
                            .saturating_add(path_bytes(path))
                            .saturating_add(term_pattern_bytes(object)),
                        P::Join { .. }
                        | P::LeftJoin { .. }
                        | P::Lateral { .. }
                        | P::Union { .. }
                        | P::Minus { .. }
                        | P::SemiJoin { .. }
                        | P::AntiJoin { .. } => (size_of::<P>() as u64).saturating_mul(2),
                        P::Filter { .. }
                        | P::Distinct { .. }
                        | P::Reduced { .. }
                        | P::Slice { .. } => size_of::<P>() as u64,
                        P::Graph { name, .. } | P::Service { name, .. } => {
                            (size_of::<P>() as u64).saturating_add(named_pattern_bytes(name))
                        }
                        P::Extend { variable, .. } | P::Assign { variable, .. } => {
                            (size_of::<P>() as u64).saturating_add(variable.as_str().len() as u64)
                        }
                        P::Unfold {
                            variable, second, ..
                        } => (size_of::<P>() as u64)
                            .saturating_add(variable.as_str().len() as u64)
                            .saturating_add(second.as_ref().map_or(0, |v| v.as_str().len() as u64)),
                        P::OrderBy { expression, .. } => (size_of::<P>() as u64).saturating_add(
                            (expression
                                .len()
                                .saturating_mul(size_of::<spargebra::algebra::OrderExpression>()))
                                as u64,
                        ),
                        P::Project { variables, .. } => {
                            (size_of::<P>() as u64).saturating_add(variables_bytes(variables))
                        }
                        P::Group {
                            variables,
                            aggregates,
                            ..
                        } => {
                            let mut total = (size_of::<P>() as u64)
                                .saturating_add(variables_bytes(variables))
                                .saturating_add(
                                    (aggregates.len().saturating_mul(size_of::<(
                                        oxrdf::Variable,
                                        AggregateExpression,
                                    )>(
                                    ))) as u64,
                                );
                            for (var, _) in aggregates {
                                total = total.saturating_add(var.as_str().len() as u64);
                            }
                            total
                        }
                        P::Values {
                            variables,
                            bindings,
                        } => {
                            let mut total = variables_bytes(variables).saturating_add(
                                (bindings
                                    .len()
                                    .saturating_mul(size_of::<Vec<Option<GroundTerm>>>()))
                                    as u64,
                            );
                            for row in bindings {
                                total = total.saturating_add(
                                    (row.len().saturating_mul(size_of::<Option<GroundTerm>>()))
                                        as u64,
                                );
                                for term in row.iter().flatten() {
                                    total = total.saturating_add(ground_bytes(term));
                                }
                            }
                            total
                        }
                    }
                }
                Algebra::Expr(expression) => {
                    use Expression as E;
                    match expression {
                        E::NamedNode(node) => node.as_str().len() as u64,
                        E::Literal(literal) => literal_bytes(literal),
                        E::Variable(var) | E::Bound(var) => var.as_str().len() as u64,
                        E::Or(..)
                        | E::And(..)
                        | E::Equal(..)
                        | E::SameTerm(..)
                        | E::Greater(..)
                        | E::GreaterOrEqual(..)
                        | E::Less(..)
                        | E::LessOrEqual(..)
                        | E::Add(..)
                        | E::Subtract(..)
                        | E::Multiply(..)
                        | E::Divide(..) => (size_of::<E>() as u64).saturating_mul(2),
                        E::Not(_) | E::UnaryPlus(_) | E::UnaryMinus(_) => size_of::<E>() as u64,
                        E::If(..) => (size_of::<E>() as u64).saturating_mul(3),
                        E::Exists(_) => size_of::<GraphPattern>() as u64,
                        E::In(_, args) => (size_of::<E>() as u64)
                            .saturating_add((args.len().saturating_mul(size_of::<E>())) as u64),
                        E::Coalesce(args) => (args.len().saturating_mul(size_of::<E>())) as u64,
                        E::FunctionCall(function, args) => {
                            let fn_bytes = match function {
                                Function::Custom(node) => node.as_str().len() as u64,
                                _ => 0,
                            };
                            fn_bytes
                                .saturating_add((args.len().saturating_mul(size_of::<E>())) as u64)
                        }
                    }
                }
                Algebra::Aggregate(aggregate) => match aggregate {
                    AggregateExpression::CountSolutions { .. } => 0,
                    AggregateExpression::FunctionCall { name, .. } => aggregate_name_bytes(name),
                    AggregateExpression::Fold { order, .. } => {
                        (order
                            .len()
                            .saturating_mul(size_of::<spargebra::algebra::OrderExpression>()))
                            as u64
                    }
                },
            };
            bytes = bytes.saturating_add(add);
            Ok(())
        });
    bytes
}
