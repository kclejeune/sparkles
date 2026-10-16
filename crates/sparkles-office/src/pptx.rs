//! PowerPoint presentations as Markdown: one `<!-- Page N -->` marker and one
//! `# Slide N: Title` heading per slide, the slide's text and tables in the order of its
//! shapes, and its speaker notes after a `Notes:` label. N is the slide's position in
//! the presentation, so it is the number PowerPoint shows. Hidden slides are skipped and
//! keep their numbers.

use crate::budget::{Budget, SLOT};
use crate::md::{self, Block, Doc};
use crate::package::{Package, Rel};
use crate::xml::{self, Node};
use crate::{Document, Error, Limits};
use std::collections::HashMap;

pub fn convert(pkg: &mut Package, limits: &Limits) -> Result<Document, Error> {
    let bytes = pkg.need("ppt/presentation.xml")?;
    let pres = xml::parse(&bytes, limits)?;
    drop(bytes);
    let rels = pkg.rels("ppt/presentation.xml");
    let mut slides: Vec<String> = Vec::new();
    for s in pres.descendants("sldId") {
        if let Some(r) = s.attr("r:id").and_then(|id| rels.get(id))
            && !r.external
        {
            slides.push(r.target.clone());
        }
    }
    if slides.is_empty() {
        return Err(Error::Corrupt("the presentation has no slides".into()));
    }
    let mut doc = Doc::new(limits.max_output);
    let mut notes: Vec<String> = Vec::new();
    for (i, path) in slides.iter().enumerate() {
        let n = i + 1;
        let Some(bytes) = pkg.read(path)? else {
            notes.push(format!("slide {n} ({path}) is missing from the file"));
            continue;
        };
        let root = xml::parse(&bytes, limits)?;
        drop(bytes);
        if root.attr("show") == Some("0") {
            continue;
        }
        let srels = pkg.rels(path);
        let mut s = Slide {
            pkg: &mut *pkg,
            rels: &srels,
            doc: &mut doc,
            title: None,
            blocks: Vec::new(),
        };
        if let Some(tree) = root.find("spTree") {
            s.shapes(tree)?;
        }
        let Slide { title, blocks, .. } = s;
        let notes_text = notes_of(pkg, limits, &srels);
        doc.push(Block::Page(n))?;
        let heading = match title.as_deref().map(md::cell).filter(|t| !t.is_empty()) {
            Some(t) => format!("Slide {n}: {t}"),
            None => format!("Slide {n}"),
        };
        doc.push(Block::Heading(1, heading))?;
        for b in blocks {
            doc.push(b)?;
        }
        if !notes_text.is_empty() {
            doc.push(Block::Para("Notes:".into()))?;
            for t in notes_text {
                doc.push(Block::Para(md::plain(&t)))?;
            }
        }
    }
    let markdown = doc.render(limits.max_text)?;
    let title = {
        let t = pkg
            .read("docProps/core.xml")
            .ok()
            .flatten()
            .and_then(|b| xml::parse(&b, limits).ok())
            .and_then(|r| r.first("title").map(|t| t.text()));
        t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
    };
    Ok(Document {
        markdown,
        title,
        slides: slides.len(),
        images: doc.images,
        notes,
    })
}

/// The paragraphs of a slide's speaker notes.
fn notes_of(pkg: &mut Package, limits: &Limits, rels: &HashMap<String, Rel>) -> Vec<String> {
    let Some(r) = rels
        .values()
        .find(|r| r.kind == "notesSlide" && !r.external)
    else {
        return Vec::new();
    };
    let Ok(Some(b)) = pkg.read(&r.target) else {
        return Vec::new();
    };
    let Ok(root) = xml::parse(&b, limits) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for sp in root.descendants("sp") {
        let Some(ph) = placeholder(sp) else { continue };
        if ph.attr("type") != Some("body") {
            continue;
        }
        for p in sp.descendants("p") {
            let t = para_text(p, &HashMap::new(), true);
            if !t.trim().is_empty() {
                out.push(t.trim().to_string());
            }
        }
    }
    out
}

fn placeholder(sp: &Node) -> Option<&Node> {
    sp.first("nvSpPr")?.first("nvPr")?.first("ph")
}

struct Slide<'a, 'p, 'd> {
    pkg: &'a mut Package<'p>,
    rels: &'a HashMap<String, Rel>,
    doc: &'d mut Doc,
    title: Option<String>,
    blocks: Vec<Block>,
}

