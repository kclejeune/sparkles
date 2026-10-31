//! Request-bounded disclosure preparation. Legacy guard methods are unchanged.
use super::*;
use sparkles::access::{Exemptions, Preparation, Reservation};
use sparkles::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::io::Read;

struct Workspace<'a> {
    preparation: &'a Preparation,
    held: Reservation,
}

impl<'a> Workspace<'a> {
    fn new(preparation: &'a Preparation) -> Result<Self> {
        Ok(Self {
            preparation,
            held: preparation.reserve(4096)?,
        })
    }

    fn copy(&mut self, text: &str) -> Result<String> {
        self.preparation.copy_string(text, &mut self.held)
    }

    fn vector<T>(&mut self, values: &mut Vec<T>) -> Result<()> {
        self.preparation.check()?;
        if values.len() == values.capacity() {
            let capacity = values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or_else(|| self.preparation.exhausted())?;
            let bytes = capacity
                .checked_mul(std::mem::size_of::<T>())
                .and_then(|n| n.checked_add(128))
                .ok_or_else(|| self.preparation.exhausted())?;
            // Earlier capacities remain charged until the temporary index dies.
            // In particular a moving realloc cannot exceed the admitted peak.
            self.held.grow_by(bytes as u64)?;
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| self.preparation.exhausted())?;
            if values.capacity() != capacity {
                return Err(self.preparation.exhausted());
            }
        }
        Ok(())
    }

    fn map<K: Eq + Hash, V>(&mut self, values: &mut HashMap<K, V>) -> Result<()> {
        self.preparation.check()?;
        if values.len() == values.capacity() {
            self.table::<(K, V)>(values.capacity())?;
            values
                .try_reserve(values.capacity().max(4))
                .map_err(|_| self.preparation.exhausted())?;
        }
        Ok(())
    }

    fn set<T: Eq + Hash>(&mut self, values: &mut HashSet<T>) -> Result<()> {
        self.preparation.check()?;
        if values.len() == values.capacity() {
            self.table::<T>(values.capacity())?;
            values
                .try_reserve(values.capacity().max(4))
                .map_err(|_| self.preparation.exhausted())?;
        }
        Ok(())
    }

    fn table<T>(&mut self, capacity: usize) -> Result<()> {
        let buckets = capacity
            .max(4)
            .checked_mul(4)
            .and_then(usize::checked_next_power_of_two)
            .ok_or_else(|| self.preparation.exhausted())?;
        let bytes = buckets
            .checked_mul(std::mem::size_of::<T>().saturating_add(32))
            .and_then(|n| n.checked_add(1024))
            .ok_or_else(|| self.preparation.exhausted())?;
        self.held.grow_by(bytes as u64)
    }
}

/// Hashes only select a collision bucket. Full controlled string comparisons
/// decide graph identity; no digest acts as a source or permission authority.
#[derive(Default)]
struct LocalIndex {
    graphs: HashMap<[u8; 32], Vec<NamedGraph>>,
}

struct NamedGraph {
    name: String,
    state: GraphFootprints,
    // Valid footprint names have the fixed prefix and64 hex digits, so hashing a
    // name here is bounded. Long graph names never enter an uncontrolled hasher.
    covered: HashSet<String>,
}

struct OwnedIndex {
    value: LocalIndex,
    _held: Reservation,
}

struct CachedIndex {
    value: Arc<Index>,
    _held: Reservation,
}

pub(super) fn exempt(
    guard: &Guard,
    snap: &Arc<Snapshot>,
    view: &GraphAccess,
    preparation: &Preparation,
) -> Result<Exemptions> {
    preparation.check()?;
    if let Some(key) = CacheKey::of(snap) {
        let cached = {
            let indexes = preparation.lock(&guard.indexes)?;
            indexes
                .iter()
                .find(|(cached, _)| cached.same(&key))
                .map(|(_, index)| index.clone())
        };
        if let Some(index) = cached {
            // The shared index belongs to the guard's cache, and this call only reads
            // it. The exemptions it returns are copies that the request admits.
            let cached = CachedIndex {
                value: index,
                _held: preparation.reserve(4096)?,
            };
            let mut output = Exemptions::new(preparation)?;
            for (name, state) in &cached.value.graphs {
                if proven(name, state, view, preparation)? {
                    output.push(name)?;
                }
            }
            preparation.check()?;
            return Ok(output);
        }
    }
    // A controlled cold call keeps its complete index local. It cannot install a
    // partial result, evict legacy shared entries or retain one entry per cap.
    let index = read_index(snap, preparation)?;
    let mut output = Exemptions::new(preparation)?;
    for bucket in index.value.graphs.values() {
        for graph in bucket {
            if proven(&graph.name, &graph.state, view, preparation)? {
                output.push(&graph.name)?;
            }
        }
    }
    preparation.check()?;
    Ok(output)
}

