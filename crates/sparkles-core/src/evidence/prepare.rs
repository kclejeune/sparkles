use super::{
    AssessmentInput, BlankAnchor, ClaimInput, Confidence, ConfidenceFacets, EvidenceInput,
    EvidenceRefTarget, GraphIdentity, MethodRef, RecordKind, RecordRef, RenditionContent,
    Sha256Digest, SourceContent, canonical::FrameHash, primitives::validate_iri,
};
use crate::{Error, Result, task::Control};
use oxrdf::{Literal, NamedOrBlankNode, Term, Triple, vocab::xsd};
use oxsdatatypes::{DateTime, TimezoneOffset};
use std::collections::BTreeMap;

/// The declared default bounds, independent of lower HTTP or host memory limits.
pub const MAX_CLAIMS: usize = 1_000;
pub const MAX_LINKS: usize = 10_000;

/// Work that a typed runtime resolver must complete before publishing any record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolutionRequirement {
    ExistingRecord {
        record: oxrdf::NamedNode,
        expected_kinds: Vec<RecordKind>,
    },
    StoredFact {
        reference_key: String,
    },
    StructuredValue {
        reference_key: String,
    },
    RetainedPassage {
        passage_key: String,
    },
    PassageIdentity {
        passage_key: String,
    },
    PassageReferenceIdentity {
        reference_key: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedContent {
    pub digest: Sha256Digest,
    pub retained: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedSourceTime {
    pub original: Literal,
    pub utc: Literal,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizedSourceTimes {
    pub published_at: Option<NormalizedSourceTime>,
    pub reported_available_at: Option<NormalizedSourceTime>,
}

/// Borrowed input cannot be changed while this preparation result is retained.
/// Its requirements are unverified data. No receipt, owner or caller authority exists here.
pub struct PreparedInput<'a> {
    input: &'a EvidenceInput,
    body_digest: Sha256Digest,
    content: BTreeMap<String, PreparedContent>,
    times: BTreeMap<String, NormalizedSourceTimes>,
    requirements: Vec<ResolutionRequirement>,
    assessment_times: BTreeMap<String, NormalizedSourceTime>,
}

impl<'a> PreparedInput<'a> {
    pub fn input(&self) -> &'a EvidenceInput {
        self.input
    }
    pub fn body_digest(&self) -> Sha256Digest {
        self.body_digest
    }
    pub fn content(&self) -> &BTreeMap<String, PreparedContent> {
        &self.content
    }
    pub fn normalized_times(&self) -> &BTreeMap<String, NormalizedSourceTimes> {
        &self.times
    }
    pub fn requirements(&self) -> &[ResolutionRequirement] {
        &self.requirements
    }
    pub fn assessment_times(&self) -> &BTreeMap<String, NormalizedSourceTime> {
        &self.assessment_times
    }
}

/// Validate the candidate's local structure and hash its versioned data encoding.
/// Existing records, snapshots and structured values still require runtime resolution.
pub fn prepare_input<'a>(input: &'a EvidenceInput, control: &Control) -> Result<PreparedInput<'a>> {
    control.check()?;
    if input.claims.len() > MAX_CLAIMS || input.links.len() > MAX_LINKS {
        return Err(Error::invalid(
            "evidence batch exceeds 1000 claims or 10000 links",
        ));
    }
    let mut aliases = BTreeMap::new();
    let mut frame = FrameHash::new(b"sparkles-evidence-input-v1", control)?;
    macro_rules! register {
        ($records:expr, $kind:expr) => {
            for (index, record) in ($records).iter().enumerate() {
                control.check()?;
                // Meter every canonical header before allocating its lookup entry.
                frame.charge_content(record.key.as_str().len().saturating_add(19))?;
                if aliases
                    .insert(record.key.as_str(), LocalRecord { kind: $kind, index })
                    .is_some()
                {
                    return Err(Error::invalid("batch record aliases must be unique"));
                }
            }
        };
    }
    register!(&input.revisions, RecordKind::SourceRevision);
    register!(&input.renditions, RecordKind::Rendition);
    register!(&input.passages, RecordKind::Passage);
    register!(&input.references, RecordKind::EvidenceRef);
    register!(&input.claims, RecordKind::Claim);
    register!(&input.links, RecordKind::EvidenceLink);
    register!(&input.assessments, RecordKind::Assessment);
    let mut preparation = Preparation {
        frame,
        aliases,
        requirements: Vec::new(),
        control,
    };
    let mut content = BTreeMap::new();
    let mut times = BTreeMap::new();
    for source in &input.revisions {
        preparation.start(RecordKind::SourceRevision, source.key.as_str())?;
        preparation.node(&source.source)?;
        preparation.text(source.source_key.as_str())?;
        preparation.optional_text(source.upstream_version.as_ref().map(|v| v.as_str()))?;
        let (digest, retained) = match &source.content {
            SourceContent::Retained {
                bytes,
                expected_digest,
            } => {
                preparation.frame.charge_content(bytes.len())?;
                let actual = Sha256Digest::of_bytes(bytes, control)?;
                expected_digest_matches(*expected_digest, actual)?;
                preparation.number(1)?;
                preparation.optional_digest(*expected_digest)?;
                (actual, true)
            }
            SourceContent::MetadataOnly { digest } => {
                preparation.number(0)?;
                (*digest, false)
            }
        };
        preparation.digest(digest)?;
        content.insert(
            source.key.as_str().to_owned(),
            PreparedContent { digest, retained },
        );
        preparation.number(source.metadata.locators.len() as u64)?;
        for locator in &source.metadata.locators {
            preparation.node(locator)?;
        }
        preparation.number(source.metadata.reported_rights.len() as u64)?;
        for rights in &source.metadata.reported_rights {
            preparation.frame.term(rights)?;
        }
        let normalized = NormalizedSourceTimes {
            published_at: preparation.source_time(&source.times.published_at)?,
            reported_available_at: preparation.source_time(&source.times.reported_available_at)?,
        };
        times.insert(source.key.as_str().to_owned(), normalized);
        preparation.number(source.lineage.upstream_revisions.len() as u64)?;
        for upstream in &source.lineage.upstream_revisions {
            preparation.reference(upstream, &[RecordKind::SourceRevision])?;
        }
        preparation.number(source.lineage.families.len() as u64)?;
        for family in &source.lineage.families {
            preparation.node(family)?;
        }
    }
    for rendition in &input.renditions {
        preparation.start(RecordKind::Rendition, rendition.key.as_str())?;
        preparation.reference(&rendition.revision, &[RecordKind::SourceRevision])?;
        preparation.text(rendition.occurrence_key.as_str())?;
        preparation.method(&rendition.conversion)?;
        if rendition.media_type.is_empty() {
            return Err(Error::invalid("rendition media type is required"));
        }
        preparation.text(&rendition.media_type)?;
        preparation.optional_text(rendition.language.as_deref())?;
        let (digest, retained, kind) = match &rendition.content {
            RenditionContent::NormalizedText(text) => {
                preparation.frame.charge_content(text.len())?;
                (Sha256Digest::of_bytes(text.as_bytes(), control)?, true, 1)
            }
            RenditionContent::RetainedBytes(bytes) => {
                preparation.frame.charge_content(bytes.len())?;
                (Sha256Digest::of_bytes(bytes, control)?, true, 2)
            }
            RenditionContent::MetadataOnly(digest) => (*digest, false, 0),
        };
        expected_digest_matches(rendition.expected_digest, digest)?;
        preparation.number(kind)?;
        preparation.digest(digest)?;
        preparation.optional_digest(rendition.expected_digest)?;
        content.insert(
            rendition.key.as_str().to_owned(),
            PreparedContent { digest, retained },
        );
    }
    let mut passage_groups: BTreeMap<usize, PassageGroup> = BTreeMap::new();
    for (passage_index, passage) in input.passages.iter().enumerate() {
        preparation.start(RecordKind::Passage, passage.key.as_str())?;
        preparation.reference(&passage.revision, &[RecordKind::SourceRevision])?;
        preparation.reference(&passage.rendition, &[RecordKind::Rendition])?;
        preparation.text(passage.occurrence_key.as_str())?;
        if passage.start >= passage.end {
            return Err(Error::invalid("span-mismatch: passage must be nonempty"));
        }
        preparation.number(passage.start)?;
        preparation.number(passage.end)?;
        preparation.digest(passage.text_digest)?;
        preparation.optional_text(passage.quote.as_deref())?;
        if let RecordRef::Local(key) = &passage.rendition {
            let rendition_index = preparation.local_index(key);
            let rendition = &input.renditions[rendition_index];
            if rendition.revision != passage.revision {
                preparation
                    .requirements
                    .push(ResolutionRequirement::PassageIdentity {
                        passage_key: passage.key.as_str().into(),
                    });
            }
            match &rendition.content {
                RenditionContent::NormalizedText(_) => {
                    let group = passage_groups.entry(rendition_index).or_default();
                    group.passages.push(passage_index);
                    group.offsets.extend([passage.start, passage.end]);
                }
                _ => preparation
                    .requirements
                    .push(ResolutionRequirement::RetainedPassage {
                        passage_key: passage.key.as_str().into(),
                    }),
            }
        } else {
            preparation
                .requirements
                .push(ResolutionRequirement::PassageIdentity {
                    passage_key: passage.key.as_str().into(),
                });
            preparation
                .requirements
                .push(ResolutionRequirement::RetainedPassage {
                    passage_key: passage.key.as_str().into(),
                });
        }
    }
    verify_local_passages(input, passage_groups, control)?;
    let mut structured_groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (reference_index, reference) in input.references.iter().enumerate() {
        preparation.start(RecordKind::EvidenceRef, reference.key.as_str())?;
        preparation.text(reference.occurrence_key.as_str())?;
        match &reference.target {
            EvidenceRefTarget::StoredFact(fact) => {
                preparation.number(1)?;
                preparation.frame.field(30, fact.dataset.as_bytes())?;
                preparation.frame.field(31, fact.branch.as_bytes())?;
                preparation.number(fact.commit)?;
                match &fact.graph {
                    GraphIdentity::Default => preparation.number(0)?,
                    GraphIdentity::Named(graph) => {
                        preparation.number(1)?;
                        preparation.node(graph)?;
                    }
                }
                preparation.triple(&fact.expected)?;
                if has_blank(&fact.expected) && fact.anchor.is_none() {
                    return Err(Error::invalid(
                        "unanchored-evidence: blank-containing facts need a retained anchor",
                    ));
                }
                match &fact.anchor {
                    None => preparation.number(0)?,
                    Some(BlankAnchor::Reifier(node)) => {
                        preparation.number(1)?;
                        preparation.node(node)?;
                    }
                    Some(BlankAnchor::ImportMap(node)) => {
                        preparation.number(2)?;
                        preparation.node(node)?;
                    }
                }
                preparation
                    .requirements
                    .push(ResolutionRequirement::StoredFact {
                        reference_key: reference.key.as_str().into(),
                    });
            }
            EvidenceRefTarget::Passage {
                revision,
                rendition,
                passage,
            } => {
                preparation.number(2)?;
                preparation.reference(revision, &[RecordKind::SourceRevision])?;
                preparation.reference(rendition, &[RecordKind::Rendition])?;
                preparation.reference(passage, &[RecordKind::Passage])?;
                if let RecordRef::Local(key) = passage {
                    let selected = &input.passages[preparation.local_index(key)];
                    if &selected.revision != revision || &selected.rendition != rendition {
                        preparation.requirements.push(
                            ResolutionRequirement::PassageReferenceIdentity {
                                reference_key: reference.key.as_str().into(),
                            },
                        );
                    }
                } else {
                    preparation.requirements.push(
                        ResolutionRequirement::PassageReferenceIdentity {
                            reference_key: reference.key.as_str().into(),
                        },
                    );
                }
            }
            EvidenceRefTarget::Structured {
                revision,
                pointer,
                method,
                value_digest,
            } => {
                preparation.number(3)?;
                preparation.reference(revision, &[RecordKind::SourceRevision])?;
                super::structured::validate_pointer(pointer, control)?;
                preparation.text(pointer)?;
                preparation.method(method)?;
                preparation.digest(*value_digest)?;
                let verified = if method.method.as_str() == super::JSON_LEXICAL_UTF8_V1
                    && method.version.as_str() == super::JSON_LEXICAL_UTF8_VERSION
                    && let RecordRef::Local(key) = revision
                    && matches!(
                        &input.revisions[preparation.local_index(key)].content,
                        SourceContent::Retained { .. }
                    ) {
                    structured_groups
                        .entry(preparation.local_index(key))
                        .or_default()
                        .push(reference_index);
                    true
                } else {
                    false
                };
                if !verified {
                    preparation
                        .requirements
                        .push(ResolutionRequirement::StructuredValue {
                            reference_key: reference.key.as_str().into(),
                        });
                }
            }
        }
    }
    verify_local_structured(input, structured_groups, control)?;
    for claim in &input.claims {
        preparation.claim(claim)?;
    }
    for link in &input.links {
        preparation.start(RecordKind::EvidenceLink, link.key.as_str())?;
        preparation.text(link.occurrence_key.as_str())?;
        preparation.reference(&link.target, &[RecordKind::Claim, RecordKind::Assessment])?;
        preparation.reference(&link.evidence, &[RecordKind::EvidenceRef])?;
        preparation.text(match link.relation {
            super::EvidenceRelation::Supports => "supports",
            super::EvidenceRelation::Contradicts => "contradicts",
            super::EvidenceRelation::Mentions => "mentions",
        })?;
        preparation.node(&link.attributed_author)?;
    }
    let mut assessment_times = BTreeMap::new();
    for assessment in &input.assessments {
        preparation.assessment(assessment)?;
        assessment_times.insert(
            assessment.key.as_str().to_owned(),
            normalize_time(&assessment.evaluated_at)?,
        );
    }
    Ok(PreparedInput {
        input,
        body_digest: preparation.frame.finish()?,
        content,
        times,
        requirements: preparation.requirements,
        assessment_times,
    })
}

