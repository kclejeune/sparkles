//! Ontologies that commands transform in memory (spec F24): `reason`, `reduce`, `relax`
//! and `materialize` with the meaning of ROBOT 1.9.11's commands, run on an ontology read from a
//! file or a database and handed from one command of a chain to the next without a
//! file, and the writers of the result. The transformations are those of
//! [`sparkles_owl::transform`], and the classification is the OWL 2 EL classifier's,
//! with the threads, deadline and budgets of the caller's options.

use super::classify::{ClassifyRequest, classify_once};
use super::file::OntologyFile;
use super::{OntologyRequest, Opened};
use crate::Dataset;
use crate::error::{Error, Result};
use crate::sparql::QueryOptions;
use sparkles_owl::classify::{Classification, Excluded, Unmapped};
use sparkles_owl::ofn::Renderer;
use sparkles_owl::transform::{
    self, Document, MaterializeOptions, ReasonOptions, ReduceOptions, RelaxOptions, Report,
};
use sparkles_owl::{Axiom, AxiomKind};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// A transformation's failure: the classifier's error, or ROBOT's refusal.
pub type TransformError = transform::Error<Error>;

/// An ontology held in memory between commands.
pub struct WorkingOntology {
    /// the name of the input in messages
    pub name: String,
    pub doc: Document,
    /// the prefixes of the input, which the writers use
    pub prefixes: BTreeMap<String, String>,
    /// the triples of the input that map to no axiom, which every classification of the
    /// ontology counts as left out
    pub unmapped: Unmapped,
}

impl WorkingOntology {
    /// Read an ontology file as `req` asks. A functional-style document read as one
    /// default graph keeps its axioms as written,
    /// each with its annotations and an n-ary axiom as one, as OWL API reads them. A file
    /// of triples is read without a store when it is one graph ([`OntologyFile::read`]),
    /// else through a database in memory, as `ontology classify FILE` reads it.
    pub fn read(
        path: &Path,
        req: &OntologyRequest,
        threads: usize,
        opts: &QueryOptions,
    ) -> Result<WorkingOntology> {
        req.validate()?;
        let source = crate::io::Source::from_path(path, None)?;
        // the native read answers the default graph of the file, leniently, at its only
        // state; any other request goes through the readers that honour it
        let plain = req.at.is_none()
            && !req.inferences
            && req.graphs() == [sparkles_reasoner::inputs::GraphRef::Default]
            && req.mode == sparkles_owl::read::Mode::Lenient;
        if plain && source.format == crate::io::RdfSyntax::OwlFunctional {
            let threads = match threads {
                0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
                n => n,
            };
            let (doc, _) = crate::io::read_ofn(&source, threads)?;
            return Ok(WorkingOntology {
                name: path.display().to_string(),
                doc: Document::new(doc.header, doc.axioms),
                prefixes: doc.prefixes.into_iter().collect(),
                unmapped: Unmapped::default(),
            });
        }
        if let Some(file) = OntologyFile::read(path, req, threads)? {
            let prefixes = file.prefixes();
            let name = file.name.clone();
            let (doc, unmapped) = file.into_counted_document(opts)?;
            return Ok(WorkingOntology {
                name,
                doc,
                prefixes,
                unmapped,
            });
        }
        let ds = Dataset::memory();
        ds.store().load(&[source])?;
        let opened = ds.ontology().open(req, opts)?;
        WorkingOntology::from_opened(path.display().to_string(), &opened, ds.prefixes(), opts)
    }

