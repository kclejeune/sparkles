//! Small documents of every format, written byte by byte so that the tests and the
//! fuzz targets' seeds need no binary file. Containers are stored without compression,
//! so that a changed byte lands in the XML.

use std::io::Write;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

/// A container of `parts`, stored without compression so that a changed byte lands in
/// the XML.
pub fn container(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut z = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, body) in parts {
        z.start_file(*name, o).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

pub const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;

pub fn docx() -> Vec<u8> {
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {W}><w:body>
<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Quarterly report</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Teams</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Ana Lima moved to the </w:t></w:r><w:r><w:t>payments team.</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>first bullet</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>nested bullet</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>step one</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>step two</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Table</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Name</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Team | unit</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>Ana</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>payments</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:t>See </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:t>the site</w:t></w:r></w:hyperlink><w:del><w:r><w:delText>deleted words</w:delText></w:r></w:del></w:p>
<w:p><w:r><w:t>&lt;!-- Page 9 --&gt;</w:t></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" name="Picture 1" descr="A chart of hires"/><a:graphic><a:graphicData><a:blip r:embed="rId8"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:sectPr/></w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles {W}>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/></w:style>
</w:styles>"#
    );
    let numbering = format!(
        r#"<w:numbering {W}>
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#
    );
    let footnotes = format!(
        r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="0"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>
<w:footnote w:id="2"><w:p><w:r><w:t>Effective 1 October.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId8" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org/site" TargetMode="External"/>
</Relationships>"#;
    let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Q3 report</dc:title></cp:coreProperties>"#;
    container(&[
        ("[Content_Types].xml", "<Types/>"),
        ("word/document.xml", &doc),
        ("word/styles.xml", &styles),
        ("word/numbering.xml", &numbering),
        ("word/footnotes.xml", &footnotes),
        ("word/_rels/document.xml.rels", rels),
        ("word/media/image1.png", "\u{89}PNG fake"),
        ("docProps/core.xml", core),
    ])
}

pub const P: &str = r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

pub fn pptx() -> Vec<u8> {
    let pres = format!(
        r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/><p:sldId id="258" r:id="rId4"/></p:sldIdLst></p:presentation>"#
    );
    let prels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide3.xml"/>
</Relationships>"#;
    let slide1 = format!(
        r#"<p:sld {P}><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Hiring plan</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody>
<a:p><a:r><a:t>Five engineers join payments</a:t></a:r></a:p>
<a:p><a:pPr lvl="1"/><a:r><a:t>Two start in October</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="4" name="Number"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
    );
    let slide1_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/>
</Relationships>"#;
    let notes = format!(
        r#"<p:notes {P}><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Mention the Berlin office.</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:notes>"#
    );
    let slide2 = format!(
        r#"<p:sld {P}><p:cSld><p:spTree>
<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="2" name="Table"/></p:nvGraphicFramePr><a:graphic><a:graphicData><a:tbl>
<a:tr><a:tc><a:txBody><a:p><a:r><a:t>Team</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>Size</a:t></a:r></a:p></a:txBody></a:tc></a:tr>
<a:tr><a:tc><a:txBody><a:p><a:r><a:t>Payments</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>5</a:t></a:r></a:p></a:txBody></a:tc></a:tr>
</a:tbl></a:graphicData></a:graphic></p:graphicFrame>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Text"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>Plain text box</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
    );
    let slide3 = format!(
        r#"<p:sld {P} show="0"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:txBody><a:p><a:r><a:t>Hidden slide text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
    );
    container(&[
        ("ppt/presentation.xml", &pres),
        ("ppt/_rels/presentation.xml.rels", prels),
        ("ppt/slides/slide1.xml", &slide1),
        ("ppt/slides/_rels/slide1.xml.rels", slide1_rels),
        ("ppt/notesSlides/notesSlide1.xml", &notes),
        ("ppt/slides/slide2.xml", &slide2),
        ("ppt/slides/slide3.xml", &slide3),
    ])
}

pub fn xlsx() -> Vec<u8> {
    let wb = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><workbookPr/><sheets>
<sheet name="People" sheetId="1" r:id="rId1"/><sheet name="Secret" sheetId="2" state="hidden" r:id="rId2"/><sheet name="Empty" sheetId="3" r:id="rId3"/></sheets></workbook>"#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet3.xml"/>
</Relationships>"#;
    let shared = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>id</t></si><si><t>name</t></si><si><t>start</t></si><si><t>active</t></si><si><t>Ana, "AL" Lima</t></si><si><r><t>Kai</t></r><r><t> Ito</t></r><rPh><t>phonetic</t></rPh></si></sst>"#;
    let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy\-mm\-dd"/></numFmts><cellXfs count="3"><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/></cellXfs></styleSheet>"#;
    let sheet1 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="B1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c><c r="D1" t="s"><v>2</v></c><c r="E1" t="s"><v>3</v></c></row>
<row r="2"><c r="B2"><v>1</v></c><c r="C2" t="s"><v>4</v></c><c r="D2" s="1"><v>45931</v></c><c r="E2" t="b"><v>1</v></c></row>
<row r="3"><c r="B3"><f>B2+1</f><v>2</v></c><c r="C3" t="s"><v>5</v></c><c r="D3" s="2"><v>45932.5</v></c><c r="E3" t="b"><v>0</v></c></row>
<row r="4"/>
<row r="5"><c r="B5" t="inlineStr"><is><t>tail</t></is></c></row>
</sheetData></worksheet>"#;
    let sheet2 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hidden</t></is></c></row></sheetData></worksheet>"#;
    let sheet3 = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#;
    container(&[
        ("xl/workbook.xml", wb),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/sharedStrings.xml", shared),
        ("xl/styles.xml", styles),
        ("xl/worksheets/sheet1.xml", sheet1),
        ("xl/worksheets/sheet2.xml", sheet2),
        ("xl/worksheets/sheet3.xml", sheet3),
    ])
}

pub const O: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0""#;

pub fn odt() -> Vec<u8> {
    let c = format!(
        r#"<office:document-content {O}><office:automatic-styles>
<text:list-style style:name="L1"><text:list-level-style-bullet text:level="1"/></text:list-style>
<text:list-style style:name="L2"><text:list-level-style-number text:level="1"/></text:list-style>
</office:automatic-styles><office:body><office:text>
<text:h text:outline-level="1">Handbook</text:h>
<text:p>Ana  Lima<text:s text:c="2"/>moved to <text:a xlink:href="https://example.org/p">payments</text:a>.<text:note text:id="ftn1"><text:note-citation>1</text:note-citation><text:note-body><text:p>Since October.</text:p></text:note-body></text:note></text:p>
<text:list text:style-name="L1"><text:list-item><text:p>apples</text:p></text:list-item><text:list-item><text:p>pears</text:p></text:list-item></text:list>
<text:list text:style-name="L2"><text:list-item><text:p>one</text:p></text:list-item><text:list-item><text:p>two</text:p></text:list-item></text:list>
<table:table><table:table-row><table:table-cell><text:p>Team</text:p></table:table-cell><table:table-cell><text:p>Size</text:p></table:table-cell></table:table-row>
<table:table-row><table:table-cell><text:p>Payments</text:p></table:table-cell><table:table-cell><text:p>5</text:p></table:table-cell></table:table-row></table:table>
<text:p>Pic<draw:frame><draw:image xlink:href="Pictures/a.png"/><svg:title>Org chart</svg:title></draw:frame></text:p>
</office:text></office:body></office:document-content>"#
    );
    let meta = r#"<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:title>Handbook title</dc:title></office:meta></office:document-meta>"#;
    container(&[
        ("mimetype", "application/vnd.oasis.opendocument.text"),
        ("content.xml", &c),
        ("meta.xml", meta),
    ])
}

pub fn odp() -> Vec<u8> {
    let c = format!(
        r#"<office:document-content {O}><office:body><office:presentation>
<draw:page draw:name="page1">
<draw:frame presentation:class="title"><draw:text-box><text:p>Roadmap</text:p></draw:text-box></draw:frame>
<draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>Launch in Berlin</text:p></text:list-item></text:list></draw:text-box></draw:frame>
<presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Say hello</text:p></draw:text-box></draw:frame></presentation:notes>
</draw:page>
<draw:page draw:name="page2"><draw:frame presentation:class="title"><draw:text-box><text:p>Budget</text:p></draw:text-box></draw:frame></draw:page>
</office:presentation></office:body></office:document-content>"#
    );
    container(&[
        (
            "mimetype",
            "application/vnd.oasis.opendocument.presentation",
        ),
        ("content.xml", &c),
    ])
}

pub fn ods() -> Vec<u8> {
    let c = format!(
        r#"<office:document-content {O}><office:automatic-styles><style:style style:name="ta2" style:family="table"><style:table-properties table:display="false"/></style:style></office:automatic-styles><office:body><office:spreadsheet>
<table:table table:name="People"><table:table-row><table:table-cell office:value-type="string"><text:p>name</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>joined</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>n</text:p></table:table-cell></table:table-row>
<table:table-row><table:table-cell office:value-type="string"><text:p>Ana, A.</text:p></table:table-cell><table:table-cell office:value-type="date" office:date-value="2026-10-01"><text:p>10/1/26</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="5"><text:p>5.00</text:p></table:table-cell></table:table-row>
<table:table-row table:number-rows-repeated="1000000"><table:table-cell table:number-columns-repeated="16384"/></table:table-row></table:table>
<table:table table:name="Hidden" table:style-name="ta2"><table:table-row><table:table-cell office:value-type="string"><text:p>x</text:p></table:table-cell></table:table-row></table:table>
</office:spreadsheet></office:body></office:document-content>"#
    );
    container(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        ("content.xml", &c),
    ])
}
