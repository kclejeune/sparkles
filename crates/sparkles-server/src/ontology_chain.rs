//! `sparkles ontology reason`, `reduce`, `relax` and `materialize` with ROBOT 1.9.11's
//! options (spec F24), and chains of them with `classify` and `check` in one process, as
//! `sparkles ontology reason --input x.owl relax reduce --output y.owl`, where each
//! command takes the ontology the one before it left in memory.
//!
//! Each command ends in a hidden list of the arguments after its own, which is parsed
//! again as the next command, so that an option's value that is also a command's name
//! stays the option's value.

use crate::ontology_cmd::{Read, classify_body_of_file, finish_check, finish_classify};
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use sparkles::handles::ontology::classify::ClassifyRequest;
use sparkles::handles::ontology::transform::{OutputFormat, TransformError, WorkingOntology};
use sparkles::handles::ontology::{self as onto, CheckRequest};
use sparkles::owl::ObjectProperty;
use sparkles::owl::transform::{
    EquivalentClasses, Error as TError, Generator, MaterializeOptions, NewOntology, ReasonOptions,
    ReduceOptions, RelaxOptions, Report, Tautologies, parse_terms,
};
use sparkles::sparql::QueryOptions;
use sparkles::store::StoreOptions;
use std::path::{Path, PathBuf};

/// Where a chain's first command reads its ontology, where any command writes it, and
/// the rest of the chain.
#[derive(Args, Debug, Clone)]
pub struct Io {
    /// The ontology file to read (Turtle, RDF/XML, OWL functional-style syntax,
    /// N-Triples, … by its extension); only on the first command of a chain
    #[arg(short = 'i', long)]
    pub input: Option<PathBuf>,
    /// A database directory to read the ontology from, on the first command of a chain
    #[arg(long, conflicts_with = "input")]
    pub loc: Option<PathBuf>,
    /// Write the ontology to this file after the command, in the syntax its extension
    /// names: .ofn, .owl (RDF/XML), .ttl or .nt, each optionally .gz (repeatable)
    #[arg(short = 'o', long)]
    pub output: Vec<PathBuf>,
    #[command(flatten)]
    pub read: Read,
    /// Worker threads of the classification (0: the algorithm pool's share for one call)
    #[arg(long, default_value_t = 0)]
    pub threads: usize,
    /// The commands that follow in the chain
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    pub rest: Vec<String>,
}

/// `reason`, with ROBOT's options. Each takes a value, as ROBOT's do.
#[derive(Args, Debug, Clone)]
pub struct ReasonArgs {
    /// The reasoner. ELK, and emr, which asserts named superclasses only, run the OWL 2
    /// EL rules alone and give ROBOT's output with ELK. Without it the classifier also
    /// takes the Horn axioms outside OWL 2 EL, and hermit, jfact, whelk and structural
    /// run it so with a warning
    #[arg(short = 'r', long)]
    pub reasoner: Option<String>,
    /// The inferred axiom generators, space separated: SubClass, EquivalentClass,
    /// ClassAssertion, and ROBOT's others, which ELK answers none of
    #[arg(short = 'A', long, default_value = "subclass")]
    pub axiom_generators: String,
    /// true: direct and indirect superclasses and types
    #[arg(short = 'd', long, default_value = "false")]
    pub include_indirect: String,
    /// all, none or asserted-only (true and false for all and none)
    #[arg(short = 'e', long, default_value = "all")]
    pub equivalent_classes_allowed: String,
    /// false, structural or all
    #[arg(short = 't', long, default_value = "false")]
    pub exclude_tautologies: String,
    /// true: add no inferred axiom already asserted, and none with owl:Thing
    #[arg(short = 'x', long, default_value = "false")]
    pub exclude_duplicate_axioms: String,
    /// true: add no inferred axiom with owl:Thing
    #[arg(short = 'T', long, default_value = "false")]
    pub exclude_owl_thing: String,
    /// true: add no inferred class axiom whose classes the ontology does not declare
    #[arg(short = 'X', long, default_value = "false")]
    pub exclude_external_entities: String,
    /// false: keep the asserted subclass axioms that are not direct
    #[arg(short = 's', long, default_value = "true")]
    pub remove_redundant_subclass_axioms: String,
    /// true: keep annotated subclass axioms when removing redundant ones
    #[arg(short = 'p', long, default_value = "false")]
    pub preserve_annotated_axioms: String,
    /// true: annotate each inferred subclass axiom with oboInOwl:is_inferred "true"
    #[arg(short = 'a', long, default_value = "false")]
    pub annotate_inferred_axioms: String,
    /// true: the output holds the inferred axioms alone
    #[arg(short = 'n', long, default_value = "false")]
    pub create_new_ontology: String,
    /// true: the output holds the inferred axioms and the annotation assertions
    #[arg(short = 'm', long, default_value = "false")]
    pub create_new_ontology_with_annotations: String,
    /// Write the module of the unsatisfiable classes to this file when there are any
    #[arg(short = 'D', long)]
    pub dump_unsatisfiable: Option<PathBuf>,
    /// true is not supported: the invalid reference check is not implemented
    #[arg(long, default_value = "false")]
    pub prevent_invalid_references: String,
    #[command(flatten)]
    pub io: Io,
}

