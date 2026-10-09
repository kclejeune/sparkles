//! `sparkles shex`: ShEx validation of a database or data files (`validate`, Jena's
//! `shex validate`) and schema printing (`parse`, Jena's `shex parse`, which can also
//! print ShExJ). Jena's flag names are aliases. Exit status: 0 when every association
//! conforms, 1 when one does not (or on a timeout or budget error), 2 for usage, parse
//! and schema errors.

use anyhow::Result;
use clap::{ArgGroup, Args, Subcommand};
use sparkles::store::StoreOptions;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ShexArgs {
    #[command(subcommand)]
    pub cmd: ShexCmd,
}

#[derive(Subcommand, Debug)]
pub enum ShexCmd {
    /// Validate a database (or data files) against a ShEx schema and a shape map; exits
    /// with status 1 when an association does not conform
    #[command(visible_aliases = ["val", "v"])]
    Validate(ValidateArgs),
    /// Parse schemas and print them (ShExC, ShExJ or a structural dump)
    #[command(visible_aliases = ["p", "print"])]
    Parse(ParseArgs),
}

#[derive(Args, Debug)]
#[command(group(ArgGroup::new("source").required(true).args(["loc", "data"])))]
#[command(group(ArgGroup::new("selection").required(true).args(["map", "shape_map", "node"])))]
pub struct ValidateArgs {
    /// Database directory
    #[arg(long)]
    pub loc: Option<PathBuf>,
    /// Data files to validate (loaded into memory)
    #[arg(long, short = 'd', visible_alias = "datafile", num_args = 1.., value_name = "FILE")]
    pub data: Vec<PathBuf>,
    /// Schema file: ShExC, ShExJ (`.json`, `.shexj`, or a text starting with `{`), or
    /// ShExR (`.ttl`, `.nt`, `.rdf`, `.trig`, `.nq`)
    #[arg(long, short = 's', visible_alias = "shapes", value_name = "FILE")]
    pub schema: PathBuf,
    /// The schema's syntax, when its file name does not say (ShExR: in the RDF syntax of
    /// the file's extension, else Turtle)
    #[arg(long, value_parser = ["shexc", "shexj", "shexr"], value_name = "FORMAT")]
    pub schema_format: Option<String>,
    /// Shape map file: a `.json` file is a JSON shape map, anything else compact syntax
    #[arg(long, short = 'm', visible_alias = "shapesMap", value_name = "FILE")]
    pub map: Option<PathBuf>,
    /// Shape map in compact syntax, e.g. '{FOCUS a ex:Person}@ex:PersonShape'
    #[arg(long, value_name = "MAP")]
    pub shape_map: Option<String>,
    /// Node to validate (an IRI, prefixed name or literal) against --shape, or START
    #[arg(long, short = 'n', visible_alias = "target", value_name = "TERM")]
    pub node: Option<String>,
    /// Shape label for --node (default: the schema's START)
    #[arg(long, requires = "node", conflicts_with_all = ["map", "shape_map"], value_name = "LABEL")]
    pub shape: Option<String>,
    /// Data graph: `default`, `union` (all graphs) or a graph IRI
    #[arg(long, default_value = "default")]
    pub graph: String,
    /// Leave materialized inferences (`urn:x-sparkles:inferred`) out of the data graph
    #[arg(long)]
    pub no_inferences: bool,
    /// Definitions of the schema's EXTERNAL shapes (ShExC or ShExJ)
    #[arg(long, value_name = "FILE")]
    pub externs: Option<PathBuf>,
    /// Report format: text (Jena's), json, shapemap (ShapeMap JSON), smap (compact)
    #[arg(long, default_value = "text", value_parser = ["text", "json", "shapemap", "smap"])]
    pub format: String,
    /// Report only nonconformant associations
    #[arg(long)]
    pub only_nonconformant: bool,
    /// Timeout in seconds
    #[arg(long)]
    pub timeout: Option<f64>,
    /// Include the Test extension's `print` output
    #[arg(long)]
    pub semact_trace: bool,
    /// Add the typing's counters (pairs, evaluations, refinement waves) to the JSON
    /// report, or print them to stderr with the other formats
    #[arg(long)]
    pub stats: bool,
    #[command(flatten)]
    pub imports: ImportArgs,
}

