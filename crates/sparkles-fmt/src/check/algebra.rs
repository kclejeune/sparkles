//! Comparing two SPARQL parses. spargebra makes up blank nodes (`[]`, paths, reifiers)
//! and variables (aggregates) with random names, so the comparison renames both by first
//! occurrence before comparing; everything else (base IRI, dataset, order) must be equal.

use super::Algebra;
use std::collections::HashSet;

/// Whether two parses denote the same algebra. `vars` are the variable names the input
/// spells; any other variable was made up by the parser.
pub fn equivalent(a: &Algebra, b: &Algebra, vars: &HashSet<String>) -> bool {
    canonical(a, vars) == canonical(b, vars)
}

/// A text that two equivalent parses share: the SSE and the SPARQL rendering.
///
/// TODO: rename blank nodes (`_:b0`, `_:b1` …) and made-up variables (`?_h0` …) by first
/// occurrence, outside string literals. Until then, parses holding either compare as
/// different, which refuses the output rather than accepting a wrong one.
pub fn canonical(a: &Algebra, vars: &HashSet<String>) -> String {
    let _ = vars;
    match a {
        Algebra::Query(q) => format!("{}\n{q}", q.to_sse()),
        Algebra::Update(u) => format!("{}\n{u}", u.to_sse()),
    }
}
