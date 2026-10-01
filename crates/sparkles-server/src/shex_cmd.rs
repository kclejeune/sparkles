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
    /// Schema file: ShExC, or ShExJ (`.json`, `.shexj`, or a text starting with `{`)
    #[arg(long, short = 's', visible_alias = "shapes", value_name = "FILE")]
    pub schema: PathBuf,
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
}

#[derive(Args, Debug)]
pub struct ParseArgs {
    /// Schema files (`-` for stdin); several are printed one after another, each after a
    /// `# FILE` header
    #[arg(required = true, value_name = "FILE")]
    pub files: Vec<PathBuf>,
    /// Output: shexc (pretty-printed), shexj, text (a structural dump)
    #[arg(long, default_value = "shexc", value_parser = ["shexc", "shexj", "text"])]
    pub out: String,
    /// Base IRI for relative IRIs (default: the file's location)
    #[arg(long, value_name = "IRI")]
    pub base: Option<String>,
}

#[cfg(feature = "shex")]
pub fn run(args: ShexArgs, opts: StoreOptions) -> Result<()> {
    let _ = (args, opts);
    anyhow::bail!("sparkles shex is not implemented yet")
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
    }
}
