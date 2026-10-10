//! HTML to Markdown for ingestion (spec C18 §7.1): the main content of a page, without
//! scripts, styles and navigation, with its headings, lists, tables and code kept as
//! Markdown so that chunking can follow them.
//!
//! The converter reads the markup with a small tolerant tokenizer rather than a full
//! HTML5 parser. When the page has a `<main>` or an `<article>`, only the first such
//! element is kept; `<nav>`, `<header>`, `<footer>`, `<aside>` and `<form>` are dropped
//! everywhere, as are `<script>`, `<style>`, `<template>`, `<svg>` and the like. The
//! output is deterministic, so the same page always gives the same rendition.

/// Elements whose content is never text.
const SKIP: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "math", "canvas", "iframe", "object", "head",
    "select", "button",
];
/// Elements of page furniture rather than content.
const FURNITURE: &[&str] = &["nav", "header", "footer", "aside", "form", "dialog"];
/// Void elements, which have no end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
/// Elements that start a block of their own.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "body",
    "html",
    "figure",
    "figcaption",
    "address",
    "details",
    "summary",
    "dl",
    "dt",
    "dd",
    "center",
];

#[derive(Debug, PartialEq)]
enum Token {
    Open {
        name: String,
        attrs: Vec<(String, String)>,
        closed: bool,
    },
    Close(String),
    Text(String),
}

/// The tokens of `html`: tags with lower-case names, text with its entities decoded.
/// Comments, doctypes and processing instructions are dropped, and the content of raw
/// text elements (`script`, `style`) is one text token.
fn tokens(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let b = html.as_bytes();
    let mut i = 0;
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Token>| {
        if !text.is_empty() {
            out.push(Token::Text(decode(text)));
            text.clear();
        }
    };
    while i < b.len() {
        if b[i] != b'<' {
            let next = html[i..].find('<').map_or(b.len(), |n| i + n);
            text.push_str(&html[i..next]);
            i = next;
            continue;
        }
        let rest = &html[i..];
        if rest.starts_with("<!--") {
            flush(&mut text, &mut out);
            i += rest.find("-->").map_or(rest.len(), |n| n + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            flush(&mut text, &mut out);
            i += rest.find('>').map_or(rest.len(), |n| n + 1);
            continue;
        }
        let close = rest.starts_with("</");
        let start = if close { 2 } else { 1 };
        let name_len = rest[start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':'))
            .unwrap_or(rest.len() - start);
        if name_len == 0 || !rest.as_bytes()[start].is_ascii_alphabetic() {
            // a lone `<` is text
            text.push('<');
            i += 1;
            continue;
        }
        flush(&mut text, &mut out);
        let name = rest[start..start + name_len].to_ascii_lowercase();
        let Some(end) = tag_end(&rest[start + name_len..]) else {
            break;
        };
        let inner = &rest[start + name_len..start + name_len + end];
        i += start + name_len + end + 1;
        if close {
            out.push(Token::Close(name));
            continue;
        }
        let closed = inner.trim_end().ends_with('/');
        out.push(Token::Open {
            name: name.clone(),
            attrs: attributes(inner.trim_end().trim_end_matches('/')),
            closed,
        });
        // raw text elements end at their own end tag only
        if matches!(name.as_str(), "script" | "style" | "textarea" | "title") && !closed {
            let lower = html[i..].to_ascii_lowercase();
            let end_tag = format!("</{name}");
            let n = lower.find(&end_tag).unwrap_or(lower.len());
            let raw = &html[i..i + n];
            if !raw.is_empty() {
                out.push(Token::Text(if name == "title" || name == "textarea" {
                    decode(raw)
                } else {
                    raw.to_string()
                }));
            }
            i += n;
            if i < b.len() {
                i += html[i..].find('>').map_or(html.len() - i, |n| n + 1);
            }
            out.push(Token::Close(name));
        }
    }
    flush(&mut text, &mut out);
    out
}

/// The position of the `>` that ends a tag, skipping quoted attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

fn attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let a = i;
        while i < c.len() && !c[i].is_whitespace() && c[i] != '=' {
            i += 1;
        }
        if a == i {
            i += 1;
            continue;
        }
        let name: String = c[a..i].iter().collect::<String>().to_ascii_lowercase();
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < c.len() && c[i] == '=' {
            i += 1;
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            if i < c.len() && (c[i] == '"' || c[i] == '\'') {
                let q = c[i];
                i += 1;
                let v = i;
                while i < c.len() && c[i] != q {
                    i += 1;
                }
                value = c[v..i].iter().collect();
                i += 1;
            } else {
                let v = i;
                while i < c.len() && !c[i].is_whitespace() {
                    i += 1;
                }
                value = c[v..i].iter().collect();
            }
        }
        out.push((name, decode(&value)));
    }
    out
}