/// `reduce`, with ROBOT's options.
#[derive(Args, Debug, Clone)]
pub struct ReduceArgs {
    /// The reasoner, as for reason (emr is not one)
    #[arg(short = 'r', long)]
    pub reasoner: Option<String>,
    /// true: keep annotated subclass axioms
    #[arg(short = 'p', long, default_value = "false")]
    pub preserve_annotated_axioms: String,
    /// true: only subclass axioms between named classes
    #[arg(short = 'c', long, default_value = "false")]
    pub named_classes_only: String,
    /// true: take subproperties and property chains into account
    #[arg(short = 's', long, default_value = "false")]
    pub include_subproperties: String,
    #[command(flatten)]
    pub io: Io,
}

/// `relax`, with ROBOT's options, which take exactly true or false.
#[derive(Args, Debug, Clone)]
pub struct RelaxArgs {
    /// true: also relax subclass axioms with a complex superclass
    #[arg(short = 's', long, default_value = "false")]
    pub include_subclass_of: String,
    /// false: also relax the named classes of equivalences
    #[arg(long, default_value = "true")]
    pub exclude_named_classes: String,
    /// true: only existentials of a named property and a named class
    #[arg(long, default_value = "false")]
    pub enforce_obo_format: String,
    #[command(flatten)]
    pub io: Io,
}

/// `materialize`, with ROBOT's options.
#[derive(Args, Debug, Clone)]
pub struct MaterializeArgs {
    /// The reasoner, as for reduce: ELK runs the classifier
    #[arg(short = 'r', long, default_value = "ELK")]
    pub reasoner: String,
    /// A property whose existential superclasses to assert, as an IRI or an OBO CURIE such
    /// as BFO:0000050 (repeatable); with none, every object property of the ontology
    #[arg(short = 't', long)]
    pub term: Vec<String>,
    /// A file of such properties, one a line, with # comments (repeatable)
    #[arg(short = 'T', long)]
    pub term_file: Vec<PathBuf>,
    /// true: validate only, as ROBOT, which adds the axioms to an ontology it does not save
    #[arg(short = 'n', long, default_value = "false")]
    pub create_new_ontology: String,
    /// Accepted and ignored, as ROBOT's materialize ignores it
    #[arg(short = 's', long, default_value = "true")]
    pub remove_redundant_subclass_axioms: String,
    /// Accepted and ignored, as ROBOT's materialize ignores it
    #[arg(short = 'a', long, default_value = "false")]
    pub annotate_inferred_axioms: String,
    #[command(flatten)]
    pub io: Io,
}

/// `classify` as a later command of a chain.
#[derive(Args, Debug, Clone)]
pub struct ClassifyStep {
    /// Classify the assertions too: the types of each individual
    #[arg(long)]
    pub assertions: bool,
    /// Describe this class
    #[arg(long)]
    pub about: Option<String>,
    /// Exit with status 0 when classes are unsatisfiable but the ontology is consistent
    #[arg(long)]
    pub allow_unsatisfiable: bool,
    /// ofn (the taxonomy as a functional-style document), json or text
    #[arg(long, default_value = "ofn")]
    pub format: String,
    #[command(flatten)]
    pub io: Io,
}

/// `check` as a later command of a chain.
#[derive(Args, Debug, Clone)]
pub struct CheckStep {
    /// The profiles: el, ql, rl, dl or all, comma separated
    #[arg(long, default_value = "dl")]
    pub profile: String,
    /// Check the terminology alone, not the assertions
    #[arg(long)]
    pub no_assertions: bool,
    /// The most violations each report lists
    #[arg(long, default_value_t = 1000)]
    pub max_violations: usize,
    /// text or json
    #[arg(long, default_value = "text")]
    pub format: String,
    #[command(flatten)]
    pub io: Io,
}