struct LocalRecord {
    kind: RecordKind,
    index: usize,
}

struct Preparation<'a> {
    frame: FrameHash<'a>,
    aliases: BTreeMap<&'a str, LocalRecord>,
    requirements: Vec<ResolutionRequirement>,
    control: &'a Control,
}

impl Preparation<'_> {
    fn text(&mut self, value: &str) -> Result<()> {
        self.frame.field(10, value.as_bytes())
    }
    fn number(&mut self, value: u64) -> Result<()> {
        self.frame.field(11, &value.to_be_bytes())
    }
    fn digest(&mut self, value: Sha256Digest) -> Result<()> {
        self.frame.field(12, value.as_bytes())
    }
    fn node(&mut self, value: &oxrdf::NamedNode) -> Result<()> {
        validate_iri(value)?;
        self.frame.field(13, value.as_str().as_bytes())
    }
    fn optional_text(&mut self, value: Option<&str>) -> Result<()> {
        self.number(u64::from(value.is_some()))?;
        if let Some(value) = value {
            self.text(value)?;
        }
        Ok(())
    }
    fn optional_digest(&mut self, value: Option<Sha256Digest>) -> Result<()> {
        self.number(u64::from(value.is_some()))?;
        if let Some(value) = value {
            self.digest(value)?;
        }
        Ok(())
    }
    fn start(&mut self, kind: RecordKind, alias: &str) -> Result<()> {
        self.control.check()?;
        self.frame.precharged_field(20, &[kind.tag()])?;
        self.frame.precharged_field(10, alias.as_bytes())
    }
    fn source_time(&mut self, time: &Option<Literal>) -> Result<Option<NormalizedSourceTime>> {
        self.number(u64::from(time.is_some()))?;
        match time {
            None => Ok(None),
            Some(time) => {
                self.frame.literal(time)?;
                Ok(Some(normalize_time(time)?))
            }
        }
    }

    fn method(&mut self, method: &MethodRef) -> Result<()> {
        self.node(&method.method)?;
        self.text(method.version.as_str())
    }
    fn triple(&mut self, triple: &Triple) -> Result<()> {
        self.frame.triple(triple)
    }

    fn local_index(&self, key: &super::RecordKey) -> usize {
        self.aliases
            .get(key.as_str())
            .expect("validated local reference")
            .index
    }

    fn reference(&mut self, reference: &RecordRef, allowed: &[RecordKind]) -> Result<()> {
        match reference {
            RecordRef::Local(key) => {
                let record = self
                    .aliases
                    .get(key.as_str())
                    .ok_or_else(|| Error::invalid("unknown batch-local record alias"))?;
                if !allowed.contains(&record.kind) {
                    return Err(Error::invalid(
                        "batch-local reference has the wrong record kind",
                    ));
                }
                self.number(0)?;
                self.text(key.as_str())
            }
            RecordRef::Existing(node) => {
                self.requirements
                    .push(ResolutionRequirement::ExistingRecord {
                        record: node.clone(),
                        expected_kinds: allowed.to_vec(),
                    });
                self.number(1)?;
                self.node(node)
            }
        }
    }

    fn scope(&mut self, scope: &super::Scope) -> Result<()> {
        self.number(scope.len() as u64)?;
        for (predicate, values) in scope {
            self.control.check()?;
            oxiri::Iri::parse(predicate.as_str())
                .map_err(|_| Error::invalid("invalid scope predicate"))?;
            if predicate.starts_with("urn:x-sparkles:evidence:")
                || predicate.starts_with("http://www.w3.org/ns/prov#")
                || [
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies",
                ]
                .contains(&predicate.as_str())
            {
                return Err(Error::invalid(
                    "scope cannot replace structural or authenticated profile fields",
                ));
            }
            self.text(predicate)?;
            self.number(values.len() as u64)?;
            for value in values {
                self.frame.term(value)?;
            }
        }
        Ok(())
    }

    fn confidence(&mut self, value: Option<&Confidence>, extraction: bool) -> Result<()> {
        self.number(u64::from(value.is_some()))?;
        if let Some(value) = value {
            value.validate(extraction)?;
            self.node(&value.scale)?;
            self.frame.literal(&value.value)?;
            self.node(&value.evaluator)?;
            self.number(u64::from(value.method.is_some()))?;
            if let Some(method) = &value.method {
                self.method(method)?;
            }
        }
        Ok(())
    }

    fn facets(&mut self, facets: &ConfidenceFacets) -> Result<()> {
        self.confidence(facets.source.as_ref(), false)?;
        self.confidence(facets.extraction.as_ref(), true)?;
        self.confidence(facets.analyst.as_ref(), false)
    }

    fn claim(&mut self, claim: &ClaimInput) -> Result<()> {
        self.start(RecordKind::Claim, claim.key.as_str())?;
        self.text(claim.occurrence_key.as_str())?;
        self.reference(&claim.revision, &[RecordKind::SourceRevision])?;
        self.triple(&claim.proposition)?;
        self.text(match claim.kind {
            super::ClaimKind::Reported => "reported",
            super::ClaimKind::Observation => "observation",
            super::ClaimKind::Hypothesis => "hypothesis",
            super::ClaimKind::Preference => "preference",
        })?;
        self.text(match claim.polarity {
            super::Polarity::Positive => "positive",
            super::Polarity::Negative => "negative",
            super::Polarity::Uncertain => "uncertain",
        })?;
        self.node(&claim.attributed_author)?;
        self.scope(&claim.scope)?;
        self.facets(&claim.confidence)
    }

    fn assessment(&mut self, assessment: &AssessmentInput) -> Result<()> {
        self.start(RecordKind::Assessment, assessment.key.as_str())?;
        self.text(assessment.occurrence_key.as_str())?;
        self.reference(
            &assessment.target,
            &[RecordKind::Claim, RecordKind::Assessment],
        )?;
        self.frame.term(&assessment.conclusion)?;
        self.method(&assessment.method)?;
        self.digest(assessment.configuration_digest)?;
        self.frame.literal(&assessment.evaluated_at)?;
        self.node(&assessment.attributed_evaluator)?;
        self.confidence(assessment.confidence.as_ref(), false)?;
        self.scope(&assessment.scope)
    }
}

