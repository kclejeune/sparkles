//! Pooled nested read models. Existing blank labels identify data, never authority.

use super::*;
use std::collections::VecDeque;

/// The most nested identities one model admits.
pub const MAX_NESTED_NODES: usize = 64;
/// The most generated field views across one nested model request.
pub const MAX_NESTED_FIELDS: usize = 512;
/// The first-discovery depth of a new nested identity.
pub const MAX_NESTED_DEPTH: usize = 16;
/// The most visible parent edges retained by one model.
pub const MAX_NESTED_ANCHORS: usize = 512;
const MAX_PREPARATION_UNITS: usize = 65_536;

#[derive(Clone)]
pub(super) struct Preparation(Arc<Mutex<Counters>>);

struct Counters {
    remaining: usize,
    fields: usize,
    anchors: usize,
}

impl Preparation {
    pub(super) fn of(opts: &QueryOptions) -> Self {
        Self(Arc::new(Mutex::new(Counters {
            remaining: opts
                .max_memory_bytes
                .map_or(MAX_PREPARATION_UNITS, |bytes| {
                    usize::try_from(bytes / 128)
                        .unwrap_or(usize::MAX)
                        .min(MAX_PREPARATION_UNITS)
                }),
            fields: 0,
            anchors: 0,
        })))
    }

    pub(super) fn unit(&self) -> Result<()> {
        let mut counters = self.0.lock();
        counters.remaining = counters
            .remaining
            .checked_sub(1)
            .ok_or_else(|| Error::invalid("nested form preparation allowance exceeded"))?;
        Ok(())
    }

    pub(super) fn field(&self) -> Result<()> {
        let mut counters = self.0.lock();
        if counters.fields >= MAX_NESTED_FIELDS {
            return Err(Error::invalid("nested form field allowance exceeded"));
        }
        counters.fields += 1;
        Ok(())
    }

    fn anchor(&self) -> Result<()> {
        let mut counters = self.0.lock();
        if counters.anchors >= MAX_NESTED_ANCHORS {
            return Err(Error::invalid("nested form anchor allowance exceeded"));
        }
        counters.anchors += 1;
        Ok(())
    }
}

/// An exact visible parent edge. Its graph is part of its identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedAnchor {
    /// `root`, or the request-local id of another admitted nested model.
    pub parent: String,
    pub field: String,
    pub graph: Option<String>,
    pub inverse: bool,
}

/// A stored blank identity and the declared child fields visible to this caller.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedNode {
    pub id: String,
    #[serde(serialize_with = "ser_term")]
    pub term: Term,
    pub anchors: Vec<NestedAnchor>,
    #[serde(flatten)]
    pub model: FormModel,
}

/// The ordinary root form with its admitted nested identity pool.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedFormModel {
    #[serde(flatten)]
    pub root: FormModel,
    pub nested_nodes: Vec<NestedNode>,
    /// the empty forms a new child takes, one for each distinct set of `sh:node` shapes
    /// that a field of the root or of a template declares
    pub templates: Vec<NestedTemplate>,
}

/// The form of a new nested child under a field whose `sh:node` shapes are `shapes`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedTemplate {
    /// the shapes as the field's `constraints.node` names them; the member is `node`
    /// because the flattened model already has a `shapes` member
    pub node: Vec<String>,
    #[serde(flatten)]
    pub model: FormModel,
}

struct Pending {
    term: Term,
    shapes: Vec<usize>,
    anchors: Vec<NestedAnchor>,
    depth: usize,
    queued: bool,
    model: Option<FormModel>,
}

pub(super) struct Pool {
    entries: Vec<Pending>,
    indices: BTreeMap<String, usize>,
    queue: VecDeque<usize>,
}

