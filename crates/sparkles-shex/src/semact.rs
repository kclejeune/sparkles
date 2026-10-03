//! Semantic actions: a registry of handlers by extension IRI. The Test extension
//! (`fail("msg")`, `print(s|p|o|"lit")`) is built in; actions of any other extension
//! succeed without running and are counted for a warning.

use crate::ast::SemAct;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use sparkles_core::id::Id;
use sparkles_core::store::Snapshot;
use std::sync::Arc;

/// The extension IRI of the built-in Test extension.
pub const TEST_EXTENSION: &str = "http://shex.io/extensions/Test/";

/// Is `iri` the Test extension: its IRI, or that IRI with a fragment
/// (`http://shex.io/extensions/Test/#a`), which the test suite uses to give external
/// actions distinct names?
pub fn is_test_extension(iri: &str) -> bool {
    iri.strip_prefix(TEST_EXTENSION)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('#'))
}

/// Where an action runs, and what it sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site<'t> {
    /// a start action: no node
    Start,
    /// a shape's action, on the focus node that matches it
    Shape(Id),
    /// a triple constraint's action, on one matched triple
    Triple([Id; 3]),
    /// a group's (EachOf, OneOf) action, on the triples the group matched
    Group(&'t [[Id; 3]]),
}

impl Site<'_> {
    /// `(s, p, o)` as far as the site has them: the focus node is `s` for a shape, and a
    /// group shows its first triple.
    pub fn spo(&self) -> [Option<Id>; 3] {
        match self {
            Site::Start => [None; 3],
            Site::Shape(f) => [Some(*f), None, None],
            Site::Triple(t) => t.map(Some),
            Site::Group(ts) => ts.first().map_or([None; 3], |t| t.map(Some)),
        }
    }
}

/// A handler of one extension IRI.
pub trait SemActHandler: Send + Sync {
    /// Run an action (`code` is `None` for `%<iri>%`); `Ok(false)` fails the expression
    /// it is attached to, with the message left in [`ActCtx::failure`].
    fn run(&self, code: Option<&str>, site: Site<'_>, cx: &mut ActCtx<'_>) -> anyhow::Result<bool>;
}

/// The semantic-action handlers of a validation.
#[derive(Clone)]
pub struct Registry {
    /// keep the Test extension's `print` output
    pub trace: bool,
    /// run the built-in Test extension (otherwise its actions are skipped like those of
    /// any unknown extension)
    pub test: bool,
    handlers: FxHashMap<String, Arc<dyn SemActHandler>>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::new(false)
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ext: Vec<&String> = self.handlers.keys().collect();
        ext.sort();
        f.debug_struct("Registry")
            .field("trace", &self.trace)
            .field("test", &self.test)
            .field("handlers", &ext)
            .finish()
    }
}

/// A failed action: its extension and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActFailure {
    pub extension: String,
    pub message: String,
}

/// The state of semantic actions while matching: what `print` wrote and which unknown
/// extensions were skipped.
pub struct ActCtx<'a> {
    pub registry: &'a Registry,
    pub snap: &'a Snapshot,
    /// `print` output, in order (with [`Registry::trace`])
    pub prints: Vec<String>,
    /// actions not run, per unknown extension IRI
    pub unknown: FxHashMap<String, usize>,
    /// the last action that failed
    pub failure: Option<ActFailure>,
}

impl<'a> ActCtx<'a> {
    pub fn new(registry: &'a Registry, snap: &'a Snapshot) -> ActCtx<'a> {
        ActCtx {
            registry,
            snap,
            prints: Vec::new(),
            unknown: FxHashMap::default(),
            failure: None,
        }
    }

    /// Add another context's prints and unknown-extension counts (per-thread contexts
    /// of one validation).
    pub fn merge(&mut self, other: ActCtx<'_>) {
        self.prints.extend(other.prints);
        for (k, n) in other.unknown {
            *self.unknown.entry(k).or_default() += n;
        }
        if self.failure.is_none() {
            self.failure = other.failure;
        }
    }

    /// The report's warnings about skipped actions, one per unknown extension, sorted.
    pub fn warnings(&self) -> Vec<String> {
        unknown_warnings(&self.unknown)
    }

    /// A store term as the Test extension prints it: an IRI bare, other terms in
    /// N-Triples syntax.
    pub fn render(&self, id: Id) -> String {
        match self.snap.term(id) {
            Some(Term::NamedNode(n)) => n.into_string(),
            Some(t) => t.to_string(),
            None => format!("{id:?}"),
        }
    }
}

