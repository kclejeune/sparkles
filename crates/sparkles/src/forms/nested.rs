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
}

struct Pending {
    term: Term,
    shapes: Vec<usize>,
    anchors: Vec<NestedAnchor>,
    depth: usize,
    queued: bool,
    model: Option<FormModel>,
}

struct Pool {
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
        let preparation = Preparation::of(opts);
        let built = self.prepare_model(req, opts, Some(preparation))?;
        let branch = self.ds.store().branch_name();
        let writable = req.writable && req.at.is_none();
        let root = built.model(writable, &branch)?;
        let mut pool = Pool {
            entries: Vec::new(),
            indices: BTreeMap::new(),
            queue: VecDeque::new(),
        };
        pool.discover(&built, &root, "root", 0)?;
        while let Some(index) = pool.queue.pop_front() {
            built.limits.unit()?;
            pool.entries[index].queued = false;
            let entry = &pool.entries[index];
            let child = built.child(&entry.term, &entry.shapes)?;
            let model = child.model(writable, &branch)?;
            let id = format!("n{index}");
            let depth = entry.depth;
            pool.discover(&child, &model, &id, depth)?;
            pool.entries[index].model = Some(model);
        }
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
        Ok(NestedFormModel { root, nested_nodes })
    }
}

impl Pool {
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

    fn child(&self, focus: &Term, shapes: &[usize]) -> Result<Built> {
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
        let target = target_graph(&read, focus, &self.target_policy)?;
        let mut types = BTreeSet::new();
        for value in read.values(
            focus,
            &FieldPath::Predicate(NamedNode::new_unchecked(RDF_TYPE)),
        )? {
            if let Term::NamedNode(iri) = value.term {
                self.limits.lexical(iri.as_str())?;
                self.limits.unit()?;
                types.insert(iri.into_string());
            }
        }
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
            masked: self.masked,
            lang: self.lang.clone(),
            prefixes: self.prefixes.clone(),
        })
    }
}
