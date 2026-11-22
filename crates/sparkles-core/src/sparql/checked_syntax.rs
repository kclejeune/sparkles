//! Immutable syntax retained by compiled EXISTS in checked graph branches.

use crate::access::{Preparation, PreparationBuffer, Reservation};
use crate::error::Result;
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal, NamedNode, Variable};
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use spargebra::term::{GroundTerm, GroundTriple, NamedNodePattern, TermPattern, TriplePattern};
use std::sync::Arc;

/// The group source is still borrowed when looking up an occurrence. The table
/// only includes addresses collected from that live source. Rewriter temporaries
/// receive their own owner and are never inserted in the occurrence table.
pub(super) struct Sources {
    occurrences: PreparationBuffer<(usize, Option<Arc<Pattern>>)>,
}

impl Sources {
    pub(super) fn new(group: &GraphPattern, preparation: &Preparation) -> Result<Self> {
        let mut occurrences = PreparationBuffer::new(preparation)?;
        super::walk::visit_exists(group, preparation, |pattern| {
            occurrences.push((std::ptr::from_ref(pattern) as usize, None))
        })?;
        Ok(Self { occurrences })
    }

    pub(super) fn get(
        &mut self,
        source: &GraphPattern,
        preparation: &Preparation,
    ) -> Result<Arc<Pattern>> {
        let address = std::ptr::from_ref(source) as usize;
        for (at, owner) in &mut *self.occurrences {
            preparation.check()?;
            if *at == address {
                if let Some(owner) = owner {
                    return Ok(owner.clone());
                }
                let pattern = Pattern::new(source, preparation)?;
                *owner = Some(pattern.clone());
                return Ok(pattern);
            }
        }
        Pattern::new(source, preparation)
    }
}

pub(super) struct Pattern {
    pub(super) syntax: GraphPattern,
    pub(super) calls_service: bool,
    _held: Reservation,
}

impl Pattern {
    fn new(source: &GraphPattern, preparation: &Preparation) -> Result<Arc<Self>> {
        preparation.stage("copying checked graph syntax")?;
        let mut layout = super::checked_layout::Layout::new(preparation);
        layout.add(size_of::<Self>() + 2 * size_of::<usize>())?;
        layout.syntax(super::walk::Algebra::Pattern(source))?;
        let held = preparation.reserve(layout.bytes())?;
        let mut copy = CopySyntax {
            preparation,
            calls_service: false,
        };
        let syntax = copy.graph(source)?;
        preparation.check()?;
        Ok(Arc::new(Self {
            syntax,
            calls_service: copy.calls_service,
            _held: held,
        }))
    }
}

pub(super) fn copy_path(
    path: &PropertyPathExpression,
    preparation: &Preparation,
) -> Result<PropertyPathExpression> {
    CopySyntax {
        preparation,
        calls_service: false,
    }
    .path(path)
}

struct CopySyntax<'a> {
    preparation: &'a Preparation,
    calls_service: bool,
}

