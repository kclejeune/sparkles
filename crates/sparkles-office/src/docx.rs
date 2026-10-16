//! Word documents (`word/document.xml`) as Markdown.
//!
//! Headings come from the paragraph's style (`Heading 1`, `Title`, or an outline level
//! the style or paragraph carries), lists from its numbering, tables from `w:tbl`.
//! Footnotes and endnotes are kept as Markdown footnotes at the end. Comments, headers,
//! footers and deleted text are left out.

use crate::budget::{Budget, SLOT};
use crate::md::{self, Block, Doc};
use crate::package::{Package, Rel};
use crate::xml::{self, Node};
use crate::{Document, Error, Limits};
use std::collections::HashMap;

#[derive(Default, Clone)]
struct Style {
    name: String,
    based_on: Option<String>,
    outline: Option<usize>,
    num: Option<(u64, usize)>,
}

#[derive(Default)]
struct Numbering {
    /// numId to abstractNumId
    nums: HashMap<u64, u64>,
    /// (abstractNumId, level) to whether the level is ordered
    ordered: HashMap<(u64, usize), bool>,
}

struct Cx<'p, 'a> {
    pkg: &'p mut Package<'a>,
    doc: Doc,
    rels: HashMap<String, Rel>,
    styles: HashMap<String, Style>,
    numbering: Numbering,
    counters: HashMap<u64, Vec<u64>>,
    footnotes: HashMap<String, String>,
    endnotes: HashMap<String, String>,
    used_notes: Vec<(String, String)>,
}

pub fn convert(pkg: &mut Package, limits: &Limits) -> Result<Document, Error> {
    let bytes = pkg.need("word/document.xml")?;
    let root = xml::parse(&bytes, limits)?;
    drop(bytes);
    let rels = pkg.rels("word/document.xml");
    let styles = styles(pkg, limits);
    let numbering = numbering(pkg, limits);
    let footnotes = notes(pkg, limits, "word/footnotes.xml", "footnote");
    let endnotes = notes(pkg, limits, "word/endnotes.xml", "endnote");
    let title = title(pkg, limits);
    let mut cx = Cx {
        pkg,
        doc: Doc::new(limits.max_output),
        rels,
        styles,
        numbering,
        counters: HashMap::new(),
        footnotes,
        endnotes,
        used_notes: Vec::new(),
    };
    let body = root
        .first("body")
        .ok_or_else(|| Error::Corrupt("word/document.xml has no body".into()))?;
    cx.blocks(body, limits.max_text)?;
    let notes: Vec<(String, String)> = std::mem::take(&mut cx.used_notes);
    for (label, text) in notes {
        cx.doc
            .push(Block::Para(format!("[^{label}]: {}", md::plain(&text))))?;
    }
    let markdown = cx.doc.render(limits.max_text)?;
    Ok(Document {
        markdown,
        title,
        slides: 0,
        images: cx.doc.images,
        notes: Vec::new(),
    })
}

