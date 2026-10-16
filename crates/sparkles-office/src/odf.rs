//! OpenDocument text, presentations and spreadsheets (`content.xml`) as Markdown and CSV.

use crate::budget::{Budget, MAX_NAME, SLOT};
use crate::md::{self, Block, Doc};
use crate::package::{self, Package};
use crate::sheet::{self, SheetBuilder};
use crate::xml::{self, Node};
use crate::{Document, Error, Limits, Sheet};
use quick_xml::events::Event;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

fn content(pkg: &mut Package, limits: &Limits) -> Result<Node, Error> {
    let b = pkg.need("content.xml")?;
    xml::parse(&b, limits)
}

fn title(pkg: &mut Package, limits: &Limits) -> Option<String> {
    let b = pkg.read("meta.xml").ok()??;
    let root = xml::parse(&b, limits).ok()?;
    let t = root.find("title")?.text();
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

struct Cx<'p, 'a> {
    pkg: &'p mut Package<'a>,
    doc: Doc,
    /// list style name to the ordered flag of each level
    list_styles: HashMap<String, Vec<bool>>,
    footnotes: Vec<(String, String)>,
    /// how many note bodies the walk is inside
    in_note: usize,
    /// how many links the walk is inside: a link inside a link is read as its text, so
    /// that the text is not copied once per level
    in_link: usize,
}

pub fn text(pkg: &mut Package, limits: &Limits) -> Result<Document, Error> {
    let root = content(pkg, limits)?;
    let body = root
        .find("text")
        .ok_or_else(|| Error::Corrupt("content.xml has no text body".into()))?;
    let mut cx = Cx::new(pkg, &root, limits);
    cx.blocks(body, limits.max_text)?;
    for (l, t) in std::mem::take(&mut cx.footnotes) {
        cx.doc
            .push(Block::Para(format!("[^{l}]: {}", md::plain(&t))))?;
    }
    let markdown = cx.doc.render(limits.max_text)?;
    let images = std::mem::take(&mut cx.doc.images);
    Ok(Document {
        markdown,
        title: title(pkg, limits),
        slides: 0,
        images,
        notes: Vec::new(),
    })
}

pub fn presentation(pkg: &mut Package, limits: &Limits) -> Result<Document, Error> {
    let root = content(pkg, limits)?;
    let pres = root
        .find("presentation")
        .ok_or_else(|| Error::Corrupt("content.xml has no presentation body".into()))?;
    let mut cx = Cx::new(pkg, &root, limits);
    let mut slides = 0;
    for (i, page) in pres.elements().filter(|n| n.name == "page").enumerate() {
        let n = i + 1;
        slides = n;
        let mut title = None;
        let start = cx.doc.blocks.len();
        let mut notes: Vec<String> = Vec::new();
        cx.frames(page, &mut title, &mut notes, false)?;
        let body: Vec<Block> = cx.doc.blocks.drain(start..).collect();
        cx.doc.push(Block::Page(n))?;
        let heading = match title.as_deref().map(md::cell).filter(|t| !t.is_empty()) {
            Some(t) => format!("Slide {n}: {t}"),
            None => format!("Slide {n}"),
        };
        cx.doc.push(Block::Heading(1, heading))?;
        for b in body {
            cx.doc.push(b)?;
        }
        if !notes.is_empty() {
            cx.doc.push(Block::Para("Notes:".into()))?;
            for t in notes {
                cx.doc.push(Block::Para(md::plain(&t)))?;
            }
        }
    }
    if slides == 0 {
        return Err(Error::Corrupt("the presentation has no slides".into()));
    }
    let markdown = cx.doc.render(limits.max_text)?;
    let images = std::mem::take(&mut cx.doc.images);
    Ok(Document {
        markdown,
        title: title(pkg, limits),
        slides,
        images,
        notes: Vec::new(),
    })
}

impl<'p, 'a> Cx<'p, 'a> {
    fn new(pkg: &'p mut Package<'a>, root: &Node, limits: &Limits) -> Cx<'p, 'a> {
        let mut list_styles = HashMap::new();
        for s in root.descendants("list-style") {
            let Some(name) = s.attr("name") else { continue };
            let mut levels: Vec<bool> = Vec::new();
            for l in s.elements() {
                let ordered = l.name == "list-level-style-number";
                if !ordered
                    && l.name != "list-level-style-bullet"
                    && l.name != "list-level-style-image"
                {
                    continue;
                }
                let lvl: usize = l.attr("level").and_then(|v| v.parse().ok()).unwrap_or(1);
                let lvl = lvl.clamp(1, 10) - 1;
                if levels.len() <= lvl {
                    levels.resize(lvl + 1, false);
                }
                levels[lvl] = ordered;
            }
            list_styles.insert(name.to_string(), levels);
        }
        Cx {
            pkg,
            doc: Doc::new(limits),
            list_styles,
            footnotes: Vec::new(),
            in_note: 0,
            in_link: 0,
        }
    }

    fn blocks(&mut self, parent: &Node, max: usize) -> Result<(), Error> {
        for n in parent.elements() {
            self.doc.budget.tick()?;
            match n.name.as_str() {
                "h" => {
                    let level = n
                        .attr("outline-level")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1);
                    let mut extra = Vec::new();
                    let t = self.inline(n, true, &mut extra)?;
                    if !t.trim().is_empty() {
                        self.doc
                            .push(Block::Heading(level, md::plain(&t).replace('\n', " ")))?;
                    }
                    self.extra(extra)?;
                }
                "p" => {
                    let mut extra = Vec::new();
                    let t = self.inline(n, false, &mut extra)?;
                    if !t.trim().is_empty() {
                        self.doc.push(Block::Para(md::plain(&t)))?;
                    }
                    self.extra(extra)?;
                }
                "list" => self.list(n, 0, None)?,
                "table" => {
                    let rows = self.table(n)?;
                    if !rows.iter().all(|r| r.iter().all(String::is_empty)) {
                        self.doc.push(Block::Table(rows))?;
                    }
                }
                "section" => self.blocks(n, max)?,
                "frame" => {
                    let mut extra = Vec::new();
                    self.frame_inline(n, &mut extra);
                    self.extra(extra)?;
                }
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

    fn extra(&mut self, extra: Vec<Block>) -> Result<(), Error> {
        for b in extra {
            self.doc.push(b)?;
        }
        Ok(())
    }

    fn list(&mut self, l: &Node, level: usize, inherited: Option<&str>) -> Result<(), Error> {
        let style = l.attr("style-name").or(inherited);
        let ordered = style
            .and_then(|s| self.list_styles.get(s))
            .and_then(|v| v.get(level).copied())
            .unwrap_or(false);
        let mut counter = 0u64;
        for item in l.elements() {
            if !matches!(item.name.as_str(), "list-item" | "list-header") {
                continue;
            }
            for c in item.elements() {
                self.doc.budget.tick()?;
                match c.name.as_str() {
                    "p" | "h" => {
                        let mut extra = Vec::new();
                        let t = self.inline(c, true, &mut extra)?;
                        if !t.trim().is_empty() {
                            counter += 1;
                            self.doc.push(Block::Item(
                                level.min(8),
                                ordered.then_some(counter),
                                md::plain(&t).replace('\n', " "),
                            ))?;
                        }
                        self.extra(extra)?;
                    }
                    "list" => self.list(c, level + 1, style)?,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn table(&mut self, t: &Node) -> Result<Vec<Vec<String>>, Error> {
        let mut rows = Vec::new();
        self.table_rows(t, &mut rows)?;
        Ok(rows)
    }

    /// The rows of a table. Every slot, the empty ones that a repeat or a span adds
    /// included, is charged to the budget before it is pushed.
    fn table_rows(&mut self, t: &Node, rows: &mut Vec<Vec<String>>) -> Result<(), Error> {
        for n in t.elements() {
            self.doc.budget.tick()?;
            match n.name.as_str() {
                "table-row" => {
                    let mut row = Vec::new();
                    for c in n.elements() {
                        let covered = c.name == "covered-table-cell";
                        if c.name != "table-cell" && !covered {
                            continue;
                        }
                        let mut parts = Vec::new();
                        if !covered {
                            for p in c
                                .elements()
                                .filter(|p| matches!(p.name.as_str(), "p" | "h"))
                            {
                                let mut extra = Vec::new();
                                let t = self.inline(p, true, &mut extra)?;
                                if !t.trim().is_empty() {
                                    parts.push(t);
                                }
                            }
                        }
                        let text = md::cell(&parts.join(" "));
                        let rep = c
                            .attr("number-columns-repeated")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 64);
                        for i in 0..rep {
                            if i == 0 {
                                self.doc.budget.charge(text.len() + SLOT)?;
                                row.push(text.clone());
                            } else {
                                self.doc.budget.charge(SLOT)?;
                                row.push(String::new());
                            }
                        }
                        let span = c
                            .attr("number-columns-spanned")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 64);
                        for _ in 1..span {
                            self.doc.budget.charge(SLOT)?;
                            row.push(String::new());
                        }
                    }
                    while row.last().is_some_and(String::is_empty) && row.len() > 1 {
                        row.pop();
                    }
                    rows.push(row);
                }
                "table-header-rows" | "table-rows" | "table-row-group" => {
                    self.table_rows(n, rows)?
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Shapes of a slide: the title and the other frames in order, and the notes.
    fn frames(
        &mut self,
        parent: &Node,
        title: &mut Option<String>,
        notes: &mut Vec<String>,
        in_notes: bool,
    ) -> Result<(), Error> {
        for n in parent.elements() {
            self.doc.budget.tick()?;
            match n.name.as_str() {
                "frame" => {
                    let class = n.attr("class").unwrap_or("");
                    if in_notes {
                        if class == "notes" {
                            for tb in n.descendants("p") {
                                let mut extra = Vec::new();
                                let t = self.inline(tb, true, &mut extra)?;
                                if !t.trim().is_empty() {
                                    notes.push(t.trim().to_string());
                                }
                            }
                        }
                        continue;
                    }
                    match class {
                        "title" => {
                            let mut parts = Vec::new();
                            for p in n.descendants("p") {
                                let mut extra = Vec::new();
                                let t = self.inline(p, true, &mut extra)?;
                                if !t.trim().is_empty() {
                                    parts.push(t.trim().to_string());
                                }
                            }
                            let t = parts.join(" ");
                            if title.is_none() && !t.is_empty() {
                                *title = Some(t);
                            } else if !t.is_empty() {
                                self.doc.push(Block::Para(md::plain(&t)))?;
                            }
                        }
                        "page" | "handout" | "header" | "footer" | "date-time" | "page-number" => {}
                        _ => {
                            self.blocks_in_frame(n)?;
                        }
                    }
                }
                "g" => self.frames(n, title, notes, in_notes)?,
                "notes" => self.frames(n, title, notes, true)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn blocks_in_frame(&mut self, frame: &Node) -> Result<(), Error> {
        let mut extra = Vec::new();
        self.frame_inline(frame, &mut extra);
        self.extra(extra)?;
        for c in frame.elements() {
            match c.name.as_str() {
                "text-box" => self.blocks(c, usize::MAX)?,
                "table" => {
                    let rows = self.table(c)?;
                    if !rows.iter().all(|r| r.iter().all(String::is_empty)) {
                        self.doc.push(Block::Table(rows))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The picture of a frame, as a block for `extra`.
    fn frame_inline(&mut self, frame: &Node, extra: &mut Vec<Block>) {
        let Some(img) = frame.first("image") else {
            return;
        };
        let alt = frame
            .first("title")
            .or_else(|| frame.first("desc"))
            .map(|n| n.text().trim().to_string())
            .filter(|t| !t.is_empty());
        let href = img.attr("href").unwrap_or("");
        let image = if href.is_empty() || href.contains("://") || href.starts_with("..") {
            None
        } else {
            let path = package::resolve("", href);
            self.pkg.image(&path, alt.clone())
        };
        if let Some(b) = self.doc.image_block(image, alt.as_deref()) {
            extra.push(b);
        }
    }

    /// The text of a paragraph or heading. `flat` writes line breaks as spaces.
    fn inline(&mut self, n: &Node, flat: bool, extra: &mut Vec<Block>) -> Result<String, Error> {
        let mut out = String::new();
        self.inline_into(n, flat, extra, &mut out)?;
        Ok(out)
    }

    fn inline_into(
        &mut self,
        n: &Node,
        flat: bool,
        extra: &mut Vec<Block>,
        out: &mut String,
    ) -> Result<(), Error> {
        for c in &n.children {
            self.doc.budget.tick()?;
            match c {
                xml::Child::Text(t) => {
                    // runs of white space are one space in ODF
                    let mut prev_space = out.ends_with(' ');
                    for ch in t.chars() {
                        if ch.is_whitespace() {
                            if !prev_space {
                                out.push(' ');
                            }
                            prev_space = true;
                        } else {
                            out.push(ch);
                            prev_space = false;
                        }
                    }
                }
                xml::Child::Node(e) => match e.name.as_str() {
                    "s" => {
                        let k = e
                            .attr("c")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(100);
                        // charged before the spaces exist
                        self.doc.budget.charge(k)?;
                        out.push_str(&" ".repeat(k));
                    }
                    "tab" => out.push(' '),
                    "line-break" => out.push(if flat { ' ' } else { '\n' }),
                    "a" if self.in_link > 0 => self.inline_into(e, flat, extra, out)?,
                    "a" => {
                        let mut inner = String::new();
                        self.in_link += 1;
                        let r = self.inline_into(e, flat, extra, &mut inner);
                        self.in_link -= 1;
                        r?;
                        let href = e.attr("href").unwrap_or("");
                        if (href.starts_with("http://")
                            || href.starts_with("https://")
                            || href.starts_with("mailto:"))
                            && !inner.trim().is_empty()
                            && !href.contains([')', ' '])
                        {
                            self.doc.budget.link(href.len() + inner.len() + 4)?;
                            out.push_str(&format!("[{}]({href})", inner.trim()));
                        } else {
                            out.push_str(&inner);
                        }
                    }
                    // a note inside a note is not legal and is left out, which also keeps
                    // the walk linear: its paragraphs are listed with the outer note's
                    "note" if self.in_note > 0 => {}
                    // a paragraph inside a paragraph is read as one of its own by whoever
                    // lists the paragraphs, so that its text is not copied into every
                    // paragraph around it
                    "p" | "h" => {}
                    "note" => {
                        let id = self.footnotes.len() + 1;
                        let label = format!("fn{id}");
                        let mut body: Vec<String> = Vec::new();
                        if let Some(b) = e.first("note-body") {
                            self.in_note += 1;
                            for p in b.descendants("p") {
                                let mut x = Vec::new();
                                let t = match self.inline(p, true, &mut x) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        self.in_note -= 1;
                                        return Err(e);
                                    }
                                };
                                let t = t.trim();
                                if !t.is_empty() {
                                    body.push(t.to_string());
                                }
                            }
                            self.in_note -= 1;
                        }
                        if !body.is_empty() && self.footnotes.len() < 1000 {
                            out.push_str(&format!("[^{label}]"));
                            let text = body.join(" ");
                            self.doc.budget.charge(text.len() + SLOT)?;
                            self.footnotes.push((label, text));
                        }
                    }
                    "frame" => self.frame_inline(e, extra),
                    "annotation" | "annotation-end" | "tracked-changes" | "note-body"
                    | "bookmark" | "bookmark-start" | "bookmark-end" => {}
                    _ => self.inline_into(e, flat, extra, out)?,
                },
            }
        }
        Ok(())
    }
}

// ---- spreadsheets -----------------------------------------------------------------

pub fn sheets(pkg: &mut Package, limits: &Limits) -> Result<Vec<Sheet>, Error> {
    let bytes = pkg.need("content.xml")?;
    let mut r = xml::reader(&bytes);
    let mut buf = Vec::new();
    let mut out: Vec<Sheet> = Vec::new();
    let mut budget = Budget::new(limits);
    let mut hidden_styles: HashSet<String> = HashSet::new();
    let mut style_name: Option<String> = None;
    // the table being read, at depth 1
    let mut depth = 0usize;
    let mut name = String::new();
    let mut hidden = false;
    let mut index = 0usize;
    let mut b: Option<SheetBuilder> = None;
    let mut row: sheet::Cells = Vec::new();
    let mut col = 0usize;
    let mut row_rep = 1usize;
    // the current cell
    let mut cell: Option<CellState> = None;
    let mut para = String::new();
    let mut in_p = false;
    // inside an `office:annotation`, the comment of a cell, which is not its value
    let mut annotation = 0usize;
    let mut paras: Vec<String> = Vec::new();
    // bytes charged for the paragraphs and spaces of the current cell
    let mut transient = 0usize;
    loop {
        budget.tick()?;
        let ev = r.read_event_into(&mut buf).map_err(corrupt)?;
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                let tag = xml::local(e.name().as_ref());
                match tag.as_str() {
                    "style" => {
                        let at = xml::attrs(e)?;
                        let get = |k: &str| {
                            at.iter()
                                .find(|(n, _)| n == k || n.ends_with(&format!(":{k}")))
                                .map(|(_, v)| v.clone())
                        };
                        // a name longer than any real one is never a hidden table's style
                        style_name = if get("family").as_deref() == Some("table") {
                            get("name").filter(|n| n.len() <= MAX_NAME)
                        } else {
                            None
                        };
                        if empty {
                            style_name = None;
                        }
                    }
                    "table-properties" => {
                        let at = xml::attrs(e)?;
                        if at
                            .iter()
                            .any(|(k, v)| k.ends_with("display") && v == "false")
                            && let Some(s) = &style_name
                            && !hidden_styles.contains(s)
                        {
                            hidden_styles.insert(s.clone());
                        }
                    }
                    "table" => {
                        depth += 1;
                        if depth == 1 {
                            let at = xml::attrs(e)?;
                            let get = |k: &str| {
                                at.iter()
                                    .find(|(n, _)| n.ends_with(&format!(":{k}")))
                                    .map(|(_, v)| v.clone())
                            };
                            name = get("name").unwrap_or_default();
                            hidden = get("style-name").is_some_and(|s| hidden_styles.contains(&s));
                            index += 1;
                            b = Some(SheetBuilder::new(limits.max_cells));
                        }
                        if empty {
                            depth -= 1;
                        }
                    }
                    "table-row" if depth == 1 => {
                        let at = xml::attrs(e)?;
                        row.clear();
                        col = 0;
                        row_rep = at
                            .iter()
                            .find(|(n, _)| n.ends_with("number-rows-repeated"))
                            .and_then(|(_, v)| v.parse().ok())
                            .unwrap_or(1);
                        if empty {
                            row_rep = 1;
                        }
                    }
                    "table-cell" | "covered-table-cell" if depth == 1 => {
                        let at = xml::attrs(e)?;
                        let get = |k: &str| {
                            at.iter()
                                .find(|(n, _)| n.ends_with(&format!(":{k}")))
                                .map(|(_, v)| v.clone())
                        };
                        let st = CellState {
                            kind: get("value-type").unwrap_or_default(),
                            value: get("value"),
                            date: get("date-value"),
                            time: get("time-value"),
                            boolean: get("boolean-value"),
                            string: get("string-value"),
                            repeat: get("number-columns-repeated")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1),
                        };
                        paras.clear();
                        budget.release(std::mem::take(&mut transient));
                        if empty {
                            finish_cell(&st, &[], &mut row, &mut col, &mut budget)?;
                        } else {
                            cell = Some(st);
                        }
                    }
                    "annotation" if !empty => annotation += 1,
                    _ if annotation > 0 => {}
                    "p" if cell.is_some() && !empty => {
                        in_p = true;
                        para.clear();
                    }
                    "s" if in_p => {
                        let n = xml::attrs(e)?
                            .iter()
                            .find(|(k, _)| k.ends_with(":c"))
                            .and_then(|(_, v)| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(100);
                        budget.charge(n)?;
                        transient += n;
                        para.push_str(&" ".repeat(n));
                    }
                    "tab" if in_p => para.push(' '),
                    "line-break" if in_p => para.push('\n'),
                    _ => {}
                }
            }
            Event::Text(t) if in_p && annotation == 0 => para.push_str(&xml::text_of(t)),
            Event::End(e) => match xml::local(e.name().as_ref()).as_str() {
                "style" => style_name = None,
                "annotation" => annotation = annotation.saturating_sub(1),
                "p" if annotation > 0 => {}
                "p" if in_p => {
                    in_p = false;
                    budget.charge(para.len() + SLOT)?;
                    transient += para.len() + SLOT;
                    paras.push(std::mem::take(&mut para));
                }
                "table-cell" | "covered-table-cell" if depth == 1 => {
                    if let Some(st) = cell.take() {
                        finish_cell(&st, &paras, &mut row, &mut col, &mut budget)?;
                        paras.clear();
                        budget.release(std::mem::take(&mut transient));
                    }
                }
                "table-row" if depth == 1 => {
                    if let Some(bld) = b.as_mut() {
                        bld.row(
                            std::mem::take(&mut row),
                            row_rep.clamp(1, 1000),
                            &mut budget,
                        )?;
                    }
                    row.clear();
                }
                "table" => {
                    if depth == 1
                        && let Some(bld) = b.take()
                    {
                        if hidden {
                            bld.discard(&mut budget);
                        } else if let Some(sh) =
                            bld.finish(&name, index, limits.max_csv, &mut budget)?
                        {
                            out.push(sh);
                        }
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            },
            Event::DocType(_) => return Err(corrupt("a DOCTYPE is not allowed")),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

struct CellState {
    kind: String,
    value: Option<String>,
    date: Option<String>,
    time: Option<String>,
    boolean: Option<String>,
    string: Option<String>,
    repeat: usize,
}

fn finish_cell(
    st: &CellState,
    paras: &[String],
    row: &mut sheet::Cells,
    col: &mut usize,
    budget: &mut Budget,
) -> Result<(), Error> {
    let text = paras.join("\n");
    let v = match st.kind.as_str() {
        "float" | "percentage" | "currency" => st.value.clone().unwrap_or(text),
        "date" => st.date.clone().unwrap_or(text),
        "time" => st.time.clone().unwrap_or(text),
        "boolean" => match st.boolean.as_deref() {
            Some("true") => "TRUE".into(),
            Some(_) => "FALSE".into(),
            None => text,
        },
        "string" => st.string.clone().unwrap_or(text),
        _ => text,
    };
    let rep = st.repeat.clamp(1, sheet::MAX_COLUMNS);
    if v.is_empty() {
        *col = (*col + rep).min(sheet::MAX_COLUMNS);
        return Ok(());
    }
    // each copy is charged before it is made, and none is made past the last column
    for _ in 0..rep.min(1000) {
        if *col >= sheet::MAX_COLUMNS {
            break;
        }
        sheet::set(row, *col, Cow::Borrowed(v.as_str()), budget)?;
        *col += 1;
    }
    Ok(())
}

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("the XML cannot be read: {e}"))
}
