//! Structural SPARQL algebra traversal used before evaluation.

use crate::access::{Preparation, PreparationBuffer};
use crate::error::{Error, Result};
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};

/// Visit every graph pattern, including patterns inside scalar and aggregate
/// expressions. Returning an error ends the traversal.
pub fn visit_graph_patterns<E>(
    root: &GraphPattern,
    mut visit: impl FnMut(&GraphPattern) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    walk(
        Algebra::Pattern(root),
        None,
        |item| {
            if let Algebra::Pattern(pattern) = item {
                visit(pattern)?;
            }
            Ok(())
        },
        |_| unreachable!("ordinary walk"),
    )
}

/// Visit the source occurrences that can supply the immutable syntax of EXISTS.
pub(super) fn visit_exists<'a>(
    root: &'a GraphPattern,
    preparation: &Preparation,
    mut exists: impl FnMut(&'a GraphPattern) -> Result<()>,
) -> Result<()> {
    visit_algebra(Algebra::Pattern(root), preparation, |item| {
        if let Algebra::Expr(Expression::Exists(pattern)) = item {
            exists(pattern)?;
        }
        Ok(())
    })
}

#[derive(Clone, Copy)]
pub(super) enum Algebra<'a> {
    Pattern(&'a GraphPattern),
    Expr(&'a Expression),
    Aggregate(&'a AggregateExpression),
}

pub(super) fn visit_algebra<'a>(
    root: Algebra<'a>,
    preparation: &Preparation,
    mut visit: impl FnMut(Algebra<'a>) -> Result<()>,
) -> Result<()> {
    walk(
        root,
        Some(preparation),
        |item| {
            preparation.check()?;
            visit(item)
        },
        |e| e,
    )
}

fn walk<'a, E>(
    root: Algebra<'a>,
    preparation: Option<&Preparation>,
    mut visit: impl FnMut(Algebra<'a>) -> std::result::Result<(), E>,
    error: impl Fn(Error) -> E,
) -> std::result::Result<(), E> {
    use Expression as Expr;
    use GraphPattern as GP;
    let mut plain = Vec::new();
    let mut admitted = preparation
        .map(PreparationBuffer::new)
        .transpose()
        .map_err(&error)?;
    let push = |work, plain: &mut Vec<_>, admitted: &mut Option<PreparationBuffer<_>>| {
        if let Some(stack) = admitted {
            stack.push(work).map_err(&error)
        } else {
            plain.push(work);
            Ok(())
        }
    };
    macro_rules! push {
        ($($item:expr),+ $(,)?) => {
            $(push($item, &mut plain, &mut admitted)?;)+
        };
    }
    push!(root);
    while let Some(next) = match &mut admitted {
        Some(stack) => stack.pop(),
        None => plain.pop(),
    } {
        visit(next)?;
        match next {
            Algebra::Pattern(gp) => match gp {
                GP::Extend {
                    inner, expression, ..
                }
                | GP::Assign {
                    inner, expression, ..
                }
                | GP::Unfold {
                    inner, expression, ..
                } => {
                    push!(Algebra::Pattern(inner), Algebra::Expr(expression));
                }
                GP::Join { left, right }
                | GP::Lateral { left, right }
                | GP::Union { left, right }
                | GP::Minus { left, right }
                | GP::SemiJoin { left, right }
                | GP::AntiJoin { left, right } => {
                    push!(Algebra::Pattern(left), Algebra::Pattern(right));
                }
                GP::LeftJoin {
                    left,
                    right,
                    expression,
                } => {
                    push!(Algebra::Pattern(left), Algebra::Pattern(right));
                    if let Some(expr) = expression {
                        push!(Algebra::Expr(expr));
                    }
                }
                GP::Filter { inner, expr } => {
                    push!(Algebra::Pattern(inner), Algebra::Expr(expr));
                }
                GP::OrderBy { inner, expression } => {
                    push!(Algebra::Pattern(inner));
                    for order in expression {
                        let expr = match order {
                            OrderExpression::Asc(expr) | OrderExpression::Desc(expr) => expr,
                        };
                        push!(Algebra::Expr(expr));
                    }
                }
                GP::Group {
                    inner, aggregates, ..
                } => {
                    push!(Algebra::Pattern(inner));
                    for (_, aggregate) in aggregates {
                        push!(Algebra::Aggregate(aggregate));
                    }
                }
                GP::Graph { inner, .. }
                | GP::Service { inner, .. }
                | GP::Project { inner, .. }
                | GP::Distinct { inner }
                | GP::Reduced { inner }
                | GP::Slice { inner, .. } => {
                    push!(Algebra::Pattern(inner));
                }
                GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => {}
            },
            Algebra::Expr(expr) => match expr {
                Expr::NamedNode(_) | Expr::Literal(_) | Expr::Variable(_) | Expr::Bound(_) => {}
                Expr::Or(a, b)
                | Expr::And(a, b)
                | Expr::Equal(a, b)
                | Expr::SameTerm(a, b)
                | Expr::Greater(a, b)
                | Expr::GreaterOrEqual(a, b)
                | Expr::Less(a, b)
                | Expr::LessOrEqual(a, b)
                | Expr::Add(a, b)
                | Expr::Subtract(a, b)
                | Expr::Multiply(a, b)
                | Expr::Divide(a, b) => {
                    push!(Algebra::Expr(a), Algebra::Expr(b));
                }
                Expr::UnaryPlus(a) | Expr::UnaryMinus(a) | Expr::Not(a) => {
                    push!(Algebra::Expr(a));
                }
                Expr::In(a, list) => {
                    push!(Algebra::Expr(a));
                    for expr in list {
                        push!(Algebra::Expr(expr));
                    }
                }
                Expr::If(a, b, c) => {
                    push!(Algebra::Expr(a), Algebra::Expr(b), Algebra::Expr(c));
                }
                Expr::Coalesce(list) | Expr::FunctionCall(_, list) => {
                    for expr in list {
                        push!(Algebra::Expr(expr));
                    }
                }
                Expr::Exists(pattern) => {
                    push!(Algebra::Pattern(pattern));
                }
            },
            Algebra::Aggregate(aggregate) => match aggregate {
                AggregateExpression::CountSolutions { .. } => {}
                AggregateExpression::FunctionCall { expr, .. } => {
                    push!(Algebra::Expr(expr));
                }
                AggregateExpression::Fold {
                    expr, value, order, ..
                } => {
                    push!(Algebra::Expr(expr));
                    if let Some(value) = value {
                        push!(Algebra::Expr(value));
                    }
                    for order in order {
                        let expr = match order {
                            OrderExpression::Asc(expr) | OrderExpression::Desc(expr) => expr,
                        };
                        push!(Algebra::Expr(expr));
                    }
                }
            },
        }
    }
    Ok(())
}
