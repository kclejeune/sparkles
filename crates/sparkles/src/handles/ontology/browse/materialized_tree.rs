//! Direct, equivalence-group hierarchies over a recorded RDFS/OWL RL closure.
//! Range probes keep page memory bounded; counts and reduction can visit more rows
//! than the returned page and retain the request's cancellation/deadline.
use super::*;

struct Relation {
    up: Option<Id>,
    types: Vec<Id>,
    ignored: Vec<Id>,
}

impl Relation {
    fn typed(&self, s: &Scope, id: Id, rdf_type: Id) -> Result<bool> {
        for &kind in &self.types {
            if s.exists(&[id, rdf_type, kind])? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn canonical(&self, s: &mut Scope, id: Id, rdf_type: Id) -> Result<(Id, usize)> {
        let Some(up) = self.up else {
            return Ok((id, 1));
        };
        let mut best = (self.typed(s, id, rdf_type)? && !self.ignored.contains(&id)).then_some(id);
        let mut count = 1;
        let mut last = None;
        s.visit(Perm::Spo, &[id.0, up.0], |s, quad| {
            let other = quad[2];
            if last == Some(other) {
                return Ok(());
            }
            last = Some(other);
            if other != id
                && s.snap.term_kind(other) == TermKind::Iri
                && s.exists(&[other, up, id])?
            {
                count += 1;
                if !self.ignored.contains(&other)
                    && self.typed(s, other, rdf_type)?
                    && best.is_none_or(|old| s.iri(other) < s.iri(old))
                {
                    best = Some(other);
                }
            }
            Ok(())
        })?;
        Ok((best.unwrap_or(id), count))
    }

    fn direct(&self, s: &mut Scope, child: Id, parent: Id, rdf_type: Id) -> Result<bool> {
        let Some(up) = self.up else {
            return Ok(false);
        };
        if child == parent || s.exists(&[parent, up, child])? {
            return Ok(false);
        }
        let mut indirect = false;
        s.visit(Perm::Spo, &[child.0, up.0], |s, quad| {
            let middle = quad[2];
            if !indirect
                && middle != child
                && middle != parent
                && s.snap.term_kind(middle) == TermKind::Iri
                && !self.ignored.contains(&middle)
                && self.typed(s, middle, rdf_type)?
                && s.exists(&[middle, up, parent])?
                && !s.exists(&[middle, up, child])?
                && !s.exists(&[parent, up, middle])?
            {
                indirect = true;
            }
            Ok(())
        })?;
        Ok(!indirect)
    }

    fn relatives(
        &self,
        s: &mut Scope,
        id: Id,
        rdf_type: Id,
        parents: bool,
        mut emit: impl FnMut(&mut Scope, Id) -> Result<()>,
    ) -> Result<()> {
        let Some(up) = self.up else {
            return Ok(());
        };
        let (perm, prefix) = if parents {
            (Perm::Spo, [id.0, up.0])
        } else {
            (Perm::Pos, [up.0, id.0])
        };
        let mut last = None;
        s.visit(perm, &prefix, |s, quad| {
            let other = if parents { quad[2] } else { quad[0] };
            if last == Some(other) {
                return Ok(());
            }
            last = Some(other);
            if other == id
                || s.snap.term_kind(other) != TermKind::Iri
                || self.ignored.contains(&other)
                || !self.typed(s, other, rdf_type)?
                || self.canonical(s, other, rdf_type)?.0 != other
            {
                return Ok(());
            }
            let (child, parent) = if parents { (id, other) } else { (other, id) };
            if self.direct(s, child, parent, rdf_type)? {
                emit(s, other)?;
            }
            Ok(())
        })
    }

    fn count(&self, s: &mut Scope, id: Id, rdf_type: Id, parents: bool) -> Result<usize> {
        let mut count = 0;
        self.relatives(s, id, rdf_type, parents, |_, _| {
            count += 1;
            Ok(())
        })?;
        Ok(count)
    }

    fn node(&self, s: &mut Scope, id: Id, rdf_type: Id) -> Result<Node> {
        Ok(Node {
            name: s.name(id)?,
            children: self.count(s, id, rdf_type, false)?,
            parents: self.count(s, id, rdf_type, true)?,
            equivalence_count: self.canonical(s, id, rdf_type)?.1,
        })
    }
}

impl Ontologies {
    pub(super) fn materialized_hierarchy(
        &self,
        req: &OntologyRequest,
        h: &HierarchyRequest,
        opts: &QueryOptions,
    ) -> Result<Hierarchy> {
        if let Some(after) = &h.after {
            oxrdf::NamedNode::new(after)
                .map_err(|_| Error::invalid("after must be an absolute named IRI"))?;
        }
        if h.offset != 0 {
            return Err(Error::invalid(
                "an inferred hierarchy uses after, not offset",
            ));
        }
        if h.scheme.is_some()
            || !matches!(
                h.kind,
                TreeKind::Class | TreeKind::ObjectProperty | TreeKind::DataProperty
            )
        {
            return Err(Error::unsupported(
                "this hierarchy has no materialized relation contract",
            ));
        }
        let mut s = self.browse_scope(
            req,
            &h.languages,
            opts,
            h.source_id.as_deref(),
            h.after.is_some(),
        )?;
        let rdf_type = s.id(RDF_TYPE);
        let up = s.id(h.kind.up());
        let relation = Relation {
            up,
            types: h
                .kind
                .types()
                .iter()
                .filter_map(|kind| s.id(kind))
                .collect(),
            ignored: h
                .kind
                .ignored()
                .iter()
                .filter_map(|kind| s.id(kind))
                .collect(),
        };
        let mut page = BTreeMap::<String, Id>::new();
        let mut total = 0;
        let mut offset = 0;
        let mut keep = |s: &mut Scope, id: Id| -> Result<()> {
            if !h.deprecated && s.deprecated(id)? {
                return Ok(());
            }
            total += 1;
            let iri = s.iri(id);
            if h.after.as_ref().is_some_and(|after| iri <= *after) {
                offset += 1;
            } else {
                page.insert(iri, id);
                if page.len() > h.limit + 1 {
                    page.pop_last();
                }
            }
            Ok(())
        };
        let mut selected = None;
        if let Some(rdf_type) = rdf_type {
            if let Some(iri) = &h.iri {
                let id = s
                    .lookup(iri)
                    .ok_or_else(|| Error::NotFound("no entity in materialized hierarchy".into()))?;
                let canonical = if up.is_some() {
                    relation.canonical(&mut s, id, rdf_type)?.0
                } else {
                    id
                };
                if !relation.typed(&s, canonical, rdf_type)? {
                    return Err(Error::NotFound(
                        "no entity of this materialized hierarchy kind".into(),
                    ));
                }
                selected = Some(canonical);
                if up.is_some() {
                    relation.relatives(&mut s, canonical, rdf_type, false, &mut keep)?;
                }
            } else {
                let mut last = None;
                s.visit(Perm::Pso, &[rdf_type.0], |s, quad| {
                    let id = quad[0];
                    if !relation.types.contains(&quad[2]) || last == Some(id) {
                        return Ok(());
                    }
                    last = Some(id);
                    if s.snap.term_kind(id) == TermKind::Iri
                        && !relation.ignored.contains(&id)
                        && (up.is_none()
                            || (relation.canonical(s, id, rdf_type)?.0 == id
                                && relation.count(s, id, rdf_type, true)? == 0))
                    {
                        keep(s, id)?;
                    }
                    Ok(())
                })?;
            }
        } else if h.iri.is_some() {
            return Err(Error::NotFound(
                "no entity in materialized hierarchy".into(),
            ));
        }
        let next_after = (page.len() > h.limit)
            .then(|| page.keys().nth(h.limit - 1).cloned())
            .flatten();
        let mut children = Vec::with_capacity(h.limit.min(page.len()));
        if let Some(rdf_type) = rdf_type {
            for (_, id) in page.into_iter().take(h.limit) {
                check(&s.opts)?;
                children.push(if up.is_some() {
                    relation.node(&mut s, id, rdf_type)?
                } else {
                    Node {
                        name: s.name(id)?,
                        children: 0,
                        parents: 0,
                        equivalence_count: 1,
                    }
                });
            }
        }
        let node = match (selected, rdf_type) {
            (Some(id), Some(rdf_type)) => Some(relation.node(&mut s, id, rdf_type)?),
            _ => None,
        };
        let ancestors = if h.ancestors {
            let mut result = Ancestors::default();
            if let (Some(id), Some(rdf_type)) = (selected, rdf_type) {
                let mut queue = std::collections::VecDeque::from([id]);
                let mut seen = BTreeSet::from([id]);
                let mut edges = BTreeSet::new();
                while let Some(child) = queue.pop_front() {
                    relation.relatives(&mut s, child, rdf_type, true, |s, parent| {
                        if edges.len() >= MAX_ANCESTORS * 4
                            || (!seen.contains(&parent) && seen.len() >= MAX_ANCESTORS)
                        {
                            result.truncated = true;
                            return Ok(());
                        }
                        edges.insert((s.iri(child), s.iri(parent)));
                        if seen.insert(parent) {
                            queue.push_back(parent);
                        }
                        Ok(())
                    })?;
                }
                for ancestor in seen.into_iter().filter(|node| *node != id) {
                    result
                        .nodes
                        .push(relation.node(&mut s, ancestor, rdf_type)?);
                }
                result.edges = edges.into_iter().collect();
            }
            Some(result)
        } else {
            None
        };
        self.verify_browse_source(&s)?;
        Ok(Hierarchy {
            inference_source: s.inference_source,
            next_after,
            commit: s.snap.commit,
            kind: h.kind,
            node,
            children,
            total,
            offset,
            sorted: false,
            roots_from: h.iri.is_none().then_some(RootsFrom::Computed),
            deprecated_roots: 0,
            ancestors,
            graphs: s.refs,
            imports: s.imports,
            warnings: s.warnings,
        })
    }
}
