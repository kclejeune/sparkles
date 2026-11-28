//! `sparkles validation`: the write-time validation of a database (SHACL, or ShEx with
//! `--lang shex`): print its status, set it, or turn it off.

use anyhow::{Context, Result, bail};
use clap::Args;
use sparkles::Dataset;
use sparkles::guard::GuardLanguage;
use sparkles::handles::validation::GuardOutcome;
use sparkles::store::StoreOptions;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ValidationArgs {
    #[arg(long)]
    pub loc: PathBuf,
    /// print the configuration and status and change nothing
    #[arg(long)]
    pub status: bool,
    /// reject or warn
    #[arg(long, value_parser = ["reject", "warn"])]
    pub mode: Option<String>,
    /// the shape language: shacl, shex, or combined
    #[arg(long, value_parser = ["shacl", "shex", "combined"])]
    pub lang: Option<String>,
    /// shapes graph of the dataset (repeatable; SHACL)
    #[arg(long)]
    pub shapes_graph: Vec<String>,
    /// a shapes file (Turtle, another RDF syntax by its extension, or SHACLC as `.shaclc`
    /// or `.shc`), stored in the database as Turtle (SHACL); with --shapes-graph, merged
    /// with the graphs
    #[arg(long)]
    pub shapes: Option<PathBuf>,
    /// a schema file (ShExC, ShExJ, or ShExR in Turtle), copied into the database with
    /// its imports resolved (ShEx)
    #[arg(long, value_name = "FILE")]
    pub schema: Option<PathBuf>,
    /// the schema's syntax: shexc, shexj or shexr (default: from the file name, else
    /// sniffed)
    #[arg(long, value_parser = ["shexc", "shexj", "shexr"], requires = "schema")]
    pub schema_format: Option<String>,
    /// a named graph of the dataset that holds the schema in ShExR (repeatable; ShEx),
    /// read from the state each write leaves and combined with --schema when given
    #[arg(long, value_name = "IRI")]
    pub schema_graph: Vec<String>,
    /// a prefix the shape map may use with --schema-graph, as `ex=http://ex.org/`
    /// (repeatable)
    #[arg(long, value_name = "PREFIX=IRI", requires = "schema_graph")]
    pub schema_prefix: Vec<String>,
    /// the query shape map, in compact syntax (ShEx), e.g. '{FOCUS a ex:Person}@ex:Person'
    #[arg(long, value_name = "MAP")]
    pub shape_map: Option<String>,
    /// default, union, or graph IRIs (repeatable)
    #[arg(long)]
    pub data_graph: Vec<String>,
    #[arg(long)]
    pub include_inferences: bool,
    /// violation, warning or info (SHACL; default violation)
    #[arg(long)]
    pub threshold: Option<String>,
    /// judge a write by the blocking results (SHACL) or nonconformant associations
    /// (ShEx) it introduces, so those the data already has do not block it (allows
    /// `--mode reject` on data that does not conform)
    #[arg(long)]
    pub grandfather: bool,
    #[arg(long, default_value_t = 10.0)]
    pub timeout: f64,
    #[arg(long, default_value_t = 100)]
    pub report_limit: usize,
    /// turn validation off
    #[arg(long, conflicts_with_all = ["mode", "status"])]
    pub off: bool,
    /// text or json
    #[arg(long, default_value = "text")]
    pub format: String,
    #[command(flatten)]
    pub imports: crate::shex_cmd::ImportArgs,
    #[command(flatten)]
    pub outbound: crate::outbound::OutboundArgs,
}

/// The language a `--mode` asks for (`--lang`, or ShEx when `--schema` or `--shape-map`
/// is given), and the flags of the other language it may not use.
fn language(a: &ValidationArgs) -> Result<GuardLanguage> {
    let shex_flags = a.schema.is_some() || a.shape_map.is_some() || !a.schema_graph.is_empty();
    let lang = match a.lang.as_deref() {
        Some("shex") => GuardLanguage::Shex,
        Some("combined") => GuardLanguage::Combined,
        Some(_) => GuardLanguage::Shacl,
        None if shex_flags => GuardLanguage::Shex,
        None => GuardLanguage::Shacl,
    };
    match lang {
        GuardLanguage::Shacl => {
            if shex_flags {
                bail!(
                    "--schema, --schema-graph and --shape-map are for ShEx (SHACL takes --shapes)"
                );
            }
        }
        GuardLanguage::Shex => {
            if a.shapes.is_some() || !a.shapes_graph.is_empty() {
                bail!("--shapes and --shapes-graph are for SHACL (ShEx takes --schema)");
            }
            if a.threshold.is_some() {
                bail!("--threshold is for SHACL: every nonconformant ShEx association blocks");
            }
        }
        GuardLanguage::Combined => {}
    }
    Ok(lang)
}