/// `--shex-max-imports`, `--shex-max-import-mb` and `--shex-import-timeout`: what the
/// imports of one ShEx schema may read.
#[derive(Args, Clone, Debug)]
pub struct ImportArgs {
    /// Most schemas one ShEx schema may import, directly or through its imports
    #[arg(long, value_name = "N", default_value_t = 64)]
    pub shex_max_imports: usize,
    /// Most schema text the imports of one ShEx schema may read, in MiB, from files and
    /// the network together
    #[arg(long, value_name = "N", default_value_t = 16)]
    pub shex_max_import_mb: u64,
    /// Time one http(s) import may take, in seconds (also held to the outbound timeout)
    #[arg(long, value_name = "SECS", default_value_t = 10.0)]
    pub shex_import_timeout: f64,
}

impl ImportArgs {
    /// The limits these flags give.
    #[cfg(feature = "shex")]
    pub fn limits(&self) -> Result<sparkles_shex::resolve::ImportLimits> {
        if !(self.shex_import_timeout.is_finite() && self.shex_import_timeout > 0.0) {
            anyhow::bail!("--shex-import-timeout must be a positive number of seconds");
        }
        Ok(sparkles_shex::resolve::ImportLimits {
            max_schemas: self.shex_max_imports,
            max_bytes: self.shex_max_import_mb.saturating_mul(1 << 20),
            timeout: std::time::Duration::from_secs_f64(self.shex_import_timeout),
        })
    }
}

#[derive(Args, Debug)]
pub struct ParseArgs {
    /// Schema files (`-` for stdin); several are printed one after another, each after a
    /// `# FILE` header
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// Output: shexc (pretty-printed), shexj, shexr (Turtle), text (a structural dump)
    #[arg(long, default_value = "shexc", value_parser = ["shexc", "shexj", "shexr", "text"])]
    pub out: String,
    /// Input syntax, when the file name does not say (stdin): shexc, shexj, or shexr
    /// (in the RDF syntax of the file's extension, else Turtle); default: by extension,
    /// else sniffed (ShExJ for a text starting with `{`, else ShExC)
    #[arg(long = "in", value_parser = ["shexc", "shexj", "shexr"], value_name = "FORMAT")]
    pub input: Option<String>,
    /// Base IRI for relative IRIs (default: the file's location)
    #[arg(long, value_name = "IRI")]
    pub base: Option<String>,
}

/// Output formats of a ShEx result map (`format` of `/{ds}/shex` and `--format`).
#[cfg(feature = "shex")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShexFormat {
    /// the Sparkles JSON report
    Json,
    /// the ShapeMap JSON result map
    ShapeMap,
    /// the compact result map (`<n>@<S>`, `<n>@!<S>`)
    Smap,
    /// Jena's text report
    Text,
}

#[cfg(feature = "shex")]
impl ShexFormat {
    /// From a format name (`json`, `shapemap`, `smap`, `text`) or a media type.
    pub fn from_name(s: &str) -> Option<ShexFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "json" | "application/json" => Some(ShexFormat::Json),
            "shapemap" => Some(ShexFormat::ShapeMap),
            "smap" => Some(ShexFormat::Smap),
            "text" | "txt" | "text/plain" => Some(ShexFormat::Text),
            _ => None,
        }
    }

    /// Accept-header offers, in preference order.
    pub const OFFERS: [&'static str; 2] = ["application/json", "text/plain"];

    pub fn media_type(self) -> &'static str {
        match self {
            ShexFormat::Json | ShexFormat::ShapeMap => "application/json",
            ShexFormat::Smap | ShexFormat::Text => "text/plain; charset=utf-8",
        }
    }
}

/// A result map in `format` (JSON compact, without a final newline), with the typing's
/// counters in the JSON report when `stats` is set.
#[cfg(feature = "shex")]
pub(crate) fn write_report(
    r: &sparkles_shex::ResultMap,
    format: ShexFormat,
    stats: bool,
) -> String {
    match format {
        ShexFormat::Json => {
            let mut v = r.to_json();
            if stats && let Some(o) = v.as_object_mut() {
                let s = &r.stats;
                o.insert(
                    "stats".into(),
                    serde_json::json!({"pairs": s.pairs, "evaluations": s.evaluations, "waves": s.waves}),
                );
            }
            v.to_string()
        }
        ShexFormat::ShapeMap => r.to_shapemap_json().to_string(),
        ShexFormat::Smap => r.to_smap(),
        ShexFormat::Text => r.to_text(),
    }
}

#[cfg(feature = "shex")]
pub fn run(args: ShexArgs, opts: StoreOptions) -> Result<()> {
    match args.cmd {
        ShexCmd::Validate(v) => enabled::validate(v, opts),
        ShexCmd::Parse(p) => enabled::parse(p),
    }
}

