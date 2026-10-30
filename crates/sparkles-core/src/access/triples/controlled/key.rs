//! Exact nonalternate legacy Debug keys, with bounded string formatting work.
use super::*;
use std::fmt::{self, Write};

const CHUNK: usize = 256;
// A byte can expand to at most six ASCII bytes under string Debug. UTF8 scalars
// occupy no more than twelve escaped ASCII bytes each. This leaves ample room
// for one256byte UTF8 chunk and its two surrounding quotes.
const SCRATCH: usize = 2048;

struct Key<'a> {
    text: Text,
    preparation: &'a Preparation,
    _scratch: Reservation,
}

struct SliceWriter {
    bytes: [u8; SCRATCH],
    len: usize,
}

impl fmt::Write for SliceWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        let target = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        target.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Key<'_> {
    fn raw(&mut self, text: &str) -> Result<()> {
        self.text.push_str(text)
    }

    fn quoted(&mut self, mut value: &str) -> Result<()> {
        self.raw("\"")?;
        while !value.is_empty() {
            self.preparation.check()?;
            let mut end = CHUNK.min(value.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            let (chunk, rest) = value.split_at(end);
            // Admission for this fixed stack frame belongs to Key::_scratch and
            // is acquired by serialize before entering this helper.
            let mut scratch = SliceWriter {
                bytes: [0; SCRATCH],
                len: 0,
            };
            write!(&mut scratch, "{chunk:?}")
                .map_err(|_| Error::invalid("permission key formatter overflow"))?;
            let escaped = std::str::from_utf8(&scratch.bytes[1..scratch.len - 1])
                .map_err(|_| Error::invalid("invalid permission key formatting"))?;
            self.raw(escaped)?;
            if chunk.len() >= CHUNK - 3 {
                self.preparation
                    .checkpoint("serializing permission policy")?;
            } else {
                self.preparation.check()?;
            }
            value = rest;
        }
        self.raw("\"")
    }

    fn boolean(&mut self, value: bool) -> Result<()> {
        self.raw(if value { "true" } else { "false" })
    }

    fn strings(&mut self, values: &[String]) -> Result<()> {
        self.raw("[")?;
        for (at, value) in values.iter().enumerate() {
            self.preparation.check()?;
            if at != 0 {
                self.raw(", ")?;
            }
            self.quoted(value)?;
        }
        self.raw("]")
    }

    fn optional_strings(&mut self, values: Option<&[String]>) -> Result<()> {
        match values {
            None => self.raw("None"),
            Some(values) => {
                self.raw("Some(")?;
                self.strings(values)?;
                self.raw(")")
            }
        }
    }

    fn graphs(&mut self, graphs: &Graphs) -> Result<()> {
        let Graphs::Only(rule) = graphs else {
            return self.raw("All");
        };
        self.raw("Only(GraphRule { default_graph: ")?;
        self.boolean(rule.default_graph)?;
        self.raw(", iris: ")?;
        self.strings(&rule.iris)?;
        self.raw(", patterns: ")?;
        self.strings(&rule.patterns)?;
        self.raw(", protected: ")?;
        self.strings(&rule.protected)?;
        self.raw(", blank_nodes: ")?;
        self.boolean(rule.blank_nodes)?;
        self.raw(", excluded: ")?;
        self.strings(&rule.excluded)?;
        self.raw(", guards: [")?;
        for (at, guard) in rule.guards.iter().enumerate() {
            self.preparation.check()?;
            if at != 0 {
                self.raw(", ")?;
            }
            self.raw("GraphGuard { pattern: ")?;
            self.quoted(&guard.pattern)?;
            self.raw(", exempt: ")?;
            self.strings(&guard.exempt)?;
            self.raw(", decide: ")?;
            self.raw(if guard.decide.is_some() {
                "Some(Decider)"
            } else {
                "None"
            })?;
            self.raw(" }")?;
        }
        self.raw("] })")
    }

    fn protection(&mut self, value: &Protection) -> Result<()> {
        self.raw("Protection { name: ")?;
        self.quoted(&value.name)?;
        self.raw(", predicates: ")?;
        self.optional_strings(value.predicates.as_deref())?;
        self.raw(", classes: ")?;
        self.optional_strings(value.classes.as_deref())?;
        self.raw(", subclasses: ")?;
        self.boolean(value.subclasses)?;
        self.raw(", graphs: ")?;
        if let Some(graphs) = &value.graphs {
            self.raw("Some(")?;
            self.graphs(graphs)?;
            self.raw(")")?;
        } else {
            self.raw("None")?;
        }
        self.raw(", pattern: ")?;
        if let Some(pattern) = &value.pattern {
            self.raw("Some(")?;
            self.quoted(pattern)?;
            self.raw(")")?;
        } else {
            self.raw("None")?;
        }
        self.raw(", prefixes: {")?;
        for (at, (prefix, iri)) in value.prefixes.iter().enumerate() {
            self.preparation.check()?;
            if at != 0 {
                self.raw(", ")?;
            }
            self.quoted(prefix)?;
            self.raw(": ")?;
            self.quoted(iri)?;
        }
        self.raw("}, hide_inferences: ")?;
        self.boolean(value.hide_inferences)?;
        self.raw(" }")
    }
}