fn digest(text: &str, preparation: &Preparation) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for part in text.as_bytes().chunks(1024) {
        preparation.check()?;
        hash.update(part);
    }
    preparation.check()?;
    Ok(hash.finalize().into())
}

fn graph<'a>(
    index: &'a mut LocalIndex,
    name: &str,
    workspace: &mut Workspace<'_>,
) -> Result<&'a mut NamedGraph> {
    let key = digest(name, workspace.preparation)?;
    if !index.graphs.contains_key(&key) {
        workspace.map(&mut index.graphs)?;
        index.graphs.insert(key, Vec::new());
    }
    let bucket = index
        .graphs
        .get_mut(&key)
        .expect("prepared collision bucket");
    let mut found = None;
    for (at, graph) in bucket.iter().enumerate() {
        if workspace.preparation.compare(&graph.name, name)?.is_eq() {
            found = Some(at);
            break;
        }
    }
    let at = if let Some(at) = found {
        at
    } else {
        workspace.vector(bucket)?;
        let name = workspace.copy(name)?;
        bucket.push(NamedGraph {
            name,
            state: GraphFootprints::default(),
            covered: HashSet::new(),
        });
        bucket.len() - 1
    };
    Ok(&mut bucket[at])
}

fn iri(snap: &Snapshot, id: Id, workspace: &mut Workspace<'_>) -> Result<Option<String>> {
    let preparation = workspace.preparation;
    preparation.with_term(snap, id, |term| match term {
        Some(oxrdf::Term::NamedNode(node)) => workspace.copy(node.as_str()).map(Some),
        _ => Ok(None),
    })
}

fn predicate(
    snap: &Snapshot,
    name: &str,
    preparation: &Preparation,
    mut each: impl FnMut([Id; 4]) -> Result<()>,
) -> Result<()> {
    if let Some(id) = preparation.lookup_iri(snap, name)? {
        preparation.scan(snap, Perm::Pso, &[id.0], |quad| {
            each(quad)?;
            Ok(true)
        })?;
    }
    preparation.check()
}