/// Decode character references: the numeric ones and the named ones that occur in
/// ordinary prose. An unknown name stays as written.
pub(crate) fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .map_or(rest.len(), |n| n + 1);
        let name = &rest[1..end];
        let semi = rest[end..].starts_with(';');
        let ch = if let Some(num) = name.strip_prefix('#') {
            let v = match num.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok(),
                None => num.parse::<u32>().ok(),
            };
            v.map(|v| char::from_u32(v).unwrap_or('\u{FFFD}'))
        } else {
            named(name)
        };
        match ch {
            Some(c) if !name.is_empty() => {
                out.push(c);
                rest = &rest[end + usize::from(semi)..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named(n: &str) -> Option<char> {
    Some(match n {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "middot" => '·',
        "bull" => '•',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "shy" => '\u{ad}',
        "zwj" => '\u{200d}',
        "zwnj" => '\u{200c}',
        "eacute" => 'é',
        "egrave" => 'è',
        "ecirc" => 'ê',
        "aacute" => 'á',
        "agrave" => 'à',
        "acirc" => 'â',
        "auml" => 'ä',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "szlig" => 'ß',
        "ccedil" => 'ç',
        "ntilde" => 'ñ',
        "oacute" => 'ó',
        "iacute" => 'í',
        "uacute" => 'ú',
        _ => return None,
    })
}

/// A page converted: its Markdown and its title.
#[derive(Debug, Default, PartialEq)]
pub struct Page {
    pub markdown: String,
    pub title: Option<String>,
}

/// The list a `<li>` belongs to: ordered, and the next number.
struct List {
    ordered: bool,
    next: u32,
}

struct Writer {
    out: String,
    /// text of the current block, before it is written
    line: String,
    /// `>` prefixes of enclosing quotes
    quote: usize,
    lists: Vec<List>,
    pre: usize,
    /// the open table's rows, each a list of cells
    table: Option<Vec<Vec<String>>>,
    cell: Option<String>,
    /// a pending link target: the text written since its start
    links: Vec<(String, usize)>,
}

impl Writer {
    fn text(&mut self, t: &str) {
        if let Some(c) = &mut self.cell {
            push_inline(c, t);
            return;
        }
        if self.pre > 0 {
            self.line.push_str(t);
            return;
        }
        push_inline(&mut self.line, t);
    }

    /// Write the current block, if any, with the quote's prefix.
    fn end_block(&mut self) {
        // inside a list, blocks are tight and keep the item's indentation
        let in_list = !self.lists.is_empty() && self.pre == 0;
        let text = if self.pre > 0 {
            std::mem::take(&mut self.line)
        } else {
            let t = if in_list {
                self.line.trim_end()
            } else {
                self.line.trim()
            }
            .to_string();
            self.line.clear();
            t
        };
        if text.trim().is_empty() {
            return;
        }
        let prefix = "> ".repeat(self.quote);
        let indent = "  ".repeat(self.lists.len().saturating_sub(1));
        if in_list {
            if !self.out.is_empty() && !self.out.ends_with('\n') {
                self.out.push('\n');
            }
        } else if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            if self.out.ends_with('\n') {
                self.out.push('\n');
            } else {
                self.out.push_str("\n\n");
            }
        }
        for (i, l) in text.lines().enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            self.out.push_str(&prefix);
            if i > 0 && !self.lists.is_empty() {
                self.out.push_str(&indent);
                self.out.push_str("  ");
            }
            self.out.push_str(l);
        }
        self.out.push_str(if in_list { "\n" } else { "\n\n" });
    }

    /// A list item's first line goes right after the previous item, without a blank
    /// line.
    fn item(&mut self) {
        self.end_block();
        let depth = self.lists.len().saturating_sub(1);
        let marker = match self.lists.last_mut() {
            Some(l) if l.ordered => {
                let m = format!("{}. ", l.next);
                l.next += 1;
                m
            }
            _ => "- ".to_string(),
        };
        self.line = format!("{}{marker}", "  ".repeat(depth));
    }
}

/// Append inline text, collapsing whitespace as HTML does.
fn push_inline(s: &mut String, t: &str) {
    for c in t.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            if !s.is_empty() && !s.ends_with(' ') && !s.ends_with('\n') {
                s.push(' ');
            }
        } else {
            s.push(c);
        }
    }
}