/// `"N semantic actions with extension <iri> were not run"`, one per extension, sorted.
pub fn unknown_warnings(unknown: &FxHashMap<String, usize>) -> Vec<String> {
    let mut v: Vec<(&String, &usize)> = unknown.iter().filter(|(_, n)| **n > 0).collect();
    v.sort();
    v.into_iter()
        .map(|(iri, n)| {
            if *n == 1 {
                format!("1 semantic action with extension <{iri}> was not run")
            } else {
                format!("{n} semantic actions with extension <{iri}> were not run")
            }
        })
        .collect()
}

impl Registry {
    /// The Test extension on, `print` output kept with `trace`.
    pub fn new(trace: bool) -> Registry {
        Registry {
            trace,
            test: true,
            handlers: FxHashMap::default(),
        }
    }

    /// Handle the actions of extension `iri` with `handler` (replacing the built-in
    /// Test extension for its IRI).
    pub fn register(&mut self, iri: impl Into<String>, handler: Arc<dyn SemActHandler>) {
        self.handlers.insert(iri.into(), handler);
    }

    /// Can any of `acts` fail (so matching must enumerate assignments for them)?
    pub fn can_fail(&self, acts: &[SemAct]) -> bool {
        acts.iter().any(|a| {
            self.handlers.contains_key(&a.name) || (self.test && is_test_extension(&a.name))
        })
    }

    /// Run the start actions, once per validation; `false`: an action failed.
    pub fn start(&self, acts: &[SemAct], cx: &mut ActCtx<'_>) -> anyhow::Result<bool> {
        self.run(acts, Site::Start, cx)
    }

    /// Run a shape's actions on a node that matches it.
    pub fn on_shape(
        &self,
        acts: &[SemAct],
        focus: Id,
        cx: &mut ActCtx<'_>,
    ) -> anyhow::Result<bool> {
        self.run(acts, Site::Shape(focus), cx)
    }

    /// Run a triple constraint's actions on one triple matched to it.
    pub fn on_tc(
        &self,
        acts: &[SemAct],
        triple: [Id; 3],
        cx: &mut ActCtx<'_>,
    ) -> anyhow::Result<bool> {
        self.run(acts, Site::Triple(triple), cx)
    }

    /// Run a group's (EachOf, OneOf) actions on the triples matched by the group.
    pub fn on_group(
        &self,
        acts: &[SemAct],
        triples: &[[Id; 3]],
        cx: &mut ActCtx<'_>,
    ) -> anyhow::Result<bool> {
        self.run(acts, Site::Group(triples), cx)
    }

