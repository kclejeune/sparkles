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
use sparkles_owl::classify::{Classification, Excluded};
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
            });
        }
        if let Some(file) = OntologyFile::read(path, req, threads)? {
            let prefixes = file.prefixes();
            let name = file.name.clone();
            let doc = file.into_document(opts)?;
            return Ok(WorkingOntology {
                name,
                doc,
                prefixes,
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
        o.assertions(opts, |ax, _, _| {
            if let Some(ax) = ax {
                axioms.push(ax);
            }
            Ok(true)
        })?;
        Ok(WorkingOntology {
            name,
            doc: Document::new(o.ontology.header.clone(), axioms),
            prefixes,
        })
    }

    /// The ontology as a file that `classify` and `check` read.
    pub fn to_file(&self) -> Result<OntologyFile> {
        OntologyFile::from_document(self.name.clone(), self.doc.clone(), self.prefixes.clone())
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
        transform::reason(&mut self.doc, o, &mut classifier(c, opts))
    }

    /// `reduce`.
    pub fn reduce(
        &mut self,
        o: &ReduceOptions,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> std::result::Result<Report, TransformError> {
        transform::reduce(&mut self.doc, o, &mut classifier(c, opts))
    }

    /// `materialize`. The ontology is unchanged when it fails.
    pub fn materialize(
        &mut self,
        o: &MaterializeOptions,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> std::result::Result<Report, TransformError> {
        transform::materialize(&mut self.doc, o, &mut classifier(c, opts))
    }

    /// `relax`, which classifies nothing.
    pub fn relax(&mut self, o: &RelaxOptions) -> Report {
        transform::relax(&mut self.doc, o)
    }

    /// Write the ontology to `path` in the syntax its extension names.
    pub fn write(&self, path: &Path) -> Result<()> {
        let (format, gz) = OutputFormat::of(path)?;
        write_axioms(
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
/// says, so it never takes a partial answer, and a stopped run fails.
fn classifier<'a>(
    c: &'a ClassifyRequest,
    opts: &'a QueryOptions,
) -> impl FnMut(&[&AxiomKind]) -> Result<(Classification, Vec<Excluded>)> + 'a {
    let c = ClassifyRequest {
        partial: false,
        ..c.clone()
    };
    move |kinds: &[&AxiomKind]| classify_once(kinds, &c, opts)
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
}

impl OutputFormat {
    /// The syntax of an output path by its extension, after a `.gz` that compresses
    /// it, and whether it is compressed. ROBOT's other syntaxes are refused, since no
    /// writer exists here.
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
            "owx" | "omn" | "obo" | "json" => {
                return Err(Error::invalid(format!(
                    "no writer for .{ext} output (ROBOT's {}): write .ofn, .owl, .ttl or .nt",
                    match ext.as_str() {
                        "owx" => "OWL/XML",
                        "omn" => "Manchester syntax",
                        "obo" => "OBO format",
                        _ => "OBO Graphs JSON",
                    }
                )));
            }
            _ => {
                return Err(Error::invalid(format!(
                    "io#INVALID FORMAT ERROR unknown format: {ext}"
                )));
            }
        };
        Ok((f, gz))
    }
}

/// Write a header and axioms to `path`: a functional-style document whose axioms are
/// one to a line, or their triples by the mapping to RDF graphs.
pub fn write_axioms<'a>(
    path: &Path,
    format: OutputFormat,
    gz: bool,
    prefixes: &BTreeMap<String, String>,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> Result<()> {
    let file = std::fs::File::create(path)
        .map_err(|e| Error::invalid(format!("cannot write {}: {e}", path.display())))?;
    let io = |e: std::io::Error| Error::invalid(format!("cannot write {}: {e}", path.display()));
    let buf = std::io::BufWriter::with_capacity(1 << 20, file);
    // each layer is finished and flushed by hand, so that an error writing the gzip
    // trailer or the last buffer fails the command
    let buf = if gz {
        let mut enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        write_body(&mut enc, format, prefixes, header, axioms).map_err(io)?;
        enc.finish().map_err(io)?
    } else {
        let mut buf = buf;
        write_body(&mut buf, format, prefixes, header, axioms).map_err(io)?;
        buf
    };
    let file = buf.into_inner().map_err(|e| io(e.into_error()))?;
    file.sync_all().map_err(io)?;
    Ok(())
}

/// The document in the syntax `format` names.
fn write_body<'a>(
    out: &mut dyn Write,
    format: OutputFormat,
    prefixes: &BTreeMap<String, String>,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> std::io::Result<()> {
    let r = Renderer::new(prefixes.clone());
    if format == OutputFormat::Functional {
        return write_functional(out, &r, header, axioms);
    }
    let rdf = match format {
        OutputFormat::RdfXml => oxrdfio::RdfFormat::RdfXml,
        OutputFormat::Turtle => oxrdfio::RdfFormat::Turtle,
        _ => oxrdfio::RdfFormat::NTriples,
    };
    let mut fresh = sparkles_owl::write::counter("genid");
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
    Ok(())
}

/// [`sparkles_owl::ofn::document`], written as it goes.
fn write_functional<'a>(
    w: &mut dyn Write,
    r: &Renderer,
    header: &sparkles_owl::Header,
    axioms: impl IntoIterator<Item = &'a Axiom>,
) -> std::io::Result<()> {
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
    for ax in axioms {
        writeln!(w, "{}", r.axiom(ax))?;
    }
    w.write_all(b")\n")
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
        assert!(OutputFormat::of(Path::new("x.owx")).is_err());
        assert!(OutputFormat::of(Path::new("x.json")).is_err());
        assert!(OutputFormat::of(Path::new("x")).is_err());
        // a file of the document classifies as the document
        let file = w.to_file().unwrap();
        let out = file.classify(&c, &QueryOptions::default()).unwrap();
        assert!(out.classification.consistent);
        assert!(out.classification.taxonomy.is_subclass(
            &sparkles_owl::Class::new("http://ex.org/A"),
            &sparkles_owl::Class::new("http://ex.org/C"),
        ));
    }
}