    /// The ontology a database's open ontology holds: its terminology and the assertions
    /// that map to axioms.
    pub fn from_opened(
        name: String,
        o: &Opened,
        prefixes: BTreeMap<String, String>,
        opts: &QueryOptions,
    ) -> Result<WorkingOntology> {
        let mut axioms: Vec<Axiom> = o.ontology.axioms.keys().cloned().collect();
        let mut unread = Unmapped::default();
        o.assertions(opts, |ax, t, _| {
            match ax {
                Some(ax) => axioms.push(ax),
                None => unread.push(t),
            }
            Ok(true)
        })?;
        let unmapped =
            super::classify::unmapped_of(&o.ontology, &unread, &std::cell::RefCell::new(None));
        Ok(WorkingOntology {
            name,
            doc: Document::new(o.ontology.header.clone(), axioms),
            prefixes,
            unmapped,
        })
    }

    /// The ontology as a file that `classify` and `check` read, whose classifications
    /// count the input's triples that map to no axiom as left out.
    pub fn to_file(&self) -> Result<OntologyFile> {
        OntologyFile::from_document(
            self.name.clone(),
            self.doc.clone(),
            self.prefixes.clone(),
            self.unmapped.clone(),
        )
    }

    /// A renderer with the input's prefixes.
    pub fn renderer(&self) -> Renderer {
        Renderer::new(self.prefixes.clone())
    }

    /// `reason`. The ontology is unchanged when it fails.
    pub fn reason(
        &mut self,
        o: &ReasonOptions,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> std::result::Result<Report, TransformError> {
        transform::reason(&mut self.doc, o, &mut classifier(c, opts, &self.unmapped))
    }

    /// `reduce`.
    pub fn reduce(
        &mut self,
        o: &ReduceOptions,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> std::result::Result<Report, TransformError> {
        transform::reduce(&mut self.doc, o, &mut classifier(c, opts, &self.unmapped))
    }

    /// `materialize`. The ontology is unchanged when it fails.
    pub fn materialize(
        &mut self,
        o: &MaterializeOptions,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> std::result::Result<Report, TransformError> {
        transform::materialize(&mut self.doc, o, &mut classifier(c, opts, &self.unmapped))
    }

    /// `relax`, which classifies nothing.
    pub fn relax(&mut self, o: &RelaxOptions) -> Report {
        transform::relax(&mut self.doc, o)
    }

    /// Write the ontology to `path` in the syntax its extension names.
    pub fn write(&self, path: &Path) -> Result<()> {
        let _ = self.write_reported(path)?;
        Ok(())
    }

    /// Write the ontology to `path` in the syntax its extension names, returning the
    /// writer's report of written and any lossy/skipped axioms.
    pub fn write_reported(&self, path: &Path) -> Result<sparkles_owl::WriteReport> {
        let (format, gz) = OutputFormat::of(path)?;
        write_axioms_reported(
            path,
            format,
            gz,
            &self.prefixes,
            &self.doc.header,
            &self.doc.axioms,
        )
    }
}

/// The classification of a transformation: the classifier with the request's threads
/// and the options' deadline and budgets. A transformation asserts what the taxonomy
/// says, so it never takes a partial answer, and a stopped run fails. A strict request
/// fails when the classification leaves an axiom out, or the `unmapped` triples of the
/// input that map to no axiom.
fn classifier<'a>(
    c: &'a ClassifyRequest,
    opts: &'a QueryOptions,
    unmapped: &'a Unmapped,
) -> impl FnMut(&[&AxiomKind]) -> Result<(Classification, Vec<Excluded>)> + 'a {
    // a transformation reads the taxonomy and what was left out, not which classes keep
    // their answer
    let c = ClassifyRequest {
        partial: false,
        skip_coverage: true,
        ..c.clone()
    };
    move |kinds: &[&AxiomKind]| {
        let (classification, excluded) =
            classify_once(kinds, &c, opts, None, &|| unmapped.clone())?;
        if c.strict && !classification.complete {
            return Err(super::classify::strict_refusal(&classification, &excluded));
        }
        Ok((classification, excluded))
    }
}

/// The syntaxes the writers write, by the extensions ROBOT reads them from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// `.ofn`
    Functional,
    /// `.owl`, as ROBOT writes it
    RdfXml,
    /// `.ttl`
    Turtle,
    /// `.nt`
    NTriples,
    /// `.owx` (OWL/XML)
    OwlXml,
    /// `.omn` (Manchester OWL Syntax)
    Manchester,
    /// `.obo` (OBO 1.4)
    Obo,
    /// `.json` (OBO Graphs JSON)
    OboGraphsJson,
}

