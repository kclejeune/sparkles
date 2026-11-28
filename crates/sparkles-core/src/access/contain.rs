//! Containment of grant sets (spec F13 §6.1): whether one view of a dataset reads every
//! quad that another reads, decided from the rules alone and never from data.
//!
//! Every test here is sound and may be incomplete. A rule that the comparison cannot
//! prove to admit a graph or a triple counts as one that does not, so a caller is
//! refused whenever its grants might read less than the view's principal.

use super::{GraphAccess, GraphRule, Graphs, TripleRules, glob};

/// Why one view's grants do not provably contain another's. A gap names a category
/// only, never a graph or a protection, so that refusals and audit records disclose
/// nothing about the grants they compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gap {
    /// the other view may read a graph that this one does not
    Graphs,
    /// a protection may hide from this view a triple that the other view reads
    Protections,
    /// a protection's pattern binds the caller, and the two callers differ
    CallerPattern,
}

impl Gap {
    /// The category as audit records and refusals name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Gap::Graphs => "graphs",
            Gap::Protections => "protections",
            Gap::CallerPattern => "caller-pattern",
        }
    }
}

/// Whether `c` reads every quad that `v` reads. Both are settled views.
pub(super) fn read_contains(c: &GraphAccess, v: &GraphAccess) -> Result<(), Gap> {
    if !graphs_contain(&c.read, &v.read) {
        return Err(Gap::Graphs);
    }
    triples_contain(c.triples.as_deref(), v.triples.as_deref())
}

/// Whether every graph in `v` is in `c`.
pub(super) fn graphs_contain(c: &Graphs, v: &Graphs) -> bool {
    match (c, v) {
        (Graphs::All, _) => true,
        (Graphs::Only(_), Graphs::All) => false,
        (Graphs::Only(c), Graphs::Only(v)) => rule_contains(c, v),
    }
}

fn rule_contains(c: &GraphRule, v: &GraphRule) -> bool {
    if (v.default_graph && !c.default_graph) || (v.blank_nodes && !c.blank_nodes) {
        return false;
    }
    // the exact graphs of `v`, and the exact graphs that the patterns of `c` never
    // match, are single IRIs that both rules can be asked about directly
    if v.iris.iter().any(|x| v.matches_iri(x) && !c.matches_iri(x))
        || c.protected
            .iter()
            .any(|x| v.matches_iri(x) && !c.matches_iri(x))
    {
        return false;
    }
    v.patterns.iter().all(|p| pattern_contained(c, v, p))
}

/// Whether every named graph that pattern `p` of `v` admits is in `c`.
fn pattern_contained(c: &GraphRule, v: &GraphRule, p: &str) -> bool {
    if !p.contains('*') {
        return !v.matches_iri(p) || c.matches_iri(p);
    }
    // some pattern of `c` admits every IRI that `p` matches
    c.patterns.iter().any(|q| glob_includes(q, p))
        // and nothing that `c` takes out of its patterns is left in `v`
        && c.excluded.iter().all(|e| {
            disjoint(p, e) || v.excluded.iter().any(|x| glob_includes(x, e))
        })
        && c.guards.iter().all(|g| {
            disjoint(p, &g.pattern)
                || v.excluded.iter().any(|x| glob_includes(x, &g.pattern))
                || v.guards.iter().any(|h| {
                    glob_includes(&h.pattern, &g.pattern)
                        && (h.decide.is_some()
                            || h.exempt
                                .iter()
                                .all(|x| !glob(&g.pattern, x) || g.passes(x)))
                })
        })
}

/// The literal text of a pattern before its first `*` and after its last.
fn ends(p: &str) -> (&str, &str) {
    match (p.find('*'), p.rfind('*')) {
        (Some(a), Some(b)) => (&p[..a], &p[b + 1..]),
        _ => (p, p),
    }
}

