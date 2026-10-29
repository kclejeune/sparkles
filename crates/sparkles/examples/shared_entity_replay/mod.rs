//! Private bounded A01 offline replay; supplied declarations convey no authority.
#[allow(dead_code)]
mod evolution;
mod model;
mod output;
mod resolution;

pub(crate) use model::*;
pub(crate) use output::encode_comparison;
#[cfg(test)]
pub(crate) use output::encode_scalar;

pub(crate) use resolution::{resolve, resolve_in_scope};

#[cfg(test)]
pub(crate) use evolution::*;

/// Explicit local delivery entrypoint. No directory scan, default inputs, network
/// fetch, mutable output file or current-pointer update is performed.
pub(crate) fn run_cli(args: &[String]) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    let mut limits = Limits::default();
    let mut view = None;
    let mut scope = None;
    let mut deadline = None;
    let mut documents = Vec::new();
    let mut sources = Vec::new();
    let mut references = Vec::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let value = iter.next().ok_or(Failure::UnknownControlField)?;
        let number = || value.parse::<usize>().map_err(|_| Failure::ReplayLimit);
        match flag.as_str() {
            "--view" => view = Some(value.as_str()),
            "--scope" => scope = Some(value.as_str()),
            "--rdf" => {
                let (format, path) = value.split_once('=').ok_or(Failure::UnknownControlField)?;
                let format = match format {
                    "ttl" => oxrdfio::RdfFormat::Turtle,
                    "nt" => oxrdfio::RdfFormat::NTriples,
                    "trig" => oxrdfio::RdfFormat::TriG,
                    "nq" => oxrdfio::RdfFormat::NQuads,
                    _ => return Err(Failure::UnsupportedProfile),
                };
                documents.push((format, path));
            }
            "--source" => sources.push(value.split_once('=').ok_or(Failure::UnknownControlField)?),
            "--reference" => references.push(value.as_str()),
            "--deadline-ms" => deadline = Some(number()?),
            "--limit-decoded" => limits.decoded = number()?,
            "--limit-source" => limits.source = number()?,
            "--limit-memory" => limits.memory = number()?,
            "--limit-entities" => limits.entities = number()?,
            "--limit-descriptions" => limits.descriptions = number()?,
            "--limit-claims" => limits.claims = number()?,
            "--limit-decisions" => limits.decisions = number()?,
            "--limit-relations" => limits.relations = number()?,
            _ => return Err(Failure::UnknownControlField),
        }
    }
    let view = view.ok_or(Failure::IncompleteInput)?;
    let scope = scope.ok_or(Failure::IncompleteInput)?;
    if documents.is_empty() {
        return Err(Failure::IncompleteInput);
    }
    let mut control = sparkles::task::Control::none();
    if let Some(ms) = deadline {
        control.deadline = Some(
            std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(
                    u64::try_from(ms).map_err(|_| Failure::ReplayLimit)?,
                ))
                .ok_or(Failure::ReplayLimit)?,
        );
    }
    let operation = Operation::new(&control, limits)?;
    let _arguments = operation.charge(args.iter().try_fold(1024usize, |n, s| {
        n.checked_add(s.len() * 4 + 128).ok_or(Failure::ReplayLimit)
    })?)?;
    let mut backing = Vec::new();
    let mut charges = Vec::new();
    let mut decoded = 0usize;
    for (format, path) in documents {
        operation.check()?;
        let mut input = std::fs::File::open(path).map_err(|_| Failure::IncompleteInput)?;
        let mut bytes = Vec::new();
        loop {
            operation.check()?;
            let mut block = [0u8; 8192];
            let n = input
                .read(&mut block)
                .map_err(|_| Failure::IncompleteInput)?;
            if n == 0 {
                break;
            }
            decoded = decoded.checked_add(n).ok_or(Failure::ReplayLimit)?;
            if decoded > limits.decoded {
                return Err(Failure::ReplayLimit);
            }
            charges.push(operation.charge(n * 2 + 64)?);
            bytes.extend_from_slice(&block[..n]);
        }
        backing.push((bytes, format));
    }
    let documents: Vec<_> = backing
        .iter()
        .map(|(bytes, format)| (bytes.as_slice(), *format))
        .collect();
    let graph = Graph::load_documents(&documents, &operation)?;
    let mut retained = RetainedSources::default();
    for (revision, path) in sources {
        operation.check()?;
        let input = std::fs::File::open(path).map_err(|_| Failure::IncompleteInput)?;
        retained.insert(&graph, revision, input, &operation)?;
    }
    let manifest = graph.one_iri(view, &format!("{SR}inputManifest"))?;
    retained.verify_selected(&graph, &manifest, &operation)?;
    for reference in references {
        retained.verify_reference(&graph, reference, &operation)?;
    }
    let result = resolve_in_scope(&graph, view, scope, &operation)?;
    encode_resolved(&result.comparison, &operation)
}

/// Reserve normalization maps, cloned UTF-8 values and the final encoder buffer
/// before entering the unchanged fixed-byte encoder.
pub(crate) fn encode_resolved(
    value: &Comparison,
    operation: &Operation<'_>,
) -> Result<Vec<u8>, Failure> {
    let mut estimate = 8192usize;
    let mut add = |s: &str| -> Result<(), Failure> {
        estimate = estimate
            .checked_add(
                s.len()
                    .checked_mul(16)
                    .and_then(|n| n.checked_add(512))
                    .ok_or(Failure::ReplayLimit)?,
            )
            .ok_or(Failure::ReplayLimit)?;
        Ok(())
    };
    add(&value.scope)?;
    for id in &value.input_ids {
        add(id)?;
    }
    for row in &value.assignments {
        operation.check()?;
        add(&row.entity)?;
        if let Some(id) = &row.representative {
            add(id)?;
        }
        for id in &row.decisions {
            add(id)?;
        }
    }
    for row in &value.conflicts {
        operation.check()?;
        for id in row.entities.iter().chain(&row.decisions) {
            add(id)?;
        }
    }
    for row in &value.unknowns {
        operation.check()?;
        add(&row.record)?;
    }
    let _encoding = operation.charge(estimate)?;
    encode_comparison(value, OutputLimit(estimate / 2), operation.control)
}