impl OutputFormat {
    /// The syntax of an output path by its extension, after a `.gz` that compresses
    /// it, and whether it is compressed.
    pub fn of(path: &Path) -> Result<(OutputFormat, bool)> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let (name, gz) = match name.strip_suffix(".gz") {
            Some(n) => (n, true),
            None => (name, false),
        };
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
        let f = match ext.as_str() {
            "ofn" => OutputFormat::Functional,
            "owl" | "rdf" => OutputFormat::RdfXml,
            "ttl" => OutputFormat::Turtle,
            "nt" => OutputFormat::NTriples,
            "owx" => OutputFormat::OwlXml,
            "omn" => OutputFormat::Manchester,
            "obo" => OutputFormat::Obo,
            "json" => OutputFormat::OboGraphsJson,
            _ => {
                return Err(Error::invalid(format!(
                    "io#INVALID FORMAT ERROR unknown format: {ext}"
                )));
            }
        };
        Ok((f, gz))
    }

    /// Human-readable syntax name for diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            OutputFormat::Functional => "OWL functional-style syntax (.ofn)",
            OutputFormat::RdfXml => "RDF/XML (.owl)",
            OutputFormat::Turtle => "Turtle (.ttl)",
            OutputFormat::NTriples => "N-Triples (.nt)",
            OutputFormat::OwlXml => "OWL/XML (.owx)",
            OutputFormat::Manchester => "Manchester OWL syntax (.omn)",
            OutputFormat::Obo => "OBO 1.4 (.obo)",
            OutputFormat::OboGraphsJson => "OBO Graphs JSON (.json)",
        }
    }
}

/// Write a header and axioms to `path` in `format`.
pub fn write_axioms<'a>(
    path: &Path,
    format: OutputFormat,
    gz: bool,
    prefixes: &BTreeMap<String, String>,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> Result<()> {
    let _ = write_axioms_reported(path, format, gz, prefixes, header, axioms)?;
    Ok(())
}

