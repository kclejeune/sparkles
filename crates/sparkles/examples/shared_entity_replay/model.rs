//! Typed comparison values supplied by the future verified-input resolution layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Comparison {
    pub profile_version: u32,
    pub scope: String,
    pub input_ids: Vec<String>,
    pub assignments: Vec<Assignment>,
    pub conflicts: Vec<Conflict>,
    pub unknowns: Vec<Unknown>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Resolved,
    Unresolved,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assignment {
    pub entity: String,
    pub status: Status,
    pub representative: Option<String>,
    pub decisions: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConflictCode {
    Decision,
    Distinct,
    Kind,
}
impl ConflictCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision-conflict",
            Self::Distinct => "distinct-conflict",
            Self::Kind => "kind-conflict",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
    pub code: ConflictCode,
    pub entities: Vec<String>,
    pub decisions: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UnknownCode {
    PossibleMatch,
    UnavailableInput,
}
impl UnknownCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PossibleMatch => "possible-match",
            Self::UnavailableInput => "unavailable-input",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unknown {
    pub record: String,
    pub code: UnknownCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    UnsupportedProfile,
    UnsupportedExtension,
    UnknownControlField,
    ImmutableRecordConflict,
    SourceDigestMismatch,
    ScopeMismatch,
    DecisionCycle,
    IncompleteInput,
    InvalidDomainRecord,
    UnresolvedReference,
    ReplayLimit,
    ReplayCancelled,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsupportedProfile => "unsupported-profile",
            Self::UnsupportedExtension => "unsupported-extension",
            Self::UnknownControlField => "unknown-control-field",
            Self::ImmutableRecordConflict => "immutable-record-conflict",
            Self::SourceDigestMismatch => "source-digest-mismatch",
            Self::ScopeMismatch => "scope-mismatch",
            Self::DecisionCycle => "decision-cycle",
            Self::IncompleteInput => "incomplete-input",
            Self::InvalidDomainRecord => "invalid-domain-record",
            Self::UnresolvedReference => "unresolved-reference",
            Self::ReplayLimit => "replay-limit",
            Self::ReplayCancelled => "replay-cancelled",
        }
    }
}

/// A lower output cap for the private encoder, not the complete operation-memory budget.
#[derive(Clone, Copy)]
pub(crate) struct OutputLimit(pub usize);
pub(crate) const MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

pub(crate) const SR: &str = "urn:x-sparkles:representation:";
pub(crate) const EV: &str = "urn:x-sparkles:evidence:";
pub(crate) const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub(crate) const PROV_REVISION: &str = "http://www.w3.org/ns/prov#wasRevisionOf";
pub(crate) type ReplayResult<T> = std::result::Result<T, Failure>;

/// Caller caps can only lower the reference-tool limits. No engine limit changes.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub decoded: usize,
    pub memory: usize,
    pub source: usize,
    pub entities: usize,
    pub descriptions: usize,
    pub claims: usize,
    pub decisions: usize,
    pub relations: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            decoded: 64 << 20,
            memory: 256 << 20,
            source: 16 << 20,
            entities: 10_000,
            descriptions: 20_000,
            claims: 50_000,
            decisions: 20_000,
            relations: 50_000,
        }
    }
}

pub(crate) struct Operation<'a> {
    pub control: &'a sparkles::task::Control,
    pub limits: Limits,
    used: std::rc::Rc<std::cell::Cell<usize>>,
}
pub(crate) struct Charge {
    used: std::rc::Rc<std::cell::Cell<usize>>,
    bytes: usize,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.used.set(self.used.get() - self.bytes);
    }
}
impl<'a> Operation<'a> {
    pub fn new(control: &'a sparkles::task::Control, limits: Limits) -> ReplayResult<Self> {
        let max = Limits::default();
        if limits.decoded > max.decoded
            || limits.memory > max.memory
            || limits.source > max.source
            || limits.entities > max.entities
            || limits.descriptions > max.descriptions
            || limits.claims > max.claims
            || limits.decisions > max.decisions
            || limits.relations > max.relations
        {
            return Err(Failure::ReplayLimit);
        }
        let result = Self {
            control,
            limits,
            used: Default::default(),
        };
        result.check()?;
        Ok(result)
    }
    pub fn check(&self) -> ReplayResult<()> {
        self.control.check().map_err(|_| Failure::ReplayCancelled)
    }
    pub fn charge(&self, bytes: usize) -> ReplayResult<Charge> {
        self.check()?;
        let next = self
            .used
            .get()
            .checked_add(bytes)
            .ok_or(Failure::ReplayLimit)?;
        if next > self.limits.memory {
            return Err(Failure::ReplayLimit);
        }
        self.used.set(next);
        Ok(Charge {
            used: self.used.clone(),
            bytes,
        })
    }
    pub fn used(&self) -> usize {
        self.used.get()
    }
}