/// Convert a page to Markdown.
pub fn to_markdown(html: &str) -> Page {
    let toks = tokens(html);
    let mut title = None;
    // the title, and where the main content is
    let mut main: Option<(usize, usize)> = None;
    {
        let mut depth: Vec<(&str, usize)> = Vec::new();
        let mut in_title = false;
        for (i, t) in toks.iter().enumerate() {
            match t {
                Token::Open { name, closed, .. } => {
                    if name == "title" {
                        in_title = true;
                    }
                    if (name == "main" || name == "article") && !closed && main.is_none() {
                        depth.push((name.as_str(), i));
                    }
                }
                Token::Close(name) => {
                    if name == "title" {
                        in_title = false;
                    }
                    if let Some((n, a)) = depth.last()
                        && n == name
                    {
                        main = Some((*a + 1, i));
                        depth.clear();
                    }
                }
                Token::Text(s) if in_title && title.is_none() => {
                    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !s.is_empty() {
                        title = Some(s);
                    }
                }
                _ => {}
            }
        }
    }
    let range = main.unwrap_or((0, toks.len()));
    let mut w = Writer {
        out: String::new(),
        line: String::new(),
        quote: 0,
        lists: Vec::new(),
        pre: 0,
        table: None,
        cell: None,
        links: Vec::new(),
    };
    // the elements being skipped, by name, with their nesting
    let mut skipping: Vec<String> = Vec::new();
    for t in &toks[range.0..range.1] {
        if let Some(top) = skipping.last().cloned() {
            match t {
                Token::Open { name, closed, .. }
                    if *name == top && !closed && !VOID.contains(&name.as_str()) =>
                {
                    skipping.push(name.clone())
                }
                Token::Close(name) if *name == top => {
                    skipping.pop();
                }
                _ => {}
            }
            continue;
        }
        match t {
            Token::Text(s) => w.text(s),
            Token::Open {
                name,
                attrs,
                closed,
            } => {
                let n = name.as_str();
                if (SKIP.contains(&n) || FURNITURE.contains(&n) || n == "title")
                    && !closed
                    && !VOID.contains(&n)
                {
                    skipping.push(name.clone());
                    continue;
                }
                let hidden = attrs.iter().any(|(k, v)| {
                    k == "hidden"
                        || (k == "aria-hidden" && v == "true")
                        || (k == "role" && v == "navigation")
                });
                if hidden && !closed && !VOID.contains(&n) {
                    skipping.push(name.clone());
                    continue;
                }
                match n {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        w.end_block();
                        let level = (n.as_bytes()[1] - b'0') as usize;
                        w.line = format!("{} ", "#".repeat(level));
                    }
                    "br" => {
                        if let Some(c) = &mut w.cell {
                            c.push(' ');
                        } else if w.pre > 0 {
                            w.line.push('\n');
                        } else {
                            w.line.push('\n');
                        }
                    }
                    "hr" => {
                        w.end_block();
                        w.line = "---".into();
                        w.end_block();
                    }
                    "pre" => {
                        w.end_block();
                        w.pre += 1;
                        w.line = "```\n".into();
                    }
                    "code" if w.pre == 0 => w.text("`"),
                    "strong" | "b" => w.text("**"),
                    "em" | "i" => w.text("*"),
                    "blockquote" => {
                        w.end_block();
                        w.quote += 1;
                    }
                    "ul" | "ol" => {
                        w.end_block();
                        let start = attrs
                            .iter()
                            .find(|(k, _)| k == "start")
                            .and_then(|(_, v)| v.parse().ok())
                            .unwrap_or(1);
                        w.lists.push(List {
                            ordered: n == "ol",
                            next: start,
                        });
                    }
                    "li" => w.item(),
                    "table" => {
                        w.end_block();
                        w.table = Some(Vec::new());
                    }
                    "tr" => {
                        if let Some(t) = &mut w.table {
                            t.push(Vec::new());
                        }
                    }
                    "td" | "th" => {
                        if w.table.is_some() {
                            w.cell = Some(String::new());
                        }
                    }
                    "a" => {
                        let href = attrs
                            .iter()
                            .find(|(k, _)| k == "href")
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default();
                        let at = w.cell.as_ref().map_or(w.line.len(), String::len);
                        w.links.push((href, at));
                    }
                    "img" => {
                        if let Some((_, alt)) = attrs.iter().find(|(k, _)| k == "alt")
                            && !alt.trim().is_empty()
                        {
                            w.text(&format!("[{}]", alt.trim()));
                        }
                    }
                    _ if BLOCK.contains(&n) => w.end_block(),
                    _ => {}
                }
            }
            Token::Close(name) => match name.as_str() {
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "li" | "dt" | "dd" => w.end_block(),
                "pre" if w.pre > 0 => {
                    if !w.line.ends_with('\n') {
                        w.line.push('\n');
                    }
                    w.line.push_str("```");
                    w.end_block();
                    w.pre -= 1;
                }
                "code" if w.pre == 0 => w.text("`"),
                "strong" | "b" => w.text("**"),
                "em" | "i" => w.text("*"),
                "blockquote" => {
                    w.end_block();
                    w.quote = w.quote.saturating_sub(1);
                }
                "ul" | "ol" => {
                    w.end_block();
                    w.lists.pop();
                    if w.lists.is_empty() && w.out.ends_with('\n') && !w.out.ends_with("\n\n") {
                        w.out.push('\n');
                    }
                }
                "td" | "th" => {
                    if let (Some(c), Some(t)) = (w.cell.take(), &mut w.table)
                        && let Some(row) = t.last_mut()
                    {
                        row.push(c.trim().replace('|', "\\|"));
                    }
                }
                "table" => {
                    if let Some(rows) = w.table.take() {
                        let rows: Vec<Vec<String>> =
                            rows.into_iter().filter(|r| !r.is_empty()).collect();
                        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                        if width > 0 {
                            let mut md = String::new();
                            for (i, r) in rows.iter().enumerate() {
                                let mut cells = r.clone();
                                cells.resize(width, String::new());
                                md.push_str(&format!("| {} |\n", cells.join(" | ")));
                                if i == 0 {
                                    md.push_str(&format!("|{}\n", " --- |".repeat(width)));
                                }
                            }
                            w.line = md.trim_end().to_string();
                            let pre = w.pre;
                            w.pre = 1;
                            w.end_block();
                            w.pre = pre;
                        }
                    }
                }
                "a" => {
                    if let Some((href, at)) = w.links.pop() {
                        let target = if let Some(c) = &mut w.cell {
                            c
                        } else {
                            &mut w.line
                        };
                        let keep = !href.is_empty()
                            && !href.starts_with('#')
                            && !href.to_ascii_lowercase().starts_with("javascript:")
                            && at <= target.len();
                        if keep {
                            let text = target[at..].trim().to_string();
                            if !text.is_empty() && text != href {
                                target.truncate(at);
                                target.push_str(&format!("[{text}]({href})"));
                            }
                        }
                    }
                }
                n if BLOCK.contains(&n) => w.end_block(),
                _ => {}
            },
        }
    }
    w.end_block();
    let markdown = w.out.trim_end().to_string();
    Page {
        markdown: if markdown.is_empty() {
            markdown
        } else {
            markdown + "\n"
        },
        title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_content_as_markdown() {
        let html = r#"<!doctype html><html><head><title>Team page &amp; notes</title>
<style>body { color: red }</style><script>var x = "<p>no</p>";</script></head>
<body><nav><a href="/">Home</a> <a href="/teams">Teams</a></nav>
<header><h1>Acme</h1></header>
<main>
  <h1>Payments team</h1>
  <p>Ana&nbsp;Lima moved to the <a href="/teams/payments">payments team</a> this week.</p>
  <h2>Members</h2>
  <ul><li>Ana Lima</li><li>Kai <b>Berg</b><ul><li>on call</li></ul></li></ul>
  <ol start="3"><li>three</li><li>four</li></ol>
  <table><tr><th>Name</th><th>Role</th></tr><tr><td>Ana</td><td>Engineer</td></tr></table>
  <pre><code>let x = 1;
let y = 2;</code></pre>
  <blockquote><p>Ship it.</p></blockquote>
  <!-- a comment -->
  <p hidden>secret</p>
</main>
<footer>© Acme</footer></body></html>"#;
        let p = to_markdown(html);
        assert_eq!(p.title.as_deref(), Some("Team page & notes"));
        let md = &p.markdown;
        assert!(md.starts_with("# Payments team\n\n"), "{md}");
        assert!(
            md.contains("Ana\u{a0}Lima moved to the [payments team](/teams/payments) this week."),
            "{md}"
        );
        assert!(md.contains("## Members"), "{md}");
        assert!(
            md.contains("- Ana Lima\n- Kai **Berg**\n  - on call"),
            "{md}"
        );
        assert!(md.contains("3. three\n4. four"), "{md}");
        assert!(
            md.contains("| Name | Role |\n| --- | --- |\n| Ana | Engineer |"),
            "{md}"
        );
        assert!(
            md.contains("```\n`let x = 1;\nlet y = 2;`\n```")
                || md.contains("```\nlet x = 1;\nlet y = 2;\n```"),
            "{md}"
        );
        assert!(md.contains("> Ship it."), "{md}");
        for gone in ["Home", "color", "var x", "secret", "Acme\n", "©"] {
            assert!(!md.contains(gone), "{gone} in {md}");
        }
    }

    #[test]
    fn without_main_the_body_is_kept() {
        let p = to_markdown("<p>One &lt;two&gt; &#x41;&#66; &unknown; & done</p><div>Next</div>");
        assert_eq!(p.markdown, "One <two> AB &unknown; & done\n\nNext\n");
        assert_eq!(p.title, None);
        // the same page always gives the same Markdown
        assert_eq!(to_markdown("<p>x</p>"), to_markdown("<p>x</p>"));
    }
}