pub(super) fn serialize(
    access: &GraphAccess,
    rules: &TripleRules,
    preparation: &Preparation,
) -> Result<(String, Reservation)> {
    // Reserve before quoted's fixed stack scratch can be entered. All owned
    // output and moving growth remain admitted by the existing Text builder.
    let scratch = preparation.reserve(SCRATCH as u64)?;
    let mut key = Key {
        text: Text::new(preparation)?,
        preparation,
        _scratch: scratch,
    };
    key.graphs(&access.read)?;
    key.raw("|[")?;
    let mut first = true;
    for rule in &rules.rules {
        preparation.check()?;
        if rule.read.is_all() {
            continue;
        }
        if !first {
            key.raw(", ")?;
        }
        first = false;
        key.raw("(")?;
        key.protection(&rule.protection)?;
        key.raw(", ")?;
        key.graphs(&rule.read)?;
        key.raw(")")?;
    }
    key.raw("]|")?;
    // Only two fixed-width integers reach ordinary formatting here.
    let _ = write!(key.text, "{:?}", rules.limits);
    preparation.check()?;
    key.text.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{Caller, Decider, GuardDecision, Limits};
    use crate::store::Snapshot;
    use crate::task::Control;

    fn quoted(value: &str) -> String {
        let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
        let mut key = Key {
            text: Text::new(&preparation).unwrap(),
            preparation: &preparation,
            _scratch: preparation.reserve(SCRATCH as u64).unwrap(),
        };
        key.quoted(value).unwrap();
        key.text.finish().unwrap().0
    }

    #[test]
    fn bounded_string_debug_matches_installed_formatter_at_utf8_boundaries() {
        let mut scalars: String = (0..=0x7ff).filter_map(char::from_u32).collect();
        scalars
            .push_str("😀\u{10ffff}\u{200d}\u{301}\u{2028}\u{2029}\u{fe0f}\u{e0100}\"'\\\0\t\n\r");
        for value in [
            String::new(),
            scalars.clone(),
            scalars.repeat(3),
            "a".repeat(1 << 18),
        ] {
            assert_eq!(quoted(&value), format!("{value:?}"));
        }
        for boundary in 250..262 {
            for marker in ["😀", "\u{301}", "'", "\"", "\\", "\0", "\n"] {
                let value = format!("{}{marker}{}", "x".repeat(boundary), "y".repeat(270));
                assert_eq!(quoted(&value), format!("{value:?}"));
            }
        }
    }

    #[test]
    fn controlled_keys_match_every_legacy_policy_field_and_collection_format() {
        struct Uncalled;
        impl GuardDecision for Uncalled {
            fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
                panic!("formatting cannot run providers")
            }
        }
        let strings = vec![
            String::new(),
            "quoted\"\\\n'\u{301}😀".into(),
            "ascii".repeat(200),
        ];
        let graphs = Graphs::Only(GraphRule {
            default_graph: true,
            iris: strings.clone(),
            patterns: strings.clone(),
            protected: strings.clone(),
            blank_nodes: true,
            excluded: strings.clone(),
            guards: vec![
                GraphGuard {
                    pattern: strings[2].clone(),
                    exempt: strings.clone(),
                    decide: None,
                },
                GraphGuard {
                    pattern: "other".into(),
                    exempt: Vec::new(),
                    decide: Some(Decider(Arc::new(Uncalled))),
                },
            ],
        });
        let protection = Protection {
            name: strings[2].clone(),
            predicates: Some(strings.clone()),
            classes: Some(Vec::new()),
            subclasses: true,
            graphs: Some(graphs.clone()),
            pattern: Some(strings[1].clone()),
            prefixes: strings
                .iter()
                .map(|s| (s.clone(), strings[1].clone()))
                .collect(),
            hide_inferences: true,
        };
        for read in [Graphs::All, Graphs::none(), graphs.clone()] {
            for full in [false, true] {
                let mut p = protection.clone();
                if !full {
                    p.predicates = None;
                    p.classes = None;
                    p.graphs = None;
                    p.pattern = None;
                    p.prefixes.clear();
                    p.subclasses = false;
                    p.hide_inferences = false;
                }
                let rules = TripleRules {
                    rules: vec![
                        Rule {
                            protection: Arc::new(p.clone()),
                            read: Graphs::none(),
                            write: graphs.clone(),
                        },
                        Rule {
                            protection: Arc::new(p.clone()),
                            read: Graphs::All,
                            write: Graphs::none(),
                        },
                        Rule {
                            protection: Arc::new(p),
                            read: graphs.clone(),
                            write: Graphs::All,
                        },
                    ],
                    caller: Caller::default(),
                    limits: Limits {
                        max_hidden: u64::MAX,
                        max_pattern_rows: 0,
                    },
                };
                let access = GraphAccess::with_triples(read.clone(), graphs.clone(), rules.clone());
                let entries: Vec<_> = rules
                    .rules
                    .iter()
                    .filter(|r| !r.read.is_all())
                    .map(|r| (&*r.protection, &r.read))
                    .collect();
                let expected = format!("{:?}|{:?}|{:?}", access.read, entries, rules.limits);
                let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
                assert_eq!(
                    serialize(&access, &rules, &preparation).unwrap().0,
                    expected
                );
            }
        }
    }
}
