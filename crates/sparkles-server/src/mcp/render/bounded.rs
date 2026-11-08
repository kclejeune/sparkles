//! Borrowed formatting for the physical owned compact response path.
//! No recursive RDF-star traversal or intermediate cell String is constructed.
use super::*;
use oxrdf::{LiteralRef, TermRef};
use sparkles::{
    error::Result,
    sparql::output::{NativeBudget, OwnedText},
};
use std::fmt::{self, Write as _};

pub(super) fn term_text(
    term: TermRef<'_>,
    prefixes: &Prefixes,
    max_chars: usize,
    budget: &NativeBudget,
) -> Result<OwnedText> {
    budget.format_text(&TermFormat {
        term,
        prefixes,
        max_chars,
        budget,
    })
}

pub(super) struct TermFormat<'a> {
    pub(super) term: TermRef<'a>,
    pub(super) prefixes: &'a Prefixes,
    pub(super) max_chars: usize,
    pub(super) budget: &'a NativeBudget,
}
impl TermFormat<'_> {
    fn check(&self) -> fmt::Result {
        self.budget.check().map_err(|_| fmt::Error)
    }
    pub(super) fn escape(&self, out: &mut impl fmt::Write, value: &str) -> fmt::Result {
        for (i, c) in value.chars().enumerate() {
            if i.is_multiple_of(1024) {
                self.check()?;
            }
            match c {
                '\\' => out.write_str("\\\\")?,
                '"' => out.write_str("\\\"")?,
                '\n' => out.write_str("\\n")?,
                '\r' => out.write_str("\\r")?,
                '\t' => out.write_str("\\t")?,
                c if (c as u32) < 0x20
                    || (0x7f..=0x9f).contains(&(c as u32))
                    || c == '\u{2028}'
                    || c == '\u{2029}' =>
                {
                    write!(out, "\\u{:04X}", c as u32)?
                }
                c => out.write_char(c)?,
            }
        }
        self.check()
    }
    fn cut<'a>(
        &self,
        value: &'a str,
        maximum: usize,
    ) -> std::result::Result<(&'a str, usize), fmt::Error> {
        let mut cut_at = value.len();
        let mut rest = 0usize;
        for (i, (at, _)) in value.char_indices().enumerate() {
            if i.is_multiple_of(1024) {
                self.check()?;
            }
            if i == maximum {
                cut_at = at;
            }
            if i >= maximum {
                rest = rest.checked_add(1).ok_or(fmt::Error)?;
            }
        }
        self.check()?;
        Ok((&value[..cut_at], rest))
    }
    fn iri(&self, out: &mut impl fmt::Write, iri: &str, maximum: usize) -> fmt::Result {
        let (kept, rest) = self.cut(iri, maximum)?;
        if rest > 0 {
            out.write_char('<')?;
            self.escape(out, kept)?;
            return write!(out, ">…(+{rest} chars)");
        }
        // Prefixes already have the established longest namespace/name order.
        for (ns, name) in &self.prefixes.by_ns {
            self.check()?;
            if let Some(local) = iri
                .strip_prefix(ns.as_str())
                .filter(|local| valid_local(local))
            {
                out.write_str(name)?;
                out.write_char(':')?;
                return out.write_str(local);
            }
        }
        out.write_char('<')?;
        self.escape(out, iri)?;
        out.write_char('>')
    }
    fn literal(&self, out: &mut impl fmt::Write, literal: LiteralRef<'_>) -> fmt::Result {
        let value = literal.value();
        let (kept, rest) = self.cut(value, self.max_chars)?;
        let datatype = literal.datatype();
        let dt = datatype.as_str();
        let bare = rest == 0
            && match dt {
                XSD_INTEGER => canonical_integer(value),
                XSD_DECIMAL => canonical_decimal(value),
                XSD_BOOLEAN => value == "true" || value == "false",
                _ => false,
            };
        if bare {
            return out.write_str(value);
        }
        out.write_char('"')?;
        self.escape(out, kept)?;
        out.write_char('"')?;
        if let Some(language) = literal.language() {
            out.write_char('@')?;
            out.write_str(language)?;
            if let Some(direction) = literal.direction() {
                write!(out, "--{direction}")?;
            }
        } else if dt != XSD_STRING && dt != RDF_LANG_STRING && dt != RDF_DIR_LANG_STRING {
            out.write_str("^^")?;
            self.iri(out, dt, usize::MAX)?;
        }
        if rest > 0 {
            write!(out, "…(+{rest} chars)")?;
        }
        self.check()
    }
}
impl fmt::Display for TermFormat<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut term = self.term;
        let mut depth = 0usize;
        loop {
            self.check()?;
            match term {
                TermRef::NamedNode(node) => self.iri(out, node.as_str(), self.max_chars)?,
                TermRef::BlankNode(node) => {
                    out.write_str("_:")?;
                    out.write_str(node.as_str())?;
                }
                TermRef::Literal(literal) => self.literal(out, literal)?,
                TermRef::Triple(triple) => {
                    out.write_str("<<( ")?;
                    match &triple.subject {
                        NamedOrBlankNode::NamedNode(node) => {
                            self.iri(out, node.as_str(), self.max_chars)?
                        }
                        NamedOrBlankNode::BlankNode(node) => {
                            out.write_str("_:")?;
                            out.write_str(node.as_str())?;
                        }
                    }
                    out.write_char(' ')?;
                    self.iri(out, triple.predicate.as_str(), self.max_chars)?;
                    out.write_char(' ')?;
                    depth = depth.checked_add(1).ok_or(fmt::Error)?;
                    term = triple.object.as_ref();
                    continue;
                }
            }
            break;
        }
        for i in 0..depth {
            if i.is_multiple_of(1024) {
                self.check()?;
            }
            out.write_str(" )>>")?;
        }
        self.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{BlankNode, NamedNode};
    use sparkles::{
        access::Preparation,
        error::{BudgetKind, Error},
        sparql::pool::QueryPool,
        task::Control,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn owned_compact_terms_match_the_existing_lexical_oracle_without_eager_cell_strings() {
        let map = BTreeMap::from([
            ("ex".into(), "http://ex.org/".into()),
            ("exa".into(), "http://ex.org/a/".into()),
            ("xsd".into(), "http://www.w3.org/2001/XMLSchema#".into()),
        ]);
        let prefixes = Prefixes::new(&map);
        let preparation = Preparation::new(Control::default(), 4 << 20).unwrap();
        let budget = NativeBudget::from_preparation(&preparation).unwrap();
        let iri = |s: &str| Term::NamedNode(NamedNode::new(s).unwrap());
        let mut terms = vec![
            iri("http://ex.org/alice"),
            iri("http://ex.org/a/b"),
            iri("http://ex.org/a."),
            iri("http://ex.org/"),
            iri("http://ex.org/x/y"),
            Term::BlankNode(BlankNode::new("b1f").unwrap()),
            Literal::new_simple_literal("a\tb\"c\nd\u{2028}\u{85}λ").into(),
            Literal::new_language_tagged_literal_unchecked("x", "en").into(),
        ];
        for (value, datatype) in [
            ("42", XSD_INTEGER),
            ("-7", XSD_INTEGER),
            ("042", XSD_INTEGER),
            ("1.5", XSD_DECIMAL),
            ("1.50", XSD_DECIMAL),
            ("true", XSD_BOOLEAN),
            ("false", XSD_BOOLEAN),
            ("-0", XSD_INTEGER),
            ("2020-01-01", "http://www.w3.org/2001/XMLSchema#date"),
            ("v", "http://other.org/dt"),
        ] {
            terms.push(Literal::new_typed_literal(value, NamedNode::new(datatype).unwrap()).into());
        }
        for maximum in [0, 1, 16, 500, usize::MAX] {
            for term in &terms {
                let expected = Terms::new(&prefixes, maximum).term(term);
                let actual = term_text(term.as_ref(), &prefixes, maximum, &budget).unwrap();
                assert_eq!(actual.as_str(), expected, "max={maximum}, term={term}");
            }
        }
    }

    #[test]
    fn borrowed_compact_term_refusal_and_last_text_clone_keep_real_receiver_lineage() {
        let prefixes = Prefixes::new(&BTreeMap::new());
        let term: Term = Literal::new_simple_literal("λ".repeat(64 << 10)).into();
        let pool = QueryPool::new(16 << 20);
        let grant = Arc::new(pool.empty_grant());
        let cancel = Arc::new(AtomicBool::new(false));
        let preparation =
            Preparation::with_grant(Control::with_cancel(cancel.clone()), 4 << 20, grant.clone())
                .unwrap();
        let budget = NativeBudget::from_preparation(&preparation).unwrap();
        let before = grant.used();
        let text = term_text(term.as_ref(), &prefixes, usize::MAX, &budget).unwrap();
        assert_eq!(text.len(), (128 << 10) + 2);
        let held = grant.used();
        assert!(held >= before + text.len() as u64);
        let last = text.clone();
        assert_eq!(last.as_str().as_ptr(), text.as_str().as_ptr());
        drop(text);
        cancel.store(true, Ordering::SeqCst);
        assert!(matches!(
            term_text(term.as_ref(), &prefixes, usize::MAX, &budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(grant.used(), held);
        drop(budget);
        drop(preparation);
        assert!(grant.used() >= last.len() as u64);
        assert!(last.as_str().starts_with('"'));
        drop(last);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
        let small = Preparation::new(Control::default(), 4096).unwrap();
        let budget = NativeBudget::from_preparation(&small).unwrap();
        assert!(
            matches!(term_text(term.as_ref(), &prefixes, usize::MAX, &budget), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory && b.limit == 4096)
        );
        assert!(matches!(term, Term::Literal(ref literal) if literal.value().len() == 128 << 10));
    }

    #[test]
    fn nested_borrowed_compact_term_formats_on_a_small_stack_without_clone_or_recursive_walk() {
        let prefixes = Prefixes::new(&BTreeMap::new());
        let mut term: Term = Literal::new_simple_literal("leaf").into();
        for _ in 0..256 {
            term = Term::Triple(Box::new(Triple::new(
                NamedNode::new_unchecked("http://ex.org/s"),
                NamedNode::new_unchecked("http://ex.org/p"),
                term,
            )));
        }
        let expected = Terms::new(&prefixes, 500).term(&term);
        let preparation = Preparation::new(Control::default(), 4 << 20).unwrap();
        let budget = NativeBudget::from_preparation(&preparation).unwrap();
        let actual = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(32 << 10)
                .spawn_scoped(scope, || term_text(term.as_ref(), &prefixes, 500, &budget))
                .unwrap()
                .join()
                .unwrap()
                .unwrap()
        });
        assert_eq!(actual.as_str(), expected);
        assert_eq!(actual.as_str().matches("<<( ").count(), 256);
        assert_eq!(actual.as_str().matches(" )>>").count(), 256);
        drop(actual);
        // The actual source term was borrowed, and is retired on this parent
        // stack rather than transferred to a small-stack recursive destructor.
        drop(term);
    }
}
