//! R2RML string templates (R2RML §7.3): text with `{reference}` parts, where `\{`,
//! `\}` and `\\` are escapes. This is not an RFC 6570 template, so it is kept apart from
//! the C05 `uritemplate` module.

/// A parsed template.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    text: String,
    pub parts: Vec<Part>,
    /// RML-Core's `rml:URI` term type: values are percent-encoded to URI-safe form,
    /// non-ASCII characters included, instead of R2RML's IRI-safe form.
    uri_safe: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Ref(String),
}

impl Template {
    pub fn parse(text: &str) -> Result<Template, String> {
        let mut parts = Vec::new();
        let mut cur = String::new();
        let mut in_ref = false;
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some(e @ ('{' | '}' | '\\')) => cur.push(e),
                    Some(e) => {
                        return Err(format!(
                            "template {text:?}: \\{e} is not an escape; only \\{{, \\}} and \\\\ are"
                        ));
                    }
                    None => return Err(format!("template {text:?} ends with a backslash")),
                },
                '{' if !in_ref => {
                    if !cur.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut cur)));
                    }
                    in_ref = true;
                }
                '{' => return Err(format!("template {text:?} has a {{ inside a reference")),
                '}' if in_ref => {
                    if cur.is_empty() {
                        return Err(format!("template {text:?} has an empty reference {{}}"));
                    }
                    parts.push(Part::Ref(std::mem::take(&mut cur)));
                    in_ref = false;
                }
                '}' => return Err(format!("template {text:?} has a }} without a {{")),
                c => cur.push(c),
            }
        }
        if in_ref {
            return Err(format!("template {text:?} has an unclosed {{"));
        }
        if !cur.is_empty() {
            parts.push(Part::Text(cur));
        }
        Ok(Template {
            text: text.to_string(),
            parts,
            uri_safe: false,
        })
    }

    /// The template with URI-safe encoding of its values (`rml:URI`).
    pub fn with_uri_safe(mut self) -> Template {
        self.uri_safe = true;
        self
    }

    pub fn uri_safe(&self) -> bool {
        self.uri_safe
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn references(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match p {
            Part::Ref(r) => Some(r.as_str()),
            Part::Text(_) => None,
        })
    }

    /// The template written back with its escapes, for `convert`.
    pub fn escape_text(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if matches!(c, '{' | '}' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
}

/// R2RML's IRI-safe form of a string: every character other than the unreserved ASCII
/// characters and the `ucschar` ranges of RFC 3987 is percent-encoded as UTF-8.
pub fn iri_safe(s: &str, out: &mut String) {
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') || ucschar(c) {
            out.push(c);
        } else {
            let mut b = [0u8; 4];
            for x in c.encode_utf8(&mut b).bytes() {
                out.push('%');
                out.push(char::from(HEX[(x >> 4) as usize]));
                out.push(char::from(HEX[(x & 15) as usize]));
            }
        }
    }
}

/// The URI-safe form of a string: every character other than the unreserved ASCII
/// characters is percent-encoded as UTF-8 (RML-Core's `rml:URI` term type).
pub fn uri_safe(s: &str, out: &mut String) {
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 15) as usize]));
        }
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn ucschar(c: char) -> bool {
    matches!(c as u32,
        0xA0..=0xD7FF
        | 0xF900..=0xFDCF
        | 0xFDF0..=0xFFEF
        | 0x10000..=0x1FFFD
        | 0x20000..=0x2FFFD
        | 0x30000..=0x3FFFD
        | 0x40000..=0x4FFFD
        | 0x50000..=0x5FFFD
        | 0x60000..=0x6FFFD
        | 0x70000..=0x7FFFD
        | 0x80000..=0x8FFFD
        | 0x90000..=0x9FFFD
        | 0xA0000..=0xAFFFD
        | 0xB0000..=0xBFFFD
        | 0xC0000..=0xCFFFD
        | 0xD0000..=0xDFFFD
        | 0xE1000..=0xEFFFD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_escapes() {
        let t = Template::parse(r"http://ex.org/{id}/\{x\}/{a b}").unwrap();
        assert_eq!(
            t.parts,
            vec![
                Part::Text("http://ex.org/".into()),
                Part::Ref("id".into()),
                Part::Text("/{x}/".into()),
                Part::Ref("a b".into()),
            ]
        );
        assert!(Template::parse("{").is_err());
        assert!(Template::parse("}").is_err());
        assert!(Template::parse("{}").is_err());
        assert!(Template::parse(r"\n").is_err());
        assert_eq!(Template::escape_text("a{b}\\"), r"a\{b\}\\");
    }

    #[test]
    fn iri_safe_encoding() {
        let mut s = String::new();
        iri_safe("Hello World/ä?#42~", &mut s);
        assert_eq!(s, "Hello%20World%2Fä%3F%2342~");
    }
}
