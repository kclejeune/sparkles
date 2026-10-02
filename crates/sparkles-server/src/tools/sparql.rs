//! `sparkles qparse` and `sparkles uparse`: a query or an update parsed by the engine's
//! parser, printed as SPARQL, as SPARQL algebra in SSE, or (queries) as the physical plan
//! (spec G05 §3.2).

use anyhow::{Context, Result};
use sparkles::sparql::QueryOptions;
use sparkles::store::StoreOptions;
use std::io::Read;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct QparseArgs {
    /// The query (default: --query, else standard input)
    text: Option<String>,
    /// File holding the query (`-` for standard input)
    #[arg(long)]
    query: Option<PathBuf>,
    /// What to print: query (SPARQL), algebra (SSE; alias op) or plan (the physical plan
    /// with estimates; alias opt). Repeatable or comma-separated
    #[arg(long, value_delimiter = ',', default_value = "query")]
    print: Vec<String>,
    /// Base IRI for relative IRIs
    #[arg(long)]
    base: Option<String>,
    /// Plan against this database's statistics (default: an empty in-memory database)
    #[arg(long, conflicts_with = "data")]
    loc: Option<PathBuf>,
    /// Plan against these data files (loaded into memory)
    #[arg(long)]
    data: Vec<PathBuf>,
}

#[derive(clap::Args)]
pub struct UparseArgs {
    /// The update (default: --update, else standard input)
    text: Option<String>,
    /// File holding the update (`-` for standard input)
    #[arg(long)]
    update: Option<PathBuf>,
    /// What to print: update (SPARQL) or algebra (SSE; alias op). Repeatable or
    /// comma-separated
    #[arg(long, value_delimiter = ',', default_value = "update")]
    print: Vec<String>,
    /// Base IRI for relative IRIs
    #[arg(long)]
    base: Option<String>,
}

/// The text from the argument, the file (`-`: standard input), or standard input.
pub(super) fn read_text(text: Option<String>, file: Option<PathBuf>) -> Result<String> {
    match (text, file) {
        (Some(t), None) => Ok(t),
        (None, Some(f)) if f.as_os_str() != "-" => {
            std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))
        }
        (Some(_), Some(_)) => anyhow::bail!("give the text or a file, not both"),
        _ => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
    }
}

/// A syntax error: printed, exit status 1.
fn syntax_error(e: impl std::fmt::Display) -> ! {
    eprintln!("{e}");
    std::process::exit(1)
}

/// The query or update as the formatter writes it (with the `fmt` feature), which keeps
/// its prefixes, comments and structure; else the parser's own rendering of the parse.
fn pretty_sparql(original: &str, rendered: &str) -> String {
    #[cfg(feature = "fmt")]
    if let Ok(f) = sparkles_fmt::format(
        original,
        sparkles_fmt::Language::Sparql,
        &sparkles_fmt::Options::default(),
    ) {
        return f.text;
    }
    let _ = original;
    format!("{}\n", readable_names(rendered))
}

/// The parser's made-up names (up to 32 hex digits) of blank nodes and aggregate variables
/// renamed in order of first occurrence: `?.0`, `?.1`, … and `_:b0`, `_:b1`, …, as Jena
/// names them. String literals and IRIs are left as they are.
fn readable_names(text: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#""(?:[^"\\]|\\.)*"|<[^<>\s]*>|(\?|_:)([0-9a-f]{24,32})\b"#).unwrap()
    });
    let mut names: std::collections::HashMap<String, String> = Default::default();
    let (mut vars, mut blanks) = (0, 0);
    re.replace_all(text, |c: &regex::Captures<'_>| {
        let Some(kind) = c.get(1) else {
            return c[0].to_string();
        };
        names
            .entry(c[0].to_string())
            .or_insert_with(|| {
                if kind.as_str() == "?" {
                    vars += 1;
                    format!("?.{}", vars - 1)
                } else {
                    blanks += 1;
                    format!("_:b{}", blanks - 1)
                }
            })
            .clone()
    })
    .into_owned()
}

