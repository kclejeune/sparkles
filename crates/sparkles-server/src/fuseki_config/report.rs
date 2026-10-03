//! The report of a conversion: one line per element of the configuration.

use serde::Serialize;

/// What happened to an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Sparkles behaves as Fuseki did
    Converted,
    /// the output says what to run
    Manual,
    /// Sparkles behaves close to Fuseki; the message says how it differs
    Approximated,
    /// the element has no effect in Sparkles
    Ignored,
    /// Sparkles has no equivalent, and clients or users will notice
    Unsupported,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Converted => "converted",
            Kind::Manual => "manual",
            Kind::Approximated => "approximated",
            Kind::Ignored => "ignored",
            Kind::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub kind: Kind,
    /// where in the configuration: `server`, `service /ds`, `dataset /ds`, `shiro.ini`
    pub place: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    /// the files that were read
    pub inputs: Vec<String>,
    pub items: Vec<Item>,
}

impl Report {
    pub fn add(&mut self, kind: Kind, place: impl Into<String>, message: impl Into<String>) {
        self.items.push(Item {
            kind,
            place: place.into(),
            message: message.into(),
        });
    }

    pub fn converted(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.add(Kind::Converted, place, message);
    }
    pub fn manual(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.add(Kind::Manual, place, message);
    }
    pub fn approximated(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.add(Kind::Approximated, place, message);
    }
    pub fn ignored(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.add(Kind::Ignored, place, message);
    }
    pub fn unsupported(&mut self, place: impl Into<String>, message: impl Into<String>) {
        self.add(Kind::Unsupported, place, message);
    }

    pub fn count(&self, kind: Kind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }

    pub fn has_unsupported(&self) -> bool {
        self.count(Kind::Unsupported) > 0
    }

    /// The report as text: the inputs, one line per item, and a summary.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for i in &self.inputs {
            out.push_str(&format!("read {i}\n"));
        }
        if !self.inputs.is_empty() {
            out.push('\n');
        }
        let width = self.items.iter().map(|i| i.place.len()).max().unwrap_or(0);
        for i in &self.items {
            out.push_str(&format!(
                "{:<12} {:<width$}  {}\n",
                i.kind.label(),
                i.place,
                i.message
            ));
        }
        let parts: Vec<String> = [
            Kind::Converted,
            Kind::Manual,
            Kind::Approximated,
            Kind::Ignored,
            Kind::Unsupported,
        ]
        .iter()
        .map(|k| format!("{} {}", self.count(*k), k.label()))
        .collect();
        out.push_str(&format!("\n{}\n", parts.join(", ")));
        out
    }
}