/// Whether every IRI that `p` matches, `q` matches. Proven only for a `q` without
/// literal text between its first and last `*`, such as `prefix*` or `prefix*suffix`.
pub(super) fn glob_includes(q: &str, p: &str) -> bool {
    if q == p {
        return true;
    }
    if !p.contains('*') {
        return glob(q, p);
    }
    if !q.contains('*') {
        return false;
    }
    let (qa, qb) = ends(q);
    if q[qa.len()..q.len() - qb.len()].bytes().any(|b| b != b'*') {
        return false;
    }
    let (pa, pb) = ends(p);
    pa.starts_with(qa) && pb.ends_with(qb)
}

/// Whether no IRI matches both `p` and `e`. Proven when their literal starts or ends
/// cannot agree.
pub(super) fn disjoint(p: &str, e: &str) -> bool {
    if !p.contains('*') {
        return !glob(e, p);
    }
    if !e.contains('*') {
        return !glob(p, e);
    }
    let ((pa, pb), (ea, eb)) = (ends(p), ends(e));
    !(pa.starts_with(ea) || ea.starts_with(pa)) || !(pb.ends_with(eb) || eb.ends_with(pb))
}

/// Whether every triple that the protections of `v` leave visible, those of `c` leave
/// visible too, within the graphs both read. Each protection that `c` keeps in force
/// for reads must be in force for `v` as well, and `c` must lift it wherever `v` does.
/// Whether a protection hides a quad then depends on the protection and the quad alone,
/// except for a pattern that binds the caller, which needs the same caller.
fn triples_contain(c: Option<&TripleRules>, v: Option<&TripleRules>) -> Result<(), Gap> {
    let Some(c) = c else {
        return Ok(());
    };
    for rc in c.rules.iter().filter(|r| !r.read.is_all()) {
        let Some((v, rv)) = v.and_then(|v| {
            v.rules
                .iter()
                .find(|r| r.protection == rc.protection)
                .map(|r| (v, r))
        }) else {
            return Err(Gap::Protections);
        };
        if rv.read.is_all() || !graphs_contain(&rc.read, &rv.read) {
            return Err(Gap::Protections);
        }
        if rc.protection.pattern.is_some() && c.caller != v.caller {
            return Err(Gap::CallerPattern);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_inclusion_is_sound() {
        assert!(glob_includes("*", "http://ex/a/*"));
        assert!(glob_includes("http://ex/*", "http://ex/a/*"));
        assert!(glob_includes("http://ex/*", "http://ex/*/b"));
        assert!(glob_includes("http://ex/*/b", "http://ex/a/*/b"));
        assert!(glob_includes("http://ex/a", "http://ex/a"));
        assert!(glob_includes("http://ex/*", "http://ex/a"));
        assert!(!glob_includes("http://ex/a/*", "http://ex/*"));
        assert!(!glob_includes("http://ex/a", "http://ex/*"));
        // literal text between stars is not proven
        assert!(!glob_includes("http://*/x/*", "http://a/x/*"));
        // the length bound of `q` holds: "ab*ba" does not include "ab*" ("ab")
        assert!(!glob_includes("ab*ba", "ab*"));
        assert!(!glob_includes("http://ex/*b", "http://ex/*"));
    }

    #[test]
    fn disjointness_is_sound() {
        assert!(disjoint("http://ex/public/*", "http://ex/secret/*"));
        assert!(!disjoint("http://ex/*", "http://ex/secret/*"));
        assert!(disjoint("http://ex/*.ttl", "http://ex/*.nq"));
        assert!(!disjoint("*", "http://ex/secret/*"));
        assert!(disjoint("http://ex/a/*", "http://ex/b"));
        assert!(!disjoint("http://ex/a/*", "http://ex/a/b"));
        // every pair the fuzz below finds overlapping is reported as overlapping
        let words = ["", "a", "b", "ab", "ba", "aa", "abba", "bab"];
        let pats = [
            "*", "a*", "*a", "a*b", "ab*", "*ba", "b*", "a*a", "ab", "ba",
        ];
        for p in pats {
            for q in pats {
                let both = words.iter().any(|w| glob(p, w) && glob(q, w));
                if both {
                    assert!(!disjoint(p, q), "{p} {q}");
                }
                let missed = words.iter().any(|w| glob(p, w) && !glob(q, w));
                if missed {
                    assert!(!glob_includes(q, p), "{q} includes {p}");
                }
            }
        }
    }
}