fn read_index(snap: &Snapshot, preparation: &Preparation) -> Result<OwnedIndex> {
    let mut workspace = Workspace::new(preparation)?;
    let mut inputs = Workspace::new(preparation)?;
    let mut covers: HashMap<Id, Vec<String>> = HashMap::new();
    let mut json: HashMap<Id, Vec<String>> = HashMap::new();
    let mut index = LocalIndex::default();
    let dg = preparation.lookup_iri(snap, DISCLOSURE_GRAPH)?;
    if let Some(dg) = dg {
        predicate(
            snap,
            &gr("covers"),
            preparation,
            |[subject, _, object, graph]| {
                if graph != dg {
                    return Ok(());
                }
                let mut row = Workspace::new(preparation)?;
                if let Some(name) = iri(snap, object, &mut row)?
                    .filter(|name| super::super::settings::is_guarded(name))
                {
                    let name = inputs.copy(&name)?;
                    if !covers.contains_key(&subject) {
                        inputs.map(&mut covers)?;
                        covers.insert(subject, Vec::new());
                    }
                    let values = covers.get_mut(&subject).expect("prepared covers subject");
                    inputs.vector(values)?;
                    values.push(name);
                }
                Ok(())
            },
        )?;
        predicate(
            snap,
            &gr("footprint"),
            preparation,
            |[subject, _, object, graph]| {
                if graph != dg {
                    return Ok(());
                }
                let text = preparation.with_term(snap, object, |term| match term {
                    Some(oxrdf::Term::Literal(literal)) => inputs.copy(literal.value()),
                    _ => inputs.copy(""),
                })?;
                if !json.contains_key(&subject) {
                    inputs.map(&mut json)?;
                    json.insert(subject, Vec::new());
                }
                let values = json.get_mut(&subject).expect("prepared footprint subject");
                inputs.vector(values)?;
                values.push(text);
                Ok(())
            },
        )?;
    }
    for (subject, names) in &covers {
        preparation.check()?;
        let mut row = Workspace::new(preparation)?;
        let name = iri(snap, *subject, &mut row)?;
        let fp = match (name.as_deref(), json.get(subject).map(Vec::as_slice)) {
            (Some(name), Some([text])) => parse(name, text, &mut workspace)?,
            _ => None,
        };
        for name_graph in names {
            preparation.check()?;
            let graph = graph(&mut index, name_graph, &mut workspace)?;
            workspace.vector(&mut graph.state.footprints)?;
            graph.state.footprints.push(fp.clone());
            if fp.is_some() {
                let name = name.as_ref().expect("validated named footprint");
                // parse verifies this exact fixed-length name against its JSON.
                if !graph.covered.contains(name) {
                    workspace.set(&mut graph.covered)?;
                    graph.covered.insert(workspace.copy(name)?);
                }
            }
        }
    }
    drop(covers);
    drop(json);
    drop(inputs);
    // A disclosure row's effect depends only on its graph and object, and repeating
    // a pair changes nothing, so recently seen pairs skip decoding and hashing. The
    // table has a fixed size and lives on the stack.
    let mut seen = [None::<(Id, Id)>; SEEN_PAIRS];
    predicate(
        snap,
        &gr("disclosure"),
        preparation,
        |[_, _, object, graph_id]| {
            if Some(graph_id) == dg {
                return Ok(());
            }
            let slot = seen_slot(graph_id, object);
            if seen[slot] == Some((graph_id, object)) {
                return Ok(());
            }
            seen[slot] = Some((graph_id, object));
            let mut row = Workspace::new(preparation)?;
            let Some(name) = iri(snap, graph_id, &mut row)?
                .filter(|name| super::super::settings::is_guarded(name))
            else {
                return Ok(());
            };
            let linked = iri(snap, object, &mut row)?;
            let graph = graph(&mut index, &name, &mut workspace)?;
            let covered = linked.as_ref().is_some_and(|name| {
                name.len() == FOOTPRINT_PREFIX.len() + 64 && graph.covered.contains(name)
            });
            if !covered {
                graph.state.missing = true;
            }
            Ok(())
        },
    )?;
    preparation.check()?;
    Ok(OwnedIndex {
        value: index,
        _held: workspace.held,
    })
}

const SEEN_PAIRS: usize = 256;

fn seen_slot(graph: Id, object: Id) -> usize {
    let mixed = (graph.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ object.0)
        .wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    (mixed >> 56) as usize % SEEN_PAIRS
}

struct ControlledRead<'a> {
    bytes: &'a [u8],
    preparation: &'a Preparation,
    until_check: usize,
    failure: Option<Error>,
}

impl Read for ControlledRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.until_check == 0 {
            if let Err(error) = self.preparation.checkpoint("reading disclosure footprint") {
                self.failure = Some(error);
                return Err(std::io::Error::other("disclosure preparation interrupted"));
            }
            self.until_check = 1024;
        }
        let length = output.len().min(self.bytes.len()).min(self.until_check);
        output[..length].copy_from_slice(&self.bytes[..length]);
        self.bytes = &self.bytes[length..];
        self.until_check -= length;
        Ok(length)
    }
}

fn parse(name: &str, text: &str, workspace: &mut Workspace<'_>) -> Result<Option<Arc<Footprint>>> {
    if text.len() > MAX_FOOTPRINT_BYTES {
        return Ok(None);
    }
    let hash = digest(text, workspace.preparation)?;
    let mut expected = String::with_capacity(FOOTPRINT_PREFIX.len() + 64);
    expected.push_str(FOOTPRINT_PREFIX);
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(expected, "{byte:02x}");
    }
    if !workspace.preparation.compare(name, &expected)?.is_eq() {
        return Ok(None);
    }
    // Same closed Footprint schema and Serde recursion limit as the old parser.
    //128 bytes per input byte conservatively covers strings, sequence relocation,
    // map nodes, the parser scratch and the returned closed-schema object together.
    let _scratch = workspace
        .preparation
        .reserve((text.len() as u64).saturating_mul(128).saturating_add(4096))?;
    let mut reader = ControlledRead {
        bytes: text.as_bytes(),
        preparation: workspace.preparation,
        until_check: 0,
        failure: None,
    };
    let parsed: std::result::Result<Footprint, _> = serde_json::from_reader(&mut reader);
    if let Some(error) = reader.failure {
        return Err(error);
    }
    workspace.preparation.check()?;
    let Some(fp) = parsed.ok().filter(|fp| fp.version == FOOTPRINT_VERSION) else {
        return Ok(None);
    };
    // The parsing workspace ends here. Only the actual retained model remains
    // charged, including allocated capacities. Admit ownership before transfer.
    retained_footprint(&fp, &mut workspace.held, workspace.preparation)?;
    Ok(Some(Arc::new(fp)))
}

