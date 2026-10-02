//! `sparkles validation`: the write-time validation of a database (SHACL, or ShEx with
//! `--lang shex`): print its status, set it, or turn it off.

use anyhow::{Context, Result, bail};
use clap::Args;
use sparkles::guard::GuardLanguage;
use sparkles::store::{Store, StoreOptions};
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
    /// the shape language: shacl, or shex (the default with --schema or --shape-map)
    #[arg(long, value_parser = ["shacl", "shex"])]
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
}

/// The language a `--mode` asks for (`--lang`, or ShEx when `--schema` or `--shape-map`
/// is given), and the flags of the other language it may not use.
fn language(a: &ValidationArgs) -> Result<GuardLanguage> {
    let shex_flags = a.schema.is_some() || a.shape_map.is_some();
    let lang = match a.lang.as_deref() {
        Some("shex") => GuardLanguage::Shex,
        Some(_) => GuardLanguage::Shacl,
        None if shex_flags => GuardLanguage::Shex,
        None => GuardLanguage::Shacl,
    };
    match lang {
        GuardLanguage::Shacl => {
            if shex_flags {
                bail!("--schema and --shape-map are for ShEx (SHACL takes --shapes)");
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
    let store = Store::open(&a.loc, opts)?;
    if a.status || (a.mode.is_none() && !a.off) {
        return status(&store, &a.format);
    }
    if a.off {
        off(&store)?;
        println!("validation off");
        return Ok(());
    }
    let mode: sparkles::guard::GuardMode =
        serde_json::from_value(serde_json::json!(a.mode.as_deref().unwrap_or("reject")))?;
    let summary = match language(&a)? {
        GuardLanguage::Shacl => set_shacl(&store, &a, mode)?,
        GuardLanguage::Shex => set_shex(&store, &a, mode)?,
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

/// Print the installed configuration and status.
fn status(store: &Store, format: &str) -> Result<()> {
    let v = crate::write_validation::install(store)?;
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

/// Remove the configuration, whatever its language (every validation file goes).
fn off(store: &Store) -> Result<()> {
    #[cfg(feature = "shacl")]
    sparkles_shacl::guard::set_config(store, None)?;
    #[cfg(all(feature = "shex", not(feature = "shacl")))]
    sparkles_shex::guard::set_config(store, None, &sparkles_shex::NoImports)?;
    #[cfg(not(any(feature = "shacl", feature = "shex")))]
    let _ = store;
    Ok(())
}

/// The summary of setting a configuration: `Ok` when installed, `Err` when `reject` was
/// refused because the data does not conform.
type SetResult =
    std::result::Result<sparkles::guard::ValidationSummary, sparkles::guard::ValidationSummary>;

#[cfg(feature = "shacl")]
fn set_shacl(
    store: &Store,
    a: &ValidationArgs,
    mode: sparkles::guard::GuardMode,
) -> Result<SetResult> {
    use sparkles_shacl::guard::{self, SetOutcome, ShapesSource, ValidationConfig};
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
    let cfg = ValidationConfig {
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
    };
    Ok(match guard::set_config(store, Some(cfg))? {
        SetOutcome::Installed(_, s) => Ok(s),
        SetOutcome::NotConforming(s) => Err(s),
        SetOutcome::Removed => bail!("validation was turned off"),
    })
}

#[cfg(not(feature = "shacl"))]
fn set_shacl(_: &Store, _: &ValidationArgs, _: sparkles::guard::GuardMode) -> Result<SetResult> {
    bail!("built without the `shacl` feature")
}

#[cfg(feature = "shex")]
fn set_shex(
    store: &Store,
    a: &ValidationArgs,
    mode: sparkles::guard::GuardMode,
) -> Result<SetResult> {
    use sparkles_shex::guard::{
        self, CONFIG_FORMAT, MapSource, SchemaSource, SetOutcome, ShexValidationConfig,
    };
    let (Some(schema), Some(map)) = (&a.schema, &a.shape_map) else {
        bail!("give --schema FILE and --shape-map MAP");
    };
    let text = std::fs::read_to_string(schema).with_context(|| format!("{}", schema.display()))?;
    let format =
        a.schema_format
            .clone()
            .or_else(|| match schema.extension().and_then(|e| e.to_str()) {
                Some("json" | "shexj") => Some("shexj".to_string()),
                Some("ttl") => Some("shexr".to_string()),
                _ => None,
            });
    // imports: relative IRIs against the schema's directory, any readable file, and
    // http(s) with the local commands' outbound defaults
    let policy = sparkles::outbound::OutboundPolicy {
        allow_private: true,
        ..Default::default()
    };
    let budget = sparkles::outbound::RequestBudget::new(&policy);
    let abs = std::path::absolute(schema)?;
    let resolver = sparkles_shex::FileResolver {
        dirs: abs.parent().map(PathBuf::from).into_iter().collect(),
        outbound: Some((policy, budget)),
        ..Default::default()
    };
    let cfg = ShexValidationConfig {
        format: CONFIG_FORMAT,
        language: GuardLanguage::Shex,
        mode,
        schema: SchemaSource {
            inline: Some(text),
            format,
            base: Some(sparkles_shex::resolve::file_url(&abs)),
            source: Some(abs.display().to_string()),
            ..Default::default()
        },
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
    };
    Ok(match guard::set_config(store, Some(cfg), &resolver)? {
        SetOutcome::Installed(_, s) => Ok(s),
        SetOutcome::NotConforming(s) => Err(s),
        SetOutcome::Removed => bail!("validation was turned off"),
    })
}

#[cfg(not(feature = "shex"))]
fn set_shex(_: &Store, _: &ValidationArgs, _: sparkles::guard::GuardMode) -> Result<SetResult> {
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