impl Forms {
    /// Read declared blank-node children without editing or allocating stored nodes.
    ///
    /// All expansions share one preparation allowance and cancellation/deadline.
    /// Refusal discards the whole result. Physical dictionary/segment decoding and
    /// source metadata preparation remain the separate controlled-reader boundary.
    pub fn nested_model(&self, req: &FormRequest, opts: &QueryOptions) -> Result<NestedFormModel> {
        let opts = &*self.ds.without_owner_graphs(opts);
        let preparation = Preparation::of(opts);
        let built = self.prepare_model(req, opts, Some(preparation))?;
        let branch = self.ds.store().branch_name();
        let writable = req.writable && req.at.is_none();
        let (root, pool) = built.pool(writable, &branch)?;
        let mut nested_nodes = Vec::new();
        for (index, entry) in pool.entries.into_iter().enumerate() {
            built.limits.unit()?;
            nested_nodes.push(NestedNode {
                id: format!("n{index}"),
                term: entry.term,
                anchors: entry.anchors,
                model: entry
                    .model
                    .ok_or_else(|| Error::invalid("nested form was not expanded"))?,
            });
        }
        let templates = built.templates(writable, &branch, &nested_nodes)?;
        Ok(NestedFormModel {
            root,
            nested_nodes,
            templates,
        })
    }
}

/// A pooled nested node: its stored identity and the declared shapes that reach it.
pub(super) struct Pooled<'a> {
    pub(super) shapes: &'a [usize],
}

impl Pool {
    pub(super) fn get(&self, term: &Term) -> Option<Pooled<'_>> {
        let Term::BlankNode(b) = term else {
            return None;
        };
        let entry = &self.entries[*self.indices.get(b.as_str())?];
        Some(Pooled {
            shapes: &entry.shapes,
        })
    }

    fn discover(
        &mut self,
        built: &Built,
        model: &FormModel,
        parent: &str,
        depth: usize,
    ) -> Result<()> {
        let preparation = built
            .limits
            .preparation
            .as_ref()
            .ok_or_else(|| Error::invalid("nested form preparation is missing"))?;
        for source in &built.layout.fields {
            built.limits.unit()?;
            if !source.constraints.node.is_empty() && matches!(source.path, FieldPath::Complex(_)) {
                return Err(Error::invalid(
                    "complex nested path producer is not admitted",
                ));
            }
        }
        for field in &model.fields {
            built.limits.unit()?;
            let predicate = field
                .path
                .trim_start_matches('^')
                .trim_start_matches('<')
                .trim_end_matches('>');
            let mut source = None;
            for candidate in &built.layout.fields {
                built.limits.unit()?;
                if let Some(p) = candidate.path.predicate() {
                    built.limits.lexical(p.as_str())?;
                    built.limits.lexical(predicate)?;
                    if p.as_str() == predicate
                        && matches!(candidate.path, FieldPath::Inverse(_)) == field.inverse
                    {
                        source = Some(candidate);
                        break;
                    }
                }
            }
            let Some(source) = source else { continue };
            if source.constraints.node.is_empty() {
                continue;
            }
            if field.truncated {
                return Err(Error::invalid(
                    "nested child field exceeds its complete value allowance",
                ));
            }
            for value in &field.values {
                let Term::BlankNode(blank) = &value.term else {
                    continue;
                };
                built.limits.unit()?;
                let index = match self.indices.get(blank.as_str()).copied() {
                    Some(index) => index,
                    None => {
                        if self.entries.len() >= MAX_NESTED_NODES || depth >= MAX_NESTED_DEPTH {
                            return Err(Error::invalid(
                                "nested form identity or depth allowance exceeded",
                            ));
                        }
                        built.limits.retained_term(&value.term, 0)?;
                        built.limits.lexical(blank.as_str())?;
                        let index = self.entries.len();
                        self.indices.insert(blank.as_str().to_string(), index);
                        self.entries.push(Pending {
                            term: value.term.clone(),
                            shapes: Vec::new(),
                            anchors: Vec::new(),
                            depth: depth + 1,
                            queued: false,
                            model: None,
                        });
                        index
                    }
                };
                let entry = &mut self.entries[index];
                let mut exists = false;
                for anchor in &entry.anchors {
                    built.limits.unit()?;
                    built.limits.lexical(&anchor.field)?;
                    built.limits.lexical(&field.id)?;
                    if let Some(graph) = &anchor.graph {
                        built.limits.lexical(graph)?;
                    }
                    if let Some(graph) = &value.graph {
                        built.limits.lexical(graph)?;
                    }
                    if anchor.parent == parent
                        && anchor.field == field.id
                        && anchor.graph == value.graph
                        && anchor.inverse == field.inverse
                    {
                        exists = true;
                        break;
                    }
                }
                if !exists {
                    preparation.anchor()?;
                    built.limits.lexical(parent)?;
                    built.limits.lexical(&field.id)?;
                    if let Some(graph) = &value.graph {
                        built.limits.lexical(graph)?;
                    }
                    entry.anchors.push(NestedAnchor {
                        parent: parent.to_string(),
                        field: field.id.clone(),
                        graph: value.graph.clone(),
                        inverse: field.inverse,
                    });
                }
                // Only a larger declaration union needs another generation.
                // A self-edge can be discovered before this model is installed.
                let mut changed = false;
                for term in &source.constraints.node {
                    built.limits.unit()?;
                    if let Some(shape) = built.shapes.shape_id(term) {
                        let mut exists = false;
                        for old in &entry.shapes {
                            built.limits.unit()?;
                            if *old == shape {
                                exists = true;
                                break;
                            }
                        }
                        if !exists {
                            built.limits.unit()?;
                            entry.shapes.push(shape);
                            changed = true;
                        }
                    }
                }
                if changed && !entry.queued {
                    built.limits.unit()?;
                    entry.queued = true;
                    self.queue.push_back(index);
                }
            }
        }
        Ok(())
    }
}

