//! Private direct-mapping orchestration retaining the binding's write controls.
// Keep the engine's existing conversion and loader paths. Dataset::run_mapping
// does not currently carry its opaque RunOptions.check into the final load.
pub(super) fn run(
    ds: &sparkles::Dataset,
    text: &str,
    kind: Option<sparkles::mapping::MappingKind>,
    inputs: &sparkles::mapping::Inputs,
    options: &sparkles::mapping::RunOptions,
    graph: Option<&str>,
    write: &sparkles::guard::WriteOptions,
) -> sparkles::Result<sparkles::handles::MappingRun> {
    use sparkles::mapping::{self, MappingKind};
    write.check()?;
    let kind = match kind.or_else(|| mapping::detect(text)) {
        Some(kind @ (MappingKind::Rml | MappingKind::Yarrrml)) => kind,
        Some(_) => {
            return Err(sparkles::Error::invalid(
                "CSVW metadata and CONSTRUCT templates map CSV files: load the table with the mapping or template instead",
            ));
        }
        None => {
            return Err(sparkles::Error::invalid(
                "the text is not an R2RML, RML or YARRRML mapping",
            ));
        }
    };
    let graph = graph
        .map(oxrdf::NamedNode::new)
        .transpose()
        .map_err(|e| sparkles::Error::invalid(e.to_string()))?;
    let mapping = mapping::parse(text, kind, None)?;
    let (output, stats) = mapping::to_nquads_file(&mapping, inputs, options, None)?;
    let receipt = ds.store().load_with(
        &[mapping::source(&output, graph, "mapping")],
        sparkles::commit::CommitKind::Load,
        write,
    )?;
    Ok(sparkles::handles::MappingRun { receipt, stats })
}
