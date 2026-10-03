//! File-level tools, the command-line equivalents of Jena's `riot`, `qparse`, `uparse`,
//! `rdfdiff`/`rdfcompare`, `iri`, `langtag`, `rsparql`, `rupdate` and `rset` (spec G05),
//! and `rdfpatch` (spec F10).
//! The subcommands are flattened into `sparkles`' own.

use anyhow::Result;
use sparkles::store::StoreOptions;

pub mod compare;
pub mod convert;
#[cfg(feature = "auth")]
pub mod endpoint;
pub mod rdfpatch;
pub mod rset;
pub mod sparql;
pub mod table;
pub mod terms;

#[derive(clap::Subcommand)]
pub enum ToolCmd {
    /// Parse, validate, count and convert RDF files between syntaxes, streaming (Jena's
    /// riot); exits with status 1 on syntax errors
    #[command(visible_alias = "riot")]
    Convert(convert::ConvertArgs),
    /// Print a SPARQL query parsed: as SPARQL, as SPARQL algebra (SSE) or as the
    /// physical plan; exits with status 1 on a syntax error
    Qparse(sparql::QparseArgs),
    /// Print a SPARQL update parsed: as SPARQL or as SPARQL algebra (SSE); exits with
    /// status 1 on a syntax error
    Uparse(sparql::UparseArgs),
    /// Compare two RDF files up to blank-node isomorphism; exits with status 0 when
    /// they are isomorphic, 1 when they differ, 2 on errors
    #[command(visible_aliases = ["rdfcompare", "rdfdiff"])]
    Compare(compare::CompareArgs),
    /// Show how IRIs parse and what is wrong with them; exits with status 1 on errors
    Iri(IriArgs),
    /// Show how language tags parse and what is wrong with them; exits with status 1 on
    /// errors
    Langtag(LangtagArgs),
    /// Run a query against any SPARQL 1.1 Protocol endpoint, given by its URL
    #[cfg(feature = "auth")]
    Rsparql(endpoint::RsparqlArgs),
    /// Run an update against any SPARQL 1.1 Protocol endpoint, given by its URL
    #[cfg(feature = "auth")]
    Rupdate(endpoint::RupdateArgs),
    /// Convert a SPARQL result set between formats (JSON, XML, TSV in; text, JSON, XML,
    /// CSV, TSV out)
    Rset(rset::RsetArgs),
    /// Parse RDF Patch files and write their rows back, with the counts of data, prefix
    /// and transaction rows on standard error (Jena's rdfpatch); exits with status 1 on
    /// errors
    Rdfpatch(rdfpatch::RdfpatchArgs),
}

#[derive(clap::Args)]
pub struct IriArgs {
    /// The IRIs (angle brackets allowed)
    #[arg(required = true)]
    iris: Vec<String>,
    /// Resolve relative references against this IRI
    #[arg(long, value_name = "IRI")]
    base: Option<String>,
    /// text or json
    #[arg(long, default_value = "text")]
    format: String,
    /// Exit with status 1 on warnings too
    #[arg(long)]
    strict: bool,
}

#[derive(clap::Args)]
pub struct LangtagArgs {
    /// The language tags (`@` and an RDF 1.2 direction such as `--ltr` allowed)
    #[arg(required = true, allow_hyphen_values = true)]
    tags: Vec<String>,
    /// text or json
    #[arg(long, default_value = "text")]
    format: String,
    /// Exit with status 1 on warnings too
    #[arg(long)]
    strict: bool,
}

pub fn run(cmd: ToolCmd, opts: StoreOptions) -> Result<()> {
    match cmd {
        ToolCmd::Convert(a) => convert::run(a),
        ToolCmd::Qparse(a) => sparql::qparse(a, opts),
        ToolCmd::Uparse(a) => sparql::uparse(a),
        ToolCmd::Compare(a) => compare::run(a),
        ToolCmd::Iri(a) => {
            let reports: Vec<_> = a
                .iris
                .iter()
                .map(|i| terms::analyze_iri(i, a.base.as_deref()))
                .collect();
            let fail = reports
                .iter()
                .any(|r| r.has_errors() || (a.strict && r.has_warnings()));
            print_reports(&a.format, reports.iter().map(|r| (r.text(), r.json())))?;
            exit_if(fail)
        }
        ToolCmd::Langtag(a) => {
            let reports: Vec<_> = a.tags.iter().map(|t| terms::analyze_langtag(t)).collect();
            let fail = reports
                .iter()
                .any(|r| r.has_errors() || (a.strict && r.has_warnings()));
            print_reports(&a.format, reports.iter().map(|r| (r.text(), r.json())))?;
            exit_if(fail)
        }
        #[cfg(feature = "auth")]
        ToolCmd::Rsparql(a) => endpoint::rsparql(a),
        #[cfg(feature = "auth")]
        ToolCmd::Rupdate(a) => endpoint::rupdate(a),
        ToolCmd::Rset(a) => rset::run(a),
        ToolCmd::Rdfpatch(a) => rdfpatch::run(a),
    }
}

fn print_reports(
    format: &str,
    reports: impl Iterator<Item = (String, serde_json::Value)>,
) -> Result<()> {
    match format {
        "json" => {
            let all: Vec<_> = reports.map(|(_, j)| j).collect();
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
        "text" => {
            for (i, (t, _)) in reports.enumerate() {
                if i > 0 {
                    println!();
                }
                print!("{t}");
            }
        }
        other => anyhow::bail!("--format {other}: expected text or json"),
    }
    Ok(())
}

fn exit_if(fail: bool) -> Result<()> {
    if fail {
        std::process::exit(1);
    }
    Ok(())
}