fn admit_path(path: &sparkles_shacl::PropertyPath, limits: &Limits, depth: usize) -> Result<()> {
    limits.unit()?;
    if depth > 64 {
        return Err(Error::invalid("form path preparation nesting exceeded"));
    }
    use sparkles_shacl::PropertyPath;
    match path {
        PropertyPath::Predicate(n) => {
            for _ in 0..6 {
                limits.lexical(n.as_str())?;
            }
        }
        PropertyPath::Sequence(paths) | PropertyPath::Alternative(paths) => {
            for path in paths {
                admit_path(path, limits, depth + 1)?;
            }
        }
        PropertyPath::Inverse(path)
        | PropertyPath::ZeroOrMore(path)
        | PropertyPath::OneOrMore(path)
        | PropertyPath::ZeroOrOne(path) => {
            admit_path(path, limits, depth + 1)?;
        }
    }
    Ok(())
}

impl Built {
    /// The root model and the pool of its declared nested children, discovered under
    /// the shared preparation allowance. `self` must carry a preparation.
    pub(super) fn pool(&self, writable: bool, branch: &str) -> Result<(FormModel, Pool)> {
        let root = self.model(writable, branch)?;
        let mut pool = Pool {
            entries: Vec::new(),
            indices: BTreeMap::new(),
            queue: VecDeque::new(),
        };
        pool.discover(self, &root, "root", 0)?;
        while let Some(index) = pool.queue.pop_front() {
            self.limits.unit()?;
            pool.entries[index].queued = false;
            let entry = &pool.entries[index];
            let anchor = entry.anchors.first().map(|a| a.graph.clone());
            let child = self.child(&entry.term, &entry.shapes, anchor)?;
            let model = child.model(writable, branch)?;
            let id = format!("n{index}");
            let depth = entry.depth;
            pool.discover(&child, &model, &id, depth)?;
            pool.entries[index].model = Some(model);
        }
        Ok((root, pool))
    }