#[cfg(feature = "shex")]
mod enabled {
    use super::{ParseArgs, ShexFormat, ValidateArgs, write_report};
    use crate::validation_common::{self as common, GraphParam};
    use anyhow::{Context, Result};
    use sparkles::store::StoreOptions;
    use sparkles_shex::resolve::file_url;
    use sparkles_shex::{
        FileResolver, ParseError, Schema, SchemaError, SchemaFormat, ShapeMap, ValidateOptions,
    };
    use std::io::{Read, Write};
    use std::path::Path;
    use std::time::Duration;

    /// Usage, parse and schema errors: exit status 2.
    fn usage(msg: impl std::fmt::Display) -> ! {
        eprintln!("error: {msg}");
        std::process::exit(2)
    }

    fn syntax(what: &str, e: &ParseError) -> ! {
        if e.line == 0 {
            // a ShExR error is not at a place in the text
            usage(format_args!("{what}: {}", e.message))
        }
        usage(format_args!(
            "{what}: syntax error at line {}, column {}: {}",
            e.line, e.column, e.message
        ))
    }

    /// The syntax of a schema file: `given` (`shexc`, `shexj`, `shexr`), else ShExJ for
    /// `.json` and `.shexj`, ShExR for the RDF extensions (`.ttl`, `.nt`, `.nq`,
    /// `.trig`, `.rdf`, `.owl`, `.n3`), else sniffed. ShExR is read in the RDF syntax of
    /// the file's extension, else as Turtle.
    fn hint(path: &Path, given: Option<&str>) -> Option<SchemaFormat> {
        use sparkles::io::RdfFormat;
        let ext = path.extension().and_then(|e| e.to_str());
        let rdf = match ext.map(str::to_ascii_lowercase).as_deref() {
            Some("ttl" | "turtle") => Some(RdfFormat::Turtle),
            Some("nt" | "ntriples") => Some(RdfFormat::NTriples),
            Some("nq" | "nquads") => Some(RdfFormat::NQuads),
            Some("trig") => Some(RdfFormat::TriG),
            Some("rdf" | "owl" | "rdfxml") => Some(RdfFormat::RdfXml),
            Some("n3") => Some(RdfFormat::N3),
            _ => None,
        };
        match given.and_then(SchemaFormat::from_name) {
            Some(SchemaFormat::ShExR(turtle)) => Some(SchemaFormat::ShExR(rdf.unwrap_or(turtle))),
            Some(f) => Some(f),
            None if matches!(ext, Some("json" | "shexj")) => Some(SchemaFormat::ShExJ),
            None => rdf.map(SchemaFormat::ShExR),
        }
    }

    fn read_text(path: &Path) -> String {
        let r = if path == Path::new("-") {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map(|_| s)
        } else {
            std::fs::read_to_string(path)
        };
        r.unwrap_or_else(|e| usage(format_args!("{}: {e}", path.display())))
    }

    /// A schema file, its relative IRIs against its own location (or `base`), in the
    /// syntax `given` or [`hint`]'s.
    fn read_schema(path: &Path, base: Option<&str>, given: Option<&str>) -> Schema {
        let text = read_text(path);
        let url = (path != Path::new("-")).then(|| file_url(path));
        sparkles_shex::parse_schema(&text, base.or(url.as_deref()), hint(path, given))
            .unwrap_or_else(|e| syntax(&path.display().to_string(), &e))
    }

    pub(super) fn validate(a: ValidateArgs, opts: StoreOptions) -> Result<()> {
        let schema = read_schema(&a.schema, None, a.schema_format.as_deref());
        let externs = a.externs.as_deref().map(|x| read_schema(x, None, None));
        // imports: relative IRIs against the schema's directory, any readable file, and
        // http(s) with the local commands' outbound defaults
        let policy = sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        };
        let budget = sparkles::outbound::RequestBudget::new(&policy);
        let dir = std::path::absolute(&a.schema)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let resolver = FileResolver {
            dirs: dir.into_iter().collect(),
            outbound: Some((policy, budget)),
            externs,
            limits: a.imports.limits().unwrap_or_else(|e| usage(e)),
            ..Default::default()
        };
        let schema = sparkles_shex::compile(&schema, &resolver)
            .unwrap_or_else(|e| usage(format_args!("{}: {}", a.schema.display(), e.message)));

