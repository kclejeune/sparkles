use super::{RecordKey, RecordKind, Sha256Digest, primitives::validate_iri};
use crate::{Budget, BudgetKind, Error, Result, task::Control};
use oxrdf::{NamedNode, NamedOrBlankNode, Term};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Identity inputs supplied by the trusted importer host, not authentication proof.
#[derive(Clone, Debug)]
pub struct IdentityNamespace {
    dataset: Uuid,
    importer: NamedNode,
}

impl IdentityNamespace {
    pub fn new(dataset: Uuid, importer: NamedNode) -> Result<Self> {
        validate_iri(&importer)?;
        Ok(Self { dataset, importer })
    }

    /// UUID-v5 over a versioned, length-framed tuple. No field is a separator.
    /// Equal proposition text is deliberately absent from occurrence identity.
    pub fn record_id(
        &self,
        kind: RecordKind,
        revision_key: &RecordKey,
        occurrence_key: &RecordKey,
        control: &Control,
    ) -> Result<NamedNode> {
        control.check()?;
        let mut hash = Sha1::new();
        hash.update(self.dataset.as_bytes());
        hash.update(b"sparkles-evidence-id-v1\0");
        hash.update([kind.tag()]);
        for field in [
            self.importer.as_str(),
            revision_key.as_str(),
            occurrence_key.as_str(),
        ] {
            control.check()?;
            hash.update((field.len() as u64).to_be_bytes());
            for chunk in field.as_bytes().chunks(64 * 1024) {
                control.check()?;
                hash.update(chunk);
            }
        }
        control.check()?;
        let hash = hash.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&hash[..16]);
        let uuid = uuid::Builder::from_sha1_bytes(bytes).into_uuid();
        Ok(NamedNode::new_unchecked(format!("urn:uuid:{uuid}")))
    }
}

/// Hash the exact RDF term encoding. This does not resolve or authenticate a fact.
/// Blank labels are scoped data and are not global entity identities or anchors.
pub fn canonical_term_digest(term: &Term, control: &Control) -> Result<Sha256Digest> {
    crate::nesting::check_triple_term(term)?;
    let mut frame = FrameHash::new(b"sparkles-evidence-term-v1", control)?;
    frame.term(term)?;
    frame.finish()
}

/// Streaming, explicitly tagged frames used by the versioned preparation format.
pub(super) struct FrameHash<'a> {
    hash: Sha256,
    control: &'a Control,
    bytes: usize,
}

impl<'a> FrameHash<'a> {
    pub(super) fn new(domain: &[u8], control: &'a Control) -> Result<Self> {
        let mut frame = Self {
            hash: Sha256::new(),
            control,
            bytes: 0,
        };
        frame.field(0, domain)?;
        Ok(frame)
    }