pub fn qparse(a: QparseArgs, opts: StoreOptions) -> Result<()> {
    let text = read_text(a.text, a.query)?;
    let parsed = match sparkles::sparql::parse_query(&text, a.base.as_deref(), &[]) {
        Ok(q) => q,
        Err(e) => syntax_error(e),
    };
    let mut store = None;
    for (i, what) in a.print.iter().enumerate() {
        if i > 0 {
            println!();
        }
        match what.as_str() {
            "query" => print!("{}", pretty_sparql(&text, &parsed.to_string())),
            "algebra" | "op" => println!("{}", sse_pretty(&readable_names(&parsed.to_sse()), 100)),
            "plan" | "opt" => {
                let s = match store.take() {
                    Some(s) => s,
                    None => crate::open_or_load(a.loc.clone(), &a.data, opts.clone())?,
                };
                let qopts = QueryOptions {
                    base_iri: a.base.clone(),
                    prefixes: s.prefixes().into_iter().collect(),
                    ..Default::default()
                };
                let (_, plan) = sparkles::sparql::explain(s.snapshot(), &text, &qopts)?;
                print!("{}", readable_names(&plan_text(&plan, 0)));
                for w in &plan.warnings {
                    eprintln!("warning: {} [{}]", w.message, w.code);
                }
                store = Some(s);
            }
            other => anyhow::bail!("--print {other}: expected query, algebra or plan"),
        }
    }
    Ok(())
}

pub fn uparse(a: UparseArgs) -> Result<()> {
    let text = read_text(a.text, a.update)?;
    let qopts = QueryOptions {
        base_iri: a.base.clone(),
        ..Default::default()
    };
    let parsed = match sparkles::sparql::update::parse_update(&text, &qopts) {
        Ok(u) => u,
        Err(e) => syntax_error(e),
    };
    for (i, what) in a.print.iter().enumerate() {
        if i > 0 {
            println!();
        }
        match what.as_str() {
            "update" | "query" => print!("{}", pretty_sparql(&text, &parsed.to_string())),
            "algebra" | "op" => println!("{}", sse_pretty(&readable_names(&parsed.to_sse()), 100)),
            other => anyhow::bail!("--print {other}: expected update or algebra"),
        }
    }
    Ok(())
}

/// The plan as `query --explain` prints it.
fn plan_text(p: &sparkles::sparql::PlanInfo, depth: usize) -> String {
    let mut s = format!(
        "{}{} {}  [est {} rows, cost {}]\n",
        "  ".repeat(depth),
        p.operator,
        p.description,
        p.estimated_rows,
        p.estimated_cost
    );
    for c in &p.children {
        s.push_str(&plan_text(c, depth + 1));
    }
    s
}

// ------------------------------------------------------------------------- SSE ----

enum Sx {
    Atom(String),
    List(Vec<Sx>),
}