/// A command of a chain.
#[derive(Subcommand, Debug, Clone)]
pub enum Step {
    Reason(Box<ReasonArgs>),
    Reduce(ReduceArgs),
    Relax(RelaxArgs),
    Materialize(MaterializeArgs),
    Classify(ClassifyStep),
    Check(CheckStep),
}

#[derive(Parser, Debug)]
#[command(name = "sparkles ontology", no_binary_name = false)]
struct Next {
    #[command(subcommand)]
    step: Step,
}

impl Step {
    fn io(&self) -> &Io {
        match self {
            Step::Reason(a) => &a.io,
            Step::Reduce(a) => &a.io,
            Step::Relax(a) => &a.io,
            Step::Materialize(a) => &a.io,
            Step::Classify(a) => &a.io,
            Step::Check(a) => &a.io,
        }
    }
}

/// Run a chain from its first command.
pub fn run(first: Step, opts: StoreOptions) -> Result<()> {
    let mut state: Option<WorkingOntology> = None;
    let rest = first.io().rest.clone();
    if !run_step(first, &mut state, &opts)? {
        std::process::exit(1);
    }
    follow(state, rest, &opts)
}

/// Continue a chain whose first command, `classify FILE` or `check FILE`, has answered
/// from the file as it does alone (`ok` is its answer): read FILE as the working
/// ontology, write it to `output`, and run the commands of `rest` on it.
pub fn resume(
    file: &Path,
    read: &Read,
    threads: usize,
    output: Vec<PathBuf>,
    rest: Vec<String>,
    ok: bool,
    opts: StoreOptions,
) -> Result<()> {
    for o in &output {
        OutputFormat::of(o)?;
    }
    if !ok {
        std::process::exit(1);
    }
    let q = QueryOptions::default();
    let w = WorkingOntology::read(file, &read.request()?, threads, &q)
        .with_context(|| format!("cannot read {}", file.display()))?;
    for o in &output {
        w.write(o)
            .with_context(|| format!("cannot write {}", o.display()))?;
    }
    follow(Some(w), rest, &opts)
}

/// Run the commands of `rest` on the working ontology, one after another.
fn follow(
    mut state: Option<WorkingOntology>,
    mut rest: Vec<String>,
    opts: &StoreOptions,
) -> Result<()> {
    while !rest.is_empty() {
        let args = std::iter::once("sparkles ontology".to_string()).chain(rest);
        let step = match Next::try_parse_from(args) {
            Ok(n) => n.step,
            Err(e) => e.exit(),
        };
        rest = step.io().rest.clone();
        if !run_step(step, &mut state, opts)? {
            std::process::exit(1);
        }
    }
    Ok(())
}