fn title(pkg: &mut Package, limits: &Limits) -> Option<String> {
    let b = pkg.read("docProps/core.xml").ok()??;
    let root = xml::parse(&b, limits).ok()?;
    let t = root.first("title")?.text();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn val(n: &Node, child: &str) -> Option<String> {
    n.first(child)?.attr("val").map(str::to_string)
}

fn styles(pkg: &mut Package, limits: &Limits) -> HashMap<String, Style> {
    let mut out = HashMap::new();
    let Ok(Some(b)) = pkg.read("word/styles.xml") else {
        return out;
    };
    let Ok(root) = xml::parse(&b, limits) else {
        return out;
    };
    for s in root.elements().filter(|n| n.name == "style") {
        let Some(id) = s.attr("styleId") else {
            continue;
        };
        let ppr = s.first("pPr");
        out.insert(
            id.to_string(),
            Style {
                name: val(s, "name").unwrap_or_default().to_lowercase(),
                based_on: val(s, "basedOn"),
                outline: ppr
                    .and_then(|p| val(p, "outlineLvl"))
                    .and_then(|v| v.parse().ok()),
                num: ppr.and_then(|p| p.first("numPr")).and_then(num_pr),
            },
        );
    }
    out
}

fn num_pr(n: &Node) -> Option<(u64, usize)> {
    let id = val(n, "numId")?.parse().ok()?;
    // clamped here, before any use: a level is only ever an index into nine
    let lvl = val(n, "ilvl")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(8);
    Some((id, lvl))
}

fn numbering(pkg: &mut Package, limits: &Limits) -> Numbering {
    let mut out = Numbering::default();
    let Ok(Some(b)) = pkg.read("word/numbering.xml") else {
        return out;
    };
    let Ok(root) = xml::parse(&b, limits) else {
        return out;
    };
    for a in root.elements().filter(|n| n.name == "abstractNum") {
        let Some(id) = a.attr("abstractNumId").and_then(|v| v.parse().ok()) else {
            continue;
        };
        for l in a.elements().filter(|n| n.name == "lvl") {
            let level = l.attr("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
            let fmt = val(l, "numFmt").unwrap_or_else(|| "decimal".into());
            out.ordered
                .insert((id, level), fmt != "bullet" && fmt != "none");
        }
    }
    for n in root.elements().filter(|n| n.name == "num") {
        if let (Some(id), Some(a)) = (
            n.attr("numId").and_then(|v| v.parse().ok()),
            val(n, "abstractNumId").and_then(|v| v.parse().ok()),
        ) {
            out.nums.insert(id, a);
        }
    }
    out
}

/// The text of each footnote or endnote, by id.
fn notes(pkg: &mut Package, limits: &Limits, part: &str, tag: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(Some(b)) = pkg.read(part) else {
        return out;
    };
    let Ok(root) = xml::parse(&b, limits) else {
        return out;
    };
    for n in root.elements().filter(|n| n.name == tag) {
        if matches!(
            n.attr("type"),
            Some("separator" | "continuationSeparator" | "continuationNotice")
        ) {
            continue;
        }
        let Some(id) = n.attr("id") else { continue };
        let text: Vec<String> = n
            .descendants("p")
            .iter()
            .map(|p| plain_text(p))
            .filter(|t| !t.trim().is_empty())
            .collect();
        out.insert(id.to_string(), text.join(" "));
    }
    out
}

/// The text of a paragraph, with nothing else: for notes and cells.
fn plain_text(p: &Node) -> String {
    let mut s = String::new();
    plain_runs(p, &mut s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn plain_runs(n: &Node, out: &mut String) {
    for c in n.elements() {
        match c.name.as_str() {
            "t" => out.push_str(&c.text()),
            "tab" | "br" | "cr" => out.push(' '),
            "noBreakHyphen" => out.push('-'),
            "del" | "moveFrom" | "drawing" | "pict" | "pPr" | "rPr" | "Fallback" => {}
            _ => plain_runs(c, out),
        }
    }
}

impl Cx<'_, '_> {
    fn style_chain(&self, id: &str) -> Vec<&Style> {
        let mut out = Vec::new();
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if out.len() >= 10 {
                break;
            }
            match self.styles.get(&c) {
                Some(s) => {
                    cur = s.based_on.clone();
                    out.push(s);
                }
                None => break,
            }
        }
        out
    }

    /// Block-level children of the body, a table cell's sibling containers and content
    /// controls.
    fn blocks(&mut self, parent: &Node, max: usize) -> Result<(), Error> {
        for n in parent.elements() {
            match n.name.as_str() {
                "p" => self.para(n)?,
                "tbl" => self.table(n)?,
                "sdt" => {
                    if let Some(c) = n.first("sdtContent") {
                        self.blocks(c, max)?;
                    }
                }
                "sdtContent" | "customXml" | "ins" | "moveTo" => self.blocks(n, max)?,
                _ => {}
            }
            if self.doc.blocks.len() > max {
                return Err(Error::TooLarge(format!(
                    "the document has more than {max} blocks"
                )));
            }
        }
        Ok(())
    }

    fn para(&mut self, p: &Node) -> Result<(), Error> {
        let ppr = p.first("pPr");
        let style_id = ppr.and_then(|p| val(p, "pStyle"));
        let mut heading: Option<usize> = None;
        let mut quote = false;
        let mut list: Option<(Option<u64>, usize, bool)> = None; // numId, level, bullet-by-name
        let mut style_num: Option<(u64, usize)> = None;
        let mut named_list: Option<(bool, usize)> = None;
        if let Some(id) = &style_id {
            for s in self.style_chain(id) {
                let name = s.name.as_str();
                if heading.is_none() {
                    if let Some(rest) = name.strip_prefix("heading ")
                        && let Ok(l) = rest.trim().parse::<usize>()
                    {
                        heading = Some(l);
                    } else if name == "title" {
                        heading = Some(1);
                    } else if let Some(o) = s.outline
                        && o < 9
                    {
                        heading = Some(o + 1);
                    }
                }
                if matches!(name, "quote" | "intense quote" | "block text") {
                    quote = true;
                }
                if style_num.is_none() {
                    style_num = s.num;
                }
                if named_list.is_none() && name.starts_with("list ") {
                    let lvl = name
                        .chars()
                        .last()
                        .and_then(|c| c.to_digit(10))
                        .map_or(0, |d| d.saturating_sub(1) as usize);
                    if name.starts_with("list bullet") {
                        named_list = Some((false, lvl));
                    } else if name.starts_with("list number") {
                        named_list = Some((true, lvl));
                    }
                }
            }
        }
        if let Some(o) = ppr
            .and_then(|p| val(p, "outlineLvl"))
            .and_then(|v| v.parse::<usize>().ok())
            && o < 9
        {
            heading = Some(o + 1);
        }
        let num = ppr
            .and_then(|p| p.first("numPr"))
            .and_then(num_pr)
            .or(style_num);
        if let Some((id, lvl)) = num
            && id != 0
        {
            list = Some((Some(id), lvl, false));
        } else if let Some((ordered, lvl)) = named_list {
            list = Some((None, lvl, ordered));
        }
        let mut text = String::new();
        let mut extra: Vec<Block> = Vec::new();
        self.inline(
            p,
            &mut text,
            &mut extra,
            heading.is_some() || list.is_some(),
        );
        let text = text.trim().to_string();
        if !text.is_empty() {
            let line = md::plain(&text);
            if let Some(l) = heading {
                self.doc.push(Block::Heading(l, line.replace('\n', " ")))?;
            } else if let Some((numid, lvl, ordered_by_name)) = list {
                let (ordered, number) = match numid {
                    Some(id) => {
                        let abs = self.numbering.nums.get(&id).copied();
                        let ordered = abs
                            .and_then(|a| self.numbering.ordered.get(&(a, lvl)).copied())
                            .unwrap_or(false);
                        let c = self.counters.entry(id).or_default();
                        c.resize(lvl + 1, 0);
                        c.truncate(lvl + 1);
                        c[lvl] += 1;
                        (ordered, c[lvl])
                    }
                    None => (ordered_by_name, 1),
                };
                self.doc.push(Block::Item(
                    lvl.min(8),
                    ordered.then_some(number),
                    line.replace('\n', " "),
                ))?;
            } else if quote {
                self.doc.push(Block::Quote(line))?;
            } else {
                self.doc.push(Block::Para(line))?;
            }
        }
        for b in extra {
            self.doc.push(b)?;
        }
        Ok(())
    }

    /// The text of the runs below `n`, with the pictures and text boxes it finds pushed
    /// to `extra`.
    fn inline(&mut self, n: &Node, out: &mut String, extra: &mut Vec<Block>, flat: bool) {
        for c in n.elements() {
            match c.name.as_str() {
                "r" => self.run(c, out, extra, flat),
                "hyperlink" => {
                    let mut inner = String::new();
                    self.inline(c, &mut inner, extra, flat);
                    let url = c
                        .attr("id")
                        .and_then(|id| self.rels.get(id))
                        .filter(|r| r.external)
                        .map(|r| r.target.clone());
                    match url {
                        Some(u) if !inner.trim().is_empty() && !u.contains([')', ' ']) => {
                            out.push_str(&format!("[{}]({u})", inner.trim()))
                        }
                        _ => out.push_str(&inner),
                    }
                }
                "ins" | "smartTag" | "fldSimple" | "sdtContent" | "customXml" | "moveTo"
                | "sdt" | "bdo" | "dir" => self.inline(c, out, extra, flat),
                "AlternateContent" => {
                    if let Some(ch) = c.first("Choice").or_else(|| c.first("Fallback")) {
                        self.inline(ch, out, extra, flat);
                    }
                }
                _ => {}
            }
        }
    }

    fn run(&mut self, r: &Node, out: &mut String, extra: &mut Vec<Block>, flat: bool) {
        for c in r.elements() {
            match c.name.as_str() {
                "t" => out.push_str(&c.text()),
                "tab" => out.push(' '),
                "br" | "cr" => {
                    if c.attr("type") != Some("page") {
                        out.push(if flat { ' ' } else { '\n' });
                    }
                }
                "noBreakHyphen" => out.push('-'),
                "footnoteReference" | "endnoteReference" => {
                    let foot = c.name == "footnoteReference";
                    let Some(id) = c.attr("id") else { continue };
                    let map = if foot {
                        &self.footnotes
                    } else {
                        &self.endnotes
                    };
                    if let Some(t) = map.get(id).cloned() {
                        let label = format!("{}{id}", if foot { "fn" } else { "en" });
                        out.push_str(&format!("[^{label}]"));
                        if !self.used_notes.iter().any(|(l, _)| *l == label) {
                            self.used_notes.push((label, t));
                        }
                    }
                }
                "drawing" | "pict" | "object" => self.drawing(c, extra),
                "AlternateContent" => {
                    if let Some(ch) = c.first("Choice").or_else(|| c.first("Fallback")) {
                        self.run(ch, out, extra, flat);
                    }
                }
                _ => {}
            }
        }
    }

    fn drawing(&mut self, d: &Node, extra: &mut Vec<Block>) {
        // text boxes
        for tb in d.descendants("txbxContent") {
            for p in tb.descendants("p") {
                let t = plain_text(p);
                if !t.is_empty() {
                    extra.push(Block::Para(md::plain(&t)));
                }
            }
        }
        let alt = d
            .find("docPr")
            .and_then(|n| n.attr("descr").or_else(|| n.attr("title")))
            .map(str::to_string);
        let rid = d
            .find("blip")
            .and_then(|b| b.attr("embed"))
            .or_else(|| d.find("imagedata").and_then(|b| b.attr("id")));
        let Some(rid) = rid else { return };
        let path = self
            .rels
            .get(rid)
            .filter(|r| !r.external)
            .map(|r| r.target.clone());
        let image = path.and_then(|p| self.pkg.image(&p, alt.clone()));
        if let Some(b) = self.doc.image_block(image, alt.as_deref()) {
            extra.push(b);
        }
    }

    fn table(&mut self, t: &Node) -> Result<(), Error> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        table_rows(t, &mut rows, &mut self.doc.budget)?;
        if rows.iter().all(|r| r.iter().all(|c| c.is_empty())) {
            return Ok(());
        }
        self.doc.push(Block::Table(rows))
    }
}

/// The rows of a table. Every slot is charged to the budget before it is pushed, the
/// empty ones that a `gridSpan` adds included, so a table never exists unbudgeted.
fn table_rows(t: &Node, rows: &mut Vec<Vec<String>>, budget: &mut Budget) -> Result<(), Error> {
    for tr in t.elements().filter(|n| n.name == "tr") {
        let mut row = Vec::new();
        for tc in tr.elements().filter(|n| n.name == "tc") {
            let mut parts: Vec<String> = Vec::new();
            for c in tc.elements() {
                match c.name.as_str() {
                    "p" => {
                        let t = plain_text(c);
                        if !t.is_empty() {
                            parts.push(t);
                        }
                    }
                    "tbl" => {
                        let mut inner = Vec::new();
                        table_rows(c, &mut inner, budget)?;
                        for r in inner {
                            let t = r.join(" ");
                            if !t.trim().is_empty() {
                                parts.push(t);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let text = md::cell(&parts.join(" "));
            budget.charge(text.len() + SLOT)?;
            row.push(text);
            let span = tc
                .first("tcPr")
                .and_then(|p| val(p, "gridSpan"))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
                .min(64);
            for _ in 1..span {
                budget.charge(SLOT)?;
                row.push(String::new());
            }
        }
        rows.push(row);
    }
    Ok(())
}