/// A complete supplied delivery, not an authenticated C25 transaction. Named
/// subject IDs identify records; named/default graph coordinates remain content.
#[derive(Default)]
pub(crate) struct Graph {
    pub records: std::collections::BTreeMap<String, std::collections::HashSet<Statement>>,
    // Backing drops before its accounting owners, including on parser refusal.
    charges: Vec<Charge>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Statement {
    pub graph: String,
    pub predicate: String,
    pub object: oxrdf::Term,
}
impl Graph {
    pub fn load_documents(
        documents: &[(&[u8], oxrdfio::RdfFormat)],
        operation: &Operation<'_>,
    ) -> ReplayResult<Self> {
        operation.check()?;
        let total = documents.iter().try_fold(0usize, |sum, (bytes, _)| {
            sum.checked_add(bytes.len()).ok_or(Failure::ReplayLimit)
        })?;
        if total > operation.limits.decoded {
            return Err(Failure::ReplayLimit);
        }
        let mut result = Self::default();
        result
            .charges
            .push(operation.charge(total.checked_add(256).ok_or(Failure::ReplayLimit)?)?);
        for (bytes, format) in documents {
            operation.check()?;
            // Charge parser/transient owned terms before the parser can allocate.
            let _parser = operation.charge(
                bytes
                    .len()
                    .checked_mul(3)
                    .and_then(|n| n.checked_add(4096))
                    .ok_or(Failure::ReplayLimit)?,
            )?;
            for quad in oxrdfio::RdfParser::from_format(*format).for_slice(bytes) {
                operation.check()?;
                let quad = quad.map_err(|_| Failure::InvalidDomainRecord)?;
                let subject = quad.subject.to_string();
                let subject = subject
                    .strip_prefix('<')
                    .and_then(|s| s.strip_suffix('>'))
                    .ok_or(Failure::InvalidDomainRecord)?;
                if matches!(quad.object, oxrdf::Term::BlankNode(_)) {
                    return Err(Failure::InvalidDomainRecord);
                }
                let graph = quad.graph_name.to_string();
                if graph.starts_with("_:") {
                    return Err(Failure::InvalidDomainRecord);
                }
                let predicate = quad.predicate.as_str();
                let estimate = subject
                    .len()
                    .checked_add(graph.len())
                    .and_then(|n| n.checked_add(predicate.len()))
                    .and_then(|n| n.checked_add(quad.object.to_string().len()))
                    .and_then(|n| n.checked_mul(4))
                    .and_then(|n| n.checked_add(512))
                    .ok_or(Failure::ReplayLimit)?;
                let charge = operation.charge(estimate)?;
                let statement = Statement {
                    graph,
                    predicate: predicate.to_owned(),
                    object: quad.object,
                };
                if result
                    .records
                    .get(subject)
                    .is_some_and(|record| record.contains(&statement))
                {
                    continue;
                }
                result
                    .records
                    .entry(subject.to_owned())
                    .or_default()
                    .insert(statement);
                result.charges.push(charge);
            }
        }
        result.check_counts(operation)?;
        Ok(result)
    }
    fn check_counts(&self, operation: &Operation<'_>) -> ReplayResult<()> {
        for (kind, cap) in [
            (format!("{SR}Entity"), operation.limits.entities),
            (format!("{SR}Description"), operation.limits.descriptions),
            (format!("{EV}Claim"), operation.limits.claims),
            (format!("{EV}ReviewDecision"), operation.limits.decisions),
            (format!("{SR}Relation"), operation.limits.relations),
        ] {
            let mut count = 0usize;
            for record in self.records.values() {
                operation.check()?;
                if record.iter().any(|s| {
                    s.predicate == RDF_TYPE
                        && matches!(&s.object, oxrdf::Term::NamedNode(n) if n.as_str() == kind)
                }) {
                    count += 1;
                    if count > cap {
                        return Err(Failure::ReplayLimit);
                    }
                }
            }
        }
        Ok(())
    }
    /// Complete record delivery is atomic: validate every existing ID before
    /// transferring any new record, so a contradictory re-delivery changes none.
    pub fn merge_delivery(
        &mut self,
        delivery: Self,
        operation: &Operation<'_>,
    ) -> ReplayResult<()> {
        for (id, record) in &delivery.records {
            operation.check()?;
            if self.records.get(id).is_some_and(|old| old != record) {
                return Err(Failure::ImmutableRecordConflict);
            }
        }
        if delivery
            .records
            .keys()
            .all(|id| self.records.contains_key(id))
        {
            return Ok(()); // Drop duplicate delivery backing and all its debits.
        }
        // The result count cap is checked before mutating this registry.
        let count = |kind: &str| {
            self.records
                .iter()
                .chain(
                    delivery
                        .records
                        .iter()
                        .filter(|(id, _)| !self.records.contains_key(*id)),
                )
                .filter(|(_, record)| {
                    record.iter().any(|s| {
                        s.predicate == RDF_TYPE
                            && matches!(&s.object, oxrdf::Term::NamedNode(n) if n.as_str() == kind)
                    })
                })
                .count()
        };
        for (kind, cap) in [
            (format!("{SR}Entity"), operation.limits.entities),
            (format!("{SR}Description"), operation.limits.descriptions),
            (format!("{EV}Claim"), operation.limits.claims),
            (format!("{EV}ReviewDecision"), operation.limits.decisions),
            (format!("{SR}Relation"), operation.limits.relations),
        ] {
            if count(&kind) > cap {
                return Err(Failure::ReplayLimit);
            }
        }
        let Self {
            records,
            mut charges,
        } = delivery;
        for (id, record) in records {
            self.records.entry(id).or_insert(record);
        }
        self.charges.append(&mut charges);
        Ok(())
    }
    pub fn objects<'a>(
        &'a self,
        id: &str,
        predicate: &str,
    ) -> std::collections::HashSet<&'a oxrdf::Term> {
        self.records
            .get(id)
            .into_iter()
            .flatten()
            .filter(|s| s.predicate == predicate)
            .map(|s| &s.object)
            .collect()
    }
    pub fn iris(&self, id: &str, predicate: &str) -> ReplayResult<Vec<String>> {
        let mut values = self
            .objects(id, predicate)
            .into_iter()
            .map(|value| match value {
                oxrdf::Term::NamedNode(value) => Ok(value.as_str().to_owned()),
                _ => Err(Failure::InvalidDomainRecord),
            })
            .collect::<ReplayResult<Vec<_>>>()?;
        values.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        Ok(values)
    }
    pub fn one_iri(&self, id: &str, predicate: &str) -> ReplayResult<String> {
        let values = self.iris(id, predicate)?;
        if values.len() != 1 {
            return Err(Failure::InvalidDomainRecord);
        }
        Ok(values.into_iter().next().unwrap())
    }
    pub fn literal(&self, id: &str, predicate: &str) -> ReplayResult<String> {
        let values = self.objects(id, predicate);
        if values.len() != 1 {
            return Err(Failure::InvalidDomainRecord);
        }
        match values.into_iter().next().unwrap() {
            oxrdf::Term::Literal(value) => Ok(value.value().to_owned()),
            _ => Err(Failure::InvalidDomainRecord),
        }
    }
    pub fn has_type(&self, id: &str, kind: &str) -> bool {
        self.objects(id, RDF_TYPE)
            .into_iter()
            .any(|v| matches!(v, oxrdf::Term::NamedNode(n) if n.as_str() == kind))
    }
}