fn expected_digest_matches(expected: Option<Sha256Digest>, actual: Sha256Digest) -> Result<()> {
    if expected.is_some_and(|expected| expected != actual) {
        return Err(Error::invalid(
            "contentDigest does not match the retained bytes",
        ));
    }
    Ok(())
}

fn normalize_time(value: &Literal) -> Result<NormalizedSourceTime> {
    if value.datatype() != xsd::DATE_TIME {
        return Err(Error::invalid("source timestamp requires xsd:dateTime"));
    }
    let parsed: DateTime = value
        .value()
        .trim_matches([' ', '\t', '\r', '\n'])
        .parse()
        .map_err(|_| {
            Error::invalid("source timestamp is invalid or outside supported precision/range")
        })?;
    if parsed.timezone_offset().is_none() {
        return Err(Error::invalid(
            "source timestamp requires an explicit timezone",
        ));
    }
    let utc = parsed.adjust(Some(TimezoneOffset::UTC)).ok_or_else(|| {
        Error::invalid("source timestamp UTC conversion is outside supported range")
    })?;
    Ok(NormalizedSourceTime {
        original: value.clone(),
        utc: Literal::new_typed_literal(utc.to_string(), xsd::DATE_TIME),
    })
}

#[cfg(test)]
std::thread_local! {
    pub(super) static PASSAGE_CHARACTER_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn verify_local_structured(
    input: &EvidenceInput,
    groups: BTreeMap<usize, Vec<usize>>,
    control: &Control,
) -> Result<()> {
    for (revision, references) in groups {
        control.check()?;
        let SourceContent::Retained { bytes, .. } = &input.revisions[revision].content else {
            unreachable!("only retained local revisions are grouped");
        };
        let json = std::str::from_utf8(bytes)
            .map_err(|_| Error::invalid("JSON reference requires retained UTF-8"))?;
        let pointers: Vec<_> = references
            .iter()
            .map(|index| {
                let EvidenceRefTarget::Structured { pointer, .. } =
                    &input.references[*index].target
                else {
                    unreachable!("only structured references are grouped");
                };
                pointer.as_str()
            })
            .collect();
        let selected = super::structured::select_json_lexical_utf8_batch(json, &pointers, control)?;
        for (index, selected) in references.into_iter().zip(selected) {
            control.check()?;
            let EvidenceRefTarget::Structured { value_digest, .. } =
                &input.references[index].target
            else {
                unreachable!("only structured references are grouped");
            };
            if selected.digest != *value_digest {
                return Err(Error::invalid(
                    "span-mismatch: JSON selected-value digest differs",
                ));
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct PassageGroup {
    passages: Vec<usize>,
    offsets: Vec<u64>,
}

fn verify_local_passages(
    input: &EvidenceInput,
    groups: BTreeMap<usize, PassageGroup>,
    control: &Control,
) -> Result<()> {
    for (rendition, mut group) in groups {
        control.check()?;
        let RenditionContent::NormalizedText(text) = &input.renditions[rendition].content else {
            unreachable!("only normalized local renditions are grouped");
        };
        group.offsets.sort_unstable();
        group.offsets.dedup();
        let positions = locate_boundaries(text, &group.offsets, control)?;
        let mut digests = BTreeMap::new();
        for index in group.passages {
            control.check()?;
            let passage = &input.passages[index];
            let start = positions[group
                .offsets
                .binary_search(&passage.start)
                .expect("start was grouped")];
            let end = positions[group
                .offsets
                .binary_search(&passage.end)
                .expect("end was grouped")];
            let selected = &text[start..end];
            let digest = match digests.entry((start, end)) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    *entry.insert(Sha256Digest::of_bytes(selected.as_bytes(), control)?)
                }
            };
            if passage
                .quote
                .as_deref()
                .is_some_and(|quote| quote != selected)
                || digest != passage.text_digest
            {
                return Err(Error::invalid(
                    "span-mismatch: quote or text digest does not match",
                ));
            }
        }
    }
    Ok(())
}

fn locate_boundaries(text: &str, offsets: &[u64], control: &Control) -> Result<Vec<usize>> {
    let mut positions = Vec::with_capacity(offsets.len());
    let mut count = 0u64;
    for (byte, _) in text.char_indices() {
        #[cfg(test)]
        PASSAGE_CHARACTER_VISITS.with(|visits| visits.set(visits.get() + 1));
        if count.is_multiple_of(16_384) {
            control.check()?;
        }
        if offsets.get(positions.len()) == Some(&count) {
            positions.push(byte);
            if positions.len() == offsets.len() {
                return Ok(positions);
            }
        }
        count += 1;
    }
    if offsets.get(positions.len()) == Some(&count) {
        positions.push(text.len());
    }
    if positions.len() != offsets.len() {
        return Err(Error::invalid(
            "span-mismatch: selector exceeds normalized text",
        ));
    }
    control.check()?;
    Ok(positions)
}

fn has_blank(triple: &Triple) -> bool {
    if matches!(triple.subject, NamedOrBlankNode::BlankNode(_)) {
        return true;
    }
    let mut term = &triple.object;
    loop {
        match term {
            Term::BlankNode(_) => return true,
            Term::Triple(inner) => {
                if matches!(inner.subject, NamedOrBlankNode::BlankNode(_)) {
                    return true;
                }
                term = &inner.object;
            }
            _ => return false,
        }
    }
}