fn data_graph(gs: &[String]) -> sparkles::guard::config::DataGraphSel {
    use sparkles::guard::config::DataGraphSel;
    match gs {
        [] => DataGraphSel::default(),
        [g] if g == "default" || g == "union" => DataGraphSel::Named(g.clone()),
        gs => DataGraphSel::Graphs(gs.to_vec()),
    }
}

pub fn run(a: ValidationArgs, opts: StoreOptions) -> Result<()> {
    let mut opts = opts;
    opts.unvalidated_writes = true;
    let ds = Dataset::open_with(&a.loc, opts)?;
    if a.status || (a.mode.is_none() && !a.off) {
        return status(&ds, &a.format);
    }
    if a.off {
        ds.validation().guard().reset()?;
        println!("validation off");
        return Ok(());
    }
    let mode: sparkles::guard::GuardMode =
        serde_json::from_value(serde_json::json!(a.mode.as_deref().unwrap_or("reject")))?;
    let outcome = match language(&a)? {
        GuardLanguage::Shacl => set_shacl(&ds, &a, mode)?,
        GuardLanguage::Shex => set_shex(&ds, &a, mode)?,
        #[cfg(all(feature = "shacl", feature = "shex"))]
        GuardLanguage::Combined => {
            use sparkles::write_guard::GuardConfig;
            let shacl = shacl_config(&a, mode)?;
            let (shex, resolver) = shex_config(&a, mode)?;
            ds.validation().guard().set_guards(
                vec![GuardConfig::Shacl(shacl), GuardConfig::Shex(shex)],
                &resolver,
            )?
        }
        #[cfg(not(all(feature = "shacl", feature = "shex")))]
        GuardLanguage::Combined => {
            bail!("combined validation needs both the `shacl` and `shex` features")
        }
    };
    let summary = match outcome {
        GuardOutcome::Installed(s) => Ok(s),
        GuardOutcome::NotConforming(s) => Err(s),
        _ => bail!("validation was turned off"),
    };
    // ShEx counts associations, every nonconformant one blocking
    let (total, blocking) = match summary.as_ref().unwrap_or_else(|s| s) {
        s if s.language == GuardLanguage::Shex => ("associations", "nonconformant"),
        _ => ("results", "blocking"),
    };
    match summary {
        Ok(s) => {
            println!(
                "validation on: {} {total} ({} {blocking}) in {} ms",
                s.total, s.blocking, s.millis
            );
            Ok(())
        }
        Err(s) => {
            println!("{}", serde_json::to_string_pretty(&s)?);
            eprintln!(
                "the data does not conform ({} {blocking} {total}); fix it or use --mode warn first",
                s.blocking
            );
            std::process::exit(1);
        }
    }
}