    /// The templates of new children: the `sh:node` shape sets that fields of the root,
    /// of pooled children and of templates declare on predicate paths, closed over the
    /// templates' own fields, at most [`MAX_NESTED_NODES`].
    fn templates(
        &self,
        writable: bool,
        branch: &str,
        pooled: &[NestedNode],
    ) -> Result<Vec<NestedTemplate>> {
        let declared = |layout: &FormLayout, out: &mut Vec<Vec<usize>>| -> Result<()> {
            for f in &layout.fields {
                self.limits.unit()?;
                if f.constraints.node.is_empty() || !matches!(f.path, FieldPath::Predicate(_)) {
                    continue;
                }
                let set: Vec<usize> = f
                    .constraints
                    .node
                    .iter()
                    .filter_map(|t| self.shapes.shape_id(t))
                    .collect();
                if !set.is_empty() && !out.contains(&set) {
                    out.push(set);
                }
            }
            Ok(())
        };
        let mut sets = Vec::new();
        declared(&self.layout, &mut sets)?;
        // the nested fields of pooled children name their shapes in the constraints
        for node in pooled {
            for f in &node.model.fields {
                self.limits.unit()?;
                if f.inverse || f.reason == Some("complex-path") {
                    continue;
                }
                let Some(J::Array(names)) = f.constraints.get("node") else {
                    continue;
                };
                let mut set = Vec::new();
                for name in names.iter().filter_map(J::as_str) {
                    self.limits.lexical(name)?;
                    let term = match name.strip_prefix("_:") {
                        Some(label) => oxrdf::BlankNode::new(label).ok().map(Term::BlankNode),
                        None => NamedNode::new(name.trim_start_matches('<').trim_end_matches('>'))
                            .ok()
                            .map(Term::NamedNode),
                    };
                    if let Some(id) = term.and_then(|t| self.shapes.shape_id(&t)) {
                        set.push(id);
                    }
                }
                if !set.is_empty() && !sets.contains(&set) {
                    sets.push(set);
                }
            }
        }
        let fresh = Term::BlankNode(oxrdf::BlankNode::new_unchecked("new"));
        let mut out = Vec::new();
        let mut i = 0;
        while i < sets.len() {
            if out.len() >= MAX_NESTED_NODES {
                return Err(Error::invalid("nested form template allowance exceeded"));
            }
            let child = self.fresh_child(&fresh, &sets[i], self.target.clone())?;
            declared(&child.layout, &mut sets)?;
            let model = child.model(writable, branch)?;
            out.push(NestedTemplate {
                node: sets[i]
                    .iter()
                    .map(|s| self.shapes.shapes()[*s].node.to_string())
                    .collect(),
                model,
            });
            i += 1;
        }
        Ok(out)
    }

    /// A new nested child that a change creates: the fields of `shapes`, no stored
    /// values, and new values in the graph of the edge from its parent.
    pub(super) fn fresh_child(
        &self,
        focus: &Term,
        shapes: &[usize],
        target: Option<String>,
    ) -> Result<Built> {
        self.limits.retained_term(focus, 0)?;
        let layout = sparkles_shacl::form::form_layout_with(&self.shapes, shapes, &mut || {
            self.limits.unit().map_err(anyhow::Error::new)
        })
        .map_err(crate::handles::from_anyhow)?;
        Ok(Built {
            focus: focus.clone(),
            new_class: None,
            types: Vec::new(),
            commit: self.commit,
            source: self.source,
            shapes: self.shapes.clone(),
            vopts: self.vopts.clone(),
            missing: None,
            node_shapes: shapes.to_vec(),
            layout,
            target,
            target_policy: self.target_policy.clone(),
            access: self.access.clone(),
            write_access: self.write_access.clone(),
            limits: self.limits.clone(),
            snap: self.snap.clone(),
            raw: self.raw.clone(),
            masked: self.masked,
            lang: self.lang.clone(),
            prefixes: self.prefixes.clone(),
        })
    }

    pub(super) fn admit_metadata(&self, field: &FormField) -> Result<()> {
        if self.limits.preparation.is_none() {
            return Ok(());
        }
        self.limits.unit()?;
        for (prefix, iri) in &self.prefixes {
            self.limits.lexical(prefix)?;
            self.limits.lexical(iri)?;
        }
        for literal in field.names.iter().chain(&field.descriptions) {
            self.limits.lexical(literal.value())?;
        }
        if let Some(group) = &field.group {
            self.limits.retained_term(group.node(), 0)?;
            match group {
                FieldGroup::Group(term) => {
                    if let Some(info) = self.shapes.group(term) {
                        for label in &info.labels {
                            self.limits.lexical(label.value())?;
                        }
                    }
                }
                FieldGroup::Shape(term) => {
                    if let Some(shape) = self.shapes.shape(term) {
                        for label in &shape.info().names {
                            self.limits.lexical(label.value())?;
                        }
                    }
                }
            }
        }
        for hint in field.editor.iter().chain(&field.viewer) {
            self.limits.lexical(hint.as_str())?;
        }
        match &field.path {
            FieldPath::Predicate(n) | FieldPath::Inverse(n) => {
                // The id, path and constraint labels can each own an escaped form.
                for _ in 0..6 {
                    self.limits.lexical(n.as_str())?;
                }
            }
            FieldPath::Complex(path) => admit_path(path, &self.limits, 0)?,
        }
        for shape in &field.sources {
            for _ in 0..6 {
                self.limits
                    .retained_term(&self.shapes.shapes()[*shape].node, 0)?;
            }
        }
        let constraints = &field.constraints;
        for value in &constraints.classes {
            self.limits.lexical(value.as_str())?;
        }
        if let Some(datatype) = &constraints.datatype {
            self.limits.lexical(datatype.as_str())?;
        }
        for (pattern, flags) in &constraints.patterns {
            self.limits.lexical(pattern)?;
            if let Some(flags) = flags {
                self.limits.lexical(flags)?;
            }
        }
        if let Some(languages) = &constraints.language_in {
            for language in languages {
                self.limits.lexical(language)?;
            }
        }
        for component in &constraints.server {
            self.limits.lexical(component)?;
        }
        for term in constraints
            .allowed
            .iter()
            .flatten()
            .chain(&constraints.has_value)
            .chain(&constraints.node)
            .chain(field.default_value.iter())
        {
            for _ in 0..6 {
                self.limits.retained_term(term, 0)?;
            }
        }
        for bound in [
            &constraints.min_inclusive,
            &constraints.min_exclusive,
            &constraints.max_inclusive,
            &constraints.max_exclusive,
        ]
        .into_iter()
        .flatten()
        {
            for _ in 0..6 {
                self.limits.retained_term(&bound.term, 0)?;
            }
        }
        Ok(())
    }

