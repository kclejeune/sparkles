//! JSON and JSON Lines sources (spec F19 §4.2). The iterator is a JSONPath and each
//! match is one iteration. References are JSONPaths relative to the match, and a
//! reference that matches several values gives one value each. A JSON document is
//! parsed whole, up to a size limit. JSON Lines is read one line at a time, with the
//! iterator applied to each line, so its memory stays flat.

use super::{Cell, Dt, Row, RowSource, check_file};
use crate::error::{Error, Result};
use crate::mapping::RunOptions;
use crate::mapping::jsonpath::Path as JPath;
use serde_json::Value as J;
use std::borrow::Cow;
use std::io::BufRead;
use std::path::PathBuf;

pub(super) struct JsonSource {
    path: PathBuf,
    name: String,
    lines: bool,
    iterator: JPath,
    refs: Vec<JPath>,
    names: Vec<String>,
    opts: RunOptions,
}

impl JsonSource {
    pub(super) fn new(
        path: PathBuf,
        name: &str,
        lines: bool,
        iterator: Option<&str>,
        refs: &[String],
        opts: &RunOptions,
    ) -> Result<JsonSource> {
        check_file(&path, name)?;
        let bad = |e: String| Error::invalid(format!("{name}: {e}"));
        let iterator = JPath::parse(iterator.unwrap_or("$")).map_err(bad)?;
        let names = refs;
        let refs = refs
            .iter()
            .map(|r| JPath::parse(r).map_err(bad))
            .collect::<Result<Vec<_>>>()?;
        Ok(JsonSource {
            path,
            name: name.to_string(),
            lines,
            iterator,
            refs,
            names: names.to_vec(),
            opts: opts.clone(),
        })
    }
}

struct JsonRow<'r> {
    node: &'r J,
    refs: &'r [JPath],
    names: &'r [String],
    pos: &'r dyn Fn() -> String,
}

/// The value of a JSON node: none for `null`. An array or an object is not a value
/// (RML-Core): a reference selects the items of an array with `[*]`.
fn push<'a>(v: &'a J, out: &mut Vec<Cell<'a>>) -> std::result::Result<(), ()> {
    match v {
        J::Null => {}
        J::Bool(b) => out.push(Cell {
            lex: Cow::Borrowed(if *b { "true" } else { "false" }),
            dt: Dt::Boolean,
        }),
        J::Number(n) => {
            // serde_json keeps an integer exactly and reads every other number as a
            // double, whose printed form may differ from the source's (ROADMAP item 239)
            let lex = n.to_string();
            let dt = if n.is_f64() { Dt::Double } else { Dt::Integer };
            out.push(Cell {
                lex: Cow::Owned(lex),
                dt,
            });
        }
        J::String(s) => out.push(Cell::str(s)),
        J::Array(_) | J::Object(_) => return Err(()),
    }
    Ok(())
}

impl Row for JsonRow<'_> {
    fn get<'a>(&'a self, r: usize, out: &mut Vec<Cell<'a>>) -> Result<()> {
        for v in self.refs[r].select(self.node) {
            if push(v, out).is_err() {
                return Err(Error::invalid(format!(
                    "{}: the reference {:?} selects a JSON {}, not a value; select the items of an array with [*]",
                    (self.pos)(),
                    self.names[r],
                    if v.is_array() { "array" } else { "object" }
                )));
            }
        }
        Ok(())
    }

    fn position(&self) -> String {
        (self.pos)()
    }
}

impl RowSource for JsonSource {
    fn scan(&mut self, f: &mut dyn FnMut(&dyn Row) -> Result<()>) -> Result<()> {
        let input = crate::tabular::open(&self.path, self.opts.max_decompressed)?;
        let name = self.name.clone();
        let io = |e: std::io::Error| Error::invalid(format!("{name}: {e}"));
        let mut n = 0u64;
        if self.lines {
            let mut r = std::io::BufReader::with_capacity(1 << 20, input);
            let mut line = String::new();
            let mut ln = 0u64;
            loop {
                line.clear();
                if r.read_line(&mut line).map_err(io)? == 0 {
                    break;
                }
                ln += 1;
                if line.trim().is_empty() {
                    continue;
                }
                let doc: J = serde_json::from_str(&line)
                    .map_err(|e| Error::invalid(format!("{}: line {ln}: {e}", self.name)))?;
                for node in self.iterator.select(&doc) {
                    n += 1;
                    if n.is_multiple_of(65_536)
                        && let Some(c) = &self.opts.check
                    {
                        c()?;
                    }
                    let k = n;
                    let pos = || format!("{}, line {ln}, iteration {k}", self.name);
                    f(&JsonRow {
                        node,
                        refs: &self.refs,
                        names: &self.names,
                        pos: &pos,
                    })?;
                }
            }
            return Ok(());
        }
        let limit = self.opts.max_json_bytes;
        let mut bytes = Vec::new();
        use std::io::Read;
        input.take(limit + 1).read_to_end(&mut bytes).map_err(io)?;
        if bytes.len() as u64 > limit {
            return Err(Error::invalid(format!(
                "{}: the JSON document is larger than {}; JSON Lines is read one line at a time, or raise --max-json-mb",
                self.name,
                crate::error::human_bytes(limit)
            )));
        }
        let doc: J = serde_json::from_slice(&bytes)
            .map_err(|e| Error::invalid(format!("{}: not JSON: {e}", self.name)))?;
        drop(bytes);
        for node in self.iterator.select(&doc) {
            n += 1;
            if n.is_multiple_of(65_536)
                && let Some(c) = &self.opts.check
            {
                c()?;
            }
            let k = n;
            let pos = || format!("{}, iteration {k}", self.name);
            f(&JsonRow {
                node,
                refs: &self.refs,
                names: &self.names,
                pos: &pos,
            })?;
        }
        Ok(())
    }
}
