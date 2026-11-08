//! A single pre-admitted vocabulary-key buffer. Borrowing RDF terms avoids the
//! recursive temporary vectors and owned subject/predicate copies of term_key.
use crate::budget::{Budget, BudgetFailure, Held};
use oxrdf::{Term, TermRef};
use sparkles_budget::ParentRefused;
use sparkles_core::id::{self, Id};
use std::sync::Arc;

pub(super) struct WorkingKey {
    bytes: Vec<u8>,
    _held: Held,
}
impl WorkingKey {
    pub fn new(term: &Term, budget: &Arc<Budget>, strict: bool) -> Result<Self, BudgetFailure> {
        let len = encoded_len(term.as_ref())
            .ok_or_else(|| BudgetFailure::parent(ParentRefused::Overflow))?;
        let mut held = Held::new_exact_mode(budget, len, strict)?;
        let mut bytes = Vec::with_capacity(len);
        if bytes.capacity() > len {
            if strict {
                held.try_grow(bytes.capacity() - len)?;
            } else {
                held.grow(bytes.capacity() - len)?;
            }
        }
        write(term.as_ref(), &mut bytes);
        debug_assert_eq!(bytes.len(), len);
        Ok(Self { bytes, _held: held })
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    /// Publication accepts a scalar id only. The key either drops in the
    /// intern operation or is backed by Terms' own prepared permanent credit.
    pub fn intern(self, terms: &super::Terms, strict: bool) -> Result<u64, BudgetFailure> {
        let Self { bytes, _held } = self;
        let result = terms.local_keyed(bytes, super::LocalTerm::Key(Vec::new()), strict);
        drop(_held);
        result
    }
}

fn varint_bytes(mut n: usize) -> usize {
    let mut len = 1;
    while n >= 128 {
        len += 1;
        n >>= 7;
    }
    len
}
fn encoded_len(term: TermRef<'_>) -> Option<usize> {
    match term {
        TermRef::NamedNode(n) => n.as_str().len().checked_add(1),
        TermRef::BlankNode(_) => Some(9),
        TermRef::Literal(l) => {
            let mut n = l.value().len().checked_add(2)?;
            if let Some(lang) = l.language() {
                n = n.checked_add(lang.len())?.checked_add(1)?;
                if l.direction().is_some() {
                    n = n.checked_add(5)?;
                }
            } else if l.datatype() != oxrdf::vocab::xsd::STRING {
                n = n.checked_add(l.datatype().as_str().len())?.checked_add(1)?;
            }
            Some(n)
        }
        TermRef::Triple(t) => {
            let mut n = 1usize;
            for child in [
                t.subject.as_ref().into(),
                t.predicate.as_ref().into(),
                t.object.as_ref(),
            ] {
                let part = encoded_len(child)?;
                n = n.checked_add(varint_bytes(part))?.checked_add(part)?;
            }
            Some(n)
        }
    }
}
fn write(term: TermRef<'_>, out: &mut Vec<u8>) {
    match term {
        TermRef::NamedNode(n) => {
            out.push(b'<');
            out.extend_from_slice(n.as_str().as_bytes());
        }
        TermRef::BlankNode(b) => {
            // This is id::default_bnode_id's exact borrowed-label policy.
            let payload = id::parse_bnode_payload(b.as_str()).unwrap_or_else(|| {
                let hash = b
                    .as_str()
                    .bytes()
                    .fold(0xcbf2_9ce4_8422_2325u64, |h, byte| {
                        (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
                    });
                (hash & (id::PAYLOAD_MASK >> 1)) | Id::LOCAL_BNODE_BIT
            });
            out.push(b'_');
            out.extend_from_slice(&payload.to_be_bytes());
        }
        TermRef::Literal(l) => {
            out.push(b'"');
            out.extend_from_slice(l.value().as_bytes());
            out.push(id::KEY_SEP);
            if let Some(language) = l.language() {
                out.push(b'@');
                out.extend_from_slice(language.as_bytes());
                if let Some(direction) = l.direction() {
                    out.extend_from_slice(match direction {
                        oxrdf::BaseDirection::Ltr => b"--ltr",
                        oxrdf::BaseDirection::Rtl => b"--rtl",
                    });
                }
            } else if l.datatype() != oxrdf::vocab::xsd::STRING {
                out.push(b'^');
                out.extend_from_slice(l.datatype().as_str().as_bytes());
            }
        }
        TermRef::Triple(t) => {
            out.push(b'(');
            for child in [
                t.subject.as_ref().into(),
                t.predicate.as_ref().into(),
                t.object.as_ref(),
            ] {
                let mut len =
                    encoded_len(child).expect("immutable term length was checked before admission");
                while len >= 128 {
                    out.push((len as u8) | 0x80);
                    len >>= 7;
                }
                out.push(len as u8);
                write(child, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{BlankNode, Literal, NamedNode, Triple};

    #[test]
    fn exact_borrowed_keys_match_the_existing_format_in_every_term_position() {
        let budget = Budget::unlimited();
        let mut terms: Vec<Term> = vec![
            NamedNode::new_unchecked("urn:é").into(),
            BlankNode::new_unchecked("b1f").into(),
            BlankNode::new_unchecked("q1f").into(),
            BlankNode::new_unchecked("application-label").into(),
            Literal::new_simple_literal("lexical-é").into(),
            Literal::new_language_tagged_literal_unchecked("hello", "en").into(),
            Literal::new_typed_literal("123", oxrdf::vocab::xsd::INTEGER).into(),
            Literal::new_directional_language_tagged_literal_unchecked(
                "hello",
                "en",
                oxrdf::BaseDirection::Rtl,
            )
            .into(),
            Literal::new_simple_literal("x".repeat(200)).into(),
            Literal::new_simple_literal("x".repeat(20_000)).into(),
        ];
        for term in terms.clone() {
            terms.push(Term::Triple(Box::new(Triple::new(
                BlankNode::new_unchecked("application-label"),
                NamedNode::new_unchecked("urn:p"),
                term,
            ))));
        }
        let mut nested = terms.last().unwrap().clone();
        for _ in 0..8 {
            nested = Term::Triple(Box::new(Triple::new(
                NamedNode::new_unchecked("urn:s"),
                NamedNode::new_unchecked("urn:p"),
                nested,
            )));
        }
        terms.push(nested);
        for term in terms {
            let key = WorkingKey::new(&term, &budget, true).unwrap();
            assert_eq!(key.as_slice(), id::term_key(&term));
            assert_eq!(key.bytes.len(), key.bytes.capacity());
            drop(key);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn refused_key_admission_preserves_the_existing_owner() {
        let budget = Budget::new(Some(32));
        let owner = Held::try_new_exact(&budget, 17).unwrap();
        let term = Term::Literal(Literal::new_simple_literal("x".repeat(1024)));
        assert!(matches!(
            WorkingKey::new(&term, &budget, true),
            Err(BudgetFailure::Local(_))
        ));
        assert_eq!(budget.used(), 17);
        drop(owner);
        assert_eq!(budget.used(), 0);
    }
}
