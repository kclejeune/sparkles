//! A bounded XML tree, and helpers for reading XML as a stream.
//!
//! Names are kept without their namespace prefix: the formats read here use fixed
//! prefixes in practice, and the elements the converters look for do not collide within
//! one part. A DOCTYPE is refused, since none of these formats has one, and quick-xml
//! expands only the five predefined entities and character references, so a document
//! cannot expand entities into more text.

use crate::{Error, Limits};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, BytesText, Event};

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Child>,
}

#[derive(Clone, Debug)]
pub enum Child {
    Node(Node),
    Text(String),
}

impl Node {
    /// An attribute by its full name (`r:id`), or else by its name without prefix.
    pub fn attr(&self, name: &str) -> Option<&str> {
        let found = self.attrs.iter().find(|(k, _)| k == name).or_else(|| {
            self.attrs
                .iter()
                .find(|(k, _)| k.rsplit_once(':').is_some_and(|(_, l)| l == name))
        });
        found.map(|(_, v)| v.as_str())
    }

    /// The child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Node> {
        self.children.iter().filter_map(|c| match c {
            Child::Node(n) => Some(n),
            Child::Text(_) => None,
        })
    }

    /// The first child element named `name`.
    pub fn first(&self, name: &str) -> Option<&Node> {
        self.elements().find(|n| n.name == name)
    }

    /// Every descendant element named `name`, in document order.
    pub fn descendants(&self, name: &str) -> Vec<&Node> {
        let mut out = Vec::new();
        self.collect(name, &mut out);
        out
    }

    fn collect<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        for n in self.elements() {
            if n.name == name {
                out.push(n);
            }
            n.collect(name, out);
        }
    }

    /// The first descendant element named `name`.
    pub fn find(&self, name: &str) -> Option<&Node> {
        for n in self.elements() {
            if n.name == name {
                return Some(n);
            }
            if let Some(f) = n.find(name) {
                return Some(f);
            }
        }
        None
    }

    /// All the text below this element, concatenated.
    pub fn text(&self) -> String {
        let mut s = String::new();
        self.push_text(&mut s);
        s
    }

    fn push_text(&self, out: &mut String) {
        for c in &self.children {
            match c {
                Child::Text(t) => out.push_str(t),
                Child::Node(n) => n.push_text(out),
            }
        }
    }
}

pub fn local(name: &[u8]) -> String {
    let n = match name.iter().rposition(|b| *b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    };
    String::from_utf8_lossy(n).into_owned()
}

pub fn attrs(e: &BytesStart) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for a in e.attributes().with_checks(false).flatten() {
        let v = match a.unescape_value() {
            Ok(v) => v.into_owned(),
            Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
        };
        out.push((String::from_utf8_lossy(a.key.as_ref()).into_owned(), v));
    }
    out
}

pub fn text_of(t: &BytesText) -> String {
    match t.unescape() {
        Ok(s) => s.into_owned(),
        Err(_) => String::from_utf8_lossy(t).into_owned(),
    }
}

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("the XML cannot be read: {e}"))
}

/// A reader over the bytes of a part.
pub fn reader(bytes: &[u8]) -> Reader<&[u8]> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    Reader::from_reader(bytes)
}

/// Parse a part into a tree under the limits.
pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Node, Error> {
    let mut r = reader(bytes);
    let mut buf = Vec::new();
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut nodes = 0usize;
    loop {
        let ev = r.read_event_into(&mut buf).map_err(corrupt)?;
        match ev {
            Event::Start(e) => {
                nodes += 1;
                if nodes > limits.max_nodes || stack.len() > limits.max_depth {
                    return Err(Error::TooLarge(format!(
                        "an XML part has more than {} elements or is nested deeper than {}",
                        limits.max_nodes, limits.max_depth
                    )));
                }
                stack.push(Node {
                    name: local(e.name().as_ref()),
                    attrs: attrs(&e),
                    children: Vec::new(),
                });
            }
            Event::Empty(e) => {
                nodes += 1;
                if nodes > limits.max_nodes {
                    return Err(Error::TooLarge(format!(
                        "an XML part has more than {} elements",
                        limits.max_nodes
                    )));
                }
                let n = Node {
                    name: local(e.name().as_ref()),
                    attrs: attrs(&e),
                    children: Vec::new(),
                };
                if let Some(top) = stack.last_mut() {
                    top.children.push(Child::Node(n));
                }
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err(corrupt("an end tag without a start tag"));
                }
                let n = stack.pop().expect("checked");
                if let Some(top) = stack.last_mut() {
                    top.children.push(Child::Node(n));
                }
            }
            Event::Text(t) => {
                let s = text_of(&t);
                if !(s.trim().is_empty() && s.contains(['\n', '\r']))
                    && let Some(top) = stack.last_mut()
                {
                    top.children.push(Child::Text(s));
                }
            }
            Event::CData(c) => {
                let s = String::from_utf8_lossy(&c).into_owned();
                if let Some(top) = stack.last_mut() {
                    top.children.push(Child::Text(s));
                }
            }
            Event::DocType(_) => return Err(corrupt("a DOCTYPE is not allowed")),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if stack.len() != 1 {
        return Err(corrupt("the document ends inside an element"));
    }
    let root = stack.pop().expect("one");
    root.children
        .into_iter()
        .find_map(|c| match c {
            Child::Node(n) => Some(n),
            Child::Text(_) => None,
        })
        .ok_or_else(|| corrupt("the part has no root element"))
}