/// One command: false when it ends the chain with exit status 1, as a classification
/// or check that fails does.
fn run_step(step: Step, state: &mut Option<WorkingOntology>, opts: &StoreOptions) -> Result<bool> {
    let io = step.io().clone();
    for o in &io.output {
        OutputFormat::of(o)?;
    }
    let q = QueryOptions::default();
    let mut w = match state.take() {
        Some(w) => {
            if io.input.is_some() || io.loc.is_some() {
                bail!("CHAINED INPUT ERROR do not use an --input option for chained commands");
            }
            w
        }
        None => read_input(&io, opts, &q)?,
    };
    let c = ClassifyRequest {
        assertions: true,
        threads: io.threads,
        ..Default::default()
    };
    let mut ok = true;
    match step {
        Step::Reason(a) => {
            let o = reason_options(&a)?;
            let c = ClassifyRequest {
                el_only: reasoner(a.reasoner.as_deref(), true)?,
                ..c.clone()
            };
            match w.reason(&o, &c, &q) {
                Ok(r) => log(&r),
                Err(e) => return Err(failed(e, a.dump_unsatisfiable.as_deref(), &w)),
            }
        }
        Step::Reduce(a) => {
            let c = ClassifyRequest {
                el_only: reasoner(a.reasoner.as_deref(), false)?,
                ..c.clone()
            };
            let o = ReduceOptions {
                preserve_annotated_axioms: is_true(&a.preserve_annotated_axioms),
                named_classes_only: is_true(&a.named_classes_only),
                include_subproperties: is_true(&a.include_subproperties),
            };
            match w.reduce(&o, &c, &q) {
                Ok(r) => log(&r),
                Err(e) => return Err(failed(e, None, &w)),
            }
        }
        Step::Relax(a) => {
            let o = RelaxOptions {
                include_subclass_of: boolean(&a.include_subclass_of, "include-subclass-of")?,
                exclude_named_classes: boolean(&a.exclude_named_classes, "exclude-named-classes")?,
                enforce_obo_format: boolean(&a.enforce_obo_format, "enforce-obo-format")?,
            };
            log(&w.relax(&o));
        }
        Step::Materialize(a) => {
            // ELK by default, as ROBOT's materialize, so the EL rules alone
            let c = ClassifyRequest {
                el_only: reasoner(Some(&a.reasoner), false)?,
                ..c.clone()
            };
            let o = materialize_options(&a)?;
            match w.materialize(&o, &c, &q) {
                Ok(r) => log(&r),
                Err(e) => return Err(failed(e, None, &w)),
            }
        }
        Step::Classify(a) => {
            let about = a
                .about
                .as_deref()
                .map(|s| s.trim_start_matches('<').trim_end_matches('>').to_string());
            let c = ClassifyRequest {
                about: about.clone(),
                assertions: a.assertions,
                ..c.clone()
            };
            let file = w.to_file()?;
            let (j, doc) = classify_body_of_file(&file, &c, &io.read, about)?;
            for o in &io.output {
                w.write(o)?;
            }
            ok = finish_classify(&j, &doc, &a.format, a.allow_unsatisfiable)?;
            *state = Some(w);
            return Ok(ok);
        }
        Step::Check(a) => {
            let profiles = onto::parse_profiles(&a.profile)?;
            if profiles.is_empty() {
                bail!("--profile names no profile: el, ql, rl, dl or all");
            }
            let check = CheckRequest {
                profiles,
                assertions: !a.no_assertions,
                max_violations: a.max_violations.max(1),
            };
            let file = w.to_file()?;
            let req = io.read.request()?;
            let reports = file.check(&check, &q)?;
            let j = sparkles::handles::ontology::wire::reports_body(
                &file.name,
                &file.view(),
                &req,
                &file.renderer(),
                &reports,
                check.assertions,
            );
            for o in &io.output {
                w.write(o)?;
            }
            ok = finish_check(&j, &a.format)?;
            *state = Some(w);
            return Ok(ok);
        }
    }
    for o in &io.output {
        w.write(o)
            .with_context(|| format!("cannot write {}", o.display()))?;
    }
    *state = Some(w);
    Ok(ok)
}

fn read_input(io: &Io, opts: &StoreOptions, q: &QueryOptions) -> Result<WorkingOntology> {
    let req = io.read.request()?;
    if let Some(f) = &io.input {
        return WorkingOntology::read(f, &req, 0, q)
            .with_context(|| format!("cannot read {}", f.display()));
    }
    if let Some(loc) = &io.loc {
        let ds = crate::open_dataset(loc, opts.clone())?;
        let o = ds.ontology().open(&req, q)?;
        let name = loc.file_name().map_or_else(
            || loc.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        return Ok(WorkingOntology::from_opened(name, &o, ds.prefixes(), q)?);
    }
    bail!("name the ontology to read: --input FILE or --loc DIR on the first command")
}

/// ROBOT's `OptionsHelper.optionIsTrue`: `true` or `yes` in any case.
fn is_true(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    s == "true" || s == "yes"
}

/// ROBOT's `CommandLineHelper.getBooleanValue`: exactly `true` or `false`.
fn boolean(s: &str, name: &str) -> Result<bool> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => bail!("BOOLEAN VALUE ERROR arg for {name} must be true or false"),
    }
}

/// ROBOT's reasoner names, and whether the run takes the OWL 2 EL rules alone. ELK and
/// emr, which ROBOT builds on ELK, do, so that the output is ROBOT's with ELK. Without a
/// name the classifier also takes the Horn axioms outside OWL 2 EL, and the DL
/// reasoners' names run it so with a warning. An unknown name is refused.
fn reasoner(name: Option<&str>, emr: bool) -> Result<bool> {
    let Some(name) = name else {
        return Ok(false);
    };
    let n = name.trim().to_lowercase();
    match n.as_str() {
        "elk" => Ok(true),
        "hermit" | "jfact" | "whelk" | "structural" => {
            eprintln!(
                "WARN --reasoner {name} is ignored: the classifier runs with its Horn rules in its place"
            );
            Ok(false)
        }
        "emr" if emr => {
            eprintln!(
                "WARN --reasoner {name} is ignored: the OWL 2 EL classifier runs, as ELK would, and asserts named superclasses only"
            );
            Ok(true)
        }
        _ => bail!("INVALID REASONER ERROR unknown reasoner: {n}"),
    }
}