fn strings_equal(a: &[String], b: &[String], preparation: &Preparation) -> Result<bool> {
    if a.len() != b.len() {
        preparation.check()?;
        return Ok(false);
    }
    for (a, b) in a.iter().zip(b) {
        if !preparation.compare(a, b)?.is_eq() {
            return Ok(false);
        }
    }
    preparation.check()?;
    Ok(true)
}

fn optional_text(a: Option<&str>, b: Option<&str>, preparation: &Preparation) -> Result<bool> {
    match (a, b) {
        (Some(a), Some(b)) => Ok(preparation.compare(a, b)?.is_eq()),
        (None, None) => {
            preparation.check()?;
            Ok(true)
        }
        _ => {
            preparation.check()?;
            Ok(false)
        }
    }
}

fn optional_strings(
    a: Option<&[String]>,
    b: Option<&[String]>,
    preparation: &Preparation,
) -> Result<bool> {
    match (a, b) {
        (Some(a), Some(b)) => strings_equal(a, b, preparation),
        (None, None) => {
            preparation.check()?;
            Ok(true)
        }
        _ => {
            preparation.check()?;
            Ok(false)
        }
    }
}

fn graph_definition(a: &Graphs, b: &FpGraphs, preparation: &Preparation) -> Result<bool> {
    match (a, b) {
        (Graphs::All, FpGraphs::All) => {
            preparation.check()?;
            Ok(true)
        }
        (
            Graphs::Only(a),
            FpGraphs::Only {
                default_graph,
                iris,
                patterns,
                protected,
                blank_nodes,
                excluded,
            },
        ) => Ok(a.guards.is_empty()
            && a.default_graph == *default_graph
            && a.blank_nodes == *blank_nodes
            && strings_equal(&a.iris, iris, preparation)?
            && strings_equal(&a.patterns, patterns, preparation)?
            && strings_equal(&a.protected, protected, preparation)?
            && strings_equal(&a.excluded, excluded, preparation)?),
        _ => {
            preparation.check()?;
            Ok(false)
        }
    }
}

fn definition(a: &Protection, b: &FpProtection, preparation: &Preparation) -> Result<bool> {
    if !preparation.compare(&a.name, &b.name)?.is_eq()
        || a.subclasses != b.subclasses
        || a.hide_inferences != b.hide_inferences
        || !optional_strings(
            a.predicates.as_deref(),
            b.predicates.as_deref(),
            preparation,
        )?
        || !optional_strings(a.classes.as_deref(), b.classes.as_deref(), preparation)?
        || !optional_text(a.pattern.as_deref(), b.pattern.as_deref(), preparation)?
        || a.prefixes.len() != b.prefixes.len()
    {
        return Ok(false);
    }
    let graphs = match (&a.graphs, &b.graphs) {
        (None, None) => true,
        (Some(a), Some(b)) => graph_definition(a, b, preparation)?,
        _ => false,
    };
    if !graphs {
        return Ok(false);
    }
    for ((ak, av), (bk, bv)) in a.prefixes.iter().zip(&b.prefixes) {
        if !preparation.compare(ak, bk)?.is_eq() || !preparation.compare(av, bv)?.is_eq() {
            return Ok(false);
        }
    }
    preparation.check()?;
    Ok(true)
}

fn uses(pattern: &str, var: &str, preparation: &Preparation) -> Result<bool> {
    let bytes = pattern.as_bytes();
    let n = var.len();
    for index in 0..bytes.len() {
        if index.is_multiple_of(1024) {
            preparation.check()?;
        }
        if matches!(bytes[index], b'?' | b'$')
            && pattern.get(index + 1..index + 1 + n) == Some(var)
            && bytes
                .get(index + 1 + n)
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c >= 0x80))
        {
            return Ok(true);
        }
    }
    preparation.check()?;
    Ok(false)
}

