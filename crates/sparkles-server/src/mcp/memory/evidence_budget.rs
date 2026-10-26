//! Count added evidence before allocating its full strings. Previously completed
//! candidates share one request budget. The final decorated response remains the
//! authority for exact serialized size; candidate charges are its lower bound.

use serde_json::Value;
use sparkles::error::{Budget, BudgetKind, Error};
use std::{fmt, io};

pub(super) struct EvidenceBudget {
    limit: usize,
    used: usize,
}

impl EvidenceBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self { limit, used: 0 }
    }

    pub(super) fn preflight(&self) -> Preflight {
        Preflight {
            count: Count {
                remaining: self.limit - self.used,
            },
            limit: self.limit,
        }
    }

    pub(super) fn charge(&mut self, value: &Value) -> Result<(), Error> {
        let remaining = self.limit - self.used;
        let mut count = Count { remaining };
        serde_json::to_writer(&mut count, value).map_err(|_| exceeded(self.limit))?;
        self.used += remaining - count.remaining;
        Ok(())
    }
}

fn exceeded(limit: usize) -> Error {
    Error::BudgetExceeded(Budget {
        kind: BudgetKind::ResultBytes,
        limit: limit as u64,
        requested: (limit as u64).saturating_add(1),
    })
}

struct Count {
    remaining: usize,
}

impl Count {
    fn take(&mut self, bytes: usize) -> Result<(), ()> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or(())?;
        Ok(())
    }
}

impl io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.take(bytes.len())
            .map_err(|()| io::Error::other("evidence result byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) struct Preflight {
    count: Count,
    limit: usize,
}

impl Preflight {
    /// A JSON string's quotes and escaped UTF-8 bytes, borrowed from the source.
    pub(super) fn string(&mut self, value: &str) -> Result<(), Error> {
        self.count.take(2).map_err(|()| exceeded(self.limit))?;
        self.escaped(value).map_err(|_| exceeded(self.limit))
    }

    /// RDF Display writes into this bounded formatter directly. Never create a
    /// full temporary RDF term merely to learn its JSON-escaped length.
    pub(super) fn display(&mut self, value: impl fmt::Display) -> Result<(), Error> {
        self.count.take(2).map_err(|()| exceeded(self.limit))?;
        fmt::write(self, format_args!("{value}")).map_err(|_| exceeded(self.limit))
    }

    fn escaped(&mut self, value: &str) -> fmt::Result {
        for byte in value.bytes() {
            let bytes = match byte {
                b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
                0..=0x1f => 6,
                _ => 1,
            };
            self.count.take(bytes).map_err(|()| fmt::Error)?;
        }
        Ok(())
    }
}

impl fmt::Write for Preflight {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.escaped(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    #[test]
    fn counts_rdf_formatting_and_json_escaping_without_changing_terms() {
        for literal in [
            Literal::new_simple_literal("\"\\\n\r\t\u{8}\u{c}\u{1b}雪☃"),
            Literal::new_language_tagged_literal("a\"\\\n雪", "en-Latn-GB").unwrap(),
            Literal::new_typed_literal("a\"\\\n雪", NamedNode::new("urn:datatype:雪").unwrap()),
        ] {
            let term = literal.to_string();
            let bytes = serde_json::to_vec(&term).unwrap().len();
            assert!(
                EvidenceBudget::new(bytes)
                    .preflight()
                    .display(&literal)
                    .is_ok()
            );
            assert!(
                EvidenceBudget::new(bytes - 1)
                    .preflight()
                    .display(&literal)
                    .is_err()
            );
        }
        let iri = "urn:kind:雪";
        let bytes = serde_json::to_vec(iri).unwrap().len();
        assert!(EvidenceBudget::new(bytes).preflight().string(iri).is_ok());
        assert!(
            EvidenceBudget::new(bytes - 1)
                .preflight()
                .string(iri)
                .is_err()
        );
    }

    #[test]
    fn completed_candidates_share_the_request_limit() {
        let candidate = serde_json::json!({"evidence":{"term":"a\\b\"c\n雪"}});
        let bytes = serde_json::to_vec(&candidate).unwrap().len();
        let mut budget = EvidenceBudget::new(bytes * 2);
        budget.charge(&candidate).unwrap();
        budget.charge(&candidate).unwrap();
        assert!(budget.charge(&candidate).is_err());
    }
}
