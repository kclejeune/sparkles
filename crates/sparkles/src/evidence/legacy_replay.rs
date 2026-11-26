//! Receipt recovery in continuously protected graphs that also served as projection outputs.

use super::{
    Batch, G, MAX_EVIDENCE, ManagedWrite, NamedNode, PROV_USED, PROV_WAS_GENERATED_BY,
    PolicyGraphs, RDF_TYPE, Result, Settings, State, Term, Uuid, ambiguous_legacy_replay, ev,
    operation_id, operation_iri,
};
use crate::access::Reservation;
use oxrdf::{Literal, vocab::xsd};
use sparkles_core::evidence::write::{RecordedOperation, StoredQuad};

const ASSOCIATED: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
const ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
const ENDED: &str = "http://www.w3.org/ns/prov#endedAtTime";

/// Only quads without a live support can describe the original publication.
struct Description {
    quads: Vec<StoredQuad>,
    _held: Reservation,
}

impl Description {
    fn objects(&self, predicate: &str) -> impl Iterator<Item = &Term> {
        self.quads
            .iter()
            .filter_map(move |(_, t)| (t.predicate.as_str() == predicate).then_some(&t.object))
    }

    fn single(&self, predicate: &str) -> Result<Option<&Term>> {
        let mut objects = self.objects(predicate);
        let first = objects.next();
        if objects.next().is_some() {
            return Err(ambiguous_legacy_replay());
        }
        Ok(first)
    }

    fn iri(&self, predicate: &str) -> Result<Option<&NamedNode>> {
        match self.single(predicate)? {
            Some(Term::NamedNode(iri)) => Ok(Some(iri)),
            None => Ok(None),
            _ => Err(ambiguous_legacy_replay()),
        }
    }

    fn literal(&self, predicate: &str, datatype: oxrdf::NamedNodeRef<'_>) -> Result<&Literal> {
        match self.single(predicate)? {
            Some(Term::Literal(l))
                if l.datatype() == datatype && sparkles_core::xsd::is_valid(l) =>
            {
                Ok(l)
            }
            _ => Err(ambiguous_legacy_replay()),
        }
    }

    fn parsed<T: std::str::FromStr>(
        &self,
        predicate: &str,
        datatype: oxrdf::NamedNodeRef<'_>,
    ) -> Result<T> {
        self.literal(predicate, datatype)?
            .value()
            .parse()
            .map_err(|_| ambiguous_legacy_replay())
    }

    fn typed(&self, class: &str) -> bool {
        self.objects(RDF_TYPE)
            .any(|o| matches!(o, Term::NamedNode(n) if n.as_str() == class))
    }

    fn operation(
        &self,
        dataset: Uuid,
        nonce: Uuid,
        principal: &NamedNode,
        key: &str,
    ) -> Result<RecordedOperation> {
        if !self.typed(&ev("Operation"))
            || !self.typed(ACTIVITY)
            || self.objects(RDF_TYPE).count() != 2
            || self.iri(ASSOCIATED)? != Some(principal)
            || self.literal(&ev("graphProtection"), xsd::STRING)?.value() != nonce.to_string()
            || self.literal(&ev("datasetId"), xsd::STRING)?.value() != dataset.to_string()
            || self.literal(&ev("idempotencyKey"), xsd::STRING)?.value() != key
        {
            return Err(ambiguous_legacy_replay());
        }
        if self.single(&ev("sourceHold"))?.is_some() {
            self.parsed::<Uuid>(&ev("sourceHold"), xsd::STRING)?;
        }
        Ok(RecordedOperation {
            authenticated: true,
            ended_at: Some(self.literal(ENDED, xsd::DATE_TIME)?.clone()),
            branch: Some(self.parsed(&ev("branchId"), xsd::STRING)?),
            commit: Some(self.parsed(&ev("commit"), xsd::INTEGER)?),
            payload_digest: Some(self.parsed(&ev("payloadDigest"), xsd::STRING)?),
        })
    }
}

pub(super) struct LegacyPublisher {
    pub(super) graph: G,
    owner: Option<PolicyGraphs>,
    pub(super) used: Option<NamedNode>,
}

