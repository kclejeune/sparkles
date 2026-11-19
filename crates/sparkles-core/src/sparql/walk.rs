//! Structural SPARQL algebra traversal used before evaluation.

use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};

/// Visit every graph pattern, including patterns inside scalar and aggregate
/// expressions. Returning an error ends the traversal.
pub fn visit_graph_patterns<E>(
    root: &GraphPattern,
    mut visit: impl FnMut(&GraphPattern) -> Result<(), E>,
) -> Result<(), E> {
    enum Work<'a> {
        Pattern(&'a GraphPattern),
        Expr(&'a Expression),
        Aggregate(&'a AggregateExpression),
    }
    use Expression as Expr;
    use GraphPattern as GP;
    let mut work = vec![Work::Pattern(root)];
    while let Some(next) = work.pop() {
        match next {
            Work::Pattern(gp) => {
                visit(gp)?;
                match gp {
                    GP::Extend {
                        inner, expression, ..
                    }
                    | GP::Assign {
                        inner, expression, ..
                    }
                    | GP::Unfold {
                        inner, expression, ..
                    } => {
                        work.extend([Work::Pattern(inner), Work::Expr(expression)]);
                    }
                    GP::Join { left, right }
                    | GP::Lateral { left, right }
                    | GP::Union { left, right }
                    | GP::Minus { left, right }
                    | GP::SemiJoin { left, right }
                    | GP::AntiJoin { left, right } => {
                        work.extend([Work::Pattern(left), Work::Pattern(right)]);
                    }
                    GP::LeftJoin {
                        left,
                        right,
                        expression,
                    } => {
                        work.extend([Work::Pattern(left), Work::Pattern(right)]);
                        if let Some(expr) = expression {
                            work.push(Work::Expr(expr));
                        }
                    }
                    GP::Filter { inner, expr } => {
                        work.extend([Work::Pattern(inner), Work::Expr(expr)]);
                    }
                    GP::OrderBy { inner, expression } => {
                        work.push(Work::Pattern(inner));
                        for order in expression {
                            let expr = match order {
                                OrderExpression::Asc(expr) | OrderExpression::Desc(expr) => expr,
                            };
                            work.push(Work::Expr(expr));
                        }
                    }
                    GP::Group {
                        inner, aggregates, ..
                    } => {
                        work.push(Work::Pattern(inner));
                        for (_, aggregate) in aggregates {
                            work.push(Work::Aggregate(aggregate));
                        }
                    }
                    GP::Graph { inner, .. }
                    | GP::Service { inner, .. }
                    | GP::Project { inner, .. }
                    | GP::Distinct { inner }
                    | GP::Reduced { inner }
                    | GP::Slice { inner, .. } => work.push(Work::Pattern(inner)),
                    GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => {}
                }
            }
            Work::Expr(expr) => match expr {
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
                    work.extend([Work::Expr(a), Work::Expr(b)]);
                }
                Expr::UnaryPlus(a) | Expr::UnaryMinus(a) | Expr::Not(a) => work.push(Work::Expr(a)),
                Expr::In(a, list) => {
                    work.push(Work::Expr(a));
                    work.extend(list.iter().map(Work::Expr));
                }
                Expr::If(a, b, c) => work.extend([Work::Expr(a), Work::Expr(b), Work::Expr(c)]),
                Expr::Coalesce(list) | Expr::FunctionCall(_, list) => {
                    work.extend(list.iter().map(Work::Expr))
                }
                Expr::Exists(pattern) => work.push(Work::Pattern(pattern)),
            },
            Work::Aggregate(aggregate) => match aggregate {
                AggregateExpression::CountSolutions { .. } => {}
                AggregateExpression::FunctionCall { expr, .. } => work.push(Work::Expr(expr)),
                AggregateExpression::Fold {
                    expr, value, order, ..
                } => {
                    work.push(Work::Expr(expr));
                    if let Some(value) = value {
                        work.push(Work::Expr(value));
                    }
                    for order in order {
                        let expr = match order {
                            OrderExpression::Asc(expr) | OrderExpression::Desc(expr) => expr,
                        };
                        work.push(Work::Expr(expr));
                    }
                }
            },
        }
    }
    Ok(())
}