    pub(super) fn charge_content(&mut self, len: usize) -> Result<()> {
        self.control.check()?;
        let requested = self.bytes.saturating_add(len);
        if requested > RecordKey::MAX_BYTES {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                limit: RecordKey::MAX_BYTES as u64,
                requested: requested as u64,
            }));
        }
        self.bytes = requested;
        Ok(())
    }

    pub(super) fn field(&mut self, tag: u8, value: &[u8]) -> Result<()> {
        self.charge_content(value.len().saturating_add(9))?;
        self.precharged_field(tag, value)
    }

    pub(super) fn precharged_field(&mut self, tag: u8, value: &[u8]) -> Result<()> {
        self.control.check()?;
        self.hash.update([tag]);
        self.hash.update((value.len() as u64).to_be_bytes());
        for chunk in value.chunks(64 * 1024) {
            self.control.check()?;
            self.hash.update(chunk);
        }
        Ok(())
    }

    pub(super) fn term(&mut self, term: &Term) -> Result<()> {
        crate::nesting::check_triple_term(term)?;
        self.term_inner(term)
    }

    fn term_inner(&mut self, term: &Term) -> Result<()> {
        self.control.check()?;
        match term {
            Term::NamedNode(node) => {
                validate_iri(node)?;
                self.field(1, node.as_str().as_bytes())
            }
            Term::BlankNode(node) => {
                oxrdf::BlankNodeRef::new(node.as_str())
                    .map_err(|_| Error::invalid("invalid scoped blank-node label"))?;
                self.field(2, node.as_str().as_bytes())
            }
            Term::Literal(value) => self.literal(value),
            Term::Triple(triple) => self.triple_inner(triple),
        }
    }

    pub(super) fn literal(&mut self, value: &oxrdf::Literal) -> Result<()> {
        self.control.check()?;
        oxiri::Iri::parse(value.datatype().as_str())
            .map_err(|_| Error::invalid("invalid literal datatype IRI"))?;
        if let Some(language) = value.language() {
            oxrdf::Literal::new_language_tagged_literal("", language)
                .map_err(|_| Error::invalid("invalid literal language"))?;
        }
        self.field(3, value.value().as_bytes())?;
        self.field(4, value.datatype().as_str().as_bytes())?;
        self.field(5, value.language().unwrap_or("").as_bytes())?;
        self.field(
            6,
            &[match value.direction() {
                None => 0,
                Some(oxrdf::BaseDirection::Ltr) => 1,
                Some(oxrdf::BaseDirection::Rtl) => 2,
            }],
        )
    }

    pub(super) fn triple(&mut self, triple: &oxrdf::Triple) -> Result<()> {
        let mut depth = 1;
        let mut object = &triple.object;
        while let Term::Triple(nested) = object {
            self.control.check()?;
            depth += 1;
            if depth > crate::nesting::MAX_TRIPLE_TERMS {
                return Err(Error::invalid(
                    "evidence triple term exceeds the nesting bound",
                ));
            }
            object = &nested.object;
        }
        self.triple_inner(triple)
    }

    fn triple_inner(&mut self, triple: &oxrdf::Triple) -> Result<()> {
        self.control.check()?;
        self.field(7, &[])?;
        match &triple.subject {
            NamedOrBlankNode::NamedNode(node) => {
                validate_iri(node)?;
                self.field(1, node.as_str().as_bytes())?;
            }
            NamedOrBlankNode::BlankNode(node) => {
                oxrdf::BlankNodeRef::new(node.as_str())
                    .map_err(|_| Error::invalid("invalid scoped blank-node label"))?;
                self.field(2, node.as_str().as_bytes())?;
            }
        }
        validate_iri(&triple.predicate)?;
        self.field(8, triple.predicate.as_str().as_bytes())?;
        self.term_inner(&triple.object)
    }

    pub(super) fn finish(self) -> Result<Sha256Digest> {
        self.control.check()?;
        Ok(Sha256Digest(self.hash.finalize().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{BaseDirection, Literal, Triple, vocab::xsd};

    fn namespace() -> IdentityNamespace {
        IdentityNamespace::new(
            Uuid::parse_str("12345678-1234-4234-8234-123456789abc").unwrap(),
            NamedNode::new_unchecked("urn:importer"),
        )
        .unwrap()
    }

    #[test]
    fn identity_frames_separate_all_fields_and_occurrence_keys() {
        let id = |revision, occurrence| {
            namespace()
                .record_id(
                    RecordKind::Claim,
                    &RecordKey::new(revision).unwrap(),
                    &RecordKey::new(occurrence).unwrap(),
                    &Control::none(),
                )
                .unwrap()
        };
        assert_eq!(id("revision", "occurrence"), id("revision", "occurrence"));
        assert_ne!(id("a\0b", "c"), id("a", "b\0c"));
        assert_ne!(id("revision", "one"), id("revision", "two"));
        // Independent Python hashlib/uuid golden for this versioned frame.
        assert_eq!(
            id("revision", "one").as_str(),
            "urn:uuid:348a4c24-e2f8-57b7-b555-64111ef6d981"
        );
        let other = IdentityNamespace::new(
            Uuid::parse_str("12345678-1234-4234-8234-123456789abc").unwrap(),
            NamedNode::new_unchecked("urn:other-importer"),
        )
        .unwrap();
        assert_ne!(
            id("revision", "one"),
            other
                .record_id(
                    RecordKind::Claim,
                    &RecordKey::new("revision").unwrap(),
                    &RecordKey::new("one").unwrap(),
                    &Control::none()
                )
                .unwrap()
        );
        assert_ne!(
            id("revision", "one"),
            namespace()
                .record_id(
                    RecordKind::Assessment,
                    &RecordKey::new("revision").unwrap(),
                    &RecordKey::new("one").unwrap(),
                    &Control::none()
                )
                .unwrap()
        );
    }

    #[test]
    fn canonical_terms_preserve_lexical_precision_datatype_and_language_direction() {
        let hash = |term| canonical_term_digest(&term, &Control::none()).unwrap();
        assert_eq!(
            hash(NamedNode::new_unchecked("urn:x").into()).to_string(),
            "sha256:9fdac5ac58d94b001645dcea6287830beb3d3e616cde026f0e116473df72731e"
        );
        let exact = Literal::new_typed_literal("0.75000000000000000001", xsd::DECIMAL);
        assert_ne!(
            hash(exact.clone().into()),
            hash(Literal::new_typed_literal("0.75", xsd::DECIMAL).into())
        );
        assert_ne!(
            hash(exact.into()),
            hash(Literal::new_simple_literal("0.75000000000000000001").into())
        );
        let directional = |direction| {
            Literal::new_directional_language_tagged_literal("تنبيه", "ar", direction).unwrap()
        };
        assert_ne!(
            hash(directional(BaseDirection::Rtl).into()),
            hash(directional(BaseDirection::Ltr).into())
        );
        let triple: Term = Triple::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            Literal::new_simple_literal("x"),
        )
        .into();
        assert_ne!(
            hash(triple.clone()),
            hash(Literal::new_simple_literal(triple.to_string()).into())
        );
        assert_eq!(
            hash(triple.clone()).to_string(),
            "sha256:9dd0df9616bb436a01f5d84b48f74fcd264a5ee58da365bcd2f5c42aa9b471cd"
        );
        let Term::Triple(triple) = &triple else {
            unreachable!()
        };
        let control = Control::none();
        let mut borrowed = FrameHash::new(b"sparkles-evidence-term-v1", &control).unwrap();
        borrowed.triple(triple).unwrap();
        assert_eq!(
            borrowed.finish().unwrap().to_string(),
            "sha256:9dd0df9616bb436a01f5d84b48f74fcd264a5ee58da365bcd2f5c42aa9b471cd"
        );
    }

    #[test]
    fn borrowed_literal_frames_preserve_independent_lexical_datatype_language_and_direction_goldens()
     {
        let cases = [
            (
                Literal::new_simple_literal("x"),
                "0d9141e6312c1f3601297b5c28e59580a7e4cfac25a05c08e68dfa382deccc3a",
            ),
            (
                Literal::new_typed_literal("0.75000000000000000001", xsd::DECIMAL),
                "5c47a36024eec23e39aeb0ca376618c7302fda180925f0a33c43f0bea8d8b939",
            ),
            (
                Literal::new_language_tagged_literal("café", "FR").unwrap(),
                "d3c0430b8c6f4003ea975d30d586074b913ddb620c94f209941ad5ffe19a060f",
            ),
            (
                Literal::new_directional_language_tagged_literal("تنبيه", "ar", BaseDirection::Rtl)
                    .unwrap(),
                "3a7e17ee535635a68ffa974c970ccafc220b5674263002853ec9c9d14a22ffcf",
            ),
        ];
        for (literal, golden) in cases {
            let control = Control::none();
            let mut frame = FrameHash::new(b"sparkles-evidence-term-v1", &control).unwrap();
            frame.literal(&literal).unwrap();
            let borrowed = frame.finish().unwrap();
            assert_eq!(borrowed.to_string(), format!("sha256:{golden}"));
            assert_eq!(
                borrowed,
                canonical_term_digest(&literal.into(), &control).unwrap()
            );
        }
    }

    #[test]
    fn unchecked_iris_and_cancelled_preparation_are_refused() {
        assert!(IdentityNamespace::new(Uuid::nil(), NamedNode::new_unchecked("relative")).is_err());
        assert!(
            canonical_term_digest(
                &NamedNode::new_unchecked("relative").into(),
                &Control::none()
            )
            .is_err()
        );
        let control = Control::none();
        control.cancel.cancel();
        assert!(matches!(
            namespace().record_id(
                RecordKind::Claim,
                &RecordKey::new("r").unwrap(),
                &RecordKey::new("c").unwrap(),
                &control
            ),
            Err(Error::Cancelled)
        ));
    }
}
