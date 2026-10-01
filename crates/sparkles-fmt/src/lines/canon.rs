//! `canonicalize`: the sorted canonical N-Quads (or N-Triples) of RDFC-1.0, blank nodes
//! relabeled `_:c14n0`, `_:c14n1`, … by oxrdf's `Rdfc10 { Sha256 }`. The whole dataset
//! is in memory, so the number of quads is capped (`LinesConfig::max_canonicalize_quads`).
//!
//! The check: the output, parsed and canonicalized again, gives the output's bytes.
//! RDFC-1.0 does not define triple terms; oxrdf labels the blank nodes inside them in a
//! way of its own, so such output carries an `unstable-labels` warning.

use super::canonical::write_quad;
use crate::{Check, FormatError, Language};
use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};
use oxrdf::{Dataset, Quad, Term};

const RDFC10: CanonicalizationAlgorithm = CanonicalizationAlgorithm::Rdfc10 {
    hash_algorithm: CanonicalizationHashAlgorithm::Sha256,
};

/// The canonical lines of `quads` (duplicates merged), sorted by their bytes.
fn canonical_lines(quads: impl IntoIterator<Item = Quad>) -> Vec<String> {
    let mut d: Dataset = quads.into_iter().collect();
    d.canonicalize(RDFC10);
    let mut lines: Vec<String> = d
        .iter()
        .map(|q| {
            let mut s = String::new();
            write_quad(&mut s, &q.into_owned());
            s
        })
        .collect();
    lines.sort_unstable();
    lines
}

/// The canonical form of `quads`, checked, and whether it relabeled blank nodes inside
/// triple terms.
pub fn canonicalize(quads: Vec<Quad>, lang: Language) -> Result<(Vec<String>, bool), FormatError> {
    let unstable = quads.iter().any(|q| in_triple_term(&q.object));
    let lines = canonical_lines(quads);
    // canonicalizing the output again gives the output
    let mut text = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for l in &lines {
        text.push_str(l);
        text.push('\n');
    }
    let again = crate::check::graph::parse(&text, lang).map_err(|_| FormatError::Unsafe {
        check: Check::Graph,
    })?;
    drop(text);
    if canonical_lines(again) != lines {
        return Err(FormatError::Unsafe {
            check: Check::Idempotence,
        });
    }
    Ok((lines, unstable))
}

/// Whether `t` is a triple term with a blank node somewhere inside.
fn in_triple_term(t: &Term) -> bool {
    fn has_blank(t: &Term) -> bool {
        match t {
            Term::BlankNode(_) => true,
            Term::Triple(t) => t.subject.is_blank_node() || has_blank(&t.object),
            _ => false,
        }
    }
    matches!(t, Term::Triple(_)) && has_blank(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quads(text: &str) -> Vec<Quad> {
        crate::check::graph::parse(text, Language::NQuads).unwrap()
    }

    #[test]
    fn isomorphic_inputs_give_the_same_bytes() {
        let a = quads(
            "_:x <http://e/p> _:y <http://e/g> .\n_:y <http://e/q> \"1\" .\n_:x <http://e/p> _:y <http://e/g> .\n",
        );
        let b = quads("_:k <http://e/q> \"1\" .\n_:j <http://e/p> _:k <http://e/g> .\n");
        let (la, ua) = canonicalize(a, Language::NQuads).unwrap();
        let (lb, _) = canonicalize(b, Language::NQuads).unwrap();
        assert_eq!(la, lb);
        assert!(!ua);
        assert_eq!(la.len(), 2);
        assert!(la.iter().all(|l| l.contains("_:c14n")));
        let (_, unstable) = canonicalize(
            quads("<http://s> <http://p> <<( _:a <http://p> <http://o> )>> .\n"),
            Language::NQuads,
        )
        .unwrap();
        assert!(unstable);
    }
}