/// Compare normalized reader attributes to the stored exact array, without a
/// caller-sized clone or sort. Both directions preserve duplicates' set meaning.
fn normalized(a: &[String], b: &[String], preparation: &Preparation) -> Result<bool> {
    for adjacent in b.windows(2) {
        if !preparation.compare(&adjacent[0], &adjacent[1])?.is_lt() {
            return Ok(false);
        }
    }
    for value in a {
        let mut found = false;
        for expected in b {
            if preparation.compare(value, expected)?.is_eq() {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    for expected in b {
        let mut found = false;
        for value in a {
            if preparation.compare(value, expected)?.is_eq() {
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    preparation.check()?;
    Ok(true)
}

fn coverage(view: &GraphAccess, fp: &Footprint, preparation: &Preparation) -> Result<Decision> {
    preparation.check()?;
    if view.read.is_all() {
        let mut hides = false;
        if let Some(rules) = &view.triples {
            for rule in &rules.rules {
                preparation.check()?;
                if !rule.read.is_all() {
                    hides = true;
                    break;
                }
            }
        }
        if !hides {
            return Ok(Decision::Proven);
        }
    }
    if fp.version != FOOTPRINT_VERSION {
        return Ok(Decision::Unproven);
    }
    for graph in &fp.graphs {
        if !preparation.allows_iri(&view.read, &graph.graph)? {
            return Ok(Decision::Denied);
        }
        if let Some(rules) = &view.triples {
            for rule in &rules.rules {
                preparation.check()?;
                let allowed = |graphs: &Graphs| {
                    if graph.graph == "default" {
                        preparation.allows(graphs, None)
                    } else {
                        preparation.allows_iri(graphs, &graph.graph)
                    }
                };
                let in_scope = match &rule.protection.graphs {
                    Some(graphs) => allowed(graphs)?,
                    None => true,
                };
                if !in_scope || allowed(&rule.read)? {
                    continue;
                }
                let mut found = false;
                for expected in &graph.protections {
                    if definition(&rule.protection, expected, preparation)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Ok(Decision::Unproven);
                }
                if let Some(pattern) = &rule.protection.pattern
                    && (uses(pattern, "user", preparation)?
                        && !optional_text(
                            rules.caller.user.as_deref(),
                            fp.caller.user.as_deref(),
                            preparation,
                        )?
                        || uses(pattern, "role", preparation)?
                            && !normalized(&rules.caller.roles, &fp.caller.roles, preparation)?
                        || uses(pattern, "group", preparation)?
                            && !normalized(&rules.caller.groups, &fp.caller.groups, preparation)?)
                {
                    return Ok(Decision::Unproven);
                }
            }
        }
    }
    preparation.check()?;
    Ok(Decision::Proven)
}

fn proven(
    name: &str,
    state: &GraphFootprints,
    view: &GraphAccess,
    preparation: &Preparation,
) -> Result<bool> {
    preparation.check()?;
    if name == DISCLOSURE_GRAPH || state.missing || state.footprints.is_empty() {
        return Ok(false);
    }
    for fp in &state.footprints {
        let Some(fp) = fp else {
            return Ok(false);
        };
        if coverage(view, fp, preparation)? != Decision::Proven {
            return Ok(false);
        }
    }
    preparation.check()?;
    Ok(true)
}

fn retained_strings(
    values: &Vec<String>,
    held: &mut Reservation,
    preparation: &Preparation,
) -> Result<()> {
    held.grow_by(
        (values.capacity() as u64)
            .saturating_mul(std::mem::size_of::<String>() as u64)
            .saturating_add(128),
    )?;
    for value in values {
        preparation.check()?;
        held.grow_by(value.capacity() as u64)?;
    }
    Ok(())
}

fn retained_graphs(
    graphs: &FpGraphs,
    held: &mut Reservation,
    preparation: &Preparation,
) -> Result<()> {
    if let FpGraphs::Only {
        iris,
        patterns,
        protected,
        excluded,
        ..
    } = graphs
    {
        for values in [iris, patterns, protected, excluded] {
            retained_strings(values, held, preparation)?;
        }
    }
    preparation.check()
}

fn retained_footprint(
    fp: &Footprint,
    held: &mut Reservation,
    preparation: &Preparation,
) -> Result<()> {
    held.grow_by(1024)?;
    for text in [&fp.dataset, &fp.branch, &fp.principal, &fp.endpoint]
        .into_iter()
        .chain(fp.caller.user.iter())
    {
        preparation.check()?;
        held.grow_by(text.capacity() as u64)?;
    }
    for values in [&fp.caller.roles, &fp.caller.groups, &fp.reports] {
        retained_strings(values, held, preparation)?;
    }
    held.grow_by(
        (fp.graphs.capacity() as u64).saturating_mul(std::mem::size_of::<FpGraph>() as u64),
    )?;
    for graph in &fp.graphs {
        preparation.check()?;
        held.grow_by(graph.graph.capacity() as u64)?;
        held.grow_by(
            (graph.protections.capacity() as u64)
                .saturating_mul(std::mem::size_of::<FpProtection>() as u64),
        )?;
        for protection in &graph.protections {
            preparation.check()?;
            held.grow_by(protection.name.capacity() as u64)?;
            for values in [&protection.predicates, &protection.classes]
                .into_iter()
                .flatten()
            {
                retained_strings(values, held, preparation)?;
            }
            if let Some(pattern) = &protection.pattern {
                held.grow_by(pattern.capacity() as u64)?;
            }
            if let Some(graphs) = &protection.graphs {
                retained_graphs(graphs, held, preparation)?;
            }
            for (key, value) in &protection.prefixes {
                preparation.check()?;
                held.grow_by(
                    (key.capacity() as u64)
                        .saturating_add(value.capacity() as u64)
                        .saturating_add(256),
                )?;
            }
        }
    }
    preparation.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::task::{Control, Progress};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn typed_json_reads_observe_actual_mid_parse_cancellation_and_deadline() {
        let fp = Footprint {
            version: FOOTPRINT_VERSION,
            dataset: "ds".into(),
            branch: "main".into(),
            source_commit: 1,
            principal: "reader".into(),
            caller: FpCaller::default(),
            endpoint: "query".into(),
            graphs: Vec::new(),
            reports: (0..1000).map(|i| format!("urn:report:{i}")).collect(),
        };
        let text = fp.to_json();
        let name = fp.iri();
        for deadline_mode in [false, true] {
            let seen = Arc::new(AtomicUsize::new(0));
            let observed = seen.clone();
            let control = Control::none();
            let cancel = control.cancel.clone();
            let deadline = deadline_mode.then(|| Instant::now() + Duration::from_millis(300));
            let control = Control {
                deadline,
                progress: Progress::new(move |_, phase| {
                    if phase == "reading disclosure footprint"
                        && observed.fetch_add(1, Ordering::SeqCst) == 1
                    {
                        if let Some(deadline) = deadline {
                            while let Some(remaining) =
                                deadline.checked_duration_since(Instant::now())
                            {
                                std::thread::park_timeout(remaining);
                            }
                        } else {
                            cancel.cancel();
                        }
                    }
                }),
                ..control
            };
            let preparation = Preparation::new(control, 8 << 20).unwrap();
            let mut workspace = Workspace::new(&preparation).unwrap();
            let result = parse(&name, &text, &mut workspace);
            assert!(
                seen.load(Ordering::SeqCst) >= 2,
                "no check after1024 bytes consumed by actual Serde parsing"
            );
            if deadline_mode {
                assert!(matches!(result, Err(Error::Timeout)));
            } else {
                assert!(matches!(result, Err(Error::Cancelled)));
            }
        }
        let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
        let mut workspace = Workspace::new(&preparation).unwrap();
        assert_eq!(
            parse(&name, &text, &mut workspace)
                .unwrap()
                .unwrap()
                .as_ref(),
            &fp
        );
    }

    #[test]
    fn graph_collision_buckets_require_full_text_identity() {
        let preparation = Preparation::new(Control::none(), 1 << 20).unwrap();
        let mut workspace = Workspace::new(&preparation).unwrap();
        let mut index = LocalIndex::default();
        let name = "urn:x-sparkles:graphrag:one";
        // Force an occupied digest bucket with a different full name. A digest
        // match must never make that existing record the requested graph.
        index.graphs.insert(
            digest(name, &preparation).unwrap(),
            vec![NamedGraph {
                name: "urn:x-sparkles:graphrag:other".into(),
                state: GraphFootprints {
                    missing: true,
                    ..Default::default()
                },
                covered: HashSet::new(),
            }],
        );
        let actual = graph(&mut index, name, &mut workspace).unwrap();
        assert_eq!(actual.name, name);
        assert!(!actual.state.missing);
        assert_eq!(index.graphs.values().next().unwrap().len(), 2);
    }
}