/// Write a header and axioms to `path` in `format`, returning the writer's report of
/// written and any skipped axioms.
pub fn write_axioms_reported<'a>(
    path: &Path,
    format: OutputFormat,
    gz: bool,
    prefixes: &BTreeMap<String, String>,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> Result<sparkles_owl::WriteReport> {
    let io = |e: std::io::Error| Error::invalid(format!("cannot write {}: {e}", path.display()));
    let werr = |e: sparkles_owl::WriteError| {
        Error::invalid(format!("cannot write {}: {e}", path.display()))
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(io)?;
    // Stage output beside the destination so a writer or gzip error never truncates or
    // overwrites an existing file. Each layer is finished and flushed by hand so an error
    // writing the gzip trailer or final buffer fails the command.
    let report = {
        let mut buf = std::io::BufWriter::with_capacity(1 << 20, tmp.as_file_mut());
        if gz {
            let mut enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
            let report = write_body(&mut enc, format, prefixes, header, axioms).map_err(werr)?;
            let mut buf = enc.finish().map_err(io)?;
            buf.flush().map_err(io)?;
            report
        } else {
            let report = write_body(&mut buf, format, prefixes, header, axioms).map_err(werr)?;
            buf.flush().map_err(io)?;
            report
        }
    };
    tmp.as_file().sync_all().map_err(io)?;
    tmp.persist(path).map_err(|e| io(e.error))?;
    Ok(report)
}

/// The document in the syntax `format` names.
fn write_body<'a>(
    out: &mut dyn Write,
    format: OutputFormat,
    prefixes: &BTreeMap<String, String>,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> std::result::Result<sparkles_owl::WriteReport, sparkles_owl::WriteError> {
    let r = Renderer::new(prefixes.clone());
    match format {
        OutputFormat::Functional => write_functional(out, &r, header, axioms).map_err(Into::into),
        OutputFormat::OwlXml => sparkles_owl::owx::write_document(out, &r, header, axioms),
        OutputFormat::Manchester => sparkles_owl::manchester::write_document(
            out,
            &r,
            header,
            axioms,
            sparkles_owl::WriteOptions::default(),
        ),
        OutputFormat::Obo => sparkles_owl::obo::write_document(
            out,
            &r,
            header,
            axioms,
            sparkles_owl::WriteOptions::default(),
        ),
        OutputFormat::OboGraphsJson => sparkles_owl::obographs::write_document(
            out,
            &r,
            header,
            axioms,
            sparkles_owl::WriteOptions::default(),
        ),
        OutputFormat::RdfXml | OutputFormat::Turtle | OutputFormat::NTriples => {
            let rdf = match format {
                OutputFormat::RdfXml => oxrdfio::RdfFormat::RdfXml,
                OutputFormat::Turtle => oxrdfio::RdfFormat::Turtle,
                _ => oxrdfio::RdfFormat::NTriples,
            };
            let mut fresh = sparkles_owl::write::counter("genid");
            let axioms: Vec<&'a Axiom> = axioms.into_iter().collect();
            let written_axioms = axioms.len();
            let triples = sparkles_owl::write::ontology_triples(header, axioms, &mut fresh);
            // the input's prefixes and the standard ones, as the renderer has them
            let ser = crate::io::with_prefixes(
                oxrdfio::RdfSerializer::from_format(rdf),
                r.prefixes()
                    .into_iter()
                    .map(|(p, n)| (p.to_string(), n.to_string())),
            );
            let mut w = ser.for_writer(out);
            for t in &triples {
                w.serialize_triple(t)?;
            }
            w.finish()?;
            Ok(sparkles_owl::WriteReport {
                written_axioms,
                ..Default::default()
            })
        }
    }
}

/// [`sparkles_owl::ofn::document`], written as it goes.
fn write_functional<'a>(
    w: &mut dyn Write,
    r: &Renderer,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> std::io::Result<sparkles_owl::WriteReport> {
    for (p, n) in r.prefixes() {
        writeln!(w, "Prefix({p}:=<{n}>)")?;
    }
    w.write_all(b"\nOntology(")?;
    if let Some(i) = &header.iri {
        write!(w, "<{}>", i.as_str())?;
        if let Some(v) = &header.version_iri {
            write!(w, " <{}>", v.as_str())?;
        }
    }
    w.write_all(b"\n")?;
    for i in &header.imports {
        writeln!(w, "Import(<{}>)", i.as_str())?;
    }
    for a in &header.annotations {
        writeln!(w, "{}", r.annotation(a))?;
    }
    w.write_all(b"\n")?;
    let mut written_axioms = 0usize;
    for ax in axioms {
        writeln!(w, "{}", r.axiom(ax))?;
        written_axioms += 1;
    }
    w.write_all(b")\n")?;
    Ok(sparkles_owl::WriteReport {
        written_axioms,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONTO: &str = r#"Prefix(:=<http://ex.org/>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(<http://ex.org/o>
Declaration(Class(:A))
Declaration(Class(:B))
Declaration(Class(:C))
Declaration(Class(:D))
Declaration(ObjectProperty(:p))
AnnotationAssertion(rdfs:label :A "a"@en)
SubClassOf(:A :B)
SubClassOf(:B :C)
SubClassOf(:A :C)
SubClassOf(Annotation(rdfs:comment "why") :A ObjectSomeValuesFrom(:p :D))
EquivalentClasses(:D ObjectIntersectionOf(:C ObjectSomeValuesFrom(:p :B)))
)
"#;

    fn read(path: &Path) -> WorkingOntology {
        WorkingOntology::read(
            path,
            &OntologyRequest::default(),
            1,
            &QueryOptions::default(),
        )
        .unwrap()
    }

    /// Each writer's output reads back as the same axioms, and the functional-style
    /// writer writes the document the renderer writes.
    #[test]
    fn writers_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("o.ofn");
        std::fs::write(&input, ONTO).unwrap();
        let mut w = read(&input);
        let c = ClassifyRequest::default();
        w.reason(&ReasonOptions::default(), &c, &QueryOptions::default())
            .unwrap();
        assert!(!w.doc.axioms.contains(&Axiom::sub_class_of(
            sparkles_owl::Class::new("http://ex.org/A"),
            sparkles_owl::Class::new("http://ex.org/C"),
        )));
        let ofn = d.path().join("r.ofn");
        w.write(&ofn).unwrap();
        assert_eq!(
            std::fs::read_to_string(&ofn).unwrap(),
            sparkles_owl::ofn::document(&w.renderer(), &w.doc.header, w.doc.axioms.iter())
        );
        for name in ["r.ofn", "r.owl", "r.ttl", "r.nt", "r.ofn.gz", "r.ttl.gz"] {
            let p = d.path().join(name);
            w.write(&p).unwrap();
            let back = read(&p);
            assert_eq!(back.doc, w.doc, "{name}");
        }
        assert_eq!(
            OutputFormat::of(Path::new("x.owx")).unwrap(),
            (OutputFormat::OwlXml, false)
        );
        assert_eq!(
            OutputFormat::of(Path::new("x.omn")).unwrap(),
            (OutputFormat::Manchester, false)
        );
        assert_eq!(
            OutputFormat::of(Path::new("x.obo")).unwrap(),
            (OutputFormat::Obo, false)
        );
        assert_eq!(
            OutputFormat::of(Path::new("x.json")).unwrap(),
            (OutputFormat::OboGraphsJson, false)
        );
        assert_eq!(
            OutputFormat::of(Path::new("x.owx.gz")).unwrap(),
            (OutputFormat::OwlXml, true)
        );
        assert!(OutputFormat::of(Path::new("x")).is_err());
        // OWL/XML and Manchester round-trip losslessly on the reasoned ontology
        let owx_path = d.path().join("r.owx");
        w.write(&owx_path).unwrap();
        let owx_doc =
            sparkles_owl::owx::parse(&std::fs::read_to_string(&owx_path).unwrap()).unwrap();
        assert_eq!(
            Document::new(owx_doc.header, owx_doc.axioms),
            w.doc,
            "r.owx"
        );
        let omn_path = d.path().join("r.omn");
        let omn_rep = w.write_reported(&omn_path).unwrap();
        assert!(omn_rep.is_lossless(), "{omn_rep:?}");
        let omn_doc =
            sparkles_owl::manchester::parse_document(&std::fs::read_to_string(&omn_path).unwrap())
                .unwrap();
        assert_eq!(
            Document::new(omn_doc.header, omn_doc.axioms),
            w.doc,
            "r.omn"
        );
        // OBO 1.4 and OBO Graphs JSON write cleanly and parse back
        let obo_path = d.path().join("r.obo");
        let _ = w.write_reported(&obo_path).unwrap();
        let obo_doc =
            sparkles_owl::obo::parse(&std::fs::read_to_string(&obo_path).unwrap()).unwrap();
        assert!(obo_doc.axioms.contains(&Axiom::sub_class_of(
            sparkles_owl::Class::new("http://ex.org/A"),
            sparkles_owl::Class::new("http://ex.org/B"),
        )));
        let json_path = d.path().join("r.json");
        let _ = w.write_reported(&json_path).unwrap();
        let json_doc =
            sparkles_owl::obographs::parse(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert!(json_doc.axioms.contains(&Axiom::sub_class_of(
            sparkles_owl::Class::new("http://ex.org/A"),
            sparkles_owl::Class::new("http://ex.org/B"),
        )));
        // a file of the document classifies as the document
        let file = w.to_file().unwrap();
        let out = file.classify(&c, &QueryOptions::default()).unwrap();
        assert!(out.classification.consistent);
        assert!(out.classification.taxonomy.is_subclass(
            &sparkles_owl::Class::new("http://ex.org/A"),
            &sparkles_owl::Class::new("http://ex.org/C"),
        ));
    }
    /// A triple that maps to no axiom, here the domain of a property that is not
    /// declared, is left out of every classification of the ontology: a strict reason
    /// refuses, and one that is not strict answers incomplete.
    #[test]
    fn a_triple_that_maps_to_no_axiom_is_left_out() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("o.ttl");
        std::fs::write(
            &input,
            "@prefix : <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
             :A a owl:Class . :B a owl:Class . :C a owl:Class .
             :p rdfs:domain :A . :A owl:disjointWith :B . :C rdfs:subClassOf :B .
             :x :p :y . :x a :B .",
        )
        .unwrap();
        let mut w = read(&input);
        assert!(w.unmapped.count > 0, "the domain triple maps to an axiom");
        assert!(
            w.unmapped
                .triples
                .iter()
                .any(|t| t.to_string()
                    == "<http://ex.org/p> <http://www.w3.org/2000/01/rdf-schema#domain> <http://ex.org/A>"),
            "{:?}",
            w.unmapped
        );
        let strict = ClassifyRequest {
            strict: true,
            ..Default::default()
        };
        let err = w
            .reason(&ReasonOptions::default(), &strict, &QueryOptions::default())
            .unwrap_err();
        assert!(format!("{err:?}").contains("map to no axiom"), "{err:?}");
    }

    /// A file of the working ontology, as a chained command classifies it, counts the
    /// input's triples that map to no axiom, which the document has lost: here an
    /// undeclared property's domain and a class assertion of a blank node that no triple
    /// describes. Its classification answers incomplete after a reason that is not
    /// strict, and a strict one refuses.
    #[test]
    fn a_file_of_the_working_ontology_counts_the_unmapped_triples() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("o.ttl");
        std::fs::write(
            &input,
            "@prefix : <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
             :A a owl:Class . :B a owl:Class . :B rdfs:subClassOf :A .
             :p rdfs:domain :A . :x a _:missing .",
        )
        .unwrap();
        let mut w = read(&input);
        assert_eq!(w.unmapped.count, 2, "{:?}", w.unmapped);
        assert!(
            w.unmapped
                .triples
                .iter()
                .any(|t| t.predicate.as_str() == sparkles_owl::vocab::RDF_TYPE
                    && matches!(t.object, oxrdf::Term::BlankNode(_))),
            "{:?}",
            w.unmapped
        );
        w.reason(
            &ReasonOptions::default(),
            &ClassifyRequest::default(),
            &QueryOptions::default(),
        )
        .unwrap();
        let file = w.to_file().unwrap();
        for assertions in [false, true] {
            let c = ClassifyRequest {
                assertions,
                ..Default::default()
            };
            let out = file.classify(&c, &QueryOptions::default()).unwrap();
            let k = &out.classification;
            assert!(!k.complete, "assertions {assertions}");
            assert_eq!(k.unmapped, w.unmapped, "assertions {assertions}");
            let strict = ClassifyRequest { strict: true, ..c };
            let Err(err) = file.classify(&strict, &QueryOptions::default()) else {
                panic!("assertions {assertions}: a strict run answered");
            };
            assert!(err.to_string().contains("map to no axiom"), "{err}");
        }
        // the document a file of the working ontology hands on keeps them too
        let (_, unmapped) = file
            .into_counted_document(&QueryOptions::default())
            .unwrap();
        assert_eq!(unmapped, w.unmapped);
    }
}