        let map = if let Some(m) = &a.map {
            let text = read_text(m);
            let what = m.display().to_string();
            if m.extension().is_some_and(|e| e == "json") {
                ShapeMap::from_json(&text).unwrap_or_else(|e| syntax(&what, &e))
            } else {
                let base = file_url(m);
                ShapeMap::parse(&text, schema.prefixes(), Some(&base))
                    .unwrap_or_else(|e| syntax(&what, &e))
            }
        } else {
            let text = match (&a.shape_map, &a.node) {
                (Some(m), _) => m.clone(),
                (None, Some(n)) => {
                    let shape = match &a.shape {
                        Some(s) => s.clone(),
                        None if schema.has_start() => "START".to_string(),
                        None => usage("the schema has no start shape; give --shape"),
                    };
                    format!("{n}@{shape}")
                }
                // clap requires one of them
                (None, None) => usage("give --map, --shape-map or --node"),
            };
            ShapeMap::parse(&text, schema.prefixes(), schema.base())
                .unwrap_or_else(|e| syntax("shape map", &e))
        };

        let format = ShexFormat::from_name(&a.format)
            .with_context(|| format!("unknown report format '{}'", a.format))?;
        let graph = GraphParam::parse(&a.graph).unwrap_or_else(|e| usage(format_args!("{e:#}")));
        let store = crate::open_or_load(a.loc, &a.data, opts)?;
        let snap = store.snapshot();
        if let GraphParam::Named(iri) = &graph
            && !common::graph_exists(&snap, iri)
        {
            usage(format_args!("no such graph: <{iri}>"));
        }
        let inferred = common::graph_exists(&snap, crate::http::INFERRED_GRAPH)
            .then_some(crate::http::INFERRED_GRAPH);
        let inputs = common::inputs(&snap, &graph, inferred, !a.no_inferences)?;
        let vopts = ValidateOptions {
            data_graph: inputs.data_graph,
            extra_graphs: inputs.extra_graphs,
            exclude_graphs: inputs.exclude_graphs,
            timeout: a.timeout.map(Duration::from_secs_f64),
            only_nonconformant: a.only_nonconformant,
            semact_trace: a.semact_trace,
            ..Default::default()
        };
        let results = match sparkles_shex::validate(&snap, &schema, &map, &vopts) {
            Ok(r) => r,
            // an undefined label, or START without a start shape
            Err(e) if e.downcast_ref::<SchemaError>().is_some() => usage(e),
            Err(e) => return Err(e),
        };
        let mut out = std::io::stdout().lock();
        out.write_all(write_report(&results, format, a.stats).as_bytes())?;
        if matches!(format, ShexFormat::Json | ShexFormat::ShapeMap) {
            writeln!(out)?;
        }
        out.flush()?;
        if a.stats && format != ShexFormat::Json {
            let s = &results.stats;
            eprintln!(
                "{} pairs, {} evaluations, waves per stratum {:?}",
                s.pairs, s.evaluations, s.waves
            );
        }
        if !results.conforms {
            std::process::exit(1);
        }
        Ok(())
    }

    pub(super) fn parse(a: ParseArgs) -> Result<()> {
        let mut out = std::io::stdout().lock();
        let several = a.files.len() > 1;
        for f in &a.files {
            let schema = read_schema(f, a.base.as_deref(), a.input.as_deref());
            if several {
                writeln!(out, "# {}", f.display())?;
            }
            match a.out.as_str() {
                "shexj" => {
                    serde_json::to_writer_pretty(&mut out, &schema.to_shexj())?;
                    writeln!(out)?;
                }
                "shexr" => out.write_all(schema.to_shexr_turtle().as_bytes())?,
                "text" => writeln!(out, "{schema:#?}")?,
                _ => {
                    let c = schema.to_shexc();
                    out.write_all(c.as_bytes())?;
                    if !c.ends_with('\n') {
                        writeln!(out)?;
                    }
                }
            }
        }
        out.flush()?;
        Ok(())
    }
}