/// Split SSE text into `(`, `)` and atoms. Strings (with a language tag or datatype) and
/// IRIs are single atoms whatever they contain.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' || c == ')' {
            out.push(c.to_string());
            i += 1;
            continue;
        }
        let start = i;
        while i < cs.len() && !cs[i].is_whitespace() && cs[i] != '(' && cs[i] != ')' {
            match cs[i] {
                '"' | '\'' => {
                    let q = cs[i];
                    i += 1;
                    while i < cs.len() && cs[i] != q {
                        if cs[i] == '\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                '<' => {
                    while i < cs.len() && cs[i] != '>' {
                        i += 1;
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        out.push(cs[start..i.min(cs.len())].iter().collect());
    }
    out
}

fn parse_sx(toks: &[String], pos: &mut usize) -> Option<Sx> {
    let t = toks.get(*pos)?;
    *pos += 1;
    if t == "(" {
        let mut items = Vec::new();
        while toks.get(*pos).is_some_and(|t| t != ")") {
            items.push(parse_sx(toks, pos)?);
        }
        *pos += 1;
        Some(Sx::List(items))
    } else {
        Some(Sx::Atom(t.clone()))
    }
}

fn flat(x: &Sx) -> String {
    match x {
        Sx::Atom(a) => a.clone(),
        Sx::List(items) => format!("({})", items.iter().map(flat).collect::<Vec<_>>().join(" ")),
    }
}

/// An atom, or a list of atoms only.
fn simple(x: &Sx) -> bool {
    match x {
        Sx::Atom(_) => true,
        Sx::List(items) => items.iter().all(|i| matches!(i, Sx::Atom(_))),
    }
}

fn pretty(x: &Sx, indent: usize, width: usize, out: &mut String) {
    let f = flat(x);
    let Sx::List(items) = x else {
        out.push_str(&f);
        return;
    };
    if indent + f.chars().count() <= width || items.is_empty() {
        out.push_str(&f);
        return;
    }
    out.push('(');
    out.push_str(&flat(&items[0]));
    let mut col = indent + 1 + flat(&items[0]).chars().count();
    let mut rest = items[1..].iter().peekable();
    // short arguments stay on the operator's line
    while let Some(c) = rest.peek() {
        let cf = flat(c);
        if !simple(c) || col + 1 + cf.chars().count() > width {
            break;
        }
        out.push(' ');
        out.push_str(&cf);
        col += 1 + cf.chars().count();
        rest.next();
    }
    for c in rest {
        out.push('\n');
        out.push_str(&" ".repeat(indent + 2));
        pretty(c, indent + 2, width, out);
    }
    out.push(')');
}

/// SSE indented: a form that fits in `width` columns stays on one line; otherwise its
/// operator and short arguments start the line and the other arguments follow, indented.
pub fn sse_pretty(sse: &str, width: usize) -> String {
    let toks = tokens(sse);
    let mut pos = 0;
    let mut out = String::new();
    while pos < toks.len() {
        let Some(x) = parse_sx(&toks, &mut pos) else {
            return sse.to_string();
        };
        if !out.is_empty() {
            out.push('\n');
        }
        pretty(&x, 0, width, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_is_indented_and_keeps_atoms_whole() {
        let q = sparkles::sparql::parse_query(
            "PREFIX ex: <http://example.org/(x)/> SELECT ?s (COUNT(*) AS ?n) WHERE { ?s ex:p \"a (b) \\\" c\"@en ; ex:q ?o FILTER(?o > 5) } GROUP BY ?s",
            None,
            &[],
        )
        .unwrap();
        let sse = q.to_sse();
        let p = sse_pretty(&sse, 40);
        assert!(p.lines().count() > 3, "{p}");
        assert!(p.contains("\"a (b) \\\" c\"@en"), "{p}");
        assert!(p.contains("<http://example.org/(x)/p>"), "{p}");
        // the same tokens as the flat form
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(squash(&p), squash(&sse));
        assert_eq!(
            sse_pretty("(bgp (triple ?s ?p ?o))", 100),
            "(bgp (triple ?s ?p ?o))"
        );
    }

    #[test]
    fn made_up_names_are_readable() {
        let t = format!(
            "(group (?s) (((count) ?{h})) (bgp (triple ?s <http://e/{h}> _:{g}) (triple _:{g} ?{h} \"?{h}\")))",
            h = "c6801599e64066d1fac02eb84008daa6",
            g = "a844876b66ffc7a56a2353810b7b1e52"
        );
        let r = readable_names(&t);
        assert!(
            r.contains("((count) ?.0)") && r.contains("_:b0) (triple _:b0 ?.0"),
            "{r}"
        );
        assert!(
            r.contains("<http://e/c6801599e64066d1fac02eb84008daa6>"),
            "{r}"
        );
        assert!(r.contains("\"?c6801599e64066d1fac02eb84008daa6\""), "{r}");
    }
}