    /// The fields of a pooled child. With the `type` target policy, a child that has no
    /// type of its own takes new values into the graph of its first anchor.
    pub(super) fn child(
        &self,
        focus: &Term,
        shapes: &[usize],
        anchor: Option<Option<String>>,
    ) -> Result<Built> {
        self.limits.retained_term(focus, 0)?;
        for _ in shapes {
            self.limits.unit()?;
        }
        for (prefix, iri) in &self.prefixes {
            self.limits.lexical(prefix)?;
            self.limits.lexical(iri)?;
        }
        if let Some(lang) = &self.lang {
            self.limits.lexical(lang)?;
        }
        self.limits.lexical(&self.target_policy)?;
        for graph in self
            .vopts
            .data_graph
            .iter()
            .chain(&self.vopts.extra_graphs)
            .chain(&self.vopts.exclude_graphs)
        {
            self.limits.lexical(graph)?;
        }
        let layout = sparkles_shacl::form::form_layout_with(&self.shapes, shapes, &mut || {
            self.limits.unit().map_err(anyhow::Error::new)
        })
        .map_err(crate::handles::from_anyhow)?;
        let read = self.read()?;
        let (target, types) = child_target(&read, focus, &self.target_policy, anchor)?;
        Ok(Built {
            focus: focus.clone(),
            new_class: None,
            types: types.into_iter().collect(),
            commit: self.commit,
            source: self.source,
            shapes: self.shapes.clone(),
            vopts: self.vopts.clone(),
            missing: None,
            node_shapes: shapes.to_vec(),
            layout,
            target,
            target_policy: self.target_policy.clone(),
            access: self.access.clone(),
            write_access: self.write_access.clone(),
            limits: self.limits.clone(),
            snap: self.snap.clone(),
            raw: self.raw.clone(),
            masked: self.masked,
            lang: self.lang.clone(),
            prefixes: self.prefixes.clone(),
        })
    }
}

/// The target graph of a stored nested child and its types: the policy resolved against
/// the child, and with `targetGraph: "type"` the graph of its edge (`anchor`) when the
/// child has no type.
pub(super) fn child_target(
    read: &Reader<'_>,
    focus: &Term,
    policy: &str,
    anchor: Option<Option<String>>,
) -> Result<(Option<String>, BTreeSet<String>)> {
    let mut target = target_graph(read, focus, policy)?;
    let mut types = BTreeSet::new();
    for value in read.values(
        focus,
        &FieldPath::Predicate(NamedNode::new_unchecked(RDF_TYPE)),
    )? {
        if let Term::NamedNode(iri) = value.term {
            read.limits.lexical(iri.as_str())?;
            read.limits.unit()?;
            types.insert(iri.into_string());
        }
    }
    if policy == "type"
        && types.is_empty()
        && let Some(graph) = anchor
    {
        target = graph;
    }
    Ok((target, types))
}
