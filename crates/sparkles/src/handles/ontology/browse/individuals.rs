//! Paged named individuals, grouped by asserted named class membership.
use super::*;
use sparkles_owl::{AxiomKind, EntityKind};

/// A page of type buckets, members of a type, or the memberships of one individual.
#[derive(Clone, Debug)]
pub struct IndividualsRequest {
    pub source_id: Option<String>,
    pub class: Option<String>,
    pub untyped: bool,
    pub iri: Option<String>,
    /// Exclusive lexical IRI continuation. The empty key identifies the Untyped bucket.
    pub after: Option<String>,
    pub limit: usize,
    pub languages: Vec<String>,
}

impl Default for IndividualsRequest {
    fn default() -> Self {
        Self {
            source_id: None,
            class: None,
            untyped: false,
            iri: None,
            after: None,
            limit: DEFAULT_LIMIT,
            languages: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct IndividualGroup {
    pub class: Option<Name>,
    /// Not counted for a direct membership lookup.
    pub total: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct IndividualNode {
    pub name: Name,
    pub types: Vec<Name>,
}

#[derive(Clone, Debug)]
pub struct Individuals {
    pub inference_source: Option<MaterializationSource>,
    pub commit: u64,
    pub groups: Vec<IndividualGroup>,
    pub individuals: Vec<IndividualNode>,
    pub total: usize,
    pub next: Option<String>,
    pub warnings: Vec<String>,
}

// Discovery and returned type lists fail visibly rather than silently dropping
// buckets or types. Typed pages apply MAX_TYPES only to the rows they return.
const MAX_GROUPS: usize = 100_000;
const MAX_TYPES: usize = 1000;

// Diagnostic accounting for the explicit large-fixture test. This counts only this
// module's visitor, not mapper reads, bounded name reads or Scope::quads probes.
#[cfg(test)]
thread_local! {
    static SCAN_ROWS: std::cell::RefCell<BTreeMap<String, usize>> = const { std::cell::RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
pub(super) fn take_scan_rows() -> BTreeMap<String, usize> {
    SCAN_ROWS.with(|rows| std::mem::take(&mut *rows.borrow_mut()))
}

fn keep_member(
    s: &Scope,
    id: Id,
    q: &IndividualsRequest,
    total: &mut usize,
    page: &mut BTreeMap<String, Id>,
) {
    *total += 1;
    let iri = s.iri(id);
    if q.after.as_ref().is_none_or(|after| iri > *after) {
        page.insert(iri, id);
        if page.len() > q.limit + 1 {
            page.pop_last();
        }
    }
}

impl Ontologies {
    pub fn individuals(
        &self,
        req: &OntologyRequest,
        q: &IndividualsRequest,
        opts: &QueryOptions,
    ) -> Result<Individuals> {
        if q.limit == 0 || q.limit > MAX_LIMIT {
            return Err(Error::invalid(format!(
                "limit must be an integer from 1 to {MAX_LIMIT}"
            )));
        }
        if usize::from(q.class.is_some()) + usize::from(q.untyped) + usize::from(q.iri.is_some())
            > 1
        {
            return Err(Error::invalid(
                "type, untyped and iri are mutually exclusive",
            ));
        }
        let mut s = self.browse_scope(
            req,
            &q.languages,
            opts,
            q.source_id.as_deref(),
            q.after.is_some(),
        )?;
        // Reuse the owning F21 mapper, including declarations, punning, complex
        // assertions and individuals used inside class expressions. This deliberately
        // retains the existing cold terminology-mapping cost, not a bounded store read.
        // Strict inferred reads already proved this complete graph scope and
        // producer. Map those exact immutable graphs, including inferred facts,
        // without re-entering generic freshness's uncontrolled historical diff.
        let verified = s.inference_source.as_ref().map(|_| OntologyRequest {
            graphs: s.refs.clone(),
            imports: sparkles_reasoner::inputs::ImportMode::None,
            inferences: false,
            ..req.clone()
        });
        let o = self.open_at(&s.snap, verified.as_ref().unwrap_or(req), &s.opts)?;
        let mut result = Individuals {
            inference_source: s.inference_source.clone(),
            commit: s.snap.commit,
            groups: Vec::new(),
            individuals: Vec::new(),
            total: 0,
            next: None,
            warnings: s.warnings.clone(),
        };
        if let Some(iri) = &q.iri {
            let id = s
                .lookup(iri)
                .ok_or_else(|| Error::NotFound(format!("no named individual <{iri}>")))?;
            if !s.is_individual(&o.ontology, id)? {
                return Err(Error::NotFound(format!("no named individual <{iri}>")));
            }
            let types = s.individual_types(&o.ontology, id)?;
            result.groups = types
                .iter()
                .map(|id| {
                    Ok(IndividualGroup {
                        class: Some(s.name(*id)?),
                        total: None,
                    })
                })
                .collect::<Result<_>>()?;
            if types.is_empty() {
                result.groups.push(IndividualGroup {
                    class: None,
                    total: None,
                });
            }
            result.individuals.push(s.individual_node(id, &types)?);
            result.total = 1;
            self.verify_browse_source(&s)?;
            return Ok(result);
        }
        let members = q.class.is_some() || q.untyped;
        let mut page = BTreeMap::<String, Id>::new();
        let mut groups = BTreeMap::<String, (Option<Id>, usize)>::new();
        if let Some(class) = &q.class {
            if let (Some(p), Some(t)) = (s.id(RDF_TYPE), s.lookup(class)) {
                // The mapper's named-class eligibility is constant for this POS
                // range. Use a real visible triple to preserve its strict/lenient
                // and reserved-vocabulary rules without duplicating that predicate.
                let mut class_eligible = None;
                let mut last = None;
                s.visit(Perm::Pos, &[p.0, t.0], |s, quad| {
                    if last == Some(quad[0]) {
                        return Ok(());
                    }
                    last = Some(quad[0]);
                    if s.snap.term_kind(quad[0]) != TermKind::Iri {
                        return Ok(());
                    }
                    let eligible = if let Some(eligible) = class_eligible {
                        eligible
                    } else {
                        let Some(triple) = triple_of(&s.snap, &quad) else {
                            return Ok(());
                        };
                        let eligible = matches!(
                            o.ontology.map_assertion(&triple),
                            Some(Axiom {
                                kind: AxiomKind::ClassAssertion(
                                    sparkles_owl::ClassExpression::Class(_),
                                    _
                                ),
                                ..
                            })
                        );
                        class_eligible = Some(eligible);
                        eligible
                    };
                    if eligible && s.is_individual(&o.ontology, quad[0])? {
                        keep_member(s, quad[0], q, &mut result.total, &mut page);
                    }
                    Ok(())
                })?;
            }
        } else {
            let mut visit = |s: &mut Scope, id: Id| -> Result<()> {
                let types = s.individual_types(&o.ontology, id)?;
                if q.untyped {
                    if types.is_empty() {
                        keep_member(s, id, q, &mut result.total, &mut page);
                    }
                } else {
                    let keys: Vec<(String, Option<Id>)> = if types.is_empty() {
                        vec![(String::new(), None)]
                    } else {
                        types.iter().map(|t| (s.iri(*t), Some(*t))).collect()
                    };
                    for (key, id) in keys {
                        groups.entry(key).or_insert((id, 0)).1 += 1;
                        if groups.len() > MAX_GROUPS {
                            return Err(Error::invalid(format!(
                                "individual type buckets exceed {MAX_GROUPS}"
                            )));
                        }
                    }
                }
                Ok(())
            };
            // Enumerate each named node once without retaining the whole signature:
            // subjects first, then object-only resources. Eligibility uses the same
            // mapper as F21's entity/accessor routes, including untyped assertions.
            for perm in [Perm::Spo, Perm::Osp] {
                let mut last = None;
                s.visit(perm, &[], |s, quad| {
                    let id = if perm == Perm::Spo { quad[0] } else { quad[2] };
                    if last == Some(id) {
                        return Ok(());
                    }
                    last = Some(id);
                    if s.snap.term_kind(id) != TermKind::Iri {
                        return Ok(());
                    }
                    if perm == Perm::Osp && !s.quads(Perm::Spo, &[id.0], 1)?.is_empty() {
                        return Ok(());
                    }
                    if s.is_individual(&o.ontology, id)? {
                        visit(s, id)?;
                    }
                    Ok(())
                })?;
            }
        }
        if members {
            if page.len() > q.limit {
                page.pop_last();
                result.next = page.last_key_value().map(|(iri, _)| iri.clone());
            }
            // The typed-page type cap applies to returned lists. Discarded
            // candidates contribute to the exact total without collecting their type lists.
            for (_, id) in page {
                let types = s.individual_types(&o.ontology, id)?;
                result.individuals.push(s.individual_node(id, &types)?);
            }
        } else {
            result.total = groups.len();
            let mut remaining = groups
                .into_iter()
                .filter(|(key, _)| q.after.as_ref().is_none_or(|after| key > after));
            let mut last = None;
            for (key, (id, total)) in remaining.by_ref().take(q.limit) {
                last = Some(key);
                result.groups.push(IndividualGroup {
                    class: id.map(|id| s.name(id)).transpose()?,
                    total: Some(total),
                });
            }
            if remaining.next().is_some() {
                result.next = last;
            }
        }
        self.verify_browse_source(&s)?;
        Ok(result)
    }
}

impl Scope {
    /// Visit visible quads without collecting the scanned range.
    pub(super) fn visit(
        &mut self,
        perm: Perm,
        prefix: &[u64],
        mut f: impl FnMut(&mut Self, [Id; 4]) -> Result<()>,
    ) -> Result<()> {
        let snap = self.snap.clone();
        let mut n = 0usize;
        check(&self.opts)?;
        snap.scan(perm, prefix, |chunk| {
            let mut each = |quad: [Id; 4]| -> Result<()> {
                n += 1;
                if n.is_multiple_of(1024) {
                    check(&self.opts)?;
                }
                if self.graphs.contains_key(&quad[3]) {
                    f(self, quad)?;
                }
                Ok(())
            };
            match chunk {
                Chunk::Block(b, start, end) => {
                    for i in start..end {
                        each(perm.to_quad(&b.key(i)))?;
                    }
                }
                Chunk::Row(key) => each(perm.to_quad(&key))?,
            }
            Ok(true)
        })?;
        #[cfg(test)]
        SCAN_ROWS.with(|rows| {
            *rows
                .borrow_mut()
                .entry(format!("{perm:?}/{}", prefix.len()))
                .or_default() += n;
        });
        check(&self.opts)
    }

    fn is_individual(&mut self, o: &read::Ontology, id: Id) -> Result<bool> {
        let iri = self.iri(id);
        if o.typing.is(&iri, EntityKind::NamedIndividual) {
            return Ok(true);
        }
        // An individual used only in a streamed assertion need not be in the mapped
        // terminology's signature. Check both ends of property/equality assertions.
        let mut found = false;
        for perm in [Perm::Spo, Perm::Osp] {
            self.visit(perm, &[id.0], |s, quad| {
                if found {
                    return Ok(());
                }
                let Some(triple) = triple_of(&s.snap, &quad) else {
                    return Ok(());
                };
                if let Some(ax) = o.map_assertion(&triple) {
                    found = ax.signature().iter().any(|e| {
                        e.kind() == EntityKind::NamedIndividual && e.iri().as_str() == iri
                    });
                }
                Ok(())
            })?;
            if found {
                break;
            }
        }
        Ok(found)
    }

    fn individual_types(&mut self, o: &read::Ontology, id: Id) -> Result<Vec<Id>> {
        let mut types = BTreeSet::new();
        if let Some(p) = self.id(RDF_TYPE) {
            self.visit(Perm::Spo, &[id.0, p.0], |s, quad| {
                if let Some(triple) = triple_of(&s.snap, &quad)
                    && let Some(Axiom {
                        kind: AxiomKind::ClassAssertion(sparkles_owl::ClassExpression::Class(_), _),
                        ..
                    }) = o.map_assertion(&triple)
                {
                    types.insert(quad[2]);
                    if types.len() > MAX_TYPES {
                        return Err(Error::invalid(format!(
                            "individual has more than {MAX_TYPES} named types"
                        )));
                    }
                }
                Ok(())
            })?;
        }
        Ok(types.into_iter().collect())
    }

    fn individual_node(&mut self, id: Id, types: &[Id]) -> Result<IndividualNode> {
        Ok(IndividualNode {
            name: self.name(id)?,
            types: types
                .iter()
                .map(|id| self.name(*id))
                .collect::<Result<_>>()?,
        })
    }
}
