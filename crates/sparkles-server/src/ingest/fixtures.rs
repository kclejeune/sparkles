//! Small PDFs for the conversion tests, written here byte by byte so that the tests need
//! no binary file: pages of Helvetica text, and pages that hold only a grey image, as a
//! scan does.

/// A page of a fixture PDF.
pub enum Page<'a> {
    /// lines of text, top to bottom
    Text(&'a [&'a str]),
    /// an image and no text, as a scanned page
    Image,
}

/// A PDF of `pages`, with a correct cross-reference table.
pub fn pdf(pages: &[Page]) -> Vec<u8> {
    // 1 the catalog, 2 the page tree, 3 the font, then each page, its content and its
    // image
    let mut bodies: Vec<(usize, String)> = Vec::new();
    let mut kids = Vec::new();
    let mut next = 4;
    for p in pages {
        let page_id = next;
        let content_id = next + 1;
        next += 2;
        kids.push(format!("{page_id} 0 R"));
        match p {
            Page::Text(lines) => {
                let mut c = String::from("BT /F1 12 Tf 72 720 Td 16 TL\n");
                for l in lines.iter() {
                    let esc = l
                        .replace('\\', "\\\\")
                        .replace('(', "\\(")
                        .replace(')', "\\)");
                    c.push_str(&format!("({esc}) Tj T*\n"));
                }
                c.push_str("ET");
                bodies.push((
                    page_id,
                    format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
                    ),
                ));
                bodies.push((
                    content_id,
                    format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()),
                ));
            }
            Page::Image => {
                let img_id = next;
                next += 1;
                let c = "q 500 0 0 700 50 50 cm /Im1 Do Q";
                bodies.push((
                    page_id,
                    format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 {img_id} 0 R >> >> /Contents {content_id} 0 R >>"
                    ),
                ));
                bodies.push((
                    content_id,
                    format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()),
                ));
                let (w, h) = (64, 64);
                let mut data: String = (0..w * h)
                    .map(|i| if (i / 7) % 3 == 0 { "00" } else { "ff" })
                    .collect();
                data.push('>');
                bodies.push((
                    img_id,
                    format!(
                        "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /ASCIIHexDecode /Length {} >>\nstream\n{data}\nendstream",
                        data.len()
                    ),
                ));
            }
        }
    }
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_string(),
    ];
    bodies.sort_by_key(|(i, _)| *i);
    objs.extend(bodies.into_iter().map(|(_, b)| b));
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, b) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{b}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// The three pages of the born-digital report of A54.
pub const REPORT: [&[&str]; 3] = [
    &[
        "Quarterly report of the engineering teams.",
        "Ana Lima moved to the payments team in October.",
        "The payments team now has five engineers.",
        "Hiring for the team continues next quarter.",
        "The team works from the Berlin office.",
        "Its budget did not change this quarter.",
    ],
    &[
        "Kai Berg leads the platform team.",
        "The checkout redesign ships on 14 October.",
        "The platform team owns the deployment tools.",
        "Two engineers joined the platform team.",
        "The on-call rotation now has eight people.",
        "Incidents fell by a third since July.",
    ],
    &[
        "Acme Corp has three teams.",
        "Each team reports to the head of engineering.",
        "The teams meet every second Thursday.",
        "Their goals for next year follow in January.",
        "This report was written by the team leads.",
        "Questions go to the engineering office.",
    ],
];

/// The report as a PDF of three text pages.
pub fn report() -> Vec<u8> {
    pdf(&[
        Page::Text(REPORT[0]),
        Page::Text(REPORT[1]),
        Page::Text(REPORT[2]),
    ])
}

/// Two scanned pages.
pub fn scanned() -> Vec<u8> {
    pdf(&[Page::Image, Page::Image])
}

/// Four pages whose third is scanned (A55).
pub fn mixed() -> Vec<u8> {
    pdf(&[
        Page::Text(REPORT[0]),
        Page::Text(REPORT[1]),
        Page::Image,
        Page::Text(REPORT[2]),
    ])
}