impl Slide<'_, '_, '_> {
    fn shapes(&mut self, tree: &Node) -> Result<(), Error> {
        for n in tree.elements() {
            match n.name.as_str() {
                "sp" => self.shape(n),
                "grpSp" => self.shapes(n)?,
                "graphicFrame" => {
                    if let Some(t) = n.find("tbl") {
                        let rows = table(t, &mut self.doc.budget)?;
                        if !rows.iter().all(|r| r.iter().all(String::is_empty)) {
                            self.blocks.push(Block::Table(rows));
                        }
                    }
                }
                "pic" => self.picture(n),
                "AlternateContent" => {
                    if let Some(c) = n.first("Choice").or_else(|| n.first("Fallback")) {
                        self.shapes(c)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn picture(&mut self, pic: &Node) {
        let alt = pic
            .find("cNvPr")
            .and_then(|c| c.attr("descr").or_else(|| c.attr("title")))
            .map(str::to_string);
        let Some(rid) = pic.find("blip").and_then(|b| b.attr("embed")) else {
            return;
        };
        let path = self
            .rels
            .get(rid)
            .filter(|r| !r.external)
            .map(|r| r.target.clone());
        let image = path.and_then(|p| self.pkg.image(&p, alt.clone()));
        if let Some(b) = self.doc.image_block(image, alt.as_deref()) {
            self.blocks.push(b);
        }
    }

    fn shape(&mut self, sp: &Node) {
        let ph = placeholder(sp);
        let kind = ph.and_then(|p| p.attr("type")).unwrap_or("");
        if matches!(kind, "sldNum" | "dt" | "ftr" | "hdr" | "sldImg") {
            return;
        }
        let Some(body) = sp.first("txBody") else {
            return;
        };
        let is_title = matches!(kind, "title" | "ctrTitle");
        // placeholders of content are bulleted unless the paragraph says otherwise
        let bulleted = ph.is_some() && matches!(kind, "" | "body" | "obj");
        if is_title {
            let t: Vec<String> = body
                .elements()
                .filter(|p| p.name == "p")
                .map(|p| para_text(p, self.rels, true))
                .filter(|t| !t.trim().is_empty())
                .collect();
            let t = t.join(" ");
            if !t.trim().is_empty() {
                if self.title.is_none() {
                    self.title = Some(t);
                } else {
                    self.blocks.push(Block::Para(md::plain(&t)));
                }
            }
            return;
        }
        let mut counters: Vec<u64> = Vec::new();
        for p in body.elements().filter(|p| p.name == "p") {
            let ppr = p.first("pPr");
            let t = para_text(p, self.rels, false);
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            let has = |n: &str| ppr.is_some_and(|x| x.first(n).is_some());
            let ordered = has("buAutoNum");
            let item = !has("buNone") && (has("buChar") || ordered || bulleted);
            if item {
                let lvl = ppr
                    .and_then(|x| x.attr("lvl"))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(8);
                let number = if ordered {
                    counters.resize(lvl + 1, 0);
                    counters.truncate(lvl + 1);
                    counters[lvl] += 1;
                    Some(counters[lvl])
                } else {
                    counters.clear();
                    None
                };
                self.blocks
                    .push(Block::Item(lvl, number, md::plain(t).replace('\n', " ")));
            } else {
                counters.clear();
                self.blocks.push(Block::Para(md::plain(t)));
            }
        }
    }
}

/// The text of a DrawingML paragraph. `flat` writes line breaks as spaces.
fn para_text(p: &Node, rels: &HashMap<String, Rel>, flat: bool) -> String {
    let mut out = String::new();
    for c in p.elements() {
        match c.name.as_str() {
            "r" => {
                let t = c.first("t").map(|t| t.text()).unwrap_or_default();
                let link = c
                    .first("rPr")
                    .and_then(|r| r.first("hlinkClick"))
                    .and_then(|h| h.attr("r:id"))
                    .and_then(|id| rels.get(id))
                    .filter(|r| r.external && !r.target.contains([')', ' ']));
                match link {
                    Some(l) if !t.trim().is_empty() => {
                        out.push_str(&format!("[{}]({})", t.trim(), l.target))
                    }
                    _ => out.push_str(&t),
                }
            }
            "fld" => {
                if !c.attr("type").is_some_and(|t| t.starts_with("slidenum")) {
                    out.push_str(&c.first("t").map(|t| t.text()).unwrap_or_default());
                }
            }
            "br" => out.push(if flat { ' ' } else { '\n' }),
            _ => {}
        }
    }
    out
}

/// The rows of a table, each cell charged to the budget as it is made.
fn table(t: &Node, budget: &mut Budget) -> Result<Vec<Vec<String>>, Error> {
    let mut rows = Vec::new();
    for tr in t.elements().filter(|n| n.name == "tr") {
        let mut row = Vec::new();
        for tc in tr.elements().filter(|n| n.name == "tc") {
            if tc.attr("hMerge") == Some("1") || tc.attr("vMerge") == Some("1") {
                budget.charge(SLOT)?;
                row.push(String::new());
                continue;
            }
            let text: Vec<String> = tc
                .descendants("p")
                .iter()
                .map(|p| para_text(p, &HashMap::new(), true))
                .filter(|t| !t.trim().is_empty())
                .collect();
            let text = md::cell(&text.join(" "));
            budget.charge(text.len() + SLOT)?;
            row.push(text);
        }
        rows.push(row);
    }
    Ok(rows)
}