#[cfg(not(feature = "shex"))]
pub fn run(_: ShexArgs, _: StoreOptions) -> Result<()> {
    anyhow::bail!("built without the `shex` feature")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct Cli {
        #[command(subcommand)]
        cmd: ShexCmd,
    }

    fn parse(args: &[&str]) -> Result<ShexCmd, clap::Error> {
        Cli::try_parse_from(std::iter::once("shex").chain(args.iter().copied())).map(|c| c.cmd)
    }

    #[cfg(feature = "shex")]
    #[test]
    fn import_flags_give_the_limits() {
        let ShexCmd::Validate(v) =
            parse(&["v", "-s", "s.shex", "--loc", "db", "-n", "<x>"]).unwrap()
        else {
            panic!("not validate");
        };
        let defaults = sparkles_shex::resolve::ImportLimits::default();
        assert_eq!(v.imports.limits().unwrap(), defaults);
        let ShexCmd::Validate(v) = parse(&[
            "v",
            "-s",
            "s.shex",
            "--loc",
            "db",
            "-n",
            "<x>",
            "--shex-max-imports",
            "3",
            "--shex-max-import-mb",
            "2",
            "--shex-import-timeout",
            "1.5",
        ])
        .unwrap() else {
            panic!("not validate");
        };
        let l = v.imports.limits().unwrap();
        assert_eq!(l.max_schemas, 3);
        assert_eq!(l.max_bytes, 2 << 20);
        assert_eq!(l.timeout, std::time::Duration::from_millis(1500));
        let mut bad = v.imports.clone();
        bad.shex_import_timeout = 0.0;
        assert!(bad.limits().is_err());
    }

    #[test]
    fn jena_names_are_aliases() {
        let ShexCmd::Validate(v) = parse(&[
            "v",
            "--shapes",
            "s.shex",
            "--datafile",
            "a.ttl",
            "b.ttl",
            "--target",
            "ex:a",
        ])
        .unwrap() else {
            panic!("not validate");
        };
        assert_eq!(v.schema, PathBuf::from("s.shex"));
        assert_eq!(v.data.len(), 2);
        assert_eq!(v.node.as_deref(), Some("ex:a"));
        assert_eq!(v.format, "text");
        let ShexCmd::Validate(v) = parse(&[
            "val",
            "-s",
            "s.shex",
            "--loc",
            "db",
            "--shapesMap",
            "m.json",
        ])
        .unwrap() else {
            panic!("not validate");
        };
        assert_eq!(v.map, Some(PathBuf::from("m.json")));
        let ShexCmd::Validate(v) = parse(&[
            "validate",
            "-s",
            "s.shex",
            "-d",
            "a.ttl",
            "-n",
            "<x>",
            "--shape",
            "ex:S",
            "--format",
            "smap",
            "--only-nonconformant",
            "--semact-trace",
        ])
        .unwrap() else {
            panic!("not validate");
        };
        assert_eq!(v.shape.as_deref(), Some("ex:S"));
        assert!(v.only_nonconformant && v.semact_trace);
        for alias in ["parse", "p", "print"] {
            let ShexCmd::Parse(p) = parse(&[alias, "a.shex", "-", "--out", "shexj"]).unwrap()
            else {
                panic!("not parse");
            };
            assert_eq!(p.files.len(), 2);
            assert_eq!(p.out, "shexj");
        }
        let ShexCmd::Parse(p) = parse(&["parse", "-", "--in", "shexr", "--out", "shexr"]).unwrap()
        else {
            panic!("not parse");
        };
        assert_eq!(
            (p.input.as_deref(), p.out.as_str()),
            (Some("shexr"), "shexr")
        );
    }

    #[test]
    fn usage_errors() {
        // a data source and a selection are required, and exclusive
        assert!(parse(&["validate", "-s", "s.shex", "-n", "ex:a"]).is_err());
        assert!(parse(&["validate", "-s", "s.shex", "-d", "a.ttl"]).is_err());
        assert!(
            parse(&[
                "validate", "-s", "s.shex", "-d", "a", "--loc", "db", "-n", "x"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "validate",
                "-s",
                "s.shex",
                "-d",
                "a",
                "-n",
                "x",
                "--shape-map",
                "m"
            ])
            .is_err()
        );
        assert!(parse(&["validate", "-s", "s", "-d", "a", "-m", "m", "--shape", "S"]).is_err());
        assert!(
            parse(&[
                "validate", "-s", "s", "-d", "a", "-n", "x", "--format", "ttl"
            ])
            .is_err()
        );
        assert!(parse(&["parse"]).is_err());
        assert!(parse(&["parse", "a.shex", "--out", "json"]).is_err());
        assert!(parse(&["parse", "a.shex", "--in", "turtle"]).is_err());
        assert!(
            parse(&[
                "validate",
                "-s",
                "s",
                "-d",
                "a",
                "-n",
                "x",
                "--schema-format",
                "ttl"
            ])
            .is_err()
        );
    }
}