fn reason_options(a: &ReasonArgs) -> Result<ReasonOptions> {
    let new = is_true(&a.create_new_ontology);
    let with = is_true(&a.create_new_ontology_with_annotations);
    if a.create_new_ontology.eq_ignore_ascii_case("true")
        && a.create_new_ontology_with_annotations
            .eq_ignore_ascii_case("true")
    {
        bail!(
            "CREATE ONTOLOGY ERROR 'create-new-ontology' and 'create-new-ontology-with-annotations' cannot both be set to true."
        );
    }
    let equivalent_classes =
        EquivalentClasses::parse(&a.equivalent_classes_allowed).map_err(|e| anyhow!(e))?;
    if is_true(&a.prevent_invalid_references) {
        bail!(
            "--prevent-invalid-references true is not supported: the invalid reference check is not implemented"
        );
    }
    Ok(ReasonOptions {
        generators: Generator::parse_list(&a.axiom_generators).map_err(|e| anyhow!(e))?,
        include_indirect: is_true(&a.include_indirect),
        equivalent_classes,
        tautologies: Tautologies::parse(&a.exclude_tautologies),
        exclude_duplicate_axioms: is_true(&a.exclude_duplicate_axioms),
        exclude_owl_thing: is_true(&a.exclude_owl_thing),
        exclude_external_entities: is_true(&a.exclude_external_entities),
        remove_redundant_subclass_axioms: is_true(&a.remove_redundant_subclass_axioms),
        preserve_annotated_axioms: is_true(&a.preserve_annotated_axioms),
        annotate_inferred_axioms: is_true(&a.annotate_inferred_axioms),
        // ROBOT reads `with annotations` first
        new_ontology: if with {
            NewOntology::WithAnnotations
        } else if new {
            NewOntology::Empty
        } else {
            NewOntology::No
        },
        dump_unsatisfiable: a.dump_unsatisfiable.is_some(),
    })
}

/// The options of `materialize`: the properties of `--term` and `--term-file`, read as
/// ROBOT reads term lists, with its warning for each term that is not an IRI.
fn materialize_options(a: &MaterializeArgs) -> Result<MaterializeOptions> {
    let mut warnings = Vec::new();
    let mut iris = std::collections::BTreeSet::new();
    for t in &a.term {
        iris.extend(parse_terms(t, &mut warnings));
    }
    for f in &a.term_file {
        let text = std::fs::read_to_string(f)
            .with_context(|| format!("cannot read the term file {}", f.display()))?;
        iris.extend(parse_terms(&text, &mut warnings));
    }
    for w in &warnings {
        eprintln!("WARN {w}");
    }
    if iris.is_empty() && !(a.term.is_empty() && a.term_file.is_empty()) {
        eprintln!(
            "WARN no term is a valid IRI: every object property of the ontology is materialized, as ROBOT does"
        );
    }
    Ok(MaterializeOptions {
        properties: iris.into_iter().map(ObjectProperty::new).collect(),
        create_new_ontology: is_true(&a.create_new_ontology),
    })
}

/// The warnings of a run, on standard error.
fn log(r: &Report) {
    for w in &r.warnings {
        eprintln!("WARN {w}");
    }
    if !r.excluded.is_empty() {
        eprintln!(
            "WARN the classifier left out {} axioms outside OWL 2 EL or used them only in part",
            r.excluded.len()
        );
    }
}

/// The error of a failed transformation, after writing the module of the unsatisfiable
/// classes when asked.
fn failed(e: TransformError, dump: Option<&Path>, w: &WorkingOntology) -> anyhow::Error {
    if let (
        TError::UnsatisfiableClasses {
            module: Some(m), ..
        },
        Some(path),
    ) = (&e, dump)
    {
        let written = OutputFormat::of(path)
            .map_err(anyhow::Error::from)
            .and_then(|(f, gz)| {
                sparkles::handles::ontology::transform::write_axioms(
                    path,
                    f,
                    gz,
                    &w.prefixes,
                    &Default::default(),
                    m.iter(),
                )
                .map_err(anyhow::Error::from)
            });
        if let Err(x) = written {
            eprintln!(
                "WARN cannot write the unsatisfiable module to {}: {x}",
                path.display()
            );
        }
    }
    match e {
        TError::Classifier(e) => anyhow::Error::from(e),
        e => anyhow!("{e}"),
    }
}
