//! The expression printer on the W3C SPARQL suites, ahead of the printers around it:
//! in every query and update the reference parser accepts, each outermost expression
//! is printed (under the default options and with every expression option flipped) and
//! put back in place of its source text. The result must keep the algebra and the
//! comments, and doing it again must change nothing.
//!
//! Set `SPARKLES_W3C_DIR` to the `rdf-tests-cg/sparql` directory; the test is skipped
//! when the suite is absent.

mod corpus;

use sparkles_fmt::check::{comments, sparql_equivalent, sparql_reference};
use sparkles_fmt::doc::DocArena;
use sparkles_fmt::lex::{LexMode, lex};
use sparkles_fmt::sparql::parse::parse;
use sparkles_fmt::sparql::print::{Ctx, RULES, node};
use sparkles_fmt::syntax::NodeKind;
use sparkles_fmt::tree::{NodeId, Tree};
use sparkles_fmt::trivia::Comments;
use sparkles_fmt::{OperatorPosition, Options, QuoteStyle};

/// The node kinds the expression printer owns.
fn is_expr(k: NodeKind) -> bool {
    use NodeKind as K;
    matches!(
        k,
        K::OrChain
            | K::AndChain
            | K::ChainOperand
            | K::Binary
            | K::Unary
            | K::Bracketed
            | K::Call
            | K::ArgList
            | K::Arg
            | K::Aggregate
            | K::InList
            | K::Exists
            | K::NotExists
    )
}

/// The outermost expression nodes, in source order.
fn roots(tree: &Tree<'_>) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(n) = stack.pop() {
        if is_expr(tree.kind(n)) {
            out.push(n);
            continue;
        }
        let mut children: Vec<NodeId> = tree.child_nodes(n).collect();
        children.reverse();
        stack.extend(children);
    }
    out
}

/// `text` with each outermost expression replaced by its printed form (and a line
/// break after it, so that a trailing comment it ends with cannot swallow the rest of
/// the line).
fn splice(text: &str, opts: &Options) -> Result<(String, usize), String> {
    let tokens = lex(text, LexMode::Sparql);
    let unit = sparql_reference(text, &tokens)
        .map_err(|e| e.to_string())?
        .unit;
    let tree = parse(text, tokens, unit).map_err(|e| format!("parse: {e}"))?;
    let comments = Comments::attach(&tree, &RULES);
    let mut out = String::new();
    let mut at = 0;
    let roots = roots(&tree);
    for &n in &roots {
        let r = tree.range(n);
        let mut cx = Ctx {
            tree: &tree,
            arena: DocArena::new(&tree.tokens),
            opts,
            comments: &comments,
        };
        let doc = node(&mut cx, n);
        let printed = sparkles_fmt::doc::print(
            &cx.arena,
            doc,
            tree.src,
            opts.line_width,
            opts.indent_width,
            None,
        )
        .map_err(|e| e.to_string())?;
        out.push_str(&text[at..r.start]);
        out.push_str(&printed.text);
        out.push('\n');
        at = r.end;
    }
    out.push_str(&text[at..]);
    Ok((out, roots.len()))
}

#[test]
fn printed_expressions_keep_the_algebra_and_are_stable() {
    let Some(dir) = corpus::suite_dir() else {
        eprintln!("W3C suite not found (set SPARKLES_W3C_DIR): skipped");
        return;
    };
    let flipped = Options {
        operator_position: OperatorPosition::Trailing,
        quote_style: QuoteStyle::Preserve,
        line_width: 40,
        ..Options::default()
    };
    let (mut files, mut exprs, mut failures) = (0, 0, Vec::new());
    for case in corpus::cases(&dir) {
        let Ok(text) = std::fs::read_to_string(&case.path) else {
            continue;
        };
        let tokens = lex(&text, LexMode::Sparql);
        let Ok(reference) = sparql_reference(&text, &tokens) else {
            continue;
        };
        files += 1;
        for (name, opts) in [
            ("default", Options::default()),
            ("flipped", flipped.clone()),
        ] {
            let fail = |why: String| format!("{} ({name}): {why}", case.rel);
            let once = match splice(&text, &opts) {
                Ok((s, n)) => {
                    exprs += n;
                    s
                }
                Err(e) => {
                    failures.push(fail(e));
                    continue;
                }
            };
            if sparql_equivalent(&reference, &once).is_err() {
                failures.push(fail(format!("algebra differs:\n{once}")));
                continue;
            }
            if let Err(e) = comments::same(&text, &once, LexMode::Sparql) {
                failures.push(fail(format!("{e}:\n{once}")));
                continue;
            }
            // the newline after each expression is the only change a second pass may
            // make again
            match splice(&once, &opts) {
                Ok((twice, _))
                    if twice.lines().eq(once.lines()) || strip(&twice) == strip(&once) => {}
                Ok((twice, _)) => failures.push(fail(format!(
                    "not stable:\n--- once ---\n{once}--- twice ---\n{twice}"
                ))),
                Err(e) => failures.push(fail(format!("second pass: {e}"))),
            }
        }
    }
    eprintln!(
        "{files} files, {exprs} expressions printed, {} failures",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The lines without blank ones (the line breaks the splice adds).
fn strip(s: &str) -> Vec<&str> {
    s.lines().filter(|l| !l.trim().is_empty()).collect()
}