impl State<'_> {
    fn legacy_description(
        &mut self,
        subject: &NamedNode,
        graph: &G,
        owner: Option<&PolicyGraphs>,
    ) -> Result<Description> {
        let mut held = self.reads.preparation().reserve(0)?;
        let (mut quads, more) = self.reads.subject_quads_up_to(
            subject.as_str(),
            graph.0.as_str(),
            MAX_EVIDENCE,
            &mut held,
        )?;
        if more {
            return Err(ambiguous_legacy_replay());
        }
        if let Some(owner) = owner {
            let mut index = 0;
            while index < quads.len() {
                if self.owned(&quads[index].1, owner)? {
                    quads.swap_remove(index);
                } else {
                    index += 1;
                }
            }
        }
        Ok(Description { quads, _held: held })
    }

    pub(super) fn legacy_publisher(
        &mut self,
        graphs: &[G],
        settings: &Settings,
        capability: &ManagedWrite,
        dataset: Uuid,
        probe: &Batch,
        key: &str,
    ) -> Result<Option<(RecordedOperation, LegacyPublisher)>> {
        let mut found = None;
        for graph in graphs {
            let Some(nonce) = graph.1 else { continue };
            let mut held = self.reads.preparation().reserve(0)?;
            // The named IRI needs no surviving type or nonce. At other IRIs
            // the key must be on a protected operation, since any record can
            // acquire an idempotencyKey triple when this graph is an output.
            let (keyed, more) = self.reads.subjects_matching_up_to_held(
                &[
                    (RDF_TYPE, &NamedNode::new_unchecked(ev("Operation")).into()),
                    (
                        &ev("idempotencyKey"),
                        &Literal::new_simple_literal(key).into(),
                    ),
                    (
                        &ev("graphProtection"),
                        &Literal::new_simple_literal(nonce.to_string()).into(),
                    ),
                ],
                graph.0.as_str(),
                MAX_EVIDENCE,
                &mut held,
            )?;
            let named =
                self.reads
                    .subject_triples(probe.operation_iri.as_str(), graph.0.as_str(), 1)?
                    != 0;
            if (!named && keyed.is_empty())
                || !self
                    .reads
                    .has_published_operation_in_output(graph.0.as_str(), graph.1)?
            {
                continue;
            }
            if more {
                return Err(ambiguous_legacy_replay());
            }
            let owner = settings
                .policies
                .iter()
                .find(|(_, policy)| policy.output == graph.0.as_str())
                .map(|(name, policy)| PolicyGraphs::new(name, policy, capability))
                .transpose()?;
            for other in keyed.iter().filter(|iri| *iri != &probe.operation_iri) {
                // A key can legitimately occur under another principal. Its unowned
                // description must account for that subject's deterministic identity.
                let description = self.legacy_description(other, graph, owner.as_ref())?;
                let principal = description
                    .iri(ASSOCIATED)?
                    .ok_or_else(ambiguous_legacy_replay)?;
                let stored_key = description
                    .literal(&ev("idempotencyKey"), xsd::STRING)?
                    .value();
                if operation_iri(operation_id(dataset, principal, stored_key)) != *other {
                    return Err(ambiguous_legacy_replay());
                }
            }
            if named {
                if found.is_some() {
                    return Err(ambiguous_legacy_replay());
                }
                let description =
                    self.legacy_description(&probe.operation_iri, graph, owner.as_ref())?;
                let operation = description.operation(dataset, nonce, &probe.principal, key)?;
                let used = description.iri(PROV_USED)?.cloned();
                found = Some((
                    operation,
                    LegacyPublisher {
                        graph: graph.clone(),
                        owner,
                        used,
                    },
                ));
            }
        }
        Ok(found)
    }

    pub(super) fn legacy_projection_matches(
        &mut self,
        projection: &NamedNode,
        policy: &NamedNode,
        publisher: &LegacyPublisher,
        predicate: &str,
        operation: &NamedNode,
    ) -> Result<bool> {
        let graph = &publisher.graph;
        let owner = publisher.owner.as_ref();
        let description = self.legacy_description(projection, graph, owner)?;
        let nonce = graph.1.ok_or_else(ambiguous_legacy_replay)?;
        let marker = Term::from(Literal::new_simple_literal(nonce.to_string()));
        if !description.typed(&ev("Projection"))
            || description.iri(&ev("reviewPolicy"))? != Some(policy)
            || description.single(&ev("graphProtection"))? != Some(&marker)
        {
            return Ok(false);
        }
        let Some(op) = description.iri(&ev("availabilityReceipt"))? else {
            return Ok(false);
        };
        // The reuse target came from the filtered operation description. For both
        // inverse lookups the projection must retain its own unowned association.
        if predicate != PROV_USED
            && (op != operation
                || (predicate == PROV_WAS_GENERATED_BY
                    && !description
                        .objects(predicate)
                        .any(|value| matches!(value, Term::NamedNode(n) if n == operation))))
        {
            return Ok(false);
        }
        let activity = self.legacy_description(op, graph, owner)?;
        Ok(activity.typed(&ev("Operation"))
            && activity.single(&ev("graphProtection"))? == Some(&marker))
    }

    pub(super) fn legacy_projection_metadata(
        &mut self,
        projection: &NamedNode,
        publisher: &LegacyPublisher,
    ) -> Result<(u32, NamedNode)> {
        let description =
            self.legacy_description(projection, &publisher.graph, publisher.owner.as_ref())?;
        let version = description.parsed(&ev("policyVersion"), xsd::STRING)?;
        let output = description
            .iri(&ev("output"))?
            .ok_or_else(ambiguous_legacy_replay)?;
        Ok((version, output.clone()))
    }
}
