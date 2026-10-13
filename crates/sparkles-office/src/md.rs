//! A small Markdown writer for the blocks the converters find.

use crate::{Error, Image};

pub enum Block {
    Heading(usize, String),
    Para(String),
    /// a list item: nesting level from 0, and its number when the list is ordered
    Item(usize, Option<u64>, String),
    Quote(String),
    Table(Vec<Vec<String>>),
    /// a page marker, as the PDF conversion writes it
    Page(usize),
    /// an image placeholder, by index
    Image(usize),
}

/// Text made safe to put in a line: no line starts a comment, so that a document cannot
/// forge a page or an image marker.
pub fn plain(s: &str) -> String {
    s.trim()
        .lines()
        .map(|l| {
            let t = l.trim();
            if t.starts_with("<!--") {
                format!("\\{t}")
            } else {
                t.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A table cell: one line, with the pipe escaped.
pub fn cell(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    one.replace('|', "\\|")
}

#[derive(Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub images: Vec<Image>,
}

impl Doc {
    pub fn push(&mut self, b: Block) {
        self.blocks.push(b);
    }

    /// The block for a picture: its placeholder, or the alt text alone when there is no
    /// room for the picture.
    pub fn image_block(&mut self, image: Option<Image>, alt: Option<&str>) -> Option<Block> {
        let alt = alt.map(str::trim).filter(|a| !a.is_empty());
        match image {
            Some(i) => {
                self.images.push(i);
                Some(Block::Image(self.images.len() - 1))
            }
            None => alt.map(|a| Block::Para(format!("[Image: {}]", plain(a)))),
        }
    }

    pub fn render(&self, max: usize) -> Result<String, Error> {
        let mut out = String::new();
        let mut prev_item = false;
        // whether the last item at the top level was numbered
        let mut top_ordered = false;
        for b in &self.blocks {
            let item = matches!(b, Block::Item(..));
            // an item continues the list before it, unless it starts a list of the
            // other kind
            let continues = match b {
                Block::Item(0, n, _) => prev_item && n.is_some() == top_ordered,
                Block::Item(..) => prev_item,
                _ => false,
            };
            if let Block::Item(0, n, _) = b {
                top_ordered = n.is_some();
            }
            if !out.is_empty() {
                out.push_str(if continues { "\n" } else { "\n\n" });
            }
            prev_item = item;
            match b {
                Block::Heading(l, t) => {
                    out.push_str(&"#".repeat((*l).clamp(1, 6)));
                    out.push(' ');
                    out.push_str(t);
                }
                Block::Para(t) => out.push_str(t),
                Block::Item(level, n, t) => {
                    out.push_str(&"    ".repeat(*level));
                    match n {
                        Some(n) => out.push_str(&format!("{n}. ")),
                        None => out.push_str("- "),
                    }
                    out.push_str(t);
                }
                Block::Quote(t) => {
                    for (i, l) in t.lines().enumerate() {
                        if i > 0 {
                            out.push('\n');
                        }
                        out.push_str("> ");
                        out.push_str(l);
                    }
                }
                Block::Table(rows) => table(rows, &mut out),
                Block::Page(n) => out.push_str(&format!("<!-- Page {n} -->")),
                Block::Image(k) => out.push_str(&format!("<!-- image:{k} -->")),
            }
            if out.len() > max {
                return Err(Error::TooLarge(format!(
                    "the document's text is longer than {max} bytes"
                )));
            }
        }
        out.push('\n');
        Ok(out)
    }
}

fn table(rows: &[Vec<String>], out: &mut String) {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return;
    }
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push('|');
        for c in 0..width {
            out.push(' ');
            out.push_str(r.get(c).map_or("", String::as_str));
            out.push_str(" |");
        }
        if i == 0 {
            out.push('\n');
            out.push('|');
            for _ in 0..width {
                out.push_str(" --- |");
            }
        }
    }
}