/// Print the installed configuration and status. A configuration whose guard could not
/// be installed is an error.
fn status(ds: &Dataset, format: &str) -> Result<()> {
    if let Some(why) = ds.guard_error() {
        bail!("{why}");
    }
    let v = ds.validation().guard().get();
    if format == "json" {
        let j = v
            .as_ref()
            .map_or_else(crate::write_validation::none_json, |v| v.json());
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    let Some(v) = v else {
        println!("validation off");
        return Ok(());
    };
    let j = v.json();
    if v.language() == GuardLanguage::Combined {
        println!(
            "validation {}",
            crate::write_validation::mode_name(v.mode())
        );
        for section in j["status"]["guards"].as_array().into_iter().flatten() {
            let status = &section["status"];
            println!(
                "{} {}: {} shapes",
                section["language"].as_str().unwrap_or_default(),
                status["mode"].as_str().unwrap_or_default(),
                status["shapeCount"].as_u64().unwrap_or(0),
            );
        }
        return Ok(());
    }
    println!(
        "validation {}",
        j["config"]["mode"].as_str().unwrap_or_default()
    );
    if v.language() != GuardLanguage::Shacl {
        println!("language   {}", v.language().name());
    }
    println!(
        "shapes     {} shapes",
        j["status"]["shapeCount"].as_u64().unwrap_or(0)
    );
    if let Some(n) = j["status"]["associations"].as_u64() {
        println!("map        {n} associations");
    }
    for w in j["status"]["warnings"].as_array().into_iter().flatten() {
        println!("warning    {}", w.as_str().unwrap_or_default());
    }
    Ok(())
}

#[cfg(feature = "shacl")]
fn set_shacl(
    ds: &Dataset,
    a: &ValidationArgs,
    mode: sparkles::guard::GuardMode,
) -> Result<GuardOutcome> {
    Ok(ds.validation().guard().set_shacl(shacl_config(a, mode)?)?)
}

#[cfg(feature = "shacl")]
fn shacl_config(
    a: &ValidationArgs,
    mode: sparkles::guard::GuardMode,
) -> Result<sparkles_shacl::guard::ValidationConfig> {
    use sparkles_shacl::guard::{self, ShapesSource, ValidationConfig};
    let threshold: sparkles::guard::Severity = serde_json::from_value(serde_json::json!(
        a.threshold.as_deref().unwrap_or("violation")
    ))
    .context("--threshold is violation, warning or info")?;
    if a.shapes.is_none() && a.shapes_graph.is_empty() {
        bail!("give --shapes FILE, --shapes-graph IRI, or both");
    }
    let mut shapes = ShapesSource {
        graphs: (!a.shapes_graph.is_empty()).then(|| a.shapes_graph.clone()),
        ..Default::default()
    };
    if let Some(f) = &a.shapes {
        shapes.inline =
            Some(std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?);
        shapes.source = Some(f.display().to_string());
        // the syntax from the file name: SHACLC, or an RDF syntax (Turtle by default)
        if let Some((syntax, _)) = sparkles_shacl::ShapesSyntax::from_path(f) {
            shapes.format = Some(syntax.media_type().to_string());
        }
    }
    Ok(ValidationConfig {
        format: 2,
        language: Some(GuardLanguage::Shacl),
        mode,
        shapes,
        data_graph: data_graph(&a.data_graph),
        include_inferences: a.include_inferences,
        threshold,
        baseline: if a.grandfather {
            guard::BaselinePolicy::Grandfather
        } else {
            guard::BaselinePolicy::Strict
        },
        timeout_seconds: a.timeout,
        report_limit: a.report_limit,
        updated: None,
    })
}

#[cfg(not(feature = "shacl"))]
fn set_shacl(
    _: &Dataset,
    _: &ValidationArgs,
    _: sparkles::guard::GuardMode,
) -> Result<GuardOutcome> {
    bail!("built without the `shacl` feature")
}

#[cfg(feature = "shex")]
fn set_shex(
    ds: &Dataset,
    a: &ValidationArgs,
    mode: sparkles::guard::GuardMode,
) -> Result<GuardOutcome> {
    let (cfg, resolver) = shex_config(a, mode)?;
    Ok(ds.validation().guard().set_shex(cfg, &resolver)?)
}

#[cfg(feature = "shex")]
fn shex_config(
    a: &ValidationArgs,
    mode: sparkles::guard::GuardMode,
) -> Result<(
    sparkles_shex::guard::ShexValidationConfig,
    sparkles_shex::FileResolver,
)> {
    use sparkles_shex::guard::{
        self, CONFIG_FORMAT, MapSource, SchemaSource, ShexValidationConfig,
    };
    let Some(map) = &a.shape_map else {
        bail!("give --schema FILE (or --schema-graph IRI) and --shape-map MAP");
    };
    if a.schema.is_none() && a.schema_graph.is_empty() {
        bail!("give --schema FILE, --schema-graph IRI, or both");
    }
    let mut schema = SchemaSource {
        graphs: (!a.schema_graph.is_empty()).then(|| a.schema_graph.clone()),
        ..Default::default()
    };
    for p in &a.schema_prefix {
        let (k, v) = p
            .split_once('=')
            .with_context(|| format!("--schema-prefix {p}: expected PREFIX=IRI"))?;
        schema.prefixes.insert(k.to_string(), v.to_string());
    }
    let mut dirs = Vec::new();
    if let Some(path) = &a.schema {
        let abs = std::path::absolute(path)?;
        schema.inline = Some(
            guard::read_schema_file(&abs).with_context(|| format!("reading {}", path.display()))?,
        );
        schema.format =
            a.schema_format
                .clone()
                .or_else(|| match path.extension().and_then(|e| e.to_str()) {
                    Some("json" | "shexj") => Some("shexj".to_string()),
                    Some("ttl") => Some("shexr".to_string()),
                    _ => None,
                });
        schema.base = Some(sparkles_shex::resolve::file_url(&abs));
        schema.source = Some(abs.display().to_string());
        dirs.extend(abs.parent().map(PathBuf::from));
    }
    let policy = a.outbound.local_policy()?;
    let budget = sparkles::outbound::RequestBudget::new(&policy);
    let resolver = sparkles_shex::FileResolver {
        dirs,
        outbound: Some((policy, budget)),
        limits: a.imports.limits()?,
        ..Default::default()
    };
    Ok((
        ShexValidationConfig {
            format: CONFIG_FORMAT,
            language: GuardLanguage::Shex,
            mode,
            schema,
            shape_map: MapSource::Compact(map.clone()),
            data_graph: data_graph(&a.data_graph),
            include_inferences: a.include_inferences,
            baseline: if a.grandfather {
                guard::BaselinePolicy::Grandfather
            } else {
                guard::BaselinePolicy::Strict
            },
            timeout_seconds: a.timeout,
            report_limit: a.report_limit,
            updated: None,
        },
        resolver,
    ))
}

#[cfg(not(feature = "shex"))]
fn set_shex(
    _: &Dataset,
    _: &ValidationArgs,
    _: sparkles::guard::GuardMode,
) -> Result<GuardOutcome> {
    bail!("built without the `shex` feature")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: ValidationArgs,
    }

    fn parse(args: &[&str]) -> Result<ValidationArgs> {
        let mut v = vec!["validation"];
        v.extend_from_slice(args);
        Ok(Cli::try_parse_from(v)?.args)
    }

    #[test]
    fn flags_of_each_language() {
        let a = parse(&["--loc", "db", "--mode", "reject", "--shapes", "s.ttl"]).unwrap();
        assert_eq!(language(&a).unwrap(), GuardLanguage::Shacl);
        let a = parse(&[
            "--loc",
            "db",
            "--lang",
            "shex",
            "--mode",
            "warn",
            "--schema",
            "s.shex",
            "--shape-map",
            "{FOCUS a <http://ex.org/P>}@<http://ex.org/S>",
        ])
        .unwrap();
        assert_eq!(language(&a).unwrap(), GuardLanguage::Shex);
        // --schema and --shape-map imply ShEx
        let a = parse(&["--loc", "db", "--mode", "reject", "--schema", "s.shex"]).unwrap();
        assert_eq!(language(&a).unwrap(), GuardLanguage::Shex);
        let a = parse(&[
            "--loc",
            "db",
            "--mode",
            "reject",
            "--shape-map",
            "<urn:a>@START",
        ])
        .unwrap();
        assert_eq!(language(&a).unwrap(), GuardLanguage::Shex);
        // the other language's flags are refused
        let a = parse(&[
            "--loc", "db", "--lang", "shacl", "--mode", "reject", "--schema", "s.shex",
        ])
        .unwrap();
        assert!(language(&a).is_err());
        let a = parse(&[
            "--loc", "db", "--mode", "reject", "--schema", "s.shex", "--shapes", "s.ttl",
        ])
        .unwrap();
        assert!(language(&a).is_err());
        let a = parse(&[
            "--loc",
            "db",
            "--lang",
            "shex",
            "--mode",
            "reject",
            "--threshold",
            "warning",
        ])
        .unwrap();
        assert!(language(&a).is_err());
        assert!(parse(&["--loc", "db", "--lang", "owl"]).is_err());
        assert!(parse(&["--loc", "db", "--schema-format", "shexc"]).is_err());
    }
}