// The complete clone is admitted first. On error the partial syntax drops
// before the reservation. Copies of large text and lists poll the preparation.
impl CopySyntax<'_> {
    fn boxed<T>(&mut self, source: &T, copy: fn(&mut Self, &T) -> Result<T>) -> Result<Box<T>> {
        self.preparation.check()?;
        Ok(Box::new(copy(self, source)?))
    }

    fn list<T>(&mut self, source: &[T], copy: fn(&mut Self, &T) -> Result<T>) -> Result<Vec<T>> {
        let mut out = Vec::new();
        out.try_reserve_exact(source.len())
            .map_err(|_| self.preparation.exhausted())?;
        for value in source {
            self.preparation.check()?;
            out.push(copy(self, value)?);
        }
        Ok(out)
    }

    fn text(&mut self, mut source: &str) -> Result<String> {
        let mut out = String::new();
        out.try_reserve_exact(source.len())
            .map_err(|_| self.preparation.exhausted())?;
        while source.len() > 4096 {
            self.preparation.stage("copying checked graph text")?;
            let mut end = 4096;
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            let (part, rest) = source.split_at(end);
            out.push_str(part);
            source = rest;
        }
        self.preparation.check()?;
        out.push_str(source);
        Ok(out)
    }

    fn iri(&mut self, node: &NamedNode) -> Result<NamedNode> {
        Ok(NamedNode::new_unchecked(self.text(node.as_str())?))
    }

    fn variable(&mut self, var: &Variable) -> Result<Variable> {
        Ok(Variable::new_unchecked(self.text(var.as_str())?))
    }

    fn literal(&mut self, literal: &Literal) -> Result<Literal> {
        let value = self.text(literal.value())?;
        if let Some(language) = literal.language() {
            let language = self.text(language)?;
            if let Some(direction) = literal.direction() {
                Ok(Literal::new_directional_language_tagged_literal_unchecked(
                    value, language, direction,
                ))
            } else {
                Ok(Literal::new_language_tagged_literal_unchecked(
                    value, language,
                ))
            }
        } else if literal.datatype() == xsd::STRING {
            Ok(Literal::new_simple_literal(value))
        } else {
            let datatype = NamedNode::new_unchecked(self.text(literal.datatype().as_str())?);
            Ok(Literal::new_typed_literal(value, datatype))
        }
    }

    fn named(&mut self, node: &NamedNodePattern) -> Result<NamedNodePattern> {
        Ok(match node {
            NamedNodePattern::NamedNode(node) => NamedNodePattern::NamedNode(self.iri(node)?),
            NamedNodePattern::Variable(var) => NamedNodePattern::Variable(self.variable(var)?),
        })
    }

    fn triple(&mut self, source: &TriplePattern) -> Result<TriplePattern> {
        Ok(TriplePattern {
            subject: self.term(&source.subject)?,
            predicate: self.named(&source.predicate)?,
            object: self.term(&source.object)?,
        })
    }

    fn term(&mut self, source: &TermPattern) -> Result<TermPattern> {
        use TermPattern as T;
        Ok(match source {
            T::NamedNode(node) => T::NamedNode(self.iri(node)?),
            T::BlankNode(node) => T::BlankNode(BlankNode::new_unchecked(self.text(node.as_str())?)),
            T::Literal(literal) => T::Literal(self.literal(literal)?),
            T::Variable(var) => T::Variable(self.variable(var)?),
            T::Triple(triple) => T::Triple(self.boxed(triple.as_ref(), Self::triple)?),
        })
    }

    fn ground(&mut self, source: &GroundTerm) -> Result<GroundTerm> {
        use GroundTerm as T;
        Ok(match source {
            T::NamedNode(node) => T::NamedNode(self.iri(node)?),
            T::Literal(literal) => T::Literal(self.literal(literal)?),
            T::Triple(triple) => T::Triple(self.boxed(triple.as_ref(), |copy, triple| {
                Ok(GroundTriple {
                    subject: copy.iri(&triple.subject)?,
                    predicate: copy.iri(&triple.predicate)?,
                    object: copy.ground(&triple.object)?,
                })
            })?),
        })
    }

    fn path(&mut self, source: &PropertyPathExpression) -> Result<PropertyPathExpression> {
        use PropertyPathExpression as P;
        macro_rules! unary {
            ($($arm:ident),+ $(,)?) => {
                if let $(
                    P::$arm(path) )|+ = source
                {
                    let copy = self.boxed(path.as_ref(), Self::path)?;
                    return Ok(match source {
                        $(P::$arm(_) => P::$arm(copy),)+
                        _ => unreachable!(),
                    });
                }
            };
        }
        unary!(
            Reverse, ZeroOrMore, OneOrMore, ZeroOrOne, Distinct, Multi, Shortest
        );
        Ok(match source {
            P::NamedNode(node) => P::NamedNode(self.iri(node)?),
            P::Sequence(a, b) => P::Sequence(
                self.boxed(a.as_ref(), Self::path)?,
                self.boxed(b.as_ref(), Self::path)?,
            ),
            P::Alternative(a, b) => P::Alternative(
                self.boxed(a.as_ref(), Self::path)?,
                self.boxed(b.as_ref(), Self::path)?,
            ),
            P::NegatedPropertySet(nodes) => P::NegatedPropertySet(self.list(nodes, Self::iri)?),
            P::Range { path, min, max } => P::Range {
                path: self.boxed(path.as_ref(), Self::path)?,
                min: *min,
                max: *max,
            },
            P::Reverse(_)
            | P::ZeroOrMore(_)
            | P::OneOrMore(_)
            | P::ZeroOrOne(_)
            | P::Distinct(_)
            | P::Multi(_)
            | P::Shortest(_) => unreachable!(),
        })
    }

    fn expr(&mut self, source: &Expression) -> Result<Expression> {
        use Expression as E;

        let pair = |copy: &mut Self, a: &Expression, b: &Expression| {
            Ok::<_, crate::error::Error>((copy.boxed(a, Self::expr)?, copy.boxed(b, Self::expr)?))
        };
        macro_rules! binary {
            ($arm:ident, $a:expr, $b:expr) => {{
                let (a, b) = pair(self, $a, $b)?;
                E::$arm(a, b)
            }};
        }
        Ok(match source {
            E::NamedNode(node) => E::NamedNode(self.iri(node)?),
            E::Literal(literal) => E::Literal(self.literal(literal)?),
            E::Variable(var) => E::Variable(self.variable(var)?),
            E::Bound(var) => E::Bound(self.variable(var)?),
            E::Or(a, b) => binary!(Or, a, b),
            E::And(a, b) => binary!(And, a, b),
            E::Equal(a, b) => binary!(Equal, a, b),
            E::SameTerm(a, b) => binary!(SameTerm, a, b),
            E::Greater(a, b) => binary!(Greater, a, b),
            E::GreaterOrEqual(a, b) => binary!(GreaterOrEqual, a, b),
            E::Less(a, b) => binary!(Less, a, b),
            E::LessOrEqual(a, b) => binary!(LessOrEqual, a, b),
            E::Add(a, b) => binary!(Add, a, b),
            E::Subtract(a, b) => binary!(Subtract, a, b),
            E::Multiply(a, b) => binary!(Multiply, a, b),
            E::Divide(a, b) => binary!(Divide, a, b),
            E::UnaryPlus(a) => E::UnaryPlus(self.boxed(a.as_ref(), Self::expr)?),
            E::UnaryMinus(a) => E::UnaryMinus(self.boxed(a.as_ref(), Self::expr)?),
            E::Not(a) => E::Not(self.boxed(a.as_ref(), Self::expr)?),
            E::In(a, list) => E::In(
                self.boxed(a.as_ref(), Self::expr)?,
                self.list(list, Self::expr)?,
            ),
            E::Exists(pattern) => E::Exists(self.boxed(pattern.as_ref(), Self::graph)?),
            E::If(a, b, c) => E::If(
                self.boxed(a.as_ref(), Self::expr)?,
                self.boxed(b.as_ref(), Self::expr)?,
                self.boxed(c.as_ref(), Self::expr)?,
            ),
            E::Coalesce(list) => E::Coalesce(self.list(list, Self::expr)?),
            E::FunctionCall(function, list) => {
                let function = match function {
                    Function::Custom(node) => Function::Custom(self.iri(node)?),
                    _ => function.clone(),
                };
                E::FunctionCall(function, self.list(list, Self::expr)?)
            }
        })
    }

    fn order(&mut self, source: &OrderExpression) -> Result<OrderExpression> {
        Ok(match source {
            OrderExpression::Asc(expr) => OrderExpression::Asc(self.expr(expr)?),
            OrderExpression::Desc(expr) => OrderExpression::Desc(self.expr(expr)?),
        })
    }

    fn aggregate(&mut self, source: &AggregateExpression) -> Result<AggregateExpression> {
        use AggregateExpression as A;
        Ok(match source {
            A::CountSolutions { distinct } => A::CountSolutions {
                distinct: *distinct,
            },
            A::FunctionCall {
                name,
                expr,
                distinct,
            } => {
                let name = match name {
                    AggregateFunction::Custom(node) => AggregateFunction::Custom(self.iri(node)?),
                    AggregateFunction::GroupConcat { separator } => {
                        AggregateFunction::GroupConcat {
                            separator: separator
                                .as_ref()
                                .map(|text| self.text(text))
                                .transpose()?,
                        }
                    }
                    _ => name.clone(),
                };
                A::FunctionCall {
                    name,
                    expr: self.expr(expr)?,
                    distinct: *distinct,
                }
            }
            A::Fold {
                expr,
                value,
                distinct,
                order,
            } => A::Fold {
                expr: self.expr(expr)?,
                value: value.as_ref().map(|value| self.expr(value)).transpose()?,
                distinct: *distinct,
                order: self.list(order, Self::order)?,
            },
        })
    }

    fn graph(&mut self, source: &GraphPattern) -> Result<GraphPattern> {
        use GraphPattern as G;
        macro_rules! binary {
            ($arm:ident, $left:expr, $right:expr) => {
                G::$arm {
                    left: self.boxed($left.as_ref(), Self::graph)?,
                    right: self.boxed($right.as_ref(), Self::graph)?,
                }
            };
        }
        Ok(match source {
            G::Bgp { patterns } => G::Bgp {
                patterns: self.list(patterns, Self::triple)?,
            },
            G::Path {
                subject,
                path,
                object,
            } => G::Path {
                subject: self.term(subject)?,
                path: self.path(path)?,
                object: self.term(object)?,
            },
            G::Join { left, right } => binary!(Join, left, right),
            G::Lateral { left, right } => binary!(Lateral, left, right),
            G::Union { left, right } => binary!(Union, left, right),
            G::Minus { left, right } => binary!(Minus, left, right),
            G::SemiJoin { left, right } => binary!(SemiJoin, left, right),
            G::AntiJoin { left, right } => binary!(AntiJoin, left, right),
            G::LeftJoin {
                left,
                right,
                expression,
            } => G::LeftJoin {
                left: self.boxed(left.as_ref(), Self::graph)?,
                right: self.boxed(right.as_ref(), Self::graph)?,
                expression: expression
                    .as_ref()
                    .map(|expr| self.expr(expr))
                    .transpose()?,
            },
            G::Filter { expr, inner } => G::Filter {
                expr: self.expr(expr)?,
                inner: self.boxed(inner.as_ref(), Self::graph)?,
            },
            G::Graph { name, inner } => G::Graph {
                name: self.named(name)?,
                inner: self.boxed(inner.as_ref(), Self::graph)?,
            },
            G::Extend {
                inner,
                variable,
                expression,
            } => G::Extend {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                variable: self.variable(variable)?,
                expression: self.expr(expression)?,
            },
            G::Assign {
                inner,
                variable,
                expression,
            } => G::Assign {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                variable: self.variable(variable)?,
                expression: self.expr(expression)?,
            },
            G::Unfold {
                inner,
                expression,
                variable,
                second,
            } => G::Unfold {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                expression: self.expr(expression)?,
                variable: self.variable(variable)?,
                second: second.as_ref().map(|var| self.variable(var)).transpose()?,
            },
            G::Values {
                variables,
                bindings,
            } => G::Values {
                variables: self.list(variables, Self::variable)?,
                bindings: self.list(bindings, |copy, row| {
                    copy.list(row, |copy, term| {
                        term.as_ref().map(|term| copy.ground(term)).transpose()
                    })
                })?,
            },
            G::OrderBy { inner, expression } => G::OrderBy {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                expression: self.list(expression, Self::order)?,
            },
            G::Project { inner, variables } => G::Project {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                variables: self.list(variables, Self::variable)?,
            },
            G::Distinct { inner } => G::Distinct {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
            },
            G::Reduced { inner } => G::Reduced {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
            },
            G::Slice {
                inner,
                start,
                length,
            } => G::Slice {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                start: *start,
                length: *length,
            },
            G::Group {
                inner,
                variables,
                aggregates,
            } => G::Group {
                inner: self.boxed(inner.as_ref(), Self::graph)?,
                variables: self.list(variables, Self::variable)?,
                aggregates: self.list(aggregates, |copy, (var, aggregate)| {
                    Ok((copy.variable(var)?, copy.aggregate(aggregate)?))
                })?,
            },
            G::Service {
                name,
                inner,
                silent,
            } => {
                self.calls_service = true;
                G::Service {
                    name: self.named(name)?,
                    inner: self.boxed(inner.as_ref(), Self::graph)?,
                    silent: *silent,
                }
            }
        })
    }
}