/// Charges live as long as the computed owned values; no partial value escapes
/// a failed replay. The caller may then encode through the fixed byte contract.
pub(crate) struct ComputedComparison {
    pub comparison: Comparison,
    pub charges: Vec<Charge>,
}

impl Graph {
    /// Version-1 typed input checks. This is not a general SHACL implementation;
    /// the separately shipped shapes remain the normative profile resource.
    pub fn validate_domain(&self, operation: &Operation<'_>) -> ReplayResult<()> {
        for id in self.records.keys() {
            operation.check()?;
            let types = self.iris(id, RDF_TYPE)?;
            for kind in types.iter().filter_map(|kind| kind.strip_prefix(SR)) {
                // Output rows are never inputs to matching. They are validated by
                // Phase1's shapes and removed by the independent replay tests.
                if matches!(kind, "Assignment" | "Conflict") {
                    continue;
                }
                self.validate_control(id, kind, operation)?;
            }
        }
        Ok(())
    }
    pub fn validate_control(
        &self,
        id: &str,
        kind: &str,
        operation: &Operation<'_>,
    ) -> ReplayResult<()> {
        operation.check()?;
        let mut allowed = vec!["profileVersion", "extension"];
        let mut required = Vec::new();
        let mut optional = Vec::new();
        let mut sets = Vec::new();
        let mut literals = Vec::new();
        let mut integers = Vec::new();
        match kind {
            "Entity" => {
                required.extend(["domainScope", "entityKind"]);
                let entity_kind = self.one_iri(id, &format!("{SR}entityKind"))?;
                match entity_kind.strip_prefix(SR) {
                    Some(
                        "Person" | "Account" | "Organization" | "LegalEntity" | "EventEpisode"
                        | "PackageFamily" | "PackageVersion" | "SoftwareRelease" | "Build"
                        | "Artifact" | "Deployment" | "DeviceModel" | "DeviceInstance" | "Disease",
                    ) => {
                        optional.extend([
                            "digestAlgorithm",
                            "verificationStatus",
                            "verificationMethod",
                        ]);
                        literals.push("digestValue");
                        let paired = self.objects(id, &format!("{SR}digestAlgorithm")).is_empty()
                            == self.objects(id, &format!("{SR}digestValue")).is_empty();
                        if !paired {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        let status = self.iris(id, &format!("{SR}verificationStatus"))?;
                        if status.iter().any(|s| {
                            s != &format!("{SR}Verified")
                                && s != &format!("{SR}UnknownVerification")
                        }) {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        let has_digest = !self.objects(id, &format!("{SR}digestValue")).is_empty();
                        let has_method = !self
                            .objects(id, &format!("{SR}verificationMethod"))
                            .is_empty();
                        if (has_digest || has_method || !status.is_empty())
                            && entity_kind != format!("{SR}Artifact")
                        {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        if status.iter().any(|s| s == &format!("{SR}Verified"))
                            && (!has_digest || !has_method)
                        {
                            return Err(Failure::InvalidDomainRecord);
                        }
                    }
                    Some("Place") => required.extend([
                        "administrativeLevel",
                        "geometry",
                        "geometryVersion",
                        "spatialUncertainty",
                    ]),
                    Some("CountObservation") => {
                        required.extend([
                            "measure",
                            "caseDefinition",
                            "population",
                            "geometryVersion",
                            "reportingAuthority",
                            "place",
                            "observedDuring",
                            "countMode",
                        ]);
                        optional.extend(["valueStatus", "denominator", "denominatorStatus"]);
                        literals.push("uncertaintyExpression");
                        integers.extend(["count", "lowerCount", "upperCount"]);
                        let mode = self.one_iri(id, &format!("{SR}countMode"))?;
                        if !matches!(
                            mode.as_str(),
                            "urn:x-sparkles:representation:Incident"
                                | "urn:x-sparkles:representation:Cumulative"
                        ) {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        let count = self.objects(id, &format!("{SR}count"));
                        let unknown = self.iris(id, &format!("{SR}valueStatus"))?;
                        if count.is_empty() == unknown.is_empty()
                            || unknown.iter().any(|v| v != &format!("{SR}UnknownValue"))
                        {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        let denominator = self.iris(id, &format!("{SR}denominator"))?;
                        let unknown_denominator =
                            self.iris(id, &format!("{SR}denominatorStatus"))?;
                        if denominator.is_empty() == unknown_denominator.is_empty()
                            || unknown_denominator
                                .iter()
                                .any(|v| v != &format!("{SR}UnknownDenominator"))
                        {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        let lower = self.optional_integer(id, "lowerCount")?;
                        let upper = self.optional_integer(id, "upperCount")?;
                        if lower.is_some() != upper.is_some() {
                            return Err(Failure::InvalidDomainRecord);
                        }
                        if let (Some(lower), Some(upper)) = (&lower, &upper) {
                            if integer_cmp(lower, upper).is_gt() {
                                return Err(Failure::InvalidDomainRecord);
                            }
                            if let Some(value) = self.optional_integer(id, "count")?
                                && (integer_cmp(&value, lower).is_lt()
                                    || integer_cmp(&value, upper).is_gt())
                            {
                                return Err(Failure::InvalidDomainRecord);
                            }
                        }
                        let place = self.one_iri(id, &format!("{SR}place"))?;
                        if self.one_iri(&place, &format!("{SR}entityKind"))? != format!("{SR}Place")
                        {
                            return Err(Failure::InvalidDomainRecord);
                        }
                    }
                    _ => return Err(Failure::InvalidDomainRecord),
                }
            }
            "Description" => {
                required.extend(["domainScope", "sourceRevision", "selection"]);
                optional.push("describes");
                let selection = self.one_iri(id, &format!("{SR}selection"))?;
                if self.one_iri(&selection, &format!("{EV}sourceRevision"))?
                    != self.one_iri(id, &format!("{SR}sourceRevision"))?
                {
                    return Err(Failure::InvalidDomainRecord);
                }
            }
            "Identifier" => {
                required.extend(["authority", "realm", "identifierScope"]);
                literals.push("lexicalValue");
                self.literal_typed(id, "lexicalValue", "string", true)?;
            }
            "Identification" => {
                required.extend(["domainScope", "identifier", "entity"]);
                sets.push("claim");
            }
            "Relation" => {
                required.extend(["domainScope", "subject", "object", "relationKind"]);
                sets.extend(["claim", "roleAssignment"]);
                self.validate_relation(id)?;
            }
            "RoleAssignment" => required.extend(["participant", "role"]),
            "ResolutionView" | "Projection" => {
                required.extend([
                    "domainScope",
                    "matchingPolicy",
                    "ontologyVersion",
                    "mappingVersion",
                    "normalizationVersion",
                    "recordedView",
                    "inputManifest",
                ]);
                if kind == "ResolutionView" {
                    sets.extend(["proposal", "decision"]);
                } else {
                    required.extend(["resolutionView", "projectionKind"]);
                    sets.push("claim");
                    if self.one_iri(id, &format!("{SR}projectionKind"))?
                        != format!("{SR}OfflineExpected")
                    {
                        return Err(Failure::InvalidDomainRecord);
                    }
                }
            }
            "Migration" => {
                required.extend(["fromVersion", "toVersion", "method", "inputManifest"]);
                sets.push("generatedRecord");
            }
            "Denominator" => {
                required.extend(["unit", "population", "observedDuring"]);
                literals.push("denominatorValue");
                self.literal_typed(id, "denominatorValue", "decimal", true)?;
            }
            "Extension" => {
                required.extend(["extensionProfile", "status"]);
                literals.push("payloadDigest");
                self.digest_literal(id, "payloadDigest")?;
                if self.one_iri(id, &format!("{SR}status"))? != format!("{SR}Uninterpreted") {
                    return Err(Failure::UnsupportedExtension);
                }
            }
            "InputManifest" => {
                sets.extend(["inputId", "requiredExtension"]);
                literals.push("contentDigest");
                self.digest_literal(id, "contentDigest")?;
                if !self.iris(id, &format!("{SR}requiredExtension"))?.is_empty() {
                    return Err(Failure::UnsupportedExtension);
                }
                if self
                    .iris(id, &format!("{SR}inputId"))?
                    .iter()
                    .any(|v| v == id)
                {
                    return Err(Failure::InvalidDomainRecord);
                }
            }
            "Policy" => required.extend([
                "positiveMatchRule",
                "separationRule",
                "conflictRule",
                "kindRule",
                "representativeRule",
                "stringRule",
            ]),
            _ => return Err(Failure::UnsupportedProfile),
        }
        self.literal_typed(id, "profileVersion", "integer", true)?;
        if self
            .literal(id, &format!("{SR}profileVersion"))?
            .parse::<u32>()
            .ok()
            != Some(1)
        {
            return Err(Failure::UnsupportedProfile);
        }
        for name in &required {
            self.one_iri(id, &format!("{SR}{name}"))?;
        }
        for name in &optional {
            if self.iris(id, &format!("{SR}{name}"))?.len() > 1 {
                return Err(Failure::InvalidDomainRecord);
            }
        }
        for name in &sets {
            self.iris(id, &format!("{SR}{name}"))?;
        }
        for name in &literals {
            self.literal_typed(
                id,
                name,
                if *name == "denominatorValue" {
                    "decimal"
                } else {
                    "string"
                },
                false,
            )?;
        }
        for name in &integers {
            self.optional_integer(id, name)?;
        }
        for name in ["claim", "generatedRecord", "inputId"] {
            if sets.contains(&name)
                && (name != "claim" || matches!(kind, "Identification" | "Relation"))
                && self.iris(id, &format!("{SR}{name}"))?.is_empty()
            {
                return Err(Failure::InvalidDomainRecord);
            }
        }
        for extension in self.iris(id, &format!("{SR}extension"))? {
            if !self.has_type(&extension, &format!("{SR}Extension")) {
                return Err(Failure::UnresolvedReference);
            }
        }
        allowed.extend(required);
        allowed.extend(optional);
        allowed.extend(sets);
        allowed.extend(literals);
        allowed.extend(integers);
        for statement in self.records.get(id).ok_or(Failure::UnresolvedReference)? {
            operation.check()?;
            let predicate = statement.predicate.as_str();
            if predicate == RDF_TYPE || predicate == "http://www.w3.org/2000/01/rdf-schema#label" {
                continue;
            }
            if matches!(kind, "Identification" | "Relation")
                && matches!(
                    predicate,
                    "urn:x-sparkles:valid-time:validDuring"
                        | "urn:x-sparkles:valid-time:occurredDuring"
                )
            {
                continue;
            }
            if kind == "Projection" && predicate == PROV_REVISION {
                continue;
            }
            if !predicate
                .strip_prefix(SR)
                .is_some_and(|name| allowed.contains(&name))
            {
                return Err(Failure::UnknownControlField);
            }
        }
        Ok(())
    }
    fn literal_typed(
        &self,
        id: &str,
        local: &str,
        datatype: &str,
        required: bool,
    ) -> ReplayResult<()> {
        let values = self.objects(id, &format!("{SR}{local}"));
        if values.len() > 1 || (required && values.is_empty()) {
            return Err(Failure::InvalidDomainRecord);
        }
        for value in values {
            if !matches!(value, oxrdf::Term::Literal(l) if l.datatype().as_str() == format!("http://www.w3.org/2001/XMLSchema#{datatype}"))
            {
                return Err(Failure::InvalidDomainRecord);
            }
            if datatype == "decimal" {
                let oxrdf::Term::Literal(value) = value else {
                    unreachable!()
                };
                let value = value.value();
                let digits = value
                    .strip_prefix('+')
                    .or_else(|| value.strip_prefix('-'))
                    .unwrap_or(value);
                if digits.is_empty()
                    || digits.bytes().any(|v| !v.is_ascii_digit() && v != b'.')
                    || digits.bytes().filter(|v| *v == b'.').count() > 1
                    || !digits.bytes().any(|v| v.is_ascii_digit())
                    || (value.starts_with('-')
                        && digits.bytes().any(|v| v.is_ascii_digit() && v != b'0'))
                {
                    return Err(Failure::InvalidDomainRecord);
                }
            }
        }
        Ok(())
    }
    pub fn optional_integer(&self, id: &str, local: &str) -> ReplayResult<Option<String>> {
        self.literal_typed(id, local, "integer", false)?;
        let values = self.objects(id, &format!("{SR}{local}"));
        let Some(oxrdf::Term::Literal(value)) = values.into_iter().next() else {
            return Ok(None);
        };
        let value = value.value();
        let digits = value
            .strip_prefix('+')
            .or_else(|| value.strip_prefix('-'))
            .unwrap_or(value);
        if digits.is_empty()
            || !digits.bytes().all(|v| v.is_ascii_digit())
            || (value.starts_with('-') && digits.bytes().any(|v| v != b'0'))
        {
            return Err(Failure::InvalidDomainRecord);
        }
        let normalized = digits.trim_start_matches('0');
        Ok(Some(
            if normalized.is_empty() {
                "0"
            } else {
                normalized
            }
            .to_owned(),
        ))
    }
    pub fn digest_literal(&self, id: &str, local: &str) -> ReplayResult<String> {
        self.literal_typed(id, local, "string", true)?;
        let value = self.literal(id, &format!("{SR}{local}"))?;
        value
            .parse::<sparkles_core::evidence::Sha256Digest>()
            .map_err(|_| Failure::InvalidDomainRecord)?;
        Ok(value)
    }
    fn validate_relation(&self, id: &str) -> ReplayResult<()> {
        let subject = self.one_iri(id, &format!("{SR}subject"))?;
        let object = self.one_iri(id, &format!("{SR}object"))?;
        let kind = self.one_iri(id, &format!("{SR}relationKind"))?;
        let object_kind = self.one_iri(&object, &format!("{SR}entityKind"))?;
        let subject_kind = if self.has_type(&subject, &format!("{SR}Description")) {
            format!("{SR}Description")
        } else {
            self.one_iri(&subject, &format!("{SR}entityKind"))?
        };
        let sk = subject_kind.strip_prefix(SR).unwrap_or("");
        let ok = object_kind.strip_prefix(SR).unwrap_or("");
        let valid = match kind.strip_prefix(SR) {
            Some("ControlsAccount") => sk == "Person" && ok == "Account",
            Some("RealizesOrganization") => sk == "LegalEntity" && ok == "Organization",
            Some("AccountingParentOf") => sk == "LegalEntity" && ok == "LegalEntity",
            Some("LocatedAt") => {
                matches!(
                    sk,
                    "Organization" | "LegalEntity" | "Deployment" | "DeviceInstance"
                ) && ok == "Place"
            }
            Some("VersionOf") => sk == "PackageVersion" && ok == "PackageFamily",
            Some("ReleaseContains") => sk == "SoftwareRelease" && ok == "PackageVersion",
            Some("BuildProduces") => sk == "Build" && ok == "Artifact",
            Some("DeploymentUses") => sk == "Deployment" && ok == "Artifact",
            Some("InstanceOfModel") => sk == "DeviceInstance" && ok == "DeviceModel",
            Some("ReportAboutEpisode") => sk == "Description" && ok == "EventEpisode",
            Some("ObservationAboutEpisode") => sk == "CountObservation" && ok == "EventEpisode",
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Failure::InvalidDomainRecord)
        }
    }
}
fn integer_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

impl Graph {
    pub fn verify_manifest(&self, id: &str, operation: &Operation<'_>) -> ReplayResult<()> {
        self.validate_control(id, "InputManifest", operation)?;
        let ids = self.iris(id, &format!("{SR}inputId"))?;
        let required = self.iris(id, &format!("{SR}requiredExtension"))?;
        let extensions = self.iris(id, &format!("{SR}extension"))?;
        let size = ids
            .iter()
            .chain(&required)
            .chain(&extensions)
            .try_fold(256usize, |n, id| {
                n.checked_add(
                    id.len()
                        .checked_mul(12)
                        .and_then(|n| n.checked_add(64))
                        .ok_or(Failure::ReplayLimit)?,
                )
                .ok_or(Failure::ReplayLimit)
            })?;
        let _encoding = operation.charge(size)?;
        let payload = format!(
            "{{\"profileVersion\":1,\"inputIds\":{},\"requiredExtensions\":{},\"extensionIds\":{}}}",
            serde_json::to_string(&ids).map_err(|_| Failure::InvalidDomainRecord)?,
            serde_json::to_string(&required).map_err(|_| Failure::InvalidDomainRecord)?,
            serde_json::to_string(&extensions).map_err(|_| Failure::InvalidDomainRecord)?
        );
        if payload.len() > operation.limits.source {
            return Err(Failure::ReplayLimit);
        }
        let digest =
            sparkles_core::evidence::Sha256Digest::of_bytes(payload.as_bytes(), operation.control)
                .map_err(|_| Failure::ReplayCancelled)?
                .to_string();
        if digest != self.digest_literal(id, "contentDigest")? {
            return Err(Failure::SourceDigestMismatch);
        }
        operation.check()
    }
}
