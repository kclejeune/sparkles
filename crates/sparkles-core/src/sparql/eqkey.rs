//! Candidate keys of joins on a FILTER or OPTIONAL equality `?a = ?b` between two inputs
//! that share no variable.
//!
//! `=` compares values ([`super::value::equals`]), so two different ids can be equal, and
//! a join on `=` cannot simply compare ids. Most values still equal only one term, and the
//! store maps each term to one id ([`crate::id`]): an IRI, a blank node, a simple literal
//! (`xsd:string`) and a literal of a datatype the engine does not interpret, which equals
//! another literal only when both lexical form and datatype are the same. These values
//! are keyed by their id. A language-tagged string equals another with the same lexical
//! form whose tag differs only in case, so it is keyed by a hash of its lexical form and
//! its tag in lower case. The values of the interpreted XSD and composite datatypes, and
//! triple terms, can equal terms with other ids (`1` and `1.0`, two `dateTime`s in
//! different time zones), so they are loose: a loose value is a candidate for every
//! other loose value.
//!
//! A key only proposes candidates. Equal values always get the same key, and loose values
//! meet every loose value, so the candidates hold every pair that `=` accepts. The join
//! still tests its whole condition on each candidate pair, so a hash that happens to
//! equal another key adds a candidate and never changes the result.

use super::ctx::{Ctx, TermKind};
use crate::error::Result;
use crate::id::{Id, KEY_SEP, Tag};

/// How a value meets the values of the other input under `=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EqKey {
    /// An unbound value, for which `=` raises an error, so it meets no value.
    Unbound,
    /// The value meets only the values with the same key.
    Key(Id),
    /// The value meets every other loose value.
    Loose,
}

const XSD: &[u8] = b"http://www.w3.org/2001/XMLSchema#";

/// The key of `value`. IRIs, blank nodes and inline literals are classified from the id
/// alone. Other literals read their vocabulary key.
pub(super) fn eq_key(ctx: &Ctx, value: Id) -> Result<EqKey> {
    if value.is_undef() {
        return Ok(EqKey::Unbound);
    }
    if value.is_inline() {
        return Ok(EqKey::Loose);
    }
    match ctx.kind(value) {
        TermKind::Iri | TermKind::BNode => return Ok(EqKey::Key(value)),
        TermKind::Literal => {}
        // a triple term equals another whose literals have equal values, and an id
        // whose term is unknown is left to the condition
        TermKind::Triple | TermKind::None => return Ok(EqKey::Loose),
    }
    let classify = |key: &[u8]| literal_key(value, key);
    let key = match value.tag() {
        Tag::Vocab => ctx.with_cursor_vocab_key(value.payload(), classify)?,
        Tag::Delta => ctx.snap.generation.dvocab.with(|v| {
            (value.payload() < ctx.snap.dvocab_len)
                .then(|| v.get(value.payload()).map(classify))
                .flatten()
        }),
        Tag::Local => ctx.with_local_key(value.payload(), classify),
        _ => None,
    };
    Ok(key.unwrap_or(EqKey::Loose))
}

/// The key of the literal `id` whose vocabulary key is `key`: `"` lexical form
/// [`KEY_SEP`], then nothing for a simple literal, `@` and the tag for a language-tagged
/// string, or `^` and the datatype IRI.
fn literal_key(id: Id, key: &[u8]) -> EqKey {
    if key.first() != Some(&b'"') {
        return EqKey::Loose;
    }
    let Some(sep) = key.iter().rposition(|&b| b == KEY_SEP) else {
        return EqKey::Loose;
    };
    let suffix = &key[sep + 1..];
    match suffix.first() {
        None => EqKey::Key(id),
        Some(b'@') => {
            use std::hash::Hasher;
            let mut h = rustc_hash::FxHasher::default();
            h.write(&key[..sep]);
            h.write_u8(KEY_SEP);
            for &b in suffix {
                h.write_u8(b.to_ascii_lowercase());
            }
            // a hash that matches a stored or local id only adds candidates
            EqKey::Key(Id::new(Tag::Local, h.finish()))
        }
        Some(_) => {
            let dt = &suffix[1..];
            if dt.starts_with(XSD) || std::str::from_utf8(dt).is_ok_and(super::cdt::is_cdt) {
                EqKey::Loose
            } else {
                EqKey::Key(id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(lex: &str, suffix: &str) -> Vec<u8> {
        let mut k = vec![b'"'];
        k.extend_from_slice(lex.as_bytes());
        k.push(KEY_SEP);
        k.extend_from_slice(suffix.as_bytes());
        k
    }

    #[test]
    fn literal_keys() {
        let id = Id::vocab(7);
        assert_eq!(literal_key(id, &key("a", "")), EqKey::Key(id));
        assert_eq!(
            literal_key(id, &key("1", "^http://www.w3.org/2001/XMLSchema#int")),
            EqKey::Loose
        );
        assert_eq!(
            literal_key(id, &key("x", "^http://example.org/dt")),
            EqKey::Key(id)
        );
        assert_eq!(
            literal_key(
                id,
                &key("[1]", "^http://w3id.org/awslabs/neptune/SPARQL-CDTs/List")
            ),
            EqKey::Loose
        );
        // tags that differ in case get the same key, whatever the id
        let lower = literal_key(Id::vocab(1), &key("chat", "@en-gb"));
        let upper = literal_key(Id::vocab(2), &key("chat", "@EN-GB"));
        assert_eq!(lower, upper);
        assert_ne!(lower, literal_key(Id::vocab(1), &key("chat", "@fr")));
        assert_ne!(lower, literal_key(Id::vocab(1), &key("chats", "@en-gb")));
    }
}