    /// Run `acts` in order, up to the first that fails.
    fn run(&self, acts: &[SemAct], site: Site<'_>, cx: &mut ActCtx<'_>) -> anyhow::Result<bool> {
        for a in acts {
            cx.failure = None;
            let ok = if let Some(h) = self.handlers.get(&a.name) {
                h.run(a.code.as_deref(), site, cx)?
            } else if self.test && is_test_extension(&a.name) {
                run_test(a.code.as_deref(), site, cx)
            } else {
                *cx.unknown.entry(a.name.clone()).or_default() += 1;
                true
            };
            if !ok {
                cx.failure.get_or_insert_with(|| ActFailure {
                    extension: a.name.clone(),
                    message: "semantic action failed".to_string(),
                });
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// An argument of a Test-extension call.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TestArg {
    S,
    P,
    O,
    /// a string literal as written, quotes included
    Str(String),
}

/// A Test-extension call: `fail(arg)` or `print(arg)`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TestCall {
    Fail(TestArg),
    Print(TestArg),
}

/// Parse the code of a Test-extension action: `fail(…)` or `print(…)` with `s`, `p`,
/// `o` or a double-quoted string (with `\"` escapes), surrounded by any whitespace.
fn parse_test(code: &str) -> Option<TestCall> {
    let code = code.trim();
    let (name, rest) = if let Some(r) = code.strip_prefix("fail") {
        ("fail", r)
    } else {
        ("print", code.strip_prefix("print")?)
    };
    let arg = rest
        .trim_start()
        .strip_prefix('(')?
        .trim_end()
        .strip_suffix(')')?
        .trim();
    let arg = match arg {
        "s" => TestArg::S,
        "p" => TestArg::P,
        "o" => TestArg::O,
        _ => {
            let inner = arg.strip_prefix('"')?.strip_suffix('"')?;
            let mut escaped = false;
            for c in inner.chars() {
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => return None,
                    _ => {}
                }
            }
            if escaped {
                return None;
            }
            TestArg::Str(arg.to_string())
        }
    };
    Some(if name == "fail" {
        TestCall::Fail(arg)
    } else {
        TestCall::Print(arg)
    })
}

/// Run a Test-extension action.
fn run_test(code: Option<&str>, site: Site<'_>, cx: &mut ActCtx<'_>) -> bool {
    // `%<Test>%`: nothing to run
    let Some(code) = code else {
        return true;
    };
    let fail = |cx: &mut ActCtx<'_>, message: String| {
        cx.failure = Some(ActFailure {
            extension: TEST_EXTENSION.to_string(),
            message,
        });
        false
    };
    let Some(call) = parse_test(code) else {
        return fail(
            cx,
            format!("unrecognized Test extension code: {}", code.trim()),
        );
    };
    let [s, p, o] = site.spo();
    let text = |arg: &TestArg, cx: &ActCtx<'_>, quoted: bool| -> Option<String> {
        let id = match arg {
            TestArg::S => s,
            TestArg::P => p,
            TestArg::O => o,
            TestArg::Str(lit) if quoted => return Some(lit.clone()),
            TestArg::Str(lit) => {
                return Some(
                    lit[1..lit.len() - 1]
                        .replace("\\\"", "\"")
                        .replace("\\\\", "\\"),
                );
            }
        };
        id.map(|id| cx.render(id))
    };
    match call {
        TestCall::Print(arg) => {
            if cx.registry.trace
                && let Some(t) = text(&arg, cx, true)
            {
                cx.prints.push(t);
            }
            true
        }
        // a failure also prints its message, as the test suite expects
        TestCall::Fail(arg) => {
            if cx.registry.trace
                && let Some(t) = text(&arg, cx, true)
            {
                cx.prints.push(t);
            }
            let msg = text(&arg, cx, false).unwrap_or_default();
            fail(cx, msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};
    use sparkles_core::store::{Store, StoreOptions};

    fn act(name: &str, code: Option<&str>) -> SemAct {
        SemAct {
            name: name.to_string(),
            code: code.map(str::to_string),
        }
    }

    fn test(code: &str) -> SemAct {
        act(TEST_EXTENSION, Some(code))
    }

    /// A store with `<http://a.example/s1> <http://a.example/p1> "o1"`, and the ids of
    /// the triple.
    fn store() -> (Store, [Id; 3]) {
        let store = Store::in_memory(StoreOptions::default());
        let mut txn = store.write();
        let iri = |s: &str| Term::NamedNode(NamedNode::new_unchecked(s));
        let s = txn.intern(&iri("http://a.example/s1")).unwrap();
        let p = txn.intern(&iri("http://a.example/p1")).unwrap();
        let o = txn
            .intern(&Literal::new_simple_literal("o1").into())
            .unwrap();
        txn.insert([s, p, o, Id::DEFAULT_GRAPH]).unwrap();
        txn.commit().unwrap();
        (store, [s, p, o])
    }

    #[test]
    fn parses_test_code() {
        use TestArg::*;
        assert_eq!(parse_test(" print(o) "), Some(TestCall::Print(O)));
        assert_eq!(parse_test("fail ( s )"), Some(TestCall::Fail(S)));
        assert_eq!(
            parse_test(r#"print("%{\\%}")"#),
            Some(TestCall::Print(Str(r#""%{\\%}""#.into())))
        );
        assert_eq!(
            parse_test(r#"fail("say \"no\"")"#),
            Some(TestCall::Fail(Str(r#""say \"no\"""#.into())))
        );
        assert_eq!(parse_test("print(x)"), None);
        assert_eq!(parse_test(r#"print("a" "b")"#), None);
        assert_eq!(parse_test("printf(o)"), None);
        assert_eq!(parse_test("print(o); fail(s)"), None);
    }

    #[test]
    fn print_with_trace() {
        let (store, t) = store();
        let snap = store.snapshot();
        let reg = Registry::new(true);
        let mut cx = ActCtx::new(&reg, &snap);
        let acts = [test("print(s)"), test("print(p)"), test("print(o)")];
        assert!(reg.on_tc(&acts, t, &mut cx).unwrap());
        assert!(
            reg.on_shape(&[test(r#"print("shape action")"#)], t[0], &mut cx)
                .unwrap()
        );
        assert!(reg.start(&[test(r#" print("%{\\%}") "#)], &mut cx).unwrap());
        // nothing to print at the start: `s` is not bound
        assert!(reg.start(&[test("print(s)")], &mut cx).unwrap());
        assert!(reg.on_group(&[test("print(o)")], &[t, t], &mut cx).unwrap());
        assert_eq!(
            cx.prints,
            [
                "http://a.example/s1",
                "http://a.example/p1",
                "\"o1\"",
                "\"shape action\"",
                r#""%{\\%}""#,
                "\"o1\""
            ]
        );
        assert!(cx.warnings().is_empty());
        // without the trace nothing is kept
        let quiet = Registry::new(false);
        let mut cx = ActCtx::new(&quiet, &snap);
        assert!(quiet.on_tc(&acts, t, &mut cx).unwrap());
        assert!(cx.prints.is_empty());
    }

    #[test]
    fn fail_stops_the_actions() {
        let (store, t) = store();
        let snap = store.snapshot();
        let reg = Registry::new(true);
        let mut cx = ActCtx::new(&reg, &snap);
        let acts = [test("print(s)"), test("fail(s)"), test("print(o)")];
        assert!(!reg.on_tc(&acts, t, &mut cx).unwrap());
        assert_eq!(cx.prints, ["http://a.example/s1", "http://a.example/s1"]);
        assert_eq!(
            cx.failure,
            Some(ActFailure {
                extension: TEST_EXTENSION.into(),
                message: "http://a.example/s1".into()
            })
        );
        assert!(!reg.start(&[test(r#"fail("no")"#)], &mut cx).unwrap());
        assert_eq!(cx.failure.as_ref().unwrap().message, "no");
        assert_eq!(cx.prints.last().unwrap(), "\"no\"");
        // the suite names external Test actions with fragments
        assert!(is_test_extension("http://shex.io/extensions/Test/#a"));
        assert!(!is_test_extension("http://shex.io/extensions/Test/a"));
        let named = [act("http://shex.io/extensions/Test/#b", Some("print(p)"))];
        assert!(reg.can_fail(&named));
        assert!(reg.on_tc(&named, t, &mut cx).unwrap());
        assert_eq!(cx.prints.last().unwrap(), "http://a.example/p1");
        // code the Test extension does not know fails, with a message
        assert!(!reg.on_shape(&[test("explode()")], t[0], &mut cx).unwrap());
        assert!(cx.failure.as_ref().unwrap().message.contains("explode()"));
        // a later success clears the failure; `%<Test>%` has nothing to run
        assert!(reg.on_tc(&[act(TEST_EXTENSION, None)], t, &mut cx).unwrap());
        assert_eq!(cx.failure, None);
        assert!(reg.can_fail(&acts));
        // with the Test extension off, its actions are skipped like unknown ones
        let off = Registry {
            test: false,
            ..Registry::new(false)
        };
        let mut cx = ActCtx::new(&off, &snap);
        assert!(!off.can_fail(&acts));
        assert!(off.on_tc(&acts, t, &mut cx).unwrap());
        assert_eq!(cx.unknown[TEST_EXTENSION], 3);
    }

    #[test]
    fn unknown_extensions_are_counted() {
        let (store, t) = store();
        let snap = store.snapshot();
        let reg = Registry::default();
        let mut cx = ActCtx::new(&reg, &snap);
        let js = act("http://ex.org/js", Some("anything at all"));
        assert!(!reg.can_fail(std::slice::from_ref(&js)));
        assert!(reg.on_tc(std::slice::from_ref(&js), t, &mut cx).unwrap());
        assert_eq!(
            cx.warnings(),
            ["1 semantic action with extension <http://ex.org/js> was not run"]
        );
        let mut other = ActCtx::new(&reg, &snap);
        assert!(
            reg.on_shape(
                &[js.clone(), act("http://ex.org/a", None)],
                t[0],
                &mut other
            )
            .unwrap()
        );
        cx.merge(other);
        assert_eq!(
            cx.warnings(),
            [
                "1 semantic action with extension <http://ex.org/a> was not run",
                "2 semantic actions with extension <http://ex.org/js> were not run"
            ]
        );
    }

    #[test]
    fn registered_handlers() {
        struct Odd;
        impl SemActHandler for Odd {
            fn run(
                &self,
                code: Option<&str>,
                site: Site<'_>,
                cx: &mut ActCtx<'_>,
            ) -> anyhow::Result<bool> {
                cx.prints.push(format!("{code:?}"));
                Ok(matches!(site, Site::Triple(_)))
            }
        }
        let (store, t) = store();
        let snap = store.snapshot();
        let mut reg = Registry::new(false);
        reg.register("http://ex.org/odd", Arc::new(Odd));
        let mut cx = ActCtx::new(&reg, &snap);
        let acts = [act("http://ex.org/odd", Some("x"))];
        assert!(reg.can_fail(&acts));
        assert!(reg.on_tc(&acts, t, &mut cx).unwrap());
        assert!(!reg.on_shape(&acts, t[0], &mut cx).unwrap());
        assert_eq!(cx.failure.as_ref().unwrap().extension, "http://ex.org/odd");
        assert!(cx.warnings().is_empty());
        assert_eq!(cx.prints, ["Some(\"x\")", "Some(\"x\")"]);
    }
}
