//! YAML frontmatter, read as far as the harnesses use it: scalars, one level of nested
//! maps such as `metadata.type`, and lists in block or flow form. Values stay strings.
//! Each value keeps the line it came from, so a fact can quote it.

/// A value of the frontmatter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Scalar(String),
    List(Vec<String>),
    Map(Vec<(String, Entry)>),
}

/// A value with the source line it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub value: Value,
    /// the line of the key, as written
    pub line: String,
}

/// A file split into its frontmatter and its body.
#[derive(Clone, Debug, Default)]
pub struct Document<'a> {
    pub entries: Vec<(String, Entry)>,
    /// whether the file starts with a frontmatter block
    pub has_frontmatter: bool,
    /// the text after the frontmatter
    pub body: &'a str,
}

impl Document<'_> {
    /// The entry of a top-level key.
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, e)| e)
    }

    /// The entry of `a.b`, a key of a nested map.
    pub fn get_nested(&self, a: &str, b: &str) -> Option<&Entry> {
        match &self.get(a)?.value {
            Value::Map(m) => m.iter().find(|(k, _)| k == b).map(|(_, e)| e),
            _ => None,
        }
    }

    /// A scalar value with its line.
    pub fn scalar(&self, key: &str) -> Option<(&str, &str)> {
        match self.get(key) {
            Some(Entry {
                value: Value::Scalar(s),
                line,
            }) if !s.is_empty() => Some((s.as_str(), line.as_str())),
            _ => None,
        }
    }

    /// A list, or a scalar as a list of one (globs are written both ways), with its line.
    pub fn list(&self, key: &str) -> Option<(Vec<String>, &str)> {
        match self.get(key) {
            Some(Entry {
                value: Value::List(l),
                line,
            }) => Some((l.clone(), line.as_str())),
            Some(Entry {
                value: Value::Scalar(s),
                line,
            }) if !s.is_empty() => Some((
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect(),
                line.as_str(),
            )),
            _ => None,
        }
    }
}

/// Split `text` into its frontmatter and body. A file without a closing `---` has no
/// frontmatter.
pub fn parse(text: &str) -> Document<'_> {
    let rest = match text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        Some(r) => r,
        None => {
            return Document {
                entries: Vec::new(),
                has_frontmatter: false,
                body: text,
            };
        }
    };
    // the closing line
    let mut offset = 0;
    let mut end = None;
    for line in rest.split_inclusive('\n') {
        let t = line.trim_end_matches(['\n', '\r']);
        if t == "---" || t == "..." {
            end = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((yaml_end, body_start)) = end else {
        return Document {
            entries: Vec::new(),
            has_frontmatter: false,
            body: text,
        };
    };
    let yaml = &rest[..yaml_end];
    Document {
        entries: parse_block(yaml),
        has_frontmatter: true,
        body: &rest[body_start..],
    }
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

/// A scalar as written: quotes removed, a trailing comment removed from a plain value.
fn scalar(raw: &str) -> String {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if let Some(inner) = t.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    match t.find(" #") {
        Some(i) => t[..i].trim_end().to_string(),
        None => t.to_string(),
    }
}

/// `[a, b]` as a list.
fn flow_list(raw: &str) -> Option<Vec<String>> {
    let t = raw.trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(scalar)
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// The `key: value` pairs of a block whose keys all have the block's least indentation.
fn parse_block(yaml: &str) -> Vec<(String, Entry)> {
    let lines: Vec<&str> = yaml.lines().map(|l| l.trim_end_matches('\r')).collect();
    let base = lines
        .iter()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| indent(l))
        .min()
        .unwrap_or(0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || indent(line) != base {
            i += 1;
            continue;
        }
        let Some((k, v)) = t.split_once(':') else {
            i += 1;
            continue;
        };
        let key = scalar(k);
        let v = v.trim();
        // the lines indented under this key
        let mut j = i + 1;
        while j < lines.len() && (lines[j].trim().is_empty() || indent(lines[j]) > base) {
            j += 1;
        }
        let child: Vec<&str> = lines[i + 1..j].to_vec();
        let value = if v.is_empty() {
            let items: Vec<&str> = child
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();
            if !items.is_empty() && items.iter().all(|l| l.starts_with('-')) {
                Value::List(
                    items
                        .iter()
                        .map(|l| scalar(l.trim_start_matches('-')))
                        .collect(),
                )
            } else if items.is_empty() {
                Value::Scalar(String::new())
            } else {
                Value::Map(parse_block(&child.join("\n")))
            }
        } else if v == "|" || v == ">" || v.starts_with("|-") || v.starts_with(">-") {
            let parts: Vec<&str> = child.iter().map(|l| l.trim()).collect();
            let sep = if v.starts_with('|') { "\n" } else { " " };
            Value::Scalar(parts.join(sep).trim().to_string())
        } else if let Some(l) = flow_list(v) {
            Value::List(l)
        } else {
            Value::Scalar(scalar(v))
        };
        out.push((
            key,
            Entry {
                value,
                line: line.trim().to_string(),
            },
        ));
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_frontmatter() {
        let d = parse(
            "---\nname: staging-db\ndescription: \"Staging has its own Postgres\"\nmetadata:\n  type: reference\nmodified: 2026-10-07T15:02:11Z\n---\nBody [[x]]\n",
        );
        assert!(d.has_frontmatter);
        assert_eq!(
            d.scalar("name").unwrap(),
            ("staging-db", "name: staging-db")
        );
        assert_eq!(
            d.scalar("description").unwrap().0,
            "Staging has its own Postgres"
        );
        let t = d.get_nested("metadata", "type").unwrap();
        assert_eq!(t.value, Value::Scalar("reference".into()));
        assert_eq!(t.line, "type: reference");
        assert_eq!(d.body, "Body [[x]]\n");
    }

    #[test]
    fn lists_and_absent_frontmatter() {
        let d = parse(
            "---\npaths:\n  - \"src/**/*.rs\"\n  - tests/*\nglobs: a, b\nalwaysApply: true\n---\nx",
        );
        assert_eq!(d.list("paths").unwrap().0, vec!["src/**/*.rs", "tests/*"]);
        assert_eq!(d.list("globs").unwrap().0, vec!["a", "b"]);
        assert_eq!(d.scalar("alwaysApply").unwrap().0, "true");
        let d = parse("---\nglobs: [\"*.ts\", '*.tsx']\n---\n");
        assert_eq!(d.list("globs").unwrap().0, vec!["*.ts", "*.tsx"]);
        let d = parse("# Title\n---\nname: x\n---\n");
        assert!(!d.has_frontmatter);
        let d = parse("---\nname: x\nno end");
        assert!(!d.has_frontmatter);
    }
}
