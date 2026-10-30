#[path = "support/ooxml.rs"]
mod ooxml;

use std::{
    io::Cursor,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use katwork::{
    docs::{
        self, DocError, DocLimits, DocPool, ExtractWarning, Extracted, Locator, MediaKind,
        colour::{self, Rgb, SchemeSlot, ThemeColours},
        deadline,
        package::{self, open_package},
        watchdog::{self, Watchdog, WedgeSource},
        xml_guard,
    },
    ids::WorkspaceId,
};
use ooxml::*;
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

fn limits() -> DocLimits {
    DocLimits::default()
}

fn read(docx: &Docx) -> Extracted {
    docs::extract(MediaKind::Docx, &docx.build(), &limits()).unwrap()
}

fn read_body(body: &str) -> Extracted {
    read(&Docx::new(body))
}

/// The blocks' text, one line per block.
fn text(extracted: &Extracted) -> String {
    extracted
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn hidden(extracted: &Extracted) -> Vec<(u32, &'static str)> {
    extracted
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            ExtractWarning::HiddenContent {
                at: Locator::DocxParagraph { ordinal, .. },
                why,
            } => Some((*ordinal, *why)),
            _ => None,
        })
        .collect()
}

/// A fresh element budget, as one package gets.
fn budget() -> xml_guard::ElementBudget {
    xml_guard::ElementBudget::new(&limits())
}

fn ordinals(extracted: &Extracted) -> Vec<u32> {
    extracted.blocks.iter().map(|block| block.ordinal).collect()
}

/// One paragraph per `(run properties, marker)`, each marker its own run.
fn paragraphs(runs: &[(&str, &str)]) -> String {
    runs.iter()
        .map(|(props, marker)| para("", &run(props, marker)))
        .collect()
}

fn cell(props: &str, content: &str) -> String {
    format!("<w:tc><w:tcPr>{props}</w:tcPr>{content}</w:tc>")
}

fn table(props: &str, rows: &[Vec<String>]) -> String {
    let rows: String = rows
        .iter()
        .map(|cells| format!("<w:tr>{}</w:tr>", cells.concat()))
        .collect();
    format!(
        "<w:tbl><w:tblPr>{props}</w:tblPr><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid>{rows}</w:tbl>"
    )
}

fn table_style(id: &str, inner: &str) -> String {
    format!(r#"<w:style w:type="table" w:styleId="{id}"><w:name w:val="{id}"/>{inner}</w:style>"#)
}

fn region(kind: &str, fill: &str) -> String {
    format!(
        r#"<w:tblStylePr w:type="{kind}"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{fill}"/></w:tcPr></w:tblStylePr>"#
    )
}

const WHITE: &str = r#"<w:color w:val="FFFFFF"/>"#;
const BLACK: &str = r#"<w:color w:val="000000"/>"#;

#[test]
fn text_blocks_have_code_point_ranges() {
    let source = "สวัสดีครับ 👋🏽 e\u{301}\r\nบรรทัดที่สอง\n\n\nผู้ใหญ่ 🇹🇭 ok";
    let extracted = docs::extract(MediaKind::Text, source.as_bytes(), &limits()).unwrap();
    assert_eq!(extracted.extractor, docs::ExtractorId::TEXT);
    let chars: Vec<char> = source.chars().collect();
    assert_eq!(extracted.blocks.len(), 2);
    for (block, ordinal) in extracted.blocks.iter().zip(1..) {
        let Locator::Text { start, end } = block.locator else {
            panic!("a text locator");
        };
        let slice: String = chars[start as usize..end as usize].iter().collect();
        assert_eq!(slice, block.text);
        assert_eq!((block.ordinal, block.piece), (ordinal, 0));
    }
    assert_eq!(extracted.blocks[0].text, "สวัสดีครับ 👋🏽 e\u{301}\r\nบรรทัดที่สอง");
    assert_eq!(extracted.blocks[1].text, "ผู้ใหญ่ 🇹🇭 ok");
}

#[test]
fn utf16_text_with_bom_is_decoded() {
    let source = "ประชุม 👋 one\n\nสอง two\r\n";
    let utf8 = docs::extract(MediaKind::Text, source.as_bytes(), &limits()).unwrap();
    let mut le = vec![0xFF, 0xFE];
    le.extend(source.encode_utf16().flat_map(u16::to_le_bytes));
    let mut be = vec![0xFE, 0xFF];
    be.extend(source.encode_utf16().flat_map(u16::to_be_bytes));
    for bytes in [le, be] {
        assert_eq!(
            docs::extract(MediaKind::Text, &bytes, &limits()).unwrap(),
            utf8
        );
    }
}

#[test]
fn long_paragraph_splits_deterministically_at_1500() {
    let words = ["ผู้ใหญ่", "ประชุม", "budget", "e\u{301}e\u{301}", "👋🏽", "x"];
    let mut source = String::new();
    let mut at = 0;
    while source.chars().count() < 4_000 {
        source.push_str(words[at % words.len()]);
        source.push(if at % 7 == 6 { '\u{3000}' } else { ' ' });
        at += 1;
    }
    let source: String = source.chars().take(4_000).collect();
    let first = docs::extract(MediaKind::Text, source.as_bytes(), &limits()).unwrap();
    let second = docs::extract(MediaKind::Text, source.as_bytes(), &limits()).unwrap();
    assert_eq!(first, second);
    assert_eq!(docs::MAX_BLOCK_CHARS, 1_500);
    assert_eq!(
        first
            .blocks
            .iter()
            .map(|block| (block.ordinal, block.piece))
            .collect::<Vec<_>>(),
        [(1, 0), (1, 1), (1, 2)]
    );
    let boundaries: std::collections::HashSet<usize> = source
        .grapheme_indices(true)
        .map(|(offset, _)| source[..offset].chars().count())
        .chain([source.chars().count()])
        .collect();
    let mut joined = String::new();
    for block in &first.blocks {
        let Locator::Text { start, end } = block.locator else {
            panic!("a text locator");
        };
        assert!(end - start <= 1_500);
        assert!(boundaries.contains(&(start as usize)) && boundaries.contains(&(end as usize)));
        joined.push_str(&block.text);
    }
    assert_eq!(joined, source);
}

#[test]
fn a_grapheme_longer_than_the_limit_stays_whole() {
    let long = format!("a{}", "\u{301}".repeat(2_000));
    let source = format!("{long} tail");
    let extracted = docs::extract(MediaKind::Text, source.as_bytes(), &limits()).unwrap();
    assert_eq!(extracted.blocks.len(), 2);
    assert_eq!(extracted.blocks[0].text, long);
    assert_eq!(extracted.blocks[1].text, " tail");
}

#[test]
fn text_blank_lines_and_ranges() {
    let extracted = docs::extract(
        MediaKind::Text,
        "one\n \t\u{3000}\ntwo".as_bytes(),
        &limits(),
    )
    .unwrap();
    assert_eq!(text(&extracted), "one\ntwo");
    let plain = docs::extract(MediaKind::Text, "ก\n\nข".as_bytes(), &limits()).unwrap();
    let with_bom = docs::extract(MediaKind::Text, "\u{FEFF}ก\n\nข".as_bytes(), &limits()).unwrap();
    assert_eq!(with_bom, plain);
    assert_eq!(plain.blocks[1].locator, Locator::Text { start: 3, end: 4 });
    let crlf = docs::extract(MediaKind::Text, b"a\r\nb\r\n\r\nc", &limits()).unwrap();
    assert_eq!(crlf.blocks[0].text, "a\r\nb");
    assert_eq!(crlf.blocks[0].locator, Locator::Text { start: 0, end: 4 });
    assert_eq!(crlf.blocks[1].locator, Locator::Text { start: 8, end: 9 });
    for bytes in [
        vec![0xFF, 0xFE, b'a'],
        vec![0xFF, 0xFE, 0x00, 0xD8, b'a', 0x00],
        vec![0xFE, 0xFF, 0xDC, 0x00],
        vec![0xC3, 0x28],
    ] {
        assert_eq!(
            docs::extract(MediaKind::Text, &bytes, &limits()),
            Err(DocError::Malformed("text_encoding"))
        );
    }
    let nul = docs::extract(MediaKind::Text, b"a\0b", &limits()).unwrap();
    assert_eq!(nul.blocks[0].text, "a\u{FFFD}b");
}

#[test]
fn contrast_ratio_matches_wcag_examples() {
    let hex = |s| colour::parse_hex(s).unwrap();
    assert!((colour::contrast_ratio(Rgb::WHITE, Rgb::BLACK) - 21.0).abs() <= 0.01);
    assert!(colour::contrast_ratio(hex("FFFFFF"), hex("F8F8F8")) < 1.2);
    assert!(colour::contrast_ratio(hex("767676"), Rgb::WHITE) >= 4.5);
    assert!(colour::contrast_ratio(hex("EAEAEA"), Rgb::WHITE) >= 1.2);
    assert!(colour::contrast_ratio(hex("EBEBEB"), Rgb::WHITE) < 1.2);
    assert!(colour::contrast_ratio(Rgb::BLACK, hex("1F3864")) >= 1.2);
    assert_eq!(colour::parse_hex("80ffffff"), Some(Rgb::WHITE));
    for bad in ["auto", "FFF", "GGGGGG", "#FFFFFF", " FFFFFF"] {
        assert_eq!(colour::parse_hex(bad), None);
    }
    assert_eq!(colour::readable_on(hex("1F3864")), Rgb::WHITE);
    assert_eq!(colour::readable_on(hex("FFFF00")), Rgb::BLACK);
}

#[test]
fn theme_colours_parse_tint_and_map_xlsx_indexes() {
    let colours = ThemeColours::parse(theme(&[]).as_bytes(), &limits(), &mut budget()).unwrap();
    assert_eq!(colours, ThemeColours::office_default());
    assert_eq!(colours.resolve(SchemeSlot::Lt1, -1.0), Rgb::BLACK);
    assert_eq!(colours.resolve(SchemeSlot::Dk1, 1.0), Rgb::WHITE);
    assert_eq!(
        colours.resolve(SchemeSlot::Accent1, 0.0),
        colour::parse_hex("4472C4").unwrap()
    );
    let lighter = colours.resolve(SchemeSlot::Accent1, 0.5);
    assert!(
        colour::contrast_ratio(lighter, Rgb::WHITE)
            < colour::contrast_ratio(colour::parse_hex("4472C4").unwrap(), Rgb::WHITE)
    );
    let partial = ThemeColours::parse(
        theme(&[
            ("accent1", ""),
            ("accent2", r#"<a:accent2><a:srgbClr val="ED7D31"><a:lumMod val="50000"/></a:srgbClr></a:accent2>"#),
            ("accent3", r#"<a:accent3><a:hslClr hue="0" sat="0" lum="0"/></a:accent3>"#),
        ])
        .as_bytes(),
        &limits(),
        &mut budget(),
    )
    .unwrap();
    assert_eq!(partial.slot(SchemeSlot::Accent1), None);
    assert_eq!(partial.slot(SchemeSlot::Accent2), None);
    assert_eq!(partial.slot(SchemeSlot::Accent3), None);
    assert_eq!(partial.slot(SchemeSlot::Dk1), Some(Rgb::BLACK));
    let order = [
        SchemeSlot::Lt1,
        SchemeSlot::Dk1,
        SchemeSlot::Lt2,
        SchemeSlot::Dk2,
        SchemeSlot::Accent1,
        SchemeSlot::Accent2,
        SchemeSlot::Accent3,
        SchemeSlot::Accent4,
        SchemeSlot::Accent5,
        SchemeSlot::Accent6,
        SchemeSlot::Hlink,
        SchemeSlot::FolHlink,
    ];
    for (index, slot) in order.into_iter().enumerate() {
        assert_eq!(SchemeSlot::from_xlsx_theme_index(index as u32), Some(slot));
    }
    assert_eq!(SchemeSlot::from_xlsx_theme_index(12), None);
    assert_eq!(
        ThemeColours::parse(b"<!DOCTYPE a><a/>", &limits(), &mut budget()),
        Err(DocError::Malformed("doctype"))
    );
}

#[test]
fn extract_refuses_kinds_whose_readers_come_later() {
    // The XLSX reader has come (Task 4): the minimal workbook reads as its one cell.
    let workbook = xlsx(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row>"#);
    let read = docs::extract(MediaKind::Xlsx, &workbook, &limits()).unwrap();
    assert_eq!(text(&read), "x");
    assert_eq!(
        docs::extract(MediaKind::Pdf, b"%PDF-1.7", &limits()),
        Err(DocError::Unsupported("pdf"))
    );
    assert_eq!(
        docs::extract(MediaKind::Image, b"\x89PNG", &limits()),
        Err(DocError::Unsupported("image"))
    );
}

#[test]
fn doc_errors_map_to_tool_codes() {
    for (error, code) in [
        (DocError::TooLarge("input"), "file_too_large"),
        (DocError::TooComplex("xml_depth"), "file_too_complex"),
        (DocError::Malformed("zip"), "file_malformed"),
        (DocError::Encrypted, "file_encrypted"),
        (
            DocError::Unsupported("strict_ooxml"),
            "file_type_unsupported",
        ),
        (DocError::Timeout, "doc_timeout"),
        (DocError::CallTimeout, "doc_timeout"),
        (DocError::Cancelled, "cancelled"),
        (DocError::Internal, "internal_error"),
    ] {
        assert_eq!(error.code(), code);
    }
}

#[test]
fn docx_paragraph_ordinals_count_table_paragraphs_and_keep_para_ids() {
    let body = [
        para("", &run("", "one")),
        para("", &run("", "two")),
        table(
            "",
            &[
                vec![
                    cell("", &para("", &run("", "c11"))),
                    cell("", &para("", &run("", "c12"))),
                ],
                vec![
                    cell("", &para("", &run("", "c21"))),
                    cell("", &para("", &run("", "c22"))),
                ],
            ],
        ),
        format!(
            r#"<w:p w14:paraId="1A2B3C4D" w14:textId="77777777">{}</w:p>"#,
            run("", "after")
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "one\ntwo\nc11\nc12\nc21\nc22\nafter");
    assert_eq!(ordinals(&extracted), [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(
        extracted.blocks[6].locator,
        Locator::DocxParagraph {
            part: "word/document.xml".into(),
            ordinal: 7,
            para_id: Some("1A2B3C4D".into()),
        }
    );
    assert!(matches!(
        &extracted.blocks[0].locator,
        Locator::DocxParagraph { para_id: None, .. }
    ));
    assert_eq!(extracted.extractor, docs::ExtractorId::DOCX);
}

#[test]
fn docx_main_part_comes_from_rels() {
    let extracted = read(&Docx::new(&para("", &run("", "from main"))).main_part("word/main.xml"));
    assert_eq!(text(&extracted), "from main");
    assert!(matches!(
        &extracted.blocks[0].locator,
        Locator::DocxParagraph { part, .. } if &**part == "word/main.xml"
    ));
}

#[test]
fn docx_tracked_insertion_in_deletion_out_instr_text_out() {
    let body = r#"<w:p><w:ins w:id="1" w:author="a" w:date="2026-10-24T00:00:00Z"><w:r><w:t>inserted</w:t></w:r></w:ins><w:del w:id="2" w:author="a" w:date="2026-10-24T00:00:00Z"><w:r><w:delText>deleted</w:delText></w:r><w:r><w:t>deletedtoo</w:t></w:r></w:del><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> REF budget </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>result</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let extracted = read_body(body);
    let shown = text(&extracted);
    assert!(
        shown.contains("inserted") && shown.contains("result"),
        "{shown}"
    );
    // Deleted text is out of the accepted view, not hidden in it: no warning.
    assert!(hidden(&extracted).is_empty());
    for absent in ["deleted", "REF"] {
        assert!(!shown.contains(absent), "{shown}");
    }
}

#[test]
fn docx_field_instruction_text_is_excluded() {
    let body = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>HYPERLINK "x"</w:instrText><w:t>notshown</w:t></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>shown</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t> after</w:t></w:r></w:p>"#;
    assert_eq!(text(&read_body(body)), "shown after");
}

#[test]
fn docx_hidden_text_excluded_and_warned() {
    let extracted = read_body(&para(
        "",
        &[run("", "seen "), run("<w:vanish/>", "secret")].concat(),
    ));
    // Hidden text at the paragraph's end leaves a gap too.
    assert_eq!(text(&extracted), "seen \u{FFFC}");
    assert_eq!(hidden(&extracted), [(1, "vanish")]);
}

#[test]
fn docx_white_tiny_and_styled_hidden_runs_are_excluded() {
    let styles = r#"<w:style w:type="character" w:styleId="Hidden"><w:name w:val="Hidden"/><w:rPr><w:vanish/></w:rPr></w:style>"#;
    let body = [
        paragraphs(&[
            (WHITE, "whiterun"),
            (r#"<w:color w:val="F8F8F8"/>"#, "nearwhite"),
            (r#"<w:sz w:val="6"/>"#, "threepoint"),
            (r#"<w:rStyle w:val="Hidden"/>"#, "styledhidden"),
        ]),
        para(
            r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#,
            &run(WHITE, "whiteonblack"),
        ),
        paragraphs(&[
            (r#"<w:color w:val="D0D0D0"/>"#, "lightgrey"),
            (r#"<w:sz w:val="16"/>"#, "eightpoint"),
        ]),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(styles));
    assert_eq!(text(&extracted), "whiteonblack\nlightgrey\neightpoint");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "low_contrast"),
            (2, "low_contrast"),
            (3, "tiny"),
            (4, "vanish")
        ]
    );
}

#[test]
fn docx_table_cell_shading_and_highlight_set_the_background() {
    let styles = table_style("Header", &region("firstRow", "1F3864"));
    let navy = cell(
        r#"<w:shd w:val="clear" w:color="auto" w:fill="1F3864"/>"#,
        &para("", &run(&format!("<w:b/>{WHITE}"), "navywhite")),
    );
    let black_cell = cell(
        r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#,
        &[
            para("", &run(BLACK, "blackonblack")),
            para("", &run("", "nocolour")),
            para("", &run(r#"<w:color w:val="auto"/>"#, "autocolour")),
        ]
        .concat(),
    );
    let header = table(
        r#"<w:tblStyle w:val="Header"/><w:tblLook w:firstRow="1"/>"#,
        &[
            vec![
                cell("", &para("", &run(WHITE, "headerwhite"))),
                cell("", &para("", &run("", "x"))),
            ],
            vec![
                cell("", &para("", &run("", "y"))),
                cell("", &para("", &run("", "z"))),
            ],
        ],
    );
    let body = [
        table("", &[vec![navy, black_cell]]),
        header,
        para(
            "",
            &run(
                &format!(r#"{BLACK}<w:highlight w:val="black"/>"#),
                "highlighted",
            ),
        ),
    ]
    .concat();
    let shown = text(&read(&Docx::new(&body).styles(&styles)));
    for present in ["navywhite", "nocolour", "autocolour", "headerwhite"] {
        assert!(shown.contains(present), "{present} in {shown}");
    }
    for absent in ["blackonblack", "highlighted"] {
        assert!(!shown.contains(absent), "{absent} in {shown}");
    }
}

#[test]
fn docx_vanish_is_a_toggle_and_val_zero_is_visible() {
    let styles = concat!(
        r#"<w:style w:type="paragraph" w:styleId="PHidden"><w:name w:val="PHidden"/><w:rPr><w:vanish/></w:rPr></w:style>"#,
        r#"<w:style w:type="character" w:styleId="CHidden"><w:name w:val="CHidden"/><w:rPr><w:vanish/></w:rPr></w:style>"#,
    );
    let body = [
        para("", &run(r#"<w:vanish w:val="0"/>"#, "valzero")),
        para("", &run(r#"<w:vanish w:val="off"/>"#, "valoff")),
        para(
            r#"<w:pStyle w:val="PHidden"/>"#,
            &run(r#"<w:rStyle w:val="CHidden"/>"#, "cancelled"),
        ),
        para(
            "",
            &run(
                r#"<w:rStyle w:val="CHidden"/><w:vanish w:val="0"/>"#,
                "override",
            ),
        ),
        para("", &run(r#"<w:rStyle w:val="CHidden"/>"#, "charhidden")),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(styles));
    assert_eq!(text(&extracted), "valzero\nvaloff\ncancelled\noverride");
    assert_eq!(hidden(&extracted), [(5, "vanish")]);

    let defaults = format!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:vanish/></w:rPr></w:rPrDefault></w:docDefaults>{styles}"#
    );
    let body = [
        para(
            r#"<w:pStyle w:val="PHidden"/>"#,
            &run(r#"<w:rStyle w:val="CHidden"/>"#, "bothlevels"),
        ),
        para("", &run(r#"<w:vanish w:val="0"/>"#, "ownvalue")),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(&defaults));
    assert_eq!(text(&extracted), "ownvalue");
    assert_eq!(hidden(&extracted), [(1, "vanish")]);
}

#[test]
fn docx_table_style_and_default_styles_apply() {
    let styles = table_style("WhiteText", &format!("<w:rPr>{WHITE}</w:rPr>"));
    let body = [
        table(
            r#"<w:tblStyle w:val="WhiteText"/>"#,
            &[vec![cell("", &para("", &run("", "styledwhite")))]],
        ),
        para("", &run("", "outside")),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(&styles));
    assert_eq!(text(&extracted), "outside");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);

    let styles = concat!(
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:vanish/></w:rPr></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Shown"><w:name w:val="Shown"/></w:style>"#,
    );
    let body = [
        para("", &run("", "defaulthidden")),
        para(r#"<w:pStyle w:val="Shown"/>"#, &run("", "ownstyle")),
        para(r#"<w:pStyle w:val="Missing"/>"#, &run("", "unknownstyle")),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(styles));
    assert_eq!(text(&extracted), "ownstyle");
    assert_eq!(hidden(&extracted), [(1, "vanish"), (3, "vanish")]);
}

#[test]
fn docx_style_chains_stop_at_cycles_and_refuse_past_16() {
    let chain = |length: usize| -> String {
        (0..length)
            .map(|at| {
                let based = if at + 1 < length {
                    format!(r#"<w:basedOn w:val="S{}"/>"#, at + 1)
                } else {
                    String::new()
                };
                let vanish = if at + 1 == length {
                    "<w:rPr><w:vanish/></w:rPr>"
                } else {
                    ""
                };
                format!(
                    r#"<w:style w:type="paragraph" w:styleId="S{at}">{based}{vanish}</w:style>"#
                )
            })
            .collect()
    };
    let body = para(r#"<w:pStyle w:val="S0"/>"#, &run("", "chained"));
    let extracted = read(&Docx::new(&body).styles(&chain(16)));
    assert_eq!(hidden(&extracted), [(1, "vanish")]);
    assert_eq!(
        docs::extract(
            MediaKind::Docx,
            &Docx::new(&body).styles(&chain(17)).build(),
            &limits()
        ),
        Err(DocError::TooComplex("style_chain"))
    );
    let cycle = concat!(
        r#"<w:style w:type="paragraph" w:styleId="A"><w:basedOn w:val="B"/></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="B"><w:basedOn w:val="A"/><w:rPr><w:vanish/></w:rPr></w:style>"#,
    );
    let extracted =
        read(&Docx::new(&para(r#"<w:pStyle w:val="A"/>"#, &run("", "cycle"))).styles(cycle));
    assert_eq!(hidden(&extracted), [(1, "vanish")]);
}

#[test]
fn docx_paragraph_mark_properties_stay_on_the_mark() {
    let extracted = read_body(&para(
        r#"<w:rPr><w:vanish/><w:color w:val="FFFFFF"/></w:rPr>"#,
        &run("", "markkept"),
    ));
    assert_eq!(text(&extracted), "markkept");
    assert!(hidden(&extracted).is_empty());
}

#[test]
fn docx_complex_script_size_uses_szcs() {
    let body = paragraphs(&[
        (r#"<w:cs/><w:szCs w:val="2"/>"#, "ไทยหนึ่ง"),
        (r#"<w:sz w:val="24"/><w:szCs w:val="2"/>"#, "ไทยสอง"),
        (r#"<w:sz w:val="24"/><w:szCs w:val="2"/>"#, "latin"),
        (r#"<w:sz w:val="3pt"/>"#, "unitsize"),
        (r#"<w:rtl/><w:sz w:val="24"/><w:szCs w:val="2"/>"#, "rtlrun"),
        (r#"<w:sz w:val="0.5in"/>"#, "halfinch"),
    ]);
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "latin\nhalfinch");
    assert_eq!(
        hidden(&extracted),
        [(1, "tiny"), (2, "tiny"), (4, "tiny"), (5, "tiny")]
    );
}

#[test]
fn docx_theme_colours_resolve_through_the_scheme_mapping() {
    let background1 = r#"<w:color w:val="000000" w:themeColor="background1"/>"#;
    let mapping = r#"<w:clrSchemeMapping w:bg1="light1" w:t1="dark1" w:bg2="light2" w:t2="dark2" w:accent1="accent1" w:accent2="accent2" w:accent3="accent3" w:accent4="accent4" w:accent5="accent5" w:accent6="accent6" w:hyperlink="hyperlink" w:followedHyperlink="followedHyperlink"/>"#;
    let body = paragraphs(&[(background1, "lightone"), ("", "plain")]);
    let extracted = read(&Docx::new(&body).theme(&theme(&[])).settings(mapping));
    assert_eq!(text(&extracted), "plain");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
    // The mapping is what decides: background1 mapped to dark1 is black on white.
    let swapped = mapping.replace(r#"w:bg1="light1""#, r#"w:bg1="dark1""#);
    let extracted = read(&Docx::new(&body).theme(&theme(&[])).settings(&swapped));
    assert_eq!(text(&extracted), "lightone\nplain");
    // No theme part: both its value (black) and the default theme's lt1 (white) count.
    let extracted = read_body(&body);
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
    let text1 = r#"<w:color w:val="000000" w:themeColor="text1"/>"#;
    let extracted = read_body(&paragraphs(&[(text1, "darkone")]));
    assert_eq!(text(&extracted), "darkone");
}

#[test]
fn docx_theme_missing_a_slot_still_reads() {
    let accent1 = r#"<w:color w:val="FFFFFF" w:themeColor="accent1"/>"#;
    let body = paragraphs(&[(accent1, "accentwhite"), ("", "rest")]);
    let extracted = read(&Docx::new(&body).theme(&theme(&[("accent1", "")])));
    assert_eq!(text(&extracted), "rest");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
    // With the slot in the theme, accent1 is 4472C4 and readable on white.
    let extracted = read(&Docx::new(&body).theme(&theme(&[])));
    assert_eq!(text(&extracted), "accentwhite\nrest");
}

#[test]
fn docx_shading_pattern_sets_the_background() {
    let shaded = |shd: &str, props: &str, marker: &str| para(shd, &run(props, marker));
    let body = [
        shaded(
            r#"<w:shd w:val="solid" w:color="000000" w:fill="FFFFFF"/>"#,
            BLACK,
            "solidblack",
        ),
        shaded(
            r#"<w:shd w:val="clear" w:color="000000" w:fill="FFFFFF"/>"#,
            BLACK,
            "clearblack",
        ),
        shaded(
            r#"<w:shd w:val="clear" w:fill="FFFFFF" w:themeFill="text1"/>"#,
            BLACK,
            "themefill",
        ),
        shaded(
            r#"<w:shd w:val="solid" w:color="auto" w:fill="FFFFFF"/>"#,
            BLACK,
            "autopattern",
        ),
        shaded(
            r#"<w:shd w:val="pct95" w:color="auto"/>"#,
            BLACK,
            "pctninetyfive",
        ),
        shaded(r#"<w:shd w:val="pct10" w:color="auto"/>"#, BLACK, "pctten"),
        shaded(
            r#"<w:shd w:val="horzStripe" w:color="000000" w:fill="FFFFFF"/>"#,
            BLACK,
            "stripeblack",
        ),
        shaded(
            r#"<w:shd w:val="horzStripe" w:color="000000" w:fill="FFFFFF"/>"#,
            WHITE,
            "stripewhite",
        ),
        shaded(r#"<w:shd w:fill="000000"/>"#, WHITE, "noval"),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).theme(&theme(&[])));
    assert_eq!(text(&extracted), "clearblack\npctten\nnoval");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "low_contrast"),
            (3, "low_contrast"),
            (4, "low_contrast"),
            (5, "low_contrast"),
            (7, "low_contrast"),
            (8, "low_contrast")
        ]
    );
}

#[test]
fn docx_tbl_look_both_forms_and_words_default() {
    let styles = table_style(
        "Shaded",
        &[
            region("firstRow", "000000"),
            region("firstCol", "000000"),
            region("lastRow", "000000"),
        ]
        .concat(),
    );
    let row = |first: &str, second: &str| {
        vec![
            cell("", &para("", &run(WHITE, first))),
            cell("", &para("", &run(WHITE, second))),
        ]
    };
    let body = [
        table(
            r#"<w:tblStyle w:val="Shaded"/><w:tblLook w:val="0020"/>"#,
            &[
                row("hexheader", "hexheadertwo"),
                row("hexbody", "hexbodytwo"),
            ],
        ),
        table(
            r#"<w:tblStyle w:val="Shaded"/><w:tblLook w:val="0000"/>"#,
            &[row("offheader", "offheadertwo")],
        ),
        table(
            r#"<w:tblStyle w:val="Shaded"/>"#,
            &[
                row("defheader", "defheadertwo"),
                row("deffirstcol", "defbody"),
                row("deflastfirst", "deflast"),
            ],
        ),
    ]
    .concat();
    let shown = text(&read(&Docx::new(&body).styles(&styles)));
    for present in [
        "hexheader",
        "hexheadertwo",
        "defheader",
        "defheadertwo",
        "deffirstcol",
        "deflastfirst",
    ] {
        assert!(
            shown.split('\n').any(|line| line == present),
            "{present} in {shown}"
        );
    }
    for absent in [
        "hexbody",
        "hexbodytwo",
        "offheader",
        "offheadertwo",
        "defbody",
        "deflast",
    ] {
        assert!(
            !shown.split('\n').any(|line| line == absent),
            "{absent} in {shown}"
        );
    }
}

#[test]
fn docx_banded_rows_and_corner_cells_follow_words_order() {
    let styles = [
        table_style(
            "Corner",
            &[region("firstRow", "000000"), region("nwCell", "FFFFFF")].concat(),
        ),
        table_style(
            "Bands",
            &[
                r#"<w:tblPr><w:tblStyleRowBandSize w:val="1"/></w:tblPr>"#,
                &region("band1Horz", "000000"),
            ]
            .concat(),
        ),
    ]
    .concat();
    let pair = |props: &str, first: &str, second: &str| {
        cell(
            "",
            &[para("", &run(props, first)), para("", &run(WHITE, second))].concat(),
        )
    };
    let body = [
        table(
            r#"<w:tblStyle w:val="Corner"/>"#,
            &[
                vec![
                    pair(BLACK, "cornerblack", "cornerwhite"),
                    pair(BLACK, "toprightblack", "toprightwhite"),
                ],
                vec![
                    pair(BLACK, "bodyblack", "bodywhite"),
                    pair(BLACK, "bodyblack2", "bodywhite2"),
                ],
            ],
        ),
        table(
            r#"<w:tblStyle w:val="Bands"/>"#,
            &[
                vec![pair(BLACK, "headblack", "headwhite")],
                vec![pair(BLACK, "bandoneblack", "bandonewhite")],
                vec![pair(BLACK, "bandtwoblack", "bandtwowhite")],
            ],
        ),
    ]
    .concat();
    let shown = text(&read(&Docx::new(&body).styles(&styles)));
    let lines: Vec<&str> = shown.split('\n').collect();
    for present in [
        "cornerblack",
        "toprightwhite",
        "bodyblack",
        "bandonewhite",
        "bandtwoblack",
        "headblack",
    ] {
        assert!(lines.contains(&present), "{present} in {shown}");
    }
    for absent in [
        "cornerwhite",
        "toprightblack",
        "bodywhite",
        "bandoneblack",
        "bandtwowhite",
        "headwhite",
    ] {
        assert!(!lines.contains(&absent), "{absent} in {shown}");
    }
}

#[test]
fn docx_move_from_is_excluded_move_to_included() {
    let body = concat!(
        r#"<w:p><w:moveFrom w:id="1" w:author="a" w:date="2026-10-24T00:00:00Z"><w:r><w:t>moved</w:t></w:r></w:moveFrom></w:p>"#,
        r#"<w:p><w:r><w:t>middle</w:t></w:r></w:p>"#,
        r#"<w:p><w:moveTo w:id="2" w:author="a" w:date="2026-10-24T00:00:00Z"><w:r><w:t>moved</w:t></w:r></w:moveTo></w:p>"#,
    );
    let extracted = read_body(body);
    assert_eq!(text(&extracted), "middle\nmoved");
    assert_eq!(ordinals(&extracted), [2, 3]);
    assert!(hidden(&extracted).is_empty());
}

#[test]
fn docx_spec_vanish_and_web_hidden_hide() {
    let extracted = read_body(&paragraphs(&[
        ("<w:specVanish/>", "specvanish"),
        ("<w:webHidden/>", "webhidden"),
        ("", "visible"),
    ]));
    assert_eq!(text(&extracted), "visible");
    assert_eq!(hidden(&extracted), [(1, "vanish"), (2, "vanish")]);
}

#[test]
fn docx_unknown_namespace_text_is_hidden() {
    let body = [
        r#"<w:p><x:wrap xmlns:x="urn:example:unknown"><w:r><w:t>wrapped</w:t></w:r></x:wrap></w:p>"#,
        r#"<w:p><mc:AlternateContent xmlns:x="urn:example:unknown"><mc:Choice Requires="x"><w:r><w:t>unknownchoice</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>fallbackread</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>"#,
        r#"<w:p><mc:AlternateContent><mc:Choice Requires="wps"><w:r><w:t>choicewps</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>fallbackold</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>"#,
        // A choice read after a skipped one may not be the one Word reads.
        r#"<w:p><mc:AlternateContent xmlns:x="urn:example:unknown"><mc:Choice Requires="x"><w:r><w:t>firstchoice</w:t></w:r></mc:Choice><mc:Choice Requires="wps"><w:r><w:t>secondchoice</w:t></w:r></mc:Choice></mc:AlternateContent></w:p>"#,
        // A property Word may or may not apply leaves the run undecided.
        r#"<w:p><w:r><w:rPr><x:wrap xmlns:x="urn:example:unknown"><w:vanish w:val="0"/></x:wrap></w:rPr><w:t>unknownprops</w:t></w:r></w:p>"#,
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "choicewps");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "unknown_namespace"),
            (2, "unknown_namespace"),
            (4, "unknown_namespace"),
            (5, "unknown_namespace")
        ]
    );
}

#[test]
fn docx_page_colour_counts_without_the_display_setting() {
    let body = paragraphs(&[(BLACK, "blacktext"), (WHITE, "whitetext"), ("", "nocolour")]);
    let page = r#"<w:background w:color="000000"/>"#;
    let docx = Docx::from_document(&document_with(page, &body));
    for settings in [None, Some(r#"<w:displayBackgroundShape w:val="0"/>"#)] {
        let docx = match settings {
            Some(inner) => docx.clone().settings(inner),
            None => docx.clone(),
        };
        let extracted = read(&docx);
        assert_eq!(text(&extracted), "nocolour");
        assert_eq!(
            hidden(&extracted),
            [(1, "low_contrast"), (2, "low_contrast")]
        );
    }
    let extracted = read(&docx.settings("<w:displayBackgroundShape/>"));
    assert_eq!(text(&extracted), "whitetext\nnocolour");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
}

#[test]
fn docx_theme_colours_of_shading_and_page_colour_resolve() {
    let body = [
        para(
            r#"<w:shd w:val="solid" w:color="000000" w:themeColor="background1"/>"#,
            &run(WHITE, "whiteonlightone"),
        ),
        table(
            "",
            &[vec![cell(
                r#"<w:shd w:val="clear" w:fill="FFFFFF" w:themeFill="background1" w:themeFillShade="00"/>"#,
                &para("", &run(WHITE, "whiteonshade")),
            )]],
        ),
        para(
            r#"<w:shd w:val="solid" w:color="000000" w:themeColor="text1" w:themeTint="00"/>"#,
            &run(BLACK, "blackontint"),
        ),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).theme(&theme(&[])));
    assert_eq!(text(&extracted), "whiteonshade\nblackontint");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);

    let page = r#"<w:background w:themeColor="text1"/>"#;
    let body = paragraphs(&[(WHITE, "whitepage"), (BLACK, "blackpage")]);
    let extracted = read(
        &Docx::from_document(&document_with(page, &body))
            .theme(&theme(&[]))
            .settings("<w:displayBackgroundShape/>"),
    );
    assert_eq!(text(&extracted), "whitepage");
    assert_eq!(hidden(&extracted), [(2, "low_contrast")]);
}

#[test]
fn docx_hidden_only_paragraph_keeps_its_ordinal_and_warns() {
    let extracted = read_body(&paragraphs(&[
        ("", "first"),
        (WHITE, "white"),
        ("", "third"),
    ]));
    assert_eq!(text(&extracted), "first\nthird");
    assert_eq!(ordinals(&extracted), [1, 3]);
    assert_eq!(hidden(&extracted), [(2, "low_contrast")]);
}

/// A floating text box with no fill (what lies beneath it the reader cannot see).
fn text_box(content: &str) -> String {
    shape_box("anchor", "", "", content)
}

/// A text box drawn in a `wp:inline` or `wp:anchor` frame (`frame_props` inside it, before the
/// graphic), with `sp_pr` as its shape properties.
fn shape_box(frame: &str, frame_props: &str, sp_pr: &str, content: &str) -> String {
    format!(
        r#"<w:r><w:drawing><wp:{frame}>{frame_props}<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:spPr>{sp_pr}</wps:spPr><wps:txbx><w:txbxContent>{content}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:{frame}></w:drawing></w:r>"#
    )
}

/// A floating text box filled white, as Word draws a new one.
fn filled_box(content: &str) -> String {
    shape_box(
        "anchor",
        "",
        r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
        content,
    )
}

#[test]
fn docx_textbox_paragraph_is_its_own_block() {
    let body = format!(
        "<w:p>{}{}{}</w:p>",
        run("", "hostbefore"),
        filled_box(&para("", &run("", "boxed"))),
        run("", "hostafter")
    );
    let extracted = read_body(&body);
    // The box stands between its host's runs, which do not join across it.
    assert_eq!(text(&extracted), "hostbefore\u{FFFC}hostafter\nboxed");
    assert_eq!(ordinals(&extracted), [1, 2]);
}

#[test]
fn docx_alternate_content_read_once() {
    let fallback = format!(
        r#"<w:r><w:pict><v:shape><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict></w:r>"#,
        para("", &run("", "boxtext"))
    );
    let body = format!(
        r#"<w:p>{}<mc:AlternateContent><mc:Choice Requires="wps">{}</mc:Choice><mc:Fallback>{fallback}</mc:Fallback></mc:AlternateContent></w:p>{}"#,
        run("", "host"),
        filled_box(&para("", &run("", "boxtext"))),
        para("", &run("", "next"))
    );
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "host\nboxtext\nnext");
    // The fallback's paragraph is not read but still counts.
    assert_eq!(ordinals(&extracted), [1, 2, 4]);
}

#[test]
fn docx_altchunk_and_ole_are_ignored_and_warned() {
    let body = format!(
        r#"<w:altChunk r:id="rIdChunk"/><w:p><w:r><w:object><v:shape><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape><o:OLEObject Type="Embed" ProgID="Excel.Sheet.12" r:id="rIdOle"/></w:object></w:r></w:p>{}"#,
        para("", &run("", "oletext")),
        para("", &run("", "body"))
    );
    let extracted = read(
        &Docx::new(&body)
            .related(
                "aFChunk",
                "chunk.html",
                b"<html><body>chunktext</body></html>",
            )
            .entry("word/embeddings/oleObject1.bin", b"oledata"),
    );
    assert_eq!(text(&extracted), "body");
    // Excluded, not hidden: no warning for the object's text.
    assert_eq!(hidden(&extracted), []);
    let embedded: Vec<_> = extracted
        .warnings
        .iter()
        .filter(|warning| matches!(warning, ExtractWarning::EmbeddedContentIgnored { .. }))
        .collect();
    assert_eq!(
        embedded,
        [
            &ExtractWarning::EmbeddedContentIgnored { what: "alt_chunk" },
            &ExtractWarning::EmbeddedContentIgnored { what: "ole_object" }
        ]
    );
}

#[test]
fn docx_non_w_namespace_prefix_is_read() {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><ns0:document xmlns:ns0="{W}"><ns0:body><ns0:p><ns0:r><ns0:rPr><ns0:vanish/></ns0:rPr><ns0:t>gone</ns0:t></ns0:r><ns0:r><ns0:t>prefixed</ns0:t></ns0:r></ns0:p></ns0:body></ns0:document>"#
    );
    let extracted = read(&Docx::from_document(&document));
    assert_eq!(text(&extracted), "\u{FFFC}prefixed");
    assert_eq!(hidden(&extracted), [(1, "vanish")]);
}

#[test]
fn docx_endnotes_are_read_and_macros_are_warned() {
    let notes = |root: &str, item: &str, marker: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:{root} {NAMESPACES}><w:{item} w:id="1"><w:p><w:r><w:t>{marker}</w:t></w:r></w:p></w:{item}></w:{root}>"#
        )
    };
    let extracted = read(
        &Docx::new(&para("", &run("", "main")))
            .related(
                "endnotes",
                "endnotes.xml",
                notes("endnotes", "endnote", "endnote text").as_bytes(),
            )
            .related(
                "footnotes",
                "footnotes.xml",
                notes("footnotes", "footnote", "footnote text").as_bytes(),
            )
            .related(
                "header",
                "header1.xml",
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {NAMESPACES}>{}</w:hdr>"#,
                    para("", &run("", "header text"))
                )
                .as_bytes(),
            )
            .entry("word/vbaProject.bin", b"macros"),
    );
    let parts: Vec<(&str, &str)> = extracted
        .blocks
        .iter()
        .map(|block| match &block.locator {
            Locator::DocxParagraph { part, .. } => (&**part, block.text.as_str()),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        parts,
        [
            ("word/document.xml", "main"),
            ("word/endnotes.xml", "endnote text"),
            ("word/footnotes.xml", "footnote text"),
            ("word/header1.xml", "header text")
        ]
    );
    let active: Vec<_> = extracted
        .warnings
        .iter()
        .filter(|warning| matches!(warning, ExtractWarning::ActiveContentIgnored { .. }))
        .collect();
    assert_eq!(
        active,
        [&ExtractWarning::ActiveContentIgnored {
            what: "vba_project"
        }]
    );
}

#[test]
fn docx_strict_and_bad_main_parts_are_refused() {
    let body = para("", &run("", "x"));
    let extract = |docx: Docx| docs::extract(MediaKind::Docx, &docx.build(), &limits());
    let office =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    let strict_type = "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
    assert_eq!(
        extract(Docx::new(&body).root_rels(&format!(
            r#"<Relationship Id="rId1" Type="{strict_type}" Target="word/document.xml"/>"#
        ))),
        Err(DocError::Unsupported("strict_ooxml"))
    );
    let strict_document = document(&body).replace(W, STRICT_W);
    assert_eq!(
        extract(Docx::from_document(&strict_document)),
        Err(DocError::Unsupported("strict_ooxml"))
    );
    for rels in [
        String::new(),
        format!(
            r#"<Relationship Id="rId1" Type="{office}" Target="word/document.xml"/><Relationship Id="rId2" Type="{office}" Target="word/document.xml"/>"#
        ),
        format!(
            r#"<Relationship Id="rId1" Type="{office}" Target="https://example.com/document.xml" TargetMode="External"/>"#
        ),
        format!(r#"<Relationship Id="rId1" Type="{office}" Target="word/missing.xml"/>"#),
        format!(r#"<Relationship Id="rId1" Type="{office}" Target="../../word/document.xml"/>"#),
    ] {
        assert_eq!(
            extract(Docx::new(&body).root_rels(&rels)),
            Err(DocError::Malformed("docx_main_part")),
            "{rels}"
        );
    }
    assert_eq!(
        extract(Docx::new(&body).related_missing("header", "header9.xml")),
        Err(DocError::Malformed("docx_part_missing"))
    );
    let not_a_document = Docx::from_document(&format!(
        r#"<?xml version="1.0"?><w:hdr {NAMESPACES}>{body}</w:hdr>"#
    ));
    assert_eq!(
        extract(not_a_document),
        Err(DocError::Malformed("docx_main_part"))
    );
}

#[test]
fn docx_unparseable_values_fail_closed() {
    let body = [
        paragraphs(&[
            (r#"<w:vanish w:val="maybe"/>"#, "maybevanish"),
            (r#"<w:sz w:val="big"/>"#, "bigsize"),
            (r#"<w:color w:val="ZZZZZZ"/>"#, "badcolour"),
            (r#"<w:color w:val="000000"/><w:highlight w:val="glow"/>"#, "badhighlight"),
            (r#"<w:color w:val="000000" w:themeColor="text1" w:themeTint="99" w:themeShade="99"/>"#, "tintandshade"),
        ]),
        "<w:p><w:t>loose</w:t></w:p>".into(),
        para("", &run("", "fine")),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "fine");
    // A `w:t` outside a run is not read, and Word would not read it as the reader does.
    assert_eq!(
        hidden(&extracted),
        [
            (1, "vanish"),
            (2, "tiny"),
            (3, "low_contrast"),
            (4, "low_contrast"),
            (5, "low_contrast"),
            (6, "undecided")
        ]
    );
}

/// A size written with a unit is read in points: in every unit, just under the 4 pt floor is
/// tiny and just over it visible; exactly 4 pt is visible. CSS's `px` is VML's, never a run's.
#[test]
fn docx_sizes_and_offsets_in_every_unit_are_read_exactly() {
    let rows = [
        ("7", true),
        ("8", false),
        ("3.99pt", true),
        ("4pt", false),
        ("1.41mm", true),
        ("1.4112mm", false),
        ("0.141cm", true),
        ("0.14112cm", false),
        ("0.0555in", true),
        ("0.05556in", false),
        ("0.3333pc", true),
        ("0.33334pc", false),
        ("0.3333pi", true),
        ("0.33334pi", false),
        ("6px", true),
    ];
    for (size, tiny) in rows {
        let extracted = read_body(&para("", &run(&format!(r#"<w:sz w:val="{size}"/>"#), "x")));
        if tiny {
            assert_eq!(hidden(&extracted), [(1, "tiny")], "{size}");
        } else {
            assert_eq!(text(&extracted), "x", "{size}");
        }
    }
    // A VML offset of 22 inches, Word's largest page side, is off the page wherever its anchor
    // lands: in pixels at 96 to the inch, and as a bare number, which CSS reads as pixels.
    for (margin, off) in [
        ("2112px", true),
        ("2111px", false),
        ("2112", true),
        ("2111", false),
        ("22in", true),
        ("21.99in", false),
    ] {
        let extracted = read_body(&format!(
            r#"<w:p><w:r><w:pict><v:shape style="position:absolute;margin-left:{margin}" fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"#,
            para("", &run("", "x"))
        ));
        if off {
            assert_eq!(hidden(&extracted), [(2, "off_page")], "{margin}");
        } else {
            assert_eq!(text(&extracted), "x", "{margin}");
        }
    }
}

/// Word's highlight palette ([MS-DOC] 2.9.119 `Ico`, as LibreOffice's DOCX import maps it):
/// text in the palette colour of its highlight is invisible, and black or white text on it,
/// whichever stands out, is visible.
#[test]
fn docx_highlight_palette_is_words() {
    let palette = [
        ("black", "000000"),
        ("blue", "0000FF"),
        ("cyan", "00FFFF"),
        ("darkBlue", "000080"),
        ("darkCyan", "008080"),
        ("darkGray", "808080"),
        ("darkGreen", "008000"),
        ("darkMagenta", "800080"),
        ("darkRed", "800000"),
        ("darkYellow", "808000"),
        ("green", "00FF00"),
        ("lightGray", "C0C0C0"),
        ("magenta", "FF00FF"),
        ("red", "FF0000"),
        ("white", "FFFFFF"),
        ("yellow", "FFFF00"),
    ];
    for (name, hex) in palette {
        let readable = colour::readable_on(colour::parse_hex(hex).unwrap());
        let readable = if readable == Rgb::WHITE {
            "FFFFFF"
        } else {
            "000000"
        };
        let body = paragraphs(&[
            (
                &format!(r#"<w:color w:val="{hex}"/><w:highlight w:val="{name}"/>"#),
                "same",
            ),
            (
                &format!(r#"<w:color w:val="{readable}"/><w:highlight w:val="{name}"/>"#),
                "readable",
            ),
        ]);
        let extracted = read_body(&body);
        assert_eq!(text(&extracted), "readable", "{name}");
        assert_eq!(hidden(&extracted), [(1, "low_contrast")], "{name}");
        // With a floor just above 1, only text of exactly the highlight's colour is hidden: the
        // value is Word's to the last bit (one step of one channel moves the ratio by 2e-5 at
        // the least).
        let exact = DocLimits {
            min_contrast: 1.0 + f32::EPSILON,
            ..limits()
        };
        let extracted = read_with(&Docx::new(&body), &exact).unwrap();
        assert_eq!(hidden(&extracted), [(1, "low_contrast")], "{name} exactly");
        let nudged = format!(
            "{:02X}{}",
            u8::from_str_radix(&hex[..2], 16).unwrap() ^ 8,
            &hex[2..]
        );
        let near = paragraphs(&[(
            &format!(r#"<w:color w:val="{nudged}"/><w:highlight w:val="{name}"/>"#),
            "near",
        )]);
        assert_eq!(
            text(&read_with(&Docx::new(&near), &exact).unwrap()),
            "near",
            "{name}"
        );
    }
}

#[test]
fn docx_w16_namespaces_are_exactly_words() {
    let choice = |uri: &str, marker: &str| {
        format!(
            r#"<w:p><mc:AlternateContent xmlns:z="{uri}"><mc:Choice Requires="z"><w:r><w:t>{marker}choice</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>{marker}fallback</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>"#
        )
    };
    let body = [
        choice("http://schemas.microsoft.com/office/word/2099/wordml", "future"),
        choice("http://schemas.microsoft.com/office/word/2015/wordml/notreal", "notreal"),
        r#"<w:p><z:wrap xmlns:z="http://schemas.microsoft.com/office/word/2099/wordml"><w:r><w:t>wrapped</w:t></w:r></z:wrap></w:p>"#.into(),
        choice("http://schemas.microsoft.com/office/word/2015/wordml/symex", "symex"),
        choice("http://schemas.microsoft.com/office/word/2018/wordml", "wsixteen"),
    ]
    .concat();
    let extracted = read_body(&body);
    // A choice of a namespace no Word knows is hidden, and so is the fallback read for it.
    assert_eq!(text(&extracted), "symexchoice\nwsixteenchoice");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "unknown_namespace"),
            (2, "unknown_namespace"),
            (3, "unknown_namespace")
        ]
    );
}

/// What a choice the reader does not understand would set, Word may apply: the reader reads it as
/// undecided, never dropped and never applied.
#[test]
fn docx_uncertain_choices_leave_what_they_set_undecided() {
    let uncertain = |inner: &str| {
        format!(
            r#"<mc:AlternateContent xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main"><mc:Choice Requires="a14">{inner}</mc:Choice><mc:Fallback/></mc:AlternateContent>"#
        )
    };
    let body = [
        para("", &run(&uncertain("<w:vanish/>"), "vanish")),
        para("", &run(&uncertain(WHITE), "white")),
        para("", &run(&uncertain(r#"<w:sz w:val="2"/>"#), "tiny")),
        // Black on white is visible if applied: undecided all the same.
        para("", &run(&uncertain(BLACK), "black")),
        para(
            &uncertain(r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#),
            &run(BLACK, "shaded"),
        ),
        table(
            "",
            &[vec![cell(
                &uncertain(r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#),
                &para("", &run(BLACK, "cellshaded")),
            )]],
        ),
        format!(
            r#"<w:p><w:r>{}</w:r><w:r><w:t>fieldcode</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>after</w:t></w:r></w:p>"#,
            uncertain(r#"<w:fldChar w:fldCharType="begin"/>"#)
        ),
        // A choice with no `Requires` names nothing the reader understands.
        r#"<w:p><mc:AlternateContent><mc:Choice><w:r><w:t>norequires</w:t></w:r></mc:Choice></mc:AlternateContent></w:p>"#.into(),
        para("", &run("", "plain")),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "\u{FFFC}after\nplain");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "vanish"),
            (2, "low_contrast"),
            (3, "tiny"),
            (4, "unknown_namespace"),
            (5, "low_contrast"),
            (6, "low_contrast"),
            (7, "unknown_namespace"),
            (8, "unknown_namespace")
        ]
    );

    // A page colour in a choice Word may take: the page is undecided.
    let page = uncertain(r#"<w:background w:color="000000"/>"#);
    let docx = Docx::from_document(&document_with(
        &page,
        &paragraphs(&[(BLACK, "blackpage"), ("", "autopage"), (WHITE, "whitepage")]),
    ));
    let extracted = read(&docx);
    assert_eq!(text(&extracted), "autopage");
    assert_eq!(
        hidden(&extracted),
        [(1, "low_contrast"), (3, "low_contrast")]
    );
    // Shown or not, the page is undecided: white text is not read as on black.
    let extracted = read(&docx.settings("<w:displayBackgroundShape/>"));
    assert_eq!(text(&extracted), "autopage");
    assert_eq!(
        hidden(&extracted),
        [(1, "low_contrast"), (3, "low_contrast")]
    );

    // A scheme mapping in such a choice: every mapped theme colour is undecided.
    let mapping = uncertain(r#"<w:clrSchemeMapping w:bg1="dark1"/>"#);
    let themed = r#"<w:color w:val="000000" w:themeColor="text1"/>"#;
    let extracted = read(
        &Docx::new(&paragraphs(&[(themed, "themed"), ("", "rest")]))
            .theme(&theme(&[]))
            .settings(&mapping),
    );
    assert_eq!(text(&extracted), "rest");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);

    // A character style's properties in such a choice: the style is undecided.
    let styles = format!(
        r#"<w:style w:type="character" w:styleId="C"><w:rPr>{}</w:rPr></w:style>"#,
        uncertain(BLACK)
    );
    let extracted = read(
        &Docx::new(&paragraphs(&[
            (r#"<w:rStyle w:val="C"/>"#, "styled"),
            ("", "rest"),
        ]))
        .styles(&styles),
    );
    assert_eq!(text(&extracted), "rest");
    assert_eq!(hidden(&extracted), [(1, "unknown_namespace")]);
}

/// A base style, a band size or a field character inside markup Word drops (an ignorable
/// element, the fallback read for an uncertain choice) is never applied, never dropped: what it
/// touches is undecided.
#[test]
fn docx_based_on_band_sizes_and_field_characters_in_dropped_markup_are_undecided() {
    let defaults = format!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr>{WHITE}</w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:styleId="Black"><w:rPr>{BLACK}</w:rPr></w:style>"#
    );
    let based = |wrapped: &str| {
        format!(r#"{defaults}<w:style w:type="paragraph" w:styleId="S">{wrapped}</w:style>"#)
    };
    let body = para(r#"<w:pStyle w:val="S"/>"#, &run("", "styled"));
    let plain = read(&Docx::new(&body).styles(&based(r#"<w:basedOn w:val="Black"/>"#)));
    assert_eq!(text(&plain), "styled");
    for wrapped in [
        r#"<x:wrap xmlns:x="urn:example:unknown"><w:basedOn w:val="Black"/></x:wrap>"#,
        r#"<mc:AlternateContent xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main"><mc:Choice Requires="a14"/><mc:Fallback><w:basedOn w:val="Black"/></mc:Fallback></mc:AlternateContent>"#,
    ] {
        let extracted = read(&Docx::new(&body).styles(&based(wrapped)));
        assert_eq!(text(&extracted), "", "{wrapped}");
        assert_eq!(hidden(&extracted), [(1, "low_contrast")], "{wrapped}");
    }

    // Row 2 is in the first band only with a band size of 2: white on black there, else white
    // on white.
    let banded = |size: &str| {
        table_style(
            "Bands",
            &format!(
                r#"<w:tblPr>{size}</w:tblPr>{}"#,
                region("band1Horz", "000000")
            ),
        )
    };
    let body = table(
        r#"<w:tblStyle w:val="Bands"/><w:tblLook w:val="0000"/>"#,
        &[
            vec![cell("", &para("", &run(WHITE, "rowone")))],
            vec![cell("", &para("", &run(WHITE, "rowtwo")))],
        ],
    );
    let plain = read(&Docx::new(&body).styles(&banded(r#"<w:tblStyleRowBandSize w:val="2"/>"#)));
    assert_eq!(text(&plain), "rowone\nrowtwo");
    let extracted = read(&Docx::new(&body).styles(&banded(
        r#"<x:wrap xmlns:x="urn:example:unknown"><w:tblStyleRowBandSize w:val="2"/></x:wrap>"#,
    )));
    assert_eq!(text(&extracted), "");
    assert_eq!(
        hidden(&extracted),
        [(1, "unknown_namespace"), (2, "low_contrast")]
    );

    let field = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><x:wrap xmlns:x="urn:example:unknown"><w:fldChar w:fldCharType="separate"/></x:wrap></w:r><w:r><w:t>stillcode</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>after</w:t></w:r></w:p>"#;
    let extracted = read_body(field);
    assert_eq!(text(&extracted), "\u{FFFC}after");
    assert_eq!(hidden(&extracted), [(1, "unknown_namespace")]);
    // The same separate in a branch Word may not take, which leaves no unread markup.
    let extracted = read_body(&field.replace(
        r#"<w:r><x:wrap xmlns:x="urn:example:unknown"><w:fldChar w:fldCharType="separate"/></x:wrap></w:r>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="x"><w:r><w:fldChar w:fldCharType="separate"/></w:r></mc:Choice></mc:AlternateContent>"#,
    ));
    assert_eq!(text(&extracted), "\u{FFFC}after");
    assert_eq!(hidden(&extracted), [(1, "unknown_namespace")]);
}

#[test]
fn docx_scheme_mapping_covers_all_twelve_names() {
    let identity = r#"<w:clrSchemeMapping w:bg1="light1" w:t1="dark1" w:bg2="light2" w:t2="dark2" w:accent1="accent1" w:accent2="accent2" w:accent3="accent3" w:accent4="accent4" w:accent5="accent5" w:accent6="accent6" w:hyperlink="hyperlink" w:followedHyperlink="followedHyperlink"/>"#;
    let body = paragraphs(&[
        (
            r#"<w:color w:val="4472C4" w:themeColor="accent1"/>"#,
            "accentone",
        ),
        (
            r#"<w:color w:val="0563C1" w:themeColor="hyperlink"/>"#,
            "link",
        ),
    ]);
    let docx = Docx::new(&body).theme(&theme(&[]));
    let extracted = read(&docx.clone().settings(identity));
    assert_eq!(text(&extracted), "accentone\nlink");
    // Mapped to light1, white on white.
    let mapped = identity
        .replace(r#"w:accent1="accent1""#, r#"w:accent1="light1""#)
        .replace(r#"w:hyperlink="hyperlink""#, r#"w:hyperlink="light1""#);
    let extracted = read(&docx.clone().settings(&mapped));
    assert_eq!(text(&extracted), "");
    assert_eq!(
        hidden(&extracted),
        [(1, "low_contrast"), (2, "low_contrast")]
    );
    // Accent 1 drawn as dark1 is black on its own blue: readable. Word writes only the
    // identity for the accents, so the slot the name gives directly counts too: blue on blue.
    let on_accent = paragraphs(&[(
        r#"<w:color w:val="000000" w:themeColor="accent1"/><w:shd w:val="clear" w:color="auto" w:fill="4472C4"/>"#,
        "onaccent",
    )]);
    let dark = identity.replace(r#"w:accent1="accent1""#, r#"w:accent1="dark1""#);
    let extracted = read(&Docx::new(&on_accent).theme(&theme(&[])).settings(&dark));
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
    // A mapping value that does not parse leaves the colour undecided.
    let bad = identity.replace(r#"w:accent1="accent1""#, r#"w:accent1="glow""#);
    let extracted = read(&docx.settings(&bad));
    assert_eq!(text(&extracted), "link");
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
}

#[test]
fn docx_theme_reads_only_its_scheme() {
    let real = theme(&[]);
    let decoy_scheme = r#"<a:clrScheme name="decoy"><a:dk1><a:srgbClr val="FFFFFF"/></a:dk1><a:lt1><a:srgbClr val="000000"/></a:lt1></a:clrScheme>"#;
    let wrapped = real.replace(
        "<a:themeElements>",
        &format!(
            r#"<a:themeElements><x:wrap xmlns:x="urn:example:unknown">{decoy_scheme}</x:wrap>"#
        ),
    );
    let in_choice = real.replace(
        "<a:themeElements>",
        &format!(
            r#"<a:themeElements><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:x="urn:example:unknown"><mc:Choice Requires="x">{decoy_scheme}</mc:Choice><mc:Fallback/></mc:AlternateContent>"#
        ),
    );
    let outside = real.replace(
        "<a:themeElements>",
        &format!("<a:objectDefaults>{decoy_scheme}</a:objectDefaults><a:themeElements>"),
    );
    let second = real.replace(
        "</a:themeElements>",
        &format!("{decoy_scheme}</a:themeElements>"),
    );
    let renamed = theme(&[("lt1", r#"<a:light1><a:srgbClr val="000000"/></a:light1>"#)]);
    let foreign = real.replace(
        "<a:clrScheme name=\"Office\">",
        r#"<a:clrScheme name="Office"><x:slot xmlns:x="urn:example:unknown"/>"#,
    );
    let written_twice = real.replace(
        "</a:clrScheme>",
        r#"<a:lt1><a:srgbClr val="000000"/></a:lt1></a:clrScheme>"#,
    );
    let two_colours = theme(&[(
        "lt1",
        r#"<a:lt1><a:srgbClr val="FFFFFF"/><a:srgbClr val="000000"/></a:lt1>"#,
    )]);
    let parse = |xml: &str| ThemeColours::parse(xml.as_bytes(), &limits(), &mut budget()).unwrap();
    assert_eq!(parse(&real).slot(SchemeSlot::Lt1), Some(Rgb::WHITE));
    // A decoy inside an element Word drops is not on the scheme's path.
    assert_eq!(parse(&wrapped).slot(SchemeSlot::Lt1), Some(Rgb::WHITE));
    for (case, xml) in [("in a choice", &in_choice), ("second", &second)] {
        let colours = parse(xml);
        for slot in [SchemeSlot::Dk1, SchemeSlot::Lt1, SchemeSlot::Accent1] {
            assert_eq!(colours.slot(slot), None, "{case}");
        }
    }
    // A scheme outside the path is not the theme's; a mapping name is not a slot name.
    assert_eq!(parse(&outside).slot(SchemeSlot::Lt1), Some(Rgb::WHITE));
    assert_eq!(parse(&renamed).slot(SchemeSlot::Lt1), None);
    // An element of another namespace in the scheme leaves every slot unknown; a slot written
    // twice, or holding two colours, is unknown.
    assert_eq!(parse(&foreign).slot(SchemeSlot::Accent1), None);
    assert_eq!(parse(&written_twice).slot(SchemeSlot::Lt1), None);
    assert_eq!(
        parse(&written_twice).slot(SchemeSlot::Accent1),
        parse(&real).slot(SchemeSlot::Accent1)
    );
    assert_eq!(parse(&two_colours).slot(SchemeSlot::Lt1), None);

    // Read through a document: the decoy's black lt1 would make this run visible.
    let background1 = r#"<w:color w:val="000000" w:themeColor="background1"/>"#;
    for xml in [&real, &wrapped, &in_choice] {
        let extracted = read(&Docx::new(&paragraphs(&[(background1, "lightone")])).theme(xml));
        assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
    }
}

#[test]
fn docx_duplicate_part_relationships_are_refused() {
    let body = para("", &run("", "x"));
    let white_normal = format!(
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr>{WHITE}</w:rPr></w:style>"#
    );
    for docx in [
        Docx::new(&body).styles("").related(
            "styles",
            "styles2.xml",
            styles(&white_normal).as_bytes(),
        ),
        Docx::new(&body).theme(&theme(&[])).related(
            "theme",
            "theme/theme2.xml",
            theme(&[]).as_bytes(),
        ),
        Docx::new(&body)
            .settings("")
            .related("settings", "settings2.xml", settings("").as_bytes()),
        // The same part twice is refused too: two relationships of one kind.
        Docx::new(&body)
            .styles("")
            .related_missing("styles", "styles.xml"),
    ] {
        assert_eq!(
            docs::extract(MediaKind::Docx, &docx.build(), &limits()),
            Err(DocError::Malformed("docx_part_ambiguous"))
        );
    }
}

#[test]
fn docx_left_out_runs_leave_a_gap_between_their_neighbours() {
    let gap = "\u{FFFC}";
    let body = [
        para(
            "",
            &[run("", "Budget: 1"), run("<w:webHidden/>", "2"), run("", "00 baht")].concat(),
        ),
        para(
            "",
            &[run("", "Paid 9"), run("<w:specVanish/>", ",9"), run("", "99 baht")].concat(),
        ),
        para(
            "",
            &[run("", "Do "), run("<w:vanish/>", "NOT "), run(WHITE, "x"), run("", "pay")].concat(),
        ),
        // Text Word draws left out at either end: a figure there may have lost digits.
        para(
            "",
            &[run("<w:webHidden/>", "lead"), run("", "Introduction"), run("<w:webHidden/>", "\t5")]
                .concat(),
        ),
        // Text of an unknown namespace left out inside one run.
        r#"<w:p><w:r><w:t>1</w:t><x:wrap xmlns:x="urn:example:unknown"><w:t>2</w:t></x:wrap><w:t>00</w:t></w:r></w:p>"#.into(),
        // A picture between two runs.
        format!(
            "<w:p>{}<w:r><w:drawing/></w:r>{}</w:p>",
            run("", "1"),
            run("", "250")
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(
        extracted
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        [
            format!("Budget: 1{gap}00 baht"),
            format!("Paid 9{gap}99 baht"),
            format!("Do {gap}pay"),
            format!("{gap}Introduction{gap}"),
            format!("1{gap}00"),
            format!("1{gap}250"),
        ]
    );
    assert_eq!(
        hidden(&extracted),
        [
            (1, "vanish"),
            (2, "vanish"),
            (3, "vanish"),
            (4, "vanish"),
            (5, "unknown_namespace")
        ]
    );
}

#[test]
fn docx_run_characters_are_what_word_draws() {
    let body = [
        r#"<w:p><w:r><w:t>Days 1</w:t><w:noBreakHyphen/><w:t>5 of January</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Line one</w:t><w:cr/><w:t>2026</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Total</w:t><w:ptab w:relativeTo="margin" w:alignment="right" w:leader="none"/><w:t>500</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>docu</w:t><w:softHyphen/><w:t>ment</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Balance </w:t><w:sym w:font="Symbol" w:char="F02D"/><w:t>500</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>A</w:t><w:sym w:font="Wingdings" w:char="F0FC"/><w:t>B</w:t></w:r></w:p>"#,
        // Word's symbol extension follows the `w:sym` rule: the Symbol font's characters are
        // read, any other font's glyph leaves a gap.
        r#"<w:p><w:r><w:t xml:space="preserve">ok </w:t><mc:AlternateContent xmlns:w16se="http://schemas.microsoft.com/office/word/2015/wordml/symex"><mc:Choice Requires="w16se"><w16se:symEx w16se:font="Segoe UI Emoji" w16se:char="1F44D"/></mc:Choice><mc:Fallback><w:t>?</w:t></mc:Fallback></mc:AlternateContent><w:t xml:space="preserve"> done</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Total 1</w:t><mc:AlternateContent xmlns:w16se="http://schemas.microsoft.com/office/word/2015/wordml/symex"><mc:Choice Requires="w16se"><w16se:symEx w16se:font="Wingdings" w16se:char="0032"/></mc:Choice><mc:Fallback/></mc:AlternateContent><w:t>00</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Total 1</w:t><mc:AlternateContent xmlns:w16se="http://schemas.microsoft.com/office/word/2015/wordml/symex"><mc:Choice Requires="w16se"><w16se:symEx w16se:font="Symbol" w16se:char="0032"/></mc:Choice><mc:Fallback/></mc:AlternateContent><w:t>00</w:t></w:r></w:p>"#,
        // The Symbol font check: Wingdings' F02D is no minus sign.
        r#"<w:p><w:r><w:t xml:space="preserve">Balance </w:t><w:sym w:font="Wingdings" w:char="F02D"/><w:t>500</w:t></w:r></w:p>"#,
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(
        extracted
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Days 1-5 of January",
            "Line one\n2026",
            "Total\t500",
            "document",
            "Balance \u{2212}500",
            "A\u{FFFC}B",
            "ok \u{FFFC} done",
            "Total 1\u{FFFC}00",
            "Total 1200",
            "Balance \u{FFFC}500",
        ]
    );
    assert!(hidden(&extracted).is_empty());
}

fn field(instruction: &str, result: &str) -> String {
    format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">{instruction}</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{result}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#
    )
}

#[test]
fn docx_field_results_word_recomputes_or_never_shows_are_hidden() {
    let body = [
        format!("<w:p>{}</w:p>", field(" PAGE ", "9")),
        format!("<w:p>{}</w:p>", field(" NUMPAGES \\* MERGEFORMAT ", "99")),
        format!("<w:p>{}</w:p>", field(r#" SET budget "9,999" "#, "9,999")),
        format!("<w:p>{}</w:p>", field(" date ", "1 January 2020")),
        r#"<w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>7</w:t></w:r></w:fldSimple></w:p>"#.into(),
        format!("<w:p>{}</w:p>", field(" REF budget ", "kept")),
        format!(
            "<w:p>{}</w:p>",
            field(r#" HYPERLINK "https://example.com" "#, "link")
        ),
        format!(
            "<w:p>{}</w:p>",
            field(" REF budget ", "dirty").replace(
                r#"<w:fldChar w:fldCharType="begin"/>"#,
                r#"<w:fldChar w:fldCharType="begin" w:dirty="true"/>"#
            )
        ),
        // A field's result inside another's instruction is never shown.
        format!(
            r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>IF </w:instrText></w:r>{}<w:r><w:instrText> = 1 "a" "b"</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>outer</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
            field(" MERGEFIELD x ", "inner")
        ),
        r#"<w:p><w:sdt><w:sdtPr><w:dataBinding w:xpath="/r[1]/x[1]" w:storeItemID="{11111111-1111-1111-1111-111111111111}"/></w:sdtPr><w:sdtContent><w:r><w:t>bound</w:t></w:r></w:sdtContent></w:sdt></w:p>"#.into(),
        r#"<w:p><w:sdt><w:sdtPr><w:alias w:val="x"/></w:sdtPr><w:sdtContent><w:r><w:t>unbound</w:t></w:r></w:sdtContent></w:sdt></w:p>"#.into(),
        r#"<w:sdt><w:sdtPr><w:dataBinding w:xpath="/r[1]" w:storeItemID="{11111111-1111-1111-1111-111111111111}"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>boundblock</w:t></w:r></w:p></w:sdtContent></w:sdt>"#.into(),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "kept\nlink\nouter\nunbound");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "field_result"),
            (2, "field_result"),
            (3, "field_result"),
            (4, "field_result"),
            (5, "field_result"),
            (8, "field_result"),
            (10, "data_binding"),
            (12, "data_binding")
        ]
    );

    // Word updates every field when the document opens: a hyperlink keeps its text.
    let body = [
        format!("<w:p>{}</w:p>", field(" REF budget ", "stale")),
        format!(
            "<w:p>{}</w:p>",
            field(r#" HYPERLINK "https://example.com" "#, "link")
        ),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).settings("<w:updateFields/>"));
    assert_eq!(text(&extracted), "link");
    assert_eq!(hidden(&extracted), [(1, "field_result")]);

    // A header's fields are recomputed on every page.
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {NAMESPACES}><w:p>{}</w:p><w:p>{}</w:p></w:hdr>"#,
        field(" REF budget ", "headerfield"),
        run("", "headertext")
    );
    let extracted = read(&Docx::new(&para("", &run("", "main"))).related(
        "header",
        "header1.xml",
        header.as_bytes(),
    ));
    assert_eq!(text(&extracted), "main\nheadertext");
    assert_eq!(hidden(&extracted), [(1, "field_result")]);
}

/// A text box's text lies on the box's own fill; where the fill, the shape's visibility or what
/// lies beneath a box with no fill cannot be decided, its text is hidden.
#[test]
fn docx_text_boxes_take_their_shapes_fill_and_visibility() {
    let fill = |hex: &str| format!(r#"<a:solidFill><a:srgbClr val="{hex}"/></a:solidFill>"#);
    let boxed = |frame: &str, frame_props: &str, sp_pr: &str, props: &str, marker: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box(frame, frame_props, sp_pr, &para("", &run(props, marker)))
        )
    };
    let vml = |attributes: &str, props: &str, marker: &str| {
        format!(
            r#"<w:p><w:r><w:pict><v:shape {attributes}><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"#,
            para("", &run(props, marker))
        )
    };
    let body = [
        boxed("anchor", "", &fill("000000"), BLACK, "blackonblackbox"),
        boxed("anchor", "", &fill("000000"), WHITE, "whiteonblackbox"),
        boxed("anchor", r#"<wp:docPr id="1" name="Box" hidden="1"/>"#, &fill("FFFFFF"), "", "hiddenshape"),
        vml(r#"style="position:absolute;visibility:hidden" fillcolor="white""#, "", "vmlhidden"),
        vml(r#"style="position:absolute" fillcolor="black""#, BLACK, "vmlblack"),
        vml(r##"style="position:absolute" fillcolor="#FFFFFF""##, BLACK, "vmlwhite"),
        boxed(
            "inline",
            "",
            r#"<a:gradFill><a:gsLst/></a:gradFill>"#,
            BLACK,
            "gradient",
        ),
        boxed("anchor", "", "<a:noFill/>", "", "floatingnofill"),
        boxed(
            "anchor",
            r#"<wp:positionH relativeFrom="page"><wp:posOffset>30000000</wp:posOffset></wp:positionH><wp:extent cx="914400" cy="914400"/>"#,
            &fill("FFFFFF"),
            "",
            "offpage",
        ),
        boxed(
            "anchor",
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>914400</wp:posOffset></wp:positionH>"#,
            &fill("FFFFFF"),
            "",
            "onpage",
        ),
        boxed("inline", "", &fill("FFFFFF"), "", "inlinewhite"),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(
        text(&extracted),
        "whiteonblackbox\nvmlwhite\nonpage\ninlinewhite"
    );
    assert_eq!(
        hidden(&extracted),
        [
            (2, "low_contrast"),
            (6, "hidden_shape"),
            (8, "hidden_shape"),
            (10, "low_contrast"),
            (14, "unknown_paint"),
            (16, "unknown_paint"),
            (18, "off_page")
        ]
    );

    // VML: a shape takes the fill of the shapetype it is built on (VML's white when that sets
    // none, unknown when no shapetype of that id came before it); a shape in a group is placed in
    // the group's coordinates, never read as a page offset.
    let shapetype = |id: &str, attributes: &str| {
        format!(r#"<w:p><w:r><w:pict><v:shapetype id="{id}" {attributes}/></w:pict></w:r></w:p>"#)
    };
    let body = [
        shapetype("plain", ""),
        shapetype("clear", r#"filled="f""#),
        vml(r##"type="#plain" style="position:absolute""##, BLACK, "typedwhite"),
        vml(r##"type="#clear" style="position:absolute""##, BLACK, "typedclear"),
        vml(r##"type="#missing" style="position:absolute""##, BLACK, "typedmissing"),
        format!(
            r#"<w:p><w:r><w:pict><v:group style="position:absolute;width:100pt;height:100pt" coordsize="21600,21600"><v:rect style="position:absolute;left:21000;top:21000;width:600;height:600" fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:rect></v:group></w:pict></w:r></w:p>"#,
            para("", &run(BLACK, "ingroup"))
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "typedwhite\ningroup");
    assert_eq!(
        hidden(&extracted),
        [(6, "unknown_paint"), (8, "unknown_paint")]
    );

    // A shape whose style colours its text: white text on a white fill.
    let styled = |fill_xml: &str, style: &str, props: &str, marker: &str| {
        format!(
            r#"<w:p><w:r><w:drawing><wp:anchor><a:graphic><a:graphicData><wps:wsp><wps:spPr>{fill_xml}</wps:spPr><wps:style>{style}</wps:style><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#,
            para("", &run(props, marker))
        )
    };
    let font = r#"<a:fillRef idx="0"/><a:fontRef idx="minor"><a:schemeClr val="lt1"/></a:fontRef>"#;
    let body = [
        styled(&fill("FFFFFF"), font, "", "fontwhite"),
        styled(&fill("FFFFFF"), font, BLACK, "ownblack"),
        styled("", r#"<a:fillRef idx="1"><a:schemeClr val="accent1"/></a:fillRef>"#, "", "stylefill"),
        styled(
            r#"<a:solidFill><a:srgbClr val="FFFFFF"><a:alpha val="50000"/></a:srgbClr></a:solidFill>"#,
            "",
            "",
            "transparent",
        ),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).theme(&theme(&[])));
    assert_eq!(text(&extracted), "ownblack");
    assert_eq!(
        hidden(&extracted),
        [
            (2, "low_contrast"),
            (6, "unknown_paint"),
            (8, "unknown_paint")
        ]
    );
}

/// Text in a nested table or an inline text box lies on every container beneath it.
#[test]
fn docx_nested_containers_show_what_lies_beneath() {
    let black = r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#;
    let inner =
        |props: &str, marker: &str| table("", &[vec![cell("", &para("", &run(props, marker)))]]);
    let inline_box = |props: &str, marker: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box("inline", "", "<a:noFill/>", &para("", &run(props, marker)))
        )
    };
    let body = [
        table(
            "",
            &[vec![cell(
                black,
                &[
                    inner(BLACK, "innerblack"),
                    inner(WHITE, "innerwhite"),
                    table("", &[vec![cell("", &inner(BLACK, "deepblack"))]]),
                    inline_box(BLACK, "boxblack"),
                    inline_box(WHITE, "boxwhite"),
                    para("", ""),
                ]
                .concat(),
            )]],
        ),
        format!(
            "<w:p><w:pPr>{black}</w:pPr>{}</w:p>",
            shape_box(
                "inline",
                "",
                "<a:noFill/>",
                &para("", &run(BLACK, "hostshaded"))
            )
        ),
    ]
    .concat();
    let shown = text(&read_body(&body));
    for present in ["innerwhite", "boxwhite"] {
        assert!(
            shown.split('\n').any(|line| line == present),
            "{present} in {shown}"
        );
    }
    for absent in ["innerblack", "deepblack", "boxblack", "hostshaded"] {
        assert!(!shown.contains(absent), "{absent} in {shown}");
    }

    // An outer table style's shading lies under an inner table too.
    let styles = table_style("Dark", &format!("<w:tcPr>{black}</w:tcPr>"));
    let body = table(
        r#"<w:tblStyle w:val="Dark"/>"#,
        &[vec![cell("", &inner(BLACK, "styledouter"))]],
    );
    assert_eq!(text(&read(&Docx::new(&body).styles(&styles))), "");
}

#[test]
fn docx_deleted_rows_and_cells_and_merged_cells_are_not_shown() {
    let deleted_row =
        r#"<w:trPr><w:del w:id="9" w:author="a" w:date="2026-10-24T00:00:00Z"/></w:trPr>"#;
    let row = |props: &str, cells: &str| format!("<w:tr>{props}{cells}</w:tr>");
    let body = format!(
        "<w:tbl><w:tblPr/>{}{}{}</w:tbl>",
        row(deleted_row, &cell("", &para("", &run("", "deletedrow")))),
        row(
            "",
            &[
                cell(
                    r#"<w:cellDel w:id="8" w:author="a" w:date="2026-10-24T00:00:00Z"/>"#,
                    &para("", &run("", "deletedcell"))
                ),
                cell("", &para("", &run("", "kept"))),
            ]
            .concat()
        ),
        row(
            "",
            &[
                cell("<w:vMerge/>", &para("", &run("", "merged"))),
                cell("", &para("", &run("", "kept2")))
            ]
            .concat()
        ),
    );
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "kept\nkept2");
    // Deleted content still counts toward the ordinals; merged-away content is hidden.
    assert_eq!(ordinals(&extracted), [3, 5]);
    assert_eq!(hidden(&extracted), [(4, "merged_cell")]);

    // The accepted view has no deleted row: the row after it is the first body row.
    let styles = table_style("Bands", &region("band1Horz", "000000"));
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="Bands"/><w:tblLook w:firstRow="1"/></w:tblPr>{}{}{}{}</w:tbl>"#,
        row("", &cell("", &para("", &run("", "head")))),
        row(deleted_row, &cell("", &para("", &run("", "gone")))),
        row("", &cell("", &para("", &run(BLACK, "bandoneblack")))),
        row("", &cell("", &para("", &run(BLACK, "bandtwoblack")))),
    );
    let shown = text(&read(&Docx::new(&body).styles(&styles)));
    assert_eq!(shown, "head\nbandtwoblack");
}

/// Markup Word refuses or drops is never read in the direction that shows text.
#[test]
fn docx_markup_word_would_not_read_fails_closed() {
    let body = [
        // Content inside mc:AlternateContent but outside its branches.
        r#"<w:p><mc:AlternateContent><w:r><w:t>outside</w:t></w:r><mc:Choice Requires="wps"><w:r><w:t>choice</w:t></w:r></mc:Choice></mc:AlternateContent></w:p>"#.into(),
        // A run inside an element WordprocessingML does not define.
        r#"<w:p><w:notAnElement><w:r><w:t>unknownw</w:t></w:r></w:notAnElement></w:p>"#.into(),
        // A paragraph there.
        r#"<w:notAnElement><w:p><w:r><w:t>unknownpara</w:t></w:r></w:p></w:notAnElement>"#.into(),
        // A property written twice is read both ways: the first value hides it. A second w:rPr
        // is undecided.
        para("", &run(r#"<w:vanish/><w:vanish w:val="0"/>"#, "twovanish")),
        r#"<w:p><w:r><w:rPr/><w:rPr><w:b/></w:rPr><w:t>secondrpr</w:t></w:r></w:p>"#.into(),
        r#"<w:p><w:pPr/><w:pPr/><w:r><w:t>secondppr</w:t></w:r></w:p>"#.into(),
        // Text not directly in its run.
        r#"<w:p><w:r><w:pict><w:t>stray</w:t></w:pict><w:t>kept</w:t></w:r></w:p>"#.into(),
        para("", &run("", "plain")),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "\u{FFFC}choice\n\u{FFFC}kept\nplain");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "unknown_namespace"),
            (2, "undecided"),
            (3, "undecided"),
            (4, "vanish"),
            (5, "undecided"),
            (6, "undecided"),
            (7, "undecided")
        ]
    );
    for (case, body) in [
        (
            "a fallback before a choice",
            r#"<w:p><mc:AlternateContent><mc:Fallback><w:r><w:t>fallback</w:t></w:r></mc:Fallback><mc:Choice Requires="wps"/></mc:AlternateContent></w:p>"#,
        ),
        (
            "two fallbacks",
            r#"<w:p><mc:AlternateContent><mc:Fallback/><mc:Fallback/></mc:AlternateContent></w:p>"#,
        ),
    ] {
        assert_eq!(
            docs::extract(MediaKind::Docx, &Docx::new(body).build(), &limits()),
            Err(DocError::Malformed("docx_alternate_content")),
            "{case}"
        );
    }
    assert_eq!(
        docs::extract(
            MediaKind::Docx,
            &Docx::new(r#"<w:p><w:r><w:t>x</w:t></w:r><w:pPr/></w:p>"#).build(),
            &limits()
        ),
        Err(DocError::Malformed("docx_structure"))
    );
}

#[test]
fn docx_hidden_content_names_its_part() {
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {NAMESPACES}>{}</w:hdr>"#,
        para("", &run("<w:vanish/>", "headerhidden"))
    );
    let extracted = read(&Docx::new(&para("", &run(WHITE, "bodyhidden"))).related(
        "header",
        "header1.xml",
        header.as_bytes(),
    ));
    let parts: Vec<(String, u32, &str)> = extracted
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            ExtractWarning::HiddenContent {
                at: Locator::DocxParagraph { part, ordinal, .. },
                why,
            } => Some((part.to_string(), *ordinal, *why)),
            _ => None,
        })
        .collect();
    assert_eq!(
        parts,
        [
            ("word/document.xml".to_owned(), 1, "low_contrast"),
            ("word/header1.xml".to_owned(), 1, "vanish")
        ]
    );
}

/// A style id defined twice, or a property written twice in one place: Word is seen to take the
/// last definition whole and the standard keeps the first's id, so each reading is judged and
/// the text is hidden only when one hides it. Real documents do both.
#[test]
fn docx_duplicate_styles_and_properties_are_read_both_ways() {
    let heading = |colour: &str, size: &str| {
        format!(
            r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:rPr><w:color w:val="{colour}"/><w:sz w:val="{size}"/></w:rPr></w:style>"#
        )
    };
    let normal = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:sz w:val="22"/></w:rPr></w:style>"#;
    let table_normal = r#"<w:style w:type="table" w:default="1" w:styleId="TableNormal"/>"#;
    let styled =
        |id: &str, marker: &str| para(&format!(r#"<w:pStyle w:val="{id}"/>"#), &run("", marker));
    type Case = (String, String, &'static str, Vec<(u32, &'static str)>);
    let cases: Vec<Case> = vec![
        // Two visible definitions: the heading stays.
        (
            [heading("2E74B5", "32"), heading("2E5090", "36")].concat(),
            styled("Heading1", "heading"),
            "heading",
            vec![],
        ),
        // Normal and TableNormal written twice, the same both times.
        (
            [normal, normal, table_normal, table_normal].concat(),
            [
                para("", &run("", "body")),
                table("", &[vec![cell("", &para("", &run("", "incell")))]]),
            ]
            .concat(),
            "body\nincell",
            vec![],
        ),
        // One definition hides, whichever comes first.
        (
            [
                heading("2E74B5", "32"),
                r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:rPr><w:vanish/></w:rPr></w:style>"#.into(),
            ]
            .concat(),
            styled("Heading1", "lastvanish"),
            "",
            vec![(1, "vanish")],
        ),
        (
            [
                r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:rPr><w:vanish/></w:rPr></w:style>"#.into(),
                heading("2E74B5", "32"),
            ]
            .concat(),
            styled("Heading1", "firstvanish"),
            "",
            vec![(1, "vanish")],
        ),
        // A paragraph style that hides, then a table style with its id.
        (
            concat!(
                r#"<w:style w:type="paragraph" w:styleId="X"><w:rPr><w:vanish/></w:rPr></w:style>"#,
                r#"<w:style w:type="table" w:styleId="X"/>"#
            )
            .into(),
            styled("X", "crosskinds"),
            "",
            vec![(1, "vanish")],
        ),
        // Two default paragraph styles, one hiding.
        (
            concat!(
                r#"<w:style w:type="paragraph" w:default="1" w:styleId="A"><w:rPr><w:vanish/></w:rPr></w:style>"#,
                r#"<w:style w:type="paragraph" w:default="1" w:styleId="B"/>"#
            )
            .into(),
            para("", &run("", "twodefaults")),
            "",
            vec![(1, "vanish")],
        ),
        // A size written twice in a style, Word's own (Mac Word 14 writes it): both visible.
        (
            r#"<w:style w:type="paragraph" w:styleId="Title"><w:rPr><w:szCs w:val="52"/><w:szCs w:val="36"/></w:rPr></w:style>"#.into(),
            para(r#"<w:pStyle w:val="Title"/>"#, &run("<w:cs/>", "title")),
            "title",
            vec![],
        ),
        // A base written twice, the first hiding.
        (
            concat!(
                r#"<w:style w:type="paragraph" w:styleId="Hide"><w:rPr><w:vanish/></w:rPr></w:style>"#,
                r#"<w:style w:type="paragraph" w:styleId="Show"/>"#,
                r#"<w:style w:type="paragraph" w:styleId="Two"><w:basedOn w:val="Hide"/><w:basedOn w:val="Show"/></w:style>"#
            )
            .into(),
            styled("Two", "twobases"),
            "",
            vec![(1, "vanish")],
        ),
    ];
    for (styles, body, shown, warned) in cases {
        let extracted = read(&Docx::new(&body).styles(&styles));
        assert_eq!(text(&extracted), shown, "{styles}");
        assert_eq!(hidden(&extracted), warned, "{styles}");
    }

    // Properties written twice in one place: the same value twice is one; two visible values
    // are visible; a hiding value hides.
    let body = [
        para("", &run(r#"<w:szCs w:val="24"/><w:szCs w:val="24"/>"#, "samesize")),
        para("", &run(r#"<w:sz w:val="24"/><w:sz w:val="28"/>"#, "twosizes")),
        para("", &run(r#"<w:sz w:val="24"/><w:sz w:val="2"/>"#, "onetiny")),
        para("", &run(r#"<w:color w:val="000000"/><w:color w:val="FFFFFF"/>"#, "onewhite")),
        para("", &run(r#"<w:vanish w:val="0"/><w:vanish/>"#, "vanishlast")),
        para(
            r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/><w:shd w:val="clear" w:color="auto" w:fill="FFFFFF"/>"#,
            &run(BLACK, "twoshadings"),
        ),
        table(
            "",
            &[vec![cell(
                r#"<w:shd w:val="clear" w:color="auto" w:fill="FFFFFF"/><w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#,
                &para("", &run(BLACK, "celltwice")),
            )]],
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "samesize\ntwosizes");
    assert_eq!(
        hidden(&extracted),
        [
            (3, "tiny"),
            (4, "low_contrast"),
            (5, "vanish"),
            (6, "low_contrast"),
            (7, "low_contrast")
        ]
    );

    // A table's style and look written twice, in its properties and in a row's exceptions.
    let styles = [
        table_style("Plain", ""),
        table_style("Dark", &region("wholeTable", "000000")),
        table_style("Header", &region("firstRow", "000000")),
    ]
    .concat();
    let body = [
        table(
            r#"<w:tblStyle w:val="Dark"/><w:tblStyle w:val="Plain"/>"#,
            &[vec![cell("", &para("", &run(BLACK, "twostyles")))]],
        ),
        table(
            r#"<w:tblStyle w:val="Header"/><w:tblLook w:firstRow="0"/><w:tblLook w:firstRow="1"/>"#,
            &[vec![cell("", &para("", &run(BLACK, "twolooks")))]],
        ),
        format!(
            r#"<w:tbl><w:tblPr><w:tblStyle w:val="Header"/><w:tblLook w:firstRow="0"/></w:tblPr><w:tr><w:tblPrEx><w:tblLook w:firstRow="1"/><w:tblLook w:firstRow="0"/></w:tblPrEx>{}</w:tr></w:tbl>"#,
            cell("", &para("", &run(BLACK, "rowlooks")))
        ),
        // The hiding look first.
        table(
            r#"<w:tblStyle w:val="Header"/><w:tblLook w:firstRow="1"/><w:tblLook w:firstRow="0"/>"#,
            &[vec![cell("", &para("", &run(BLACK, "firstlook")))]],
        ),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(&styles));
    assert_eq!(text(&extracted), "");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "low_contrast"),
            (2, "low_contrast"),
            (3, "low_contrast"),
            (4, "low_contrast")
        ]
    );

    // A container written twice is still undecided: two w:rPr in w:rPrDefault, two w:pPr in
    // w:pPrDefault.
    for defaults in [
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:vanish/></w:rPr><w:rPr><w:vanish w:val="0"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        r#"<w:docDefaults><w:pPrDefault><w:pPr><w:shd w:val="clear" w:color="auto" w:fill="FFFFFF"/></w:pPr><w:pPr><w:shd w:val="clear" w:color="auto" w:fill="000000"/></w:pPr></w:pPrDefault></w:docDefaults>"#,
    ] {
        let extracted = read(&Docx::new(&para("", &run("", "defaults"))).styles(defaults));
        assert_eq!(text(&extracted), "", "{defaults}");
        assert_eq!(hidden(&extracted), [(1, "undecided")], "{defaults}");
    }
}

/// A table style band size that is missing or 0 is read both ways (no banding, and bands of
/// one row or column), and the text is hidden when either hides it.
#[test]
fn docx_band_sizes_missing_or_zero_are_read_both_ways() {
    let banded = |size: &str| {
        table_style(
            "Bands",
            &[
                format!("<w:tblPr>{size}</w:tblPr>"),
                region("band1Horz", "000000"),
                region("band1Vert", "000000"),
            ]
            .concat(),
        )
    };
    let rows = [
        vec![cell("", &para("", &run(WHITE, "white")))],
        vec![cell("", &para("", &run(BLACK, "black")))],
    ];
    let row_table = table(
        r#"<w:tblStyle w:val="Bands"/><w:tblLook w:firstRow="0" w:firstColumn="0" w:noVBand="1"/>"#,
        &rows,
    );
    let column_table = table(
        r#"<w:tblStyle w:val="Bands"/><w:tblLook w:firstRow="0" w:firstColumn="0" w:noHBand="1"/>"#,
        &[vec![
            cell("", &para("", &run(WHITE, "white"))),
            cell("", &para("", &run(BLACK, "black"))),
        ]],
    );
    for (sizes, shown, warned) in [
        // Bands of one: white on the first band's black, black on the second band's white.
        (
            r#"<w:tblStyleRowBandSize w:val="1"/><w:tblStyleColBandSize w:val="1"/>"#,
            "white\nblack\nwhite\nblack",
            vec![],
        ),
        // Missing, or 0: with no banding the first band's white text lies on the page.
        (
            "",
            "black\nblack",
            vec![(1, "low_contrast"), (3, "low_contrast")],
        ),
        (
            r#"<w:tblStyleRowBandSize w:val="0"/><w:tblStyleColBandSize w:val="0"/>"#,
            "black\nblack",
            vec![(1, "low_contrast"), (3, "low_contrast")],
        ),
    ] {
        let extracted = read(
            &Docx::new(&[row_table.clone(), column_table.clone()].concat()).styles(&banded(sizes)),
        );
        assert_eq!(text(&extracted), shown, "{sizes}");
        assert_eq!(hidden(&extracted), warned, "{sizes}");
    }
    // A band size written twice is read both ways as well.
    let extracted = read(&Docx::new(&row_table).styles(&banded(
        r#"<w:tblStyleRowBandSize w:val="1"/><w:tblStyleRowBandSize w:val="2"/>"#,
    )));
    assert_eq!(text(&extracted), "white");
    assert_eq!(hidden(&extracted), [(2, "low_contrast")]);
}

/// Word lays out no object anchored in hidden text, inline or floating: a text box whose drawing
/// sits in a hidden run is hidden with it.
#[test]
fn docx_text_boxes_in_hidden_runs_are_hidden() {
    let white_fill = r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#;
    // A `w:drawing` text box, and a VML one in `w:pict`.
    let drawing = |frame: &str, marker: &str| {
        let boxed = shape_box(frame, "", white_fill, &para("", &run("", marker)));
        boxed["<w:r>".len()..boxed.len() - "</w:r>".len()].to_owned()
    };
    let pict = |style: &str, marker: &str| {
        format!(
            r#"<w:pict><v:shape style="{style}" fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict>"#,
            para("", &run("", marker))
        )
    };
    let in_run =
        |run_props: &str, object: String| format!("<w:r><w:rPr>{run_props}</w:rPr>{object}</w:r>");
    let styles = concat!(
        r#"<w:style w:type="character" w:styleId="Gone"><w:rPr><w:vanish/></w:rPr></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="GonePara"><w:rPr><w:vanish/></w:rPr></w:style>"#
    );
    // As Word writes a text box: a `wps` choice and a VML fallback.
    let word_shape = |marker: &str| {
        format!(
            r#"<mc:AlternateContent><mc:Choice Requires="wps">{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent>"#,
            drawing("anchor", marker),
            pict("position:absolute", marker)
        )
    };
    let body = [
        format!(
            "<w:p>{}</w:p>",
            in_run("<w:vanish/>", drawing("inline", "inlinehidden"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run(
                r#"<w:rStyle w:val="Gone"/>"#,
                drawing("inline", "stylehidden")
            )
        ),
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="GonePara"/></w:pPr>{}</w:p>"#,
            in_run("", drawing("inline", "parastylehidden"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run("<w:vanish/>", drawing("anchor", "anchorhidden"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run("<w:vanish/>", pict("position:absolute", "vmlfloating"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run("<w:vanish/>", pict("", "vmlinline"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run("<w:vanish/>", word_shape("wordshape"))
        ),
        // Controls: a visible host, a vanish turned off, and a host run too small to read (the
        // drawing's size is its own).
        format!(
            "<w:p>{}</w:p>",
            in_run("", drawing("inline", "visiblehost"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run(r#"<w:vanish w:val="0"/>"#, drawing("inline", "vanishoff"))
        ),
        format!(
            "<w:p>{}</w:p>",
            in_run(r#"<w:sz w:val="2"/>"#, drawing("inline", "tinyhost"))
        ),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(styles));
    assert_eq!(text(&extracted), "visiblehost\nvanishoff\ntinyhost");
    let warned: Vec<&str> = hidden(&extracted).into_iter().map(|(_, why)| why).collect();
    assert_eq!(warned, ["vanish"; 7]);

    // A host run whose properties are written twice leaves its text box undecided.
    let extracted = read_body(&format!(
        "<w:p><w:r><w:rPr/><w:rPr/>{}</w:r></w:p>",
        drawing("inline", "undecidedhost")
    ));
    assert_eq!(text(&extracted), "");
    assert_eq!(hidden(&extracted), [(2, "undecided")]);

    // A run's properties after its content are refused, as a paragraph's are.
    for body in [
        format!(
            r#"<w:p><w:r>{}<w:rPr><w:vanish/></w:rPr></w:r></w:p>"#,
            drawing("inline", "late")
        ),
        r#"<w:p><w:r><w:t>late</w:t><w:rPr><w:vanish/></w:rPr></w:r></w:p>"#.into(),
    ] {
        assert_eq!(
            docs::extract(MediaKind::Docx, &Docx::new(&body).build(), &limits()),
            Err(DocError::Malformed("docx_structure"))
        );
    }
}

/// A field's type is the first word of its instruction, read past leading white space. A type
/// that cannot be read (none, too long, or written by a nested field) leaves the result
/// undecided; types Word computes, never shows or draws from their code are hidden.
#[test]
fn docx_field_types_are_read_past_white_space() {
    let spaces = " ".repeat(70);
    let one_space_runs = format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>{}<w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        r#"<w:r><w:instrText xml:space="preserve"> </w:instrText></w:r>"#.repeat(70)
    );
    let nested_type = format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>{}<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        field(r#" QUOTE "PAGE" "#, "PAGE")
    );
    // The type's word begun, then finished by a nested field's result: `PA` + `GE`.
    let finished_by_nested = format!(
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PA</w:instrText></w:r>{}<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        field(r#" QUOTE "GE" "#, "GE")
    );
    // ... or by a `w:fldSimple`'s.
    let finished_by_simple = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PA</w:instrText></w:r><w:fldSimple w:instr=" QUOTE GE "><w:r><w:t>GE</w:t></w:r></w:fldSimple><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#.to_owned();
    let rows: Vec<(String, Option<&str>)> = vec![
        (field(&format!("{spaces}PAGE "), "9"), Some("field_result")),
        (
            field(&format!("{spaces}SET x 1 "), "9"),
            Some("field_result"),
        ),
        (
            field(&format!("\t\t\n{spaces}PAGE"), "9"),
            Some("field_result"),
        ),
        (
            field(&format!("{}PAGE", " ".repeat(60)), "9"),
            Some("field_result"),
        ),
        (
            field(&format!("{}PAGE", " ".repeat(61)), "9"),
            Some("field_result"),
        ),
        (one_space_runs, Some("field_result")),
        (
            format!(r#"<w:fldSimple w:instr="{spaces}PAGE"><w:r><w:t>9</w:t></w:r></w:fldSimple>"#),
            Some("field_result"),
        ),
        (nested_type, Some("undecided")),
        (finished_by_nested, Some("undecided")),
        (finished_by_simple, Some("undecided")),
        (field("", "9"), Some("undecided")),
        (field(&"X".repeat(65), "9"), Some("undecided")),
        (field(r#"\* MERGEFORMAT"#, "9"), Some("undecided")),
        (field(" REF budget ", "9"), None),
        (field(" FOO ", "9"), None),
        (field(&format!("{spaces}hyperlink \"x\""), "9"), None),
    ];
    for (body, why) in rows {
        let extracted = read_body(&format!("<w:p>{body}</w:p>"));
        match why {
            Some(why) => {
                assert_eq!(text(&extracted), "", "{body}");
                assert_eq!(hidden(&extracted), [(1, why)], "{body}");
            }
            None => assert_eq!(text(&extracted), "9", "{body}"),
        }
    }
    // Every type Word never shows, computes, or draws from its code, in either field form.
    for kind in [
        "ASK",
        "SET",
        "TC",
        "XE",
        "TA",
        "RD",
        "PRIVATE",
        "PAGE",
        "NUMPAGES",
        "SECTIONPAGES",
        "SECTION",
        "PAGEREF",
        "STYLEREF",
        "DATE",
        "TIME",
        "AUTONUM",
        "AUTONUMLGL",
        "AUTONUMOUT",
        "LISTNUM",
        "EQ",
        "MACROBUTTON",
        "GOTOBUTTON",
        "SYMBOL",
        "FORMDROPDOWN",
        "FORMCHECKBOX",
        "DISPLAYBARCODE",
        "MERGEBARCODE",
        "BARCODE",
    ] {
        for body in [
            field(&format!(" {kind} x "), "9"),
            format!(r#"<w:fldSimple w:instr=" {kind} x "><w:r><w:t>9</w:t></w:r></w:fldSimple>"#),
        ] {
            let extracted = read_body(&format!("<w:p>{body}</w:p>"));
            assert_eq!(text(&extracted), "", "{kind}");
            assert_eq!(hidden(&extracted), [(1, "field_result")], "{kind}");
        }
    }
}

/// A field stored with no result, where Word draws one, leaves a gap as a dropped run does: its
/// neighbours never join into a figure. A field Word never shows draws nothing there.
#[test]
fn docx_fields_with_no_result_leave_a_gap() {
    let bare = |instruction: &str| {
        format!(
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">{instruction}</w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#
        )
    };
    let around = |field: &str| {
        format!(
            "<w:p>{}{field}{}</w:p>",
            run("", "Budget: 1"),
            run("", "00 baht")
        )
    };
    let body = [
        around(&bare(" PAGE ")),
        around(r#"<w:fldSimple w:instr=" PAGE "/>"#),
        around(&bare(" PAGEREF x ")),
        around(&bare(" TITLE ")),
        around(&bare(" DATE ")),
        around(&bare(" REF x ")),
        around(
            &bare(" PAGE ").replace(
                r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t></w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            ),
        ),
        // A field Word never shows whose begin sits where Word's choice cannot be decided.
        around(&bare(r#" SET x "1" "#).replacen(
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
            r#"<mc:AlternateContent><mc:Choice Requires="x"><w:r><w:fldChar w:fldCharType="begin"/></w:r></mc:Choice></mc:AlternateContent>"#,
            1,
        )),
        // Never shown: nothing is drawn there.
        around(&bare(r#" SET x "1" "#)),
        around(&bare(r#" XE "budget" "#)),
    ]
    .concat();
    let gap = "Budget: 1\u{FFFC}00 baht";
    assert_eq!(
        read_body(&body)
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        [
            gap,
            gap,
            gap,
            gap,
            gap,
            gap,
            gap,
            gap,
            "Budget: 100 baht",
            "Budget: 100 baht"
        ]
    );
    // In a footer, every field is recomputed on every page.
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:ftr {NAMESPACES}><w:p>{}{}{}</w:p></w:ftr>"#,
        run("", "Total 1"),
        bare(" REF x "),
        run("", "0 pages")
    );
    let extracted = read(&Docx::new(&para("", &run("", "main"))).related(
        "footer",
        "footer1.xml",
        footer.as_bytes(),
    ));
    assert_eq!(text(&extracted), "main\nTotal 1\u{FFFC}0 pages");
}

/// A text box is its own story and a `w:fldSimple` its own field: field characters inside one
/// never end, start the result of, or name a field begun outside it, and a field begun inside
/// one never runs past it.
#[test]
fn docx_field_characters_stay_in_their_story_and_field() {
    let begin = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#;
    let separate = r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#;
    let end = r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
    let instr = |text: &str| {
        format!(r#"<w:r><w:instrText xml:space="preserve">{text}</w:instrText></w:r>"#)
    };
    let in_box = |content: &str| {
        shape_box(
            "inline",
            "",
            r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
            &format!("<w:p>{content}</w:p>"),
        )
    };
    let around = |inner: &str| {
        format!(
            "<w:p>{}{inner}{}</w:p>",
            run("", "Budget: 1"),
            run("", "00 baht")
        )
    };
    let page = |inside: &str| {
        format!(
            "{begin}{}{separate}{inside}{}{end}",
            instr(" PAGE "),
            run("", "2")
        )
    };
    let body = [
        // A field begun in a `w:fldSimple` and not ended there: the next end is its own.
        around(&page(&format!(
            r#"<w:fldSimple w:instr=" REF x ">{begin}</w:fldSimple>{end}"#
        ))),
        // An end in a box does not end the field around the box.
        around(&page(&in_box(end))),
        // A field begun in a box ends with the box.
        around(&format!(
            "{}{}",
            in_box(&format!(
                "{begin}{}{separate}{}",
                instr(" PAGE "),
                run("", "9")
            )),
            run("", "2")
        )),
        // A separate in a box does not start the result of the field around the box, and an
        // instruction in a box does not name it.
        around(&format!(
            "{begin}{}{}{}{end}",
            instr(" REF x "),
            in_box(separate),
            run("", "2")
        )),
        around(&format!(
            "{begin}{}{separate}{}{end}",
            in_box(&instr(" REF x ")),
            run("", "2")
        )),
        // An end in a `w:fldSimple` does not end it.
        around(&format!(
            r#"<w:fldSimple w:instr=" REF x ">{end}{}</w:fldSimple>"#,
            run("", "2")
        )),
    ]
    .concat();
    let extracted = read_body(&body);
    let blocks: Vec<&str> = extracted
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(
        blocks,
        [
            "Budget: 1\u{FFFC}00 baht",
            "Budget: 1\u{FFFC}00 baht",
            "Budget: 1\u{FFFC}200 baht",
            "Budget: 1\u{FFFC}00 baht",
            "Budget: 1\u{FFFC}00 baht",
            "Budget: 1\u{FFFC}00 baht"
        ]
    );
    // A field left open past its `w:fldSimple`, or ended or separated where it cannot be, is
    // undecided; a box's own field hides its own result and nothing after the box.
    assert_eq!(
        hidden(&extracted),
        [
            (1, "unknown_namespace"),
            (2, "field_result"),
            (5, "field_result"),
            (6, "unknown_namespace"),
            (8, "undecided"),
            (10, "unknown_namespace")
        ]
    );
}

/// `w15:dataBinding` binds a control as `w:dataBinding` does: Word refills it from the custom
/// XML part, so its cached text is hidden. Properties after the control's content are refused.
#[test]
fn docx_word_2013_data_bindings_hide_cached_text() {
    let w15 = "http://schemas.microsoft.com/office/word/2012/wordml";
    let binding = format!(
        r#"<w15:dataBinding xmlns:w15="{w15}" w:xpath="/r[1]/x[1]" w:storeItemID="{{11111111-1111-1111-1111-111111111111}}"/>"#
    );
    let body = [
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">lead </w:t></w:r><w:sdt><w:sdtPr>{binding}</w:sdtPr><w:sdtContent><w:r><w:t>cached</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
        ),
        format!(
            r#"<w:sdt><w:sdtPr>{binding}</w:sdtPr><w:sdtContent><w:p><w:r><w:t>cachedblock</w:t></w:r></w:p></w:sdtContent></w:sdt>"#
        ),
        format!(
            r#"<w:p><w:sdt><w:sdtPr><mc:AlternateContent xmlns:w15="{w15}"><mc:Choice Requires="w15">{binding}</mc:Choice></mc:AlternateContent></w:sdtPr><w:sdtContent><w:r><w:t>cachedchoice</w:t></w:r></w:sdtContent></w:sdt></w:p>"#
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "lead \u{FFFC}");
    assert_eq!(
        hidden(&extracted),
        [
            (1, "data_binding"),
            (2, "data_binding"),
            (3, "data_binding")
        ]
    );
    for binding in [
        binding.clone(),
        r#"<w:dataBinding w:xpath="/r[1]" w:storeItemID="{11111111-1111-1111-1111-111111111111}"/>"#.into(),
    ] {
        let late = format!(
            r#"<w:p><w:sdt><w:sdtContent><w:r><w:t>late</w:t></w:r></w:sdtContent><w:sdtPr>{binding}</w:sdtPr></w:sdt></w:p>"#
        );
        assert_eq!(
            docs::extract(MediaKind::Docx, &Docx::new(&late).build(), &limits()),
            Err(DocError::Malformed("docx_structure"))
        );
    }
}

/// An inline equation, or any element the reader does not read that holds text, stands where
/// Word draws it: a gap, and an `unknown_namespace` warning.
#[test]
fn docx_equations_and_unread_inline_content_leave_a_gap() {
    let math = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    let body = [
        format!(
            r#"<w:p>{}<m:oMath {math}><m:r><m:t>2</m:t></m:r></m:oMath>{}</w:p>"#,
            run("", "Budget: 1"),
            run("", "00 baht")
        ),
        format!(
            r#"<w:p>{}<m:oMath {math}><m:r><w:rPr><w:rFonts w:ascii="Cambria Math"/></w:rPr><m:t>2</m:t></m:r></m:oMath>{}</w:p>"#,
            run("", "Total 1"),
            run("", "00")
        ),
        format!(r#"<w:p><m:oMathPara {math}><m:oMath><m:r><m:t>x=1</m:t></m:r></m:oMath></m:oMathPara></w:p>"#),
        format!(
            r#"<w:p>{}<w:hyperlink><m:oMath {math}><m:r><m:t>7</m:t></m:r></m:oMath></w:hyperlink>{}</w:p>"#,
            run("", "rate "),
            run("", "%")
        ),
        // An empty element of an unknown namespace holds nothing Word draws.
        format!(
            r#"<w:p>{}<x:mark xmlns:x="urn:example:unknown"/>{}</w:p>"#,
            run("", "Budget: 1"),
            run("", "00 baht")
        ),
        // Markup inside text, and character data directly in mc:AlternateContent.
        r#"<w:p><w:r><w:t>1<w:b>2</w:b>00</w:t></w:r></w:p>"#.into(),
        r#"<w:p><w:r><w:t>A<mc:AlternateContent>secret</mc:AlternateContent>B</w:t></w:r></w:p>"#.into(),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(
        extracted
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        [
            "Budget: 1\u{FFFC}00 baht",
            "Total 1\u{FFFC}00",
            "rate \u{FFFC}%",
            "Budget: 100 baht",
            "1\u{FFFC}00",
            "A\u{FFFC}B"
        ]
    );
    assert_eq!(
        hidden(&extracted),
        [
            (1, "unknown_namespace"),
            (2, "unknown_namespace"),
            (3, "unknown_namespace"),
            (4, "unknown_namespace"),
            (6, "undecided"),
            (7, "unknown_namespace")
        ]
    );

    // An equation draws characters its attributes name (a delimiter, an operator, an accent)
    // whatever text it holds, and so may an element the reader does not read that holds
    // anything at all: a gap and a warning wherever it stands.
    let around = |inner: &str| {
        format!(
            "<w:p>{}{inner}{}</w:p>",
            run("", "Budget: 1"),
            run("", "00 baht")
        )
    };
    let equation = |inner: &str| format!("<m:oMath {math}>{inner}</m:oMath>");
    let delimiter =
        equation(r#"<m:d><m:dPr><m:begChr m:val="2"/><m:endChr m:val=""/></m:dPr><m:e/></m:d>"#);
    let unknown = r#"xmlns:x="urn:example:unknown""#;
    let drawn = [
        delimiter.clone(),
        equation(
            r#"<m:nary><m:naryPr><m:chr m:val="2"/></m:naryPr><m:sub/><m:sup/><m:e/></m:nary>"#,
        ),
        equation(
            r#"<m:groupChr><m:groupChrPr><m:chr m:val="2"/></m:groupChrPr><m:e/></m:groupChr>"#,
        ),
        equation(r#"<m:acc><m:accPr><m:chr m:val="2"/></m:accPr><m:e/></m:acc>"#),
        equation(r#"<m:r><m:t xml:space="preserve"> </m:t></m:r>"#),
        format!(
            r#"<m:oMathPara {math}><m:oMath><m:d><m:dPr><m:sepChr m:val="2"/></m:dPr><m:e/><m:e/></m:d></m:oMath></m:oMathPara>"#
        ),
        format!("<w:hyperlink>{delimiter}</w:hyperlink>"),
        format!(r#"<x:wrap {unknown}><x:sign x:char="2"/></x:wrap>"#),
        format!(r#"<x:sign {unknown} x:char="2"/>"#),
        format!(r#"<w:r><x:sign {unknown} x:char="2"/></w:r>"#),
    ];
    // Nothing inside and nothing named: nothing drawn there.
    let empty = [
        format!("<m:oMath {math}/>"),
        format!("<m:oMath {math}></m:oMath>"),
        format!("<x:mark {unknown}/>"),
    ];
    let body = [
        drawn.iter().map(|inner| around(inner)).collect::<String>(),
        empty.iter().map(|inner| around(inner)).collect(),
        // A paragraph holding only an equation, and one between paragraphs, where the schema
        // allows it.
        format!("<w:p>{delimiter}</w:p>"),
        format!(
            r#"<m:oMathPara {math}><m:oMath><m:d><m:dPr><m:begChr m:val="7"/></m:dPr><m:e/></m:d></m:oMath></m:oMathPara>"#
        ),
        para("", &run("", "after")),
        // Inside text, unread markup is undecided as markup Word does not read there is; inside
        // an instruction it leaves the field's type undecided.
        format!(r#"<w:p><w:r><w:t>1<x:sign {unknown} x:char="2"/>00</w:t></w:r></w:p>"#),
        format!(
            r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> REF<x:sign {unknown} x:char="2"/> x </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>r</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#
        ),
        // In a branch Word does not take, or in a deletion, it is not there at all.
        around(&format!(
            r#"<mc:AlternateContent><mc:Choice Requires="w14"><w:r/></mc:Choice><mc:Fallback>{delimiter}</mc:Fallback></mc:AlternateContent>"#
        )),
        around(&format!("<w:del>{delimiter}</w:del>")),
        // Text in it between paragraphs warns; white space alone is nothing.
        format!(r#"<x:note {unknown}>2</x:note>"#),
        para("", &run("", "after2")),
        format!(r#"<x:note {unknown}> </x:note>"#),
        para("", &run("", "after3")),
        // An equation in a deleted cell is not there either.
        format!(
            r#"<w:tbl><w:tr><w:tc><w:tcPr><w:cellDel w:id="1" w:author="a"/></w:tcPr><m:oMathPara {math}><m:oMath><m:r><m:t>2</m:t></m:r></m:oMath></m:oMathPara><w:p/></w:tc></w:tr></w:tbl>"#
        ),
        para("", &run("", "after4")),
    ]
    .concat();
    let extracted = read_body(&body);
    let gap = "Budget: 1\u{FFFC}00 baht";
    let mut expected = vec![gap; drawn.len()];
    expected.extend(["Budget: 100 baht"; 3]);
    expected.extend(["after", "1\u{FFFC}00"]);
    expected.extend([
        "Budget: 100 baht",
        "Budget: 100 baht",
        "after2",
        "after3",
        "after4",
    ]);
    assert_eq!(
        extracted
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    let mut warned: Vec<(u32, &str)> = (1..=10).map(|at| (at, "unknown_namespace")).collect();
    // The paragraph holding only an equation, and the one after the equation between paragraphs.
    warned.extend([
        (14, "unknown_namespace"),
        (15, "unknown_namespace"),
        (16, "undecided"),
        (17, "unknown_namespace"),
        (20, "unknown_namespace"),
    ]);
    assert_eq!(hidden(&extracted), warned);
}

/// An equation inside WordprocessingML the reader passes over in a run or a paragraph (a run in
/// a run, a container a run cannot hold, an element Word does not define) leaves the gap and the
/// warning it leaves between runs; inside a drawing or a run's properties Word draws nothing of
/// it.
#[test]
fn docx_unread_content_in_markup_the_reader_passes_over_leaves_a_gap() {
    let math = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    let delimiter = format!(
        r#"<m:oMath {math}><m:d><m:dPr><m:begChr m:val="2"/><m:endChr m:val=""/></m:dPr><m:e/></m:d></m:oMath>"#
    );
    let written = format!(r#"<m:oMath {math}><m:r><m:t>2</m:t></m:r></m:oMath>"#);
    let around = |inner: &str| {
        format!(
            "<w:p>{}{inner}{}</w:p>",
            run("", "Budget: 1"),
            run("", "00 baht")
        )
    };
    let in_run = |inner: &str| {
        format!("<w:p><w:r><w:t>Budget: 1</w:t>{inner}<w:t>00 baht</w:t></w:r></w:p>")
    };
    let gap = "Budget: 1\u{FFFC}00 baht";
    let unread = vec![(1, "unknown_namespace")];
    let rows = [
        ("a run in a run", in_run(&format!("<w:r>{delimiter}</w:r>"))),
        (
            "a run in a run between runs",
            around(&format!("<w:r><w:r>{written}</w:r></w:r>")),
        ),
        (
            "a hyperlink in a run",
            in_run(&format!("<w:hyperlink>{delimiter}</w:hyperlink>")),
        ),
        (
            "a content control in a run",
            in_run(&format!(
                "<w:sdt><w:sdtContent>{delimiter}</w:sdtContent></w:sdt>"
            )),
        ),
        (
            "an insertion in a run",
            in_run(&format!(
                r#"<w:ins w:id="1" w:author="a">{delimiter}</w:ins>"#
            )),
        ),
        (
            "a simple field in a run",
            in_run(&format!(
                r#"<w:fldSimple w:instr=" REF x ">{delimiter}</w:fldSimple>"#
            )),
        ),
        (
            "custom XML in a run",
            in_run(&format!(
                r#"<w:customXml w:element="x">{delimiter}</w:customXml>"#
            )),
        ),
        (
            "a smart tag in a run",
            in_run(&format!(
                r#"<w:smartTag w:element="x">{delimiter}</w:smartTag>"#
            )),
        ),
        (
            "an element Word does not define in a run",
            in_run(&format!("<w:notAnElement>{delimiter}</w:notAnElement>")),
        ),
        (
            "an element Word does not define in a paragraph",
            around(&format!("<w:notAnElement>{delimiter}</w:notAnElement>")),
        ),
        (
            "a content control's own markup",
            around(&format!(
                "<w:sdt><w:sdtPr/>{delimiter}<w:sdtContent/></w:sdt>"
            )),
        ),
    ];
    for (case, body) in rows {
        let extracted = read_body(&body);
        assert_eq!(text(&extracted), gap, "{case}");
        assert_eq!(hidden(&extracted), unread, "{case}");
    }

    // Text in a run in a run is undecided, as it was; an unknown element in a drawing or in a
    // run's properties is nothing drawn there.
    let chart = r#"<w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="Chart 1"><a:extLst><a:ext uri="{FF2B5EF4-FFF2-40B4-BE49-F238E27FC236}"><a16:creationId xmlns:a16="http://schemas.microsoft.com/office/drawing/2014/main" id="{00000000-0000-0000-0000-000000000001}"/></a:ext></a:extLst></wp:docPr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId9"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#;
    let rows = [
        (
            "text in a run in a run",
            in_run("<w:r><w:t>2</w:t></w:r>"),
            gap,
            vec![(1, "undecided")],
        ),
        ("a chart", around(chart), gap, vec![]),
        (
            "an unknown element in a run's properties",
            format!(
                r#"<w:p>{}<w:r><w:rPr><x:a xmlns:x="urn:example:unknown" x:b="2"/></w:rPr><w:t>00 baht</w:t></w:r></w:p>"#,
                run("", "Budget: 1")
            ),
            "Budget: 100 baht",
            vec![],
        ),
        (
            "an unknown element in a section's properties",
            format!(
                r#"{}<w:sectPr><x:a xmlns:x="urn:example:unknown" x:b="2"/></w:sectPr>"#,
                around("")
            ),
            "Budget: 100 baht",
            vec![],
        ),
        (
            "an unknown element in a content control's end properties",
            around(
                r#"<w:sdt><w:sdtPr/><w:sdtEndPr><x:a xmlns:x="urn:example:unknown" x:b="2"/></w:sdtEndPr><w:sdtContent/></w:sdt>"#,
            ),
            "Budget: 100 baht",
            vec![],
        ),
        (
            "an unknown element in custom XML properties",
            around(
                r#"<w:customXml w:element="x"><w:customXmlPr><x:a xmlns:x="urn:example:unknown" x:b="2"/></w:customXmlPr></w:customXml>"#,
            ),
            "Budget: 100 baht",
            vec![],
        ),
        (
            "an unknown element in smart tag properties",
            around(
                r#"<w:smartTag w:element="x"><w:smartTagPr><x:a xmlns:x="urn:example:unknown" x:b="2"/></w:smartTagPr></w:smartTag>"#,
            ),
            "Budget: 100 baht",
            vec![],
        ),
    ];
    for (case, body, shown, warned) in rows {
        let extracted = read_body(&body);
        assert_eq!(text(&extracted), shown, "{case}");
        assert_eq!(hidden(&extracted), warned, "{case}");
    }
}

/// Unread content between paragraphs gives the paragraph after it one warning, its first reason,
/// with that paragraph's id; after the last paragraph, the last one; with no paragraph, none.
#[test]
fn docx_unread_content_between_paragraphs_warns_one_paragraph_once() {
    let math = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    let equation = format!(
        r#"<m:oMathPara {math}><m:oMath><m:d><m:dPr><m:begChr m:val="2"/></m:dPr><m:e/></m:d></m:oMath></m:oMathPara>"#
    );
    let plain = para("", &run("", "plain"));
    let vanished = para("", &[run("", "shown"), run("<w:vanish/>", "gone")].concat());
    let rows = [
        (
            "three before a paragraph",
            format!("{equation}{equation}{equation}{plain}"),
            "plain",
            vec![(1, "unknown_namespace")],
        ),
        (
            "two before a paragraph that hides a run",
            format!("{equation}{equation}{vanished}"),
            "shown\u{FFFC}",
            vec![(1, "unknown_namespace")],
        ),
        (
            "after the last paragraph",
            format!("{plain}{equation}"),
            "plain",
            vec![(1, "unknown_namespace")],
        ),
        (
            "after the last paragraph, which warns already",
            format!("{vanished}{equation}"),
            "shown\u{FFFC}",
            vec![(1, "vanish")],
        ),
        (
            "between paragraphs in a cell",
            table(
                "",
                &[vec![cell(
                    "",
                    &format!("{plain}{equation}{equation}{plain}"),
                )]],
            ),
            "plain\nplain",
            vec![(2, "unknown_namespace")],
        ),
        ("and no paragraph", equation.clone(), "", vec![]),
    ];
    for (case, body, shown, warned) in rows {
        let extracted = read_body(&body);
        assert_eq!(text(&extracted), shown, "{case}");
        assert_eq!(hidden(&extracted), warned, "{case}");
    }

    // The warning names the paragraph as its block does, before it or after it.
    let ids = |locators: Vec<&Locator>| -> Vec<Option<String>> {
        locators
            .into_iter()
            .map(|locator| match locator {
                Locator::DocxParagraph { para_id, .. } => para_id.clone(),
                _ => unreachable!("a DOCX locator"),
            })
            .collect()
    };
    let identified = format!(r#"<w:p w14:paraId="1A2B3C4D">{}</w:p>"#, run("", "x"));
    for body in [
        format!("{equation}{identified}"),
        format!("{identified}{equation}"),
    ] {
        let extracted = read_body(&body);
        let warned = ids(extracted
            .warnings
            .iter()
            .filter_map(|warning| match warning {
                ExtractWarning::HiddenContent { at, .. } => Some(at),
                _ => None,
            })
            .collect());
        let shown = ids(extracted
            .blocks
            .iter()
            .map(|block| &block.locator)
            .collect());
        assert_eq!(warned, [Some("1A2B3C4D".to_owned())], "{body}");
        assert_eq!(shown, warned, "{body}");
    }
}

/// Text in markup Word would not read as the reader does is undecided where it stands, never
/// dropped unmarked: a `w:delText` or `w:delInstrText` outside a deletion, a `w:t` or
/// `w:instrText` out of its place, an instruction no field takes, an element Word does not
/// define, and character data where WordprocessingML holds none. In a paragraph it leaves a gap
/// and an `undecided` warning, so the figures either side never join; between paragraphs the
/// paragraph after it warns. Deleted text, field data and white space between elements are not
/// text Word draws.
#[test]
fn docx_text_word_would_not_read_there_is_undecided() {
    let around = |inner: &str| {
        format!(
            "<w:p>{}{inner}{}</w:p>",
            run("", "Payment: 1"),
            run("", "00,000 baht")
        )
    };
    let in_run = |inner: &str| {
        format!("<w:p><w:r><w:t>Payment: 1</w:t>{inner}<w:t>00,000 baht</w:t></w:r></w:p>")
    };
    let begin = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#;
    let instr = |text: &str| {
        format!(r#"<w:r><w:instrText xml:space="preserve">{text}</w:instrText></w:r>"#)
    };
    let separate = r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#;
    let end = r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
    let gap = "Payment: 1\u{FFFC}00,000 baht";
    let joined = "Payment: 100,000 baht";
    let undecided = vec![(1, "undecided")];
    type Row = (&'static str, String, &'static str, Vec<(u32, &'static str)>);
    let rows: Vec<Row> = vec![
        // Text elements out of their place.
        ("delText in a run", in_run("<w:delText>2</w:delText>"), gap, undecided.clone()),
        ("a run of delText", around("<w:r><w:delText>2</w:delText></w:r>"), gap, undecided.clone()),
        (
            "a run of delText in an insertion",
            around(r#"<w:ins w:id="1" w:author="a"><w:r><w:delText>2</w:delText></w:r></w:ins>"#),
            gap,
            undecided.clone(),
        ),
        (
            "delText of a space",
            around(r#"<w:r><w:delText xml:space="preserve"> </w:delText></w:r>"#),
            gap,
            undecided.clone(),
        ),
        ("delText in a paragraph", around("<w:delText>2</w:delText>"), gap, undecided.clone()),
        (
            "a paragraph of delText",
            "<w:p><w:r><w:delText>Pay 1,200,000 baht</w:delText></w:r></w:p>".into(),
            "",
            undecided.clone(),
        ),
        (
            "delText at a paragraph's end",
            format!("<w:p>{}<w:r><w:delText>0</w:delText></w:r></w:p>", run("", "Total 100")),
            "Total 100\u{FFFC}",
            undecided.clone(),
        ),
        (
            "delInstrText in no field",
            around("<w:r><w:delInstrText>2</w:delInstrText></w:r>"),
            gap,
            undecided.clone(),
        ),
        (
            "instrText in no field",
            around("<w:r><w:instrText>2</w:instrText></w:r>"),
            gap,
            undecided.clone(),
        ),
        (
            "instrText in a field's result",
            around(&format!(
                "{begin}{}{separate}<w:r><w:instrText>2</w:instrText></w:r>{end}",
                instr(" REF x ")
            )),
            gap,
            undecided.clone(),
        ),
        (
            "instrText out of its run",
            around("<w:instrText>2</w:instrText>"),
            gap,
            undecided.clone(),
        ),
        ("t out of its run", around("<w:t>2</w:t>"), gap, undecided.clone()),
        (
            "a paragraph of t out of its run",
            "<w:p><w:t>loose</w:t></w:p>".into(),
            "",
            undecided.clone(),
        ),
        // Elements Word does not define, and character data where none may be.
        (
            "an element Word does not define in a run",
            in_run("<w:notAnElement>2</w:notAnElement>"),
            gap,
            undecided.clone(),
        ),
        (
            "an element Word does not define in a paragraph",
            around("<w:notAnElement>2</w:notAnElement>"),
            gap,
            undecided.clone(),
        ),
        (
            "text in an element that holds none",
            in_run("<w:lastRenderedPageBreak>2</w:lastRenderedPageBreak>"),
            gap,
            undecided.clone(),
        ),
        ("text directly in a run", in_run("2"), gap, undecided.clone()),
        ("text directly in a paragraph", around("2"), gap, undecided.clone()),
        (
            "text directly in a hyperlink",
            around("<w:hyperlink>2</w:hyperlink>"),
            gap,
            undecided.clone(),
        ),
        (
            "text in a run in a run",
            in_run("<w:r>2</w:r>"),
            gap,
            undecided.clone(),
        ),
        (
            "a field's data out of its place",
            around("<w:fldData>2</w:fldData>"),
            gap,
            undecided.clone(),
        ),
        (
            "a field character out of its place",
            around("<w:fldChar>2</w:fldChar>"),
            gap,
            undecided.clone(),
        ),
        // Between paragraphs, the paragraph after it warns.
        (
            "delText between paragraphs",
            format!("<w:delText>2</w:delText>{}", para("", &run("", "after"))),
            "after",
            undecided.clone(),
        ),
        (
            "an element Word does not define between paragraphs",
            format!("<w:notAnElement>2</w:notAnElement>{}", para("", &run("", "after"))),
            "after",
            undecided.clone(),
        ),
        (
            "text directly in the body",
            format!("2{}", para("", &run("", "after"))),
            "after",
            undecided.clone(),
        ),
        // Not text Word draws.
        (
            "delText in a deletion",
            around(r#"<w:del w:id="1" w:author="a"><w:r><w:delText>2</w:delText></w:r></w:del>"#),
            joined,
            vec![],
        ),
        (
            "delText moved away",
            around(r#"<w:moveFrom w:id="1" w:author="a"><w:r><w:delText>2</w:delText></w:r></w:moveFrom>"#),
            joined,
            vec![],
        ),
        (
            "an empty element Word does not define",
            around(r#"<w:notAnElement w:val="2"/>"#),
            joined,
            vec![],
        ),
        (
            "white space between elements",
            "<w:p>\n  <w:r>\n    <w:t>Payment: 1</w:t>\n  </w:r>\n  <w:r>\n    <w:t>00,000 baht</w:t>\n  </w:r>\n</w:p>\n".into(),
            joined,
            vec![],
        ),
        (
            "a field's data",
            format!(
                r#"<w:p>{}<w:r><w:fldChar w:fldCharType="begin"><w:fldData xml:space="preserve">CNDJ6nn5us4RjIIAqgBLqQsCAAAACAAAAA4AAABfAFIAZQBmADEAMgAzADQANQA2ADcAOAAAAA==</w:fldData></w:fldChar></w:r>{}{separate}<w:r><w:t>Table 1</w:t></w:r>{end}</w:p>"#,
                run("", "See "),
                instr(r#" REF _Ref12345678 \h "#)
            ),
            "See Table 1",
            vec![],
        ),
        (
            "a field's data in its separator",
            format!(
                r#"<w:p>{}{begin}{}<w:r><w:fldChar w:fldCharType="separate"><w:fldData>AAAA</w:fldData></w:fldChar></w:r><w:r><w:t>Table 1</w:t></w:r>{end}</w:p>"#,
                run("", "See "),
                instr(" REF x ")
            ),
            "See Table 1",
            vec![],
        ),
        (
            "a simple field's data",
            format!(
                r#"<w:p>{}<w:fldSimple w:instr=" REF x "><w:fldData>AAAA</w:fldData><w:r><w:t>Table 1</w:t></w:r></w:fldSimple></w:p>"#,
                run("", "See ")
            ),
            "See Table 1",
            vec![],
        ),
        (
            "a form field's data",
            format!(
                r#"<w:p>{}<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Text1"/><w:enabled/><w:calcOnExit w:val="0"/><w:textInput/></w:ffData></w:fldChar></w:r>{}{separate}<w:r><w:t>Somchai</w:t></w:r>{end}</w:p>"#,
                run("", "Name: "),
                instr(" FORMTEXT ")
            ),
            "Name: Somchai",
            vec![],
        ),
    ];
    for (case, body, shown, warned) in rows {
        let extracted = read_body(&body);
        assert_eq!(text(&extracted), shown, "{case}");
        assert_eq!(hidden(&extracted), warned, "{case}");
    }
}

/// The accepted view has no row deleted in a conflict (`w14:conflictDel`, which Word treats as a
/// tracked deletion), and no cell a tracked change merged into the one above.
#[test]
fn docx_conflict_deletions_and_tracked_merges_are_not_shown() {
    let row = |props: &str, cell_props: &str, marker: &str| {
        format!(
            "<w:tr>{props}{}</w:tr>",
            cell(cell_props, &para("", &run("", marker)))
        )
    };
    let body = format!(
        r#"<w:tbl><w:tblPr/>{}{}{}{}{}{}</w:tbl>"#,
        row("", "", "top"),
        row(
            r#"<w:trPr><w14:conflictDel w:id="9" w:author="a" w:date="2026-01-01T00:00:00Z"/></w:trPr>"#,
            "",
            "conflictdeleted"
        ),
        row(
            "",
            r#"<w:cellMerge w:id="1" w:author="a" w:vMerge="cont"/>"#,
            "mergedaway"
        ),
        row(
            "",
            r#"<w:cellMerge w:id="2" w:author="a" w:vMerge="rest"/>"#,
            "restarted"
        ),
        row("", "<w:hMerge/>", "hmerged"),
        row("", r#"<w:vMerge w:val="restart"/>"#, "vrestart"),
    );
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "top\nrestarted\nvrestart");
    assert_eq!(hidden(&extracted), [(3, "merged_cell"), (5, "merged_cell")]);
}

/// A `w:shd` with no pattern is drawn as its fill, as for `clear` (generators write it so).
#[test]
fn docx_shading_with_no_pattern_shows_its_fill() {
    let body = [
        para(r#"<w:shd w:fill="E8EEF5"/>"#, &run(BLACK, "blackonlight")),
        para(r#"<w:shd w:fill="E8EEF5"/>"#, &run(WHITE, "whiteonlight")),
        table(
            "",
            &[vec![cell(
                r#"<w:shd w:fill="1F3864"/>"#,
                &para("", &run(WHITE, "whiteondark")),
            )]],
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "blackonlight\nwhiteondark");
    assert_eq!(hidden(&extracted), [(2, "low_contrast")]);
}

/// Shape markup Word would not lay out as the reader reads it: properties after the text box
/// was read are refused; a shape outside its frame is undecided. Extensions Word does not draw
/// from (`a:extLst`) are passed over.
#[test]
fn docx_late_and_misplaced_shape_markup_fails_closed() {
    let white_fill = r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#;
    let late_frame = |late: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box("anchor", "", white_fill, &para("", &run("", "late")))
                .replace("</a:graphic>", &format!("</a:graphic>{late}"))
        )
    };
    for late in [
        r#"<wp:docPr id="1" name="Box" hidden="1"/>"#,
        r#"<wp:extent cx="1" cy="1"/>"#,
        r#"<wp:positionH relativeFrom="page"><wp:posOffset>40000000</wp:posOffset></wp:positionH>"#,
        r#"<wp:simplePos x="0" y="0"/>"#,
    ] {
        assert_eq!(
            docs::extract(
                MediaKind::Docx,
                &Docx::new(&late_frame(late)).build(),
                &limits()
            ),
            Err(DocError::Malformed("docx_structure")),
            "{late}"
        );
    }
    let wsp = |marker: &str| {
        format!(
            r#"<wps:wsp><wps:spPr>{white_fill}</wps:spPr><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp>"#,
            para("", &run("", marker))
        )
    };
    let body = [
        format!("<w:p><w:r><w:drawing>{}</w:drawing></w:r></w:p>", wsp("noframe")),
        format!("<w:p><w:r>{}</w:r></w:p>", wsp("inrun")),
        format!("<w:p>{}</w:p>", wsp("inpara")),
        format!(
            r#"<w:p><w:r><v:shape fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:r></w:p>"#,
            para("", &run("", "vmlnopict"))
        ),
        // Word keeps a fill it does not draw in an extension: the box is white.
        format!(
            "<w:p>{}</w:p>",
            shape_box(
                "anchor",
                "",
                &format!(
                    r#"{white_fill}<a:extLst><a:ext uri="{{909E8E84-426E-40DD-AFC4-6F175D3DCCD1}}"><a14:hiddenFill xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a14:hiddenFill></a:ext></a:extLst>"#
                ),
                &para("", &run("", "extension"))
            )
        ),
        // A frame outside `w:drawing`.
        format!(
            "<w:p>{}</w:p>",
            shape_box("inline", "", white_fill, &para("", &run("", "nodrawing")))
                .replace("<w:drawing>", "")
                .replace("</w:drawing>", "")
        ),
    ]
    .concat();
    // Inside a text box, as at the top level: its shape and frame are no frame for the markup
    // in its paragraphs.
    let in_box = |inner: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box("inline", "", white_fill, &format!("<w:p>{inner}</w:p>"))
        )
    };
    let nested = [
        in_box(&format!("<w:r>{}</w:r>", wsp("boxinrun"))),
        in_box(&wsp("boxinpara")),
        in_box(&format!(
            r#"<w:r><v:shape fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:r>"#,
            para("", &run("", "boxvmlnopict"))
        )),
        in_box(
            &shape_box("inline", "", white_fill, &para("", &run("", "boxnodrawing")))
                .replace("<w:drawing>", "")
                .replace("</w:drawing>", ""),
        ),
        // A box in its frame inside a box is read.
        in_box(&shape_box(
            "inline",
            "",
            white_fill,
            &para("", &run("", "nestedbox")),
        )),
    ]
    .concat();
    let extracted = read_body(&(body + &nested));
    assert_eq!(text(&extracted), "extension\nnestedbox");
    // Each box's paragraph comes after the paragraph holding it.
    assert_eq!(
        hidden(&extracted),
        [
            (2, "undecided"),
            (4, "undecided"),
            (6, "undecided"),
            (8, "undecided"),
            (12, "undecided"),
            (15, "undecided"),
            (18, "undecided"),
            (21, "undecided"),
            (24, "undecided")
        ]
    );
}

/// Word reads a shape's text only from `wps:txbx` in a `wps:wsp`, or `v:textbox` in a VML shape:
/// a `w:txbxContent` anywhere else is undecided, an extension's included.
#[test]
fn docx_text_box_content_stands_only_in_its_shapes_text_box() {
    let white_fill = r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#;
    let story = |marker: &str| {
        format!(
            "<w:txbxContent>{}</w:txbxContent>",
            para("", &run("", marker))
        )
    };
    let boxed = |sp_pr: &str, marker: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box("inline", "", sp_pr, &para("", &run("", marker)))
        )
    };
    let body = [
        boxed(&format!("{white_fill}{}", story("inspPr")), "real1"),
        boxed(
            &format!(
                r#"{white_fill}<a:extLst><a:ext uri="{{00000000-0000-0000-0000-000000000000}}">{}</a:ext></a:extLst>"#,
                story("inext")
            ),
            "real2",
        ),
        boxed(white_fill, "real3").replace(
            "</wps:spPr>",
            &format!("</wps:spPr><wps:bodyPr>{}</wps:bodyPr>", story("inbodyPr")),
        ),
        boxed(white_fill, "x").replace(
            &format!("<wps:txbx>{}</wps:txbx>", story("x")),
            &story("nowpstxbx"),
        ),
        boxed(white_fill, "real5").replace(
            "</a:graphicData>",
            &format!("{}</a:graphicData>", story("ingraphicData")),
        ),
        format!(
            r#"<w:p><w:r><w:pict><v:shape fillcolor="white">{}</v:shape></w:pict></w:r></w:p>"#,
            story("novtextbox")
        ),
        // A `wps:txbx` outside its shape, a `v:textbox` outside any.
        boxed(white_fill, "real7").replace(
            "</wps:spPr>",
            &format!("<wps:txbx>{}</wps:txbx></wps:spPr>", story("txbxinspPr")),
        ),
        format!(
            r#"<w:p><w:r><w:pict><v:textbox>{}</v:textbox></w:pict></w:r></w:p>"#,
            story("textboxinpict")
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "real1\nreal2\nreal3\nreal5\nreal7");
    assert_eq!(
        hidden(&extracted),
        [
            (2, "undecided"),
            (5, "undecided"),
            (8, "undecided"),
            (11, "undecided"),
            (14, "undecided"),
            (16, "undecided"),
            (18, "undecided"),
            (21, "undecided")
        ]
    );
}

/// Word lays out no `v:shapetype`: it is a template the shapes built on it take their fill from.
/// Text in a shapetype's text box is undecided wherever the shapetype stands and whatever uses
/// it; a shape lays out only its own `v:textbox`, as Word's own text box does.
#[test]
fn docx_a_shapetype_is_never_laid_out() {
    let story = |marker: &str| {
        format!(
            "<v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox>",
            para("", &run("", marker))
        )
    };
    let pict = |inner: &str| format!("<w:p><w:r><w:pict>{inner}</w:pict></w:r></w:p>");
    let shapetype = |id: &str, inner: &str| {
        format!(
            r#"<v:shapetype id="{id}" coordsize="21600,21600" fillcolor="white">{inner}</v:shapetype>"#
        )
    };
    let body = [
        pict(&shapetype("unused", &story("unused"))),
        pict(&format!(
            r##"{}<v:shape type="#hiddenonly" style="position:absolute;visibility:hidden"/>"##,
            shapetype("hiddenonly", &story("hiddenonly"))
        )),
        pict(&format!(
            r#"<v:group style="position:absolute;width:100pt;height:100pt" coordsize="21600,21600">{}</v:group>"#,
            shapetype("ingroup", &story("ingroup"))
        )),
        pict(&format!(
            r##"{}<v:shape type="#used" style="position:absolute" fillcolor="white"/>"##,
            shapetype("used", &story("used"))
        )),
        pict(&format!(
            r##"{}<v:shape type="#both" style="position:absolute" fillcolor="white">{}</v:shape>"##,
            shapetype("both", &story("typetext")),
            story("shapetext")
        )),
        // Word's own text box: the shapetype Word writes holds no text box.
        pict(&format!(
            r##"<v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype><v:shape id="Text Box 2" o:spid="_x0000_s1026" type="#_x0000_t202" style="position:absolute;margin-left:0;margin-top:14.4pt;width:185.9pt;height:110.6pt;z-index:251659264;visibility:visible;mso-wrap-style:square;mso-position-horizontal:center;mso-position-horizontal-relative:text;mso-position-vertical:absolute;mso-position-vertical-relative:text"><v:textbox style="mso-fit-shape-to-text:t"><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape>"##,
            para("", &run("", "word2007"))
        )),
        pict(&format!(
            r#"<v:rect style="position:absolute" fillcolor="white">{}</v:rect>"#,
            story("rect")
        )),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "shapetext\nword2007\nrect");
    assert_eq!(
        hidden(&extracted),
        [
            (2, "undecided"),
            (4, "undecided"),
            (6, "undecided"),
            (8, "undecided"),
            (10, "undecided")
        ]
    );
}

/// A `wp:posOffset` is one number however its text is written: a comment, a character reference,
/// CDATA or a processing instruction inside it splits it into pieces, which are read whole.
#[test]
fn docx_an_offset_written_in_pieces_is_read_whole() {
    let boxed = |offset: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box(
                "anchor",
                &format!(
                    r#"<wp:positionH relativeFrom="page"><wp:posOffset>{offset}</wp:posOffset></wp:positionH><wp:extent cx="914400" cy="914400"/>"#
                ),
                r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
                &para("", &run("", "x"))
            )
        )
    };
    for (offset, shown, warned) in [
        ("30000000", "", vec![(2, "off_page")]),
        ("3<!-- -->0000000", "", vec![(2, "off_page")]),
        ("3&#48;000000", "", vec![(2, "off_page")]),
        ("3<![CDATA[0]]>000000", "", vec![(2, "off_page")]),
        ("-3<!-- -->0000000", "", vec![(2, "off_page")]),
        ("30000<?x y?>000", "", vec![(2, "off_page")]),
        // On the page, and a piece of white space after the number.
        ("9<!-- -->14400", "x", vec![]),
        ("914400<!-- --> ", "x", vec![]),
    ] {
        let extracted = read_body(&boxed(offset));
        assert_eq!(text(&extracted), shown, "{offset}");
        assert_eq!(hidden(&extracted), warned, "{offset}");
    }
}

/// Markup as Word writes it is untouched by the rules for markup Word would not read: bookmarks,
/// proofing marks, field data, a form field, equations, a chart, a picture with its extensions,
/// SmartArt, a text box with its VML fallback, a table, a content control, tracked changes, a
/// note mark and section properties read with gaps only at the equations, the picture and the
/// note mark.
#[test]
fn docx_word_markup_reads_as_word_draws_it() {
    let math = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    let cambria = r#"<w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/></w:rPr>"#;
    let body = [
        r#"<w:p w14:paraId="0A1B2C3D" w14:textId="77777777" w:rsidR="00A10001" w:rsidRDefault="00A10001"><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:bookmarkStart w:id="0" w:name="_GoBack"/><w:bookmarkEnd w:id="0"/><w:r><w:t>Quarterly</w:t></w:r><w:proofErr w:type="spellStart"/><w:r><w:t xml:space="preserve"> reprot</w:t></w:r><w:proofErr w:type="spellEnd"/></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"><w:fldData xml:space="preserve">CNDJ6nn5us4RjIIAqgBLqQsCAAAACAAAAA4AAABfAFIAZQBmADEAMgAzADQANQA2ADcAOAAAAA==</w:fldData></w:fldChar></w:r><w:r><w:instrText xml:space="preserve"> REF _Ref12345678 \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>Table 1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t xml:space="preserve"> below.</w:t></w:r></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:t xml:space="preserve">Name: </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Text1"/><w:enabled/><w:calcOnExit w:val="0"/><w:textInput/></w:ffData></w:fldChar></w:r><w:r><w:instrText xml:space="preserve"> FORMTEXT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:rPr><w:noProof/></w:rPr><w:t>Somchai</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:lastRenderedPageBreak/><w:t>Page two</w:t></w:r></w:p>"#.to_owned(),
        format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Area </w:t></w:r><m:oMath {math}><m:sSup><m:sSupPr><m:ctrlPr><w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/><w:i/></w:rPr></m:ctrlPr></m:sSupPr><m:e><m:r>{cambria}<m:t>x</m:t></m:r></m:e><m:sup><m:r>{cambria}<m:t>2</m:t></m:r></m:sup></m:sSup></m:oMath><w:r><w:t xml:space="preserve"> units</w:t></w:r></w:p>"#
        ),
        format!(
            r#"<w:p><m:oMathPara {math}><m:oMath><m:f><m:fPr><m:ctrlPr><w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/><w:i/></w:rPr></m:ctrlPr></m:fPr><m:num><m:r>{cambria}<m:t>a</m:t></m:r></m:num><m:den><m:r>{cambria}<m:t>b</m:t></m:r></m:den></m:f></m:oMath></m:oMathPara></w:p>"#
        ),
        r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="5486400" cy="3200400"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Chart 1"/><wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId4"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:t>Logo</w:t></w:r><w:r><w:drawing><wp:inline><wp:extent cx="952500" cy="952500"/><wp:docPr id="2" name="Picture 2"><a:extLst><a:ext uri="{FF2B5EF4-FFF2-40B4-BE49-F238E27FC236}"><a16:creationId xmlns:a16="http://schemas.microsoft.com/office/drawing/2014/main" id="{00000000-0000-0000-0000-000000000002}"/></a:ext></a:extLst></wp:docPr><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="2" name="logo.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId5"><a:extLst><a:ext uri="{28A0092B-C50C-407E-A947-70E740481C1C}"><a14:useLocalDpi xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main" val="0"/></a:ext></a:extLst></a:blip><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="952500" cy="952500"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:t xml:space="preserve"> here</w:t></w:r></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="5486400" cy="3200400"/><wp:docPr id="3" name="Diagram 3"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/diagram"><dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" r:dm="rId6" r:lo="rId7" r:qs="rId8" r:cs="rId9"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#.to_owned(),
        r##"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor distT="45720" distB="45720" distL="114300" distR="114300" simplePos="0" relativeHeight="251659264" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:align>center</wp:align></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>182880</wp:posOffset></wp:positionV><wp:extent cx="2360930" cy="1404620"/><wp:effectExtent l="0" t="0" r="22860" b="11430"/><wp:wrapSquare wrapText="bothSides"/><wp:docPr id="217" name="Text Box 2"/><wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr txBox="1"><a:spLocks noChangeArrowheads="1"/></wps:cNvSpPr><wps:spPr bwMode="auto"><a:xfrm><a:off x="0" y="0"/><a:ext cx="2360930" cy="1404620"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:ln w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill><a:miter lim="800000"/><a:headEnd/><a:tailEnd/></a:ln></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Boxed note</w:t></w:r></w:p></w:txbxContent></wps:txbx><wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" anchor="t" anchorCtr="0"><a:spAutoFit/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic><wp14:sizeRelH xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" relativeFrom="margin"><wp14:pctWidth>40000</wp14:pctWidth></wp14:sizeRelH><wp14:sizeRelV xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" relativeFrom="margin"><wp14:pctHeight>20000</wp14:pctHeight></wp14:sizeRelV></wp:anchor></w:drawing></mc:Choice><mc:Fallback><w:pict><v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype><v:shape id="Text Box 2" o:spid="_x0000_s1026" type="#_x0000_t202" style="position:absolute;margin-left:0;margin-top:14.4pt;width:185.9pt;height:110.6pt;z-index:251659264;visibility:visible;mso-wrap-style:square;mso-position-horizontal:center;mso-position-horizontal-relative:text;mso-position-vertical:absolute;mso-position-vertical-relative:text"><v:textbox style="mso-fit-shape-to-text:t"><w:txbxContent><w:p><w:r><w:t>Boxed note</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"##.to_owned(),
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="0" w:type="auto"/><w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr><w:tblGrid><w:gridCol w:w="4508"/><w:gridCol w:w="4508"/></w:tblGrid><w:tr w:rsidR="00A10001" w14:paraId="11111111" w14:textId="22222222"><w:tc><w:tcPr><w:tcW w:w="4508" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Cell A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:tcW w:w="4508" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>Cell B</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_owned(),
        r#"<w:p><w:hyperlink r:id="rId11" w:history="1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>example.com</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> and </w:t></w:r><w:sdt><w:sdtPr><w:id w:val="-1"/><w:placeholder><w:docPart w:val="DefaultPlaceholder_-1854013440"/></w:placeholder><w15:appearance xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" w15:val="hidden"/></w:sdtPr><w:sdtEndPr/><w:sdtContent><w:r><w:t>control</w:t></w:r></w:sdtContent></w:sdt></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:t xml:space="preserve">Total: </w:t></w:r><w:del w:id="1" w:author="A" w:date="2026-10-01T00:00:00Z"><w:r><w:delText>1,000</w:delText></w:r></w:del><w:ins w:id="2" w:author="A" w:date="2026-10-01T00:00:00Z"><w:r><w:t>2,000</w:t></w:r></w:ins></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:t>Note</w:t></w:r><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="1"/></w:r><w:r><w:tab/><w:t>tabbed</w:t><w:br/><w:t>next</w:t></w:r></w:p>"#.to_owned(),
        r#"<w:sectPr w:rsidR="00A10001"><w:headerReference w:type="default" r:id="rId10"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/><w:cols w:space="708"/><w:docGrid w:linePitch="360"/></w:sectPr>"#.to_owned(),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(
        extracted
            .blocks
            .iter()
            .map(|block| (block.ordinal, block.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, "Quarterly reprot"),
            (2, "See Table 1 below."),
            (3, "Name: Somchai"),
            (4, "Page two"),
            (5, "Area \u{FFFC} units"),
            (8, "Logo\u{FFFC} here"),
            (11, "Boxed note"),
            (13, "Cell A"),
            (14, "Cell B"),
            (15, "example.com and control"),
            (16, "Total: 2,000"),
            (17, "Note\u{FFFC}\ttabbed\nnext"),
        ]
    );
    assert_eq!(
        hidden(&extracted),
        [(5, "unknown_namespace"), (6, "unknown_namespace")]
    );
}

/// A followed relationship's mode is `Internal` (or none) or `External`; any other is refused,
/// and a Strict relationship named from a transitional package is unsupported.
#[test]
fn docx_relationship_modes_and_strict_types() {
    let white_normal = styles(
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:color w:val="FFFFFF"/></w:rPr></w:style>"#,
    );
    let with_mode = |mode: &str| {
        let docx = Docx::new(&para("", &run("", "whitebystyle"))).related(
            "styles",
            "styles.xml",
            white_normal.as_bytes(),
        );
        let entries: Vec<(String, Vec<u8>)> = docx
            .entries()
            .into_iter()
            .map(|(name, bytes)| {
                if name == "word/_rels/document.xml.rels" {
                    let xml = String::from_utf8(bytes).unwrap().replace(
                        r#"Target="styles.xml""#,
                        &format!(r#"Target="styles.xml"{mode}"#),
                    );
                    (name, xml.into_bytes())
                } else {
                    (name, bytes)
                }
            })
            .collect();
        zip(
            &entries
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                .collect::<Vec<_>>(),
            true,
        )
    };
    for (mode, expected) in [
        ("", Ok("")),
        (r#" TargetMode="Internal""#, Ok("")),
        (r#" TargetMode="External""#, Ok("whitebystyle")),
        (
            r#" TargetMode="internal""#,
            Err(DocError::Malformed("docx_rels")),
        ),
        (
            r#" TargetMode="Bogus""#,
            Err(DocError::Malformed("docx_rels")),
        ),
    ] {
        let read = docs::extract(MediaKind::Docx, &with_mode(mode), &limits())
            .map(|extracted| text(&extracted));
        assert_eq!(read.as_deref().map_err(|error| *error), expected, "{mode}");
    }
    let strict = Docx::new(&para("", &run("", "x"))).related(
        "http://purl.oclc.org/ooxml/officeDocument/relationships/styles",
        "styles.xml",
        white_normal.as_bytes(),
    );
    assert_eq!(
        docs::extract(MediaKind::Docx, &strict.build(), &limits()),
        Err(DocError::Unsupported("strict_ooxml"))
    );
}

/// A right-to-left override (`w:bdo`) makes Word draw its text in reverse order: hidden, with a
/// gap. A left-to-right one is read.
#[test]
fn docx_bidi_overrides_are_hidden() {
    let body = [
        format!(
            r#"<w:p>{}<w:bdo w:val="rtl">{}</w:bdo>{}</w:p>"#,
            run("", "Budget "),
            run("", "123"),
            run("", " baht")
        ),
        format!(
            r#"<w:p><w:bdo w:val="ltr">{}</w:bdo></w:p>"#,
            run("", "ltr")
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "Budget \u{FFFC} baht\nltr");
    assert_eq!(hidden(&extracted), [(1, "reversed")]);
}

/// What may lie beneath a run is listed once per colour, and past 64 colours it is undecided:
/// nested shadings of two candidate colours each would double the list at every level.
#[test]
fn docx_shading_candidates_are_deduplicated_and_capped() {
    let nested = |shading: &str, depth: usize, props: &str| {
        let open = format!("<w:tbl><w:tr><w:tc><w:tcPr>{shading}</w:tcPr>").repeat(depth);
        format!(
            "{open}{}{}",
            para("", &run(props, "x")),
            "</w:tc></w:tr></w:tbl>".repeat(depth)
        )
    };
    let two_candidates =
        r#"<w:shd w:val="pct50" w:color="123456" w:themeColor="accent1" w:fill="auto"/>"#;
    let extracted = read_body(&nested(two_candidates, 40, r#"<w:color w:val="ABCDEF"/>"#));
    assert_eq!(hidden(&extracted), [(1, "undecided")]);
    // Stripes over stripes add the same black at every level: two colours, however deep.
    let stripes = r#"<w:shd w:val="horzStripe" w:color="000000" w:fill="auto"/>"#;
    let extracted = read_body(&nested(stripes, 70, WHITE));
    assert_eq!(hidden(&extracted), [(1, "low_contrast")]);
}

/// One row per fail-closed rule: removing the rule changes what the row reads.
#[test]
fn docx_each_fail_closed_rule_is_pinned() {
    let unknown =
        |inner: &str| format!(r#"<x:wrap xmlns:x="urn:example:unknown">{inner}</x:wrap>"#);
    let white_fill = r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#;
    let anchored = |frame_props: &str, sp_pr: &str, marker: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box("anchor", frame_props, sp_pr, &para("", &run("", marker)))
        )
    };
    let inline = |sp_pr: &str, marker: &str| {
        format!(
            "<w:p>{}</w:p>",
            shape_box("inline", "", sp_pr, &para("", &run("", marker)))
        )
    };
    let vml = |attributes: &str, inner: &str, marker: &str| {
        format!(
            r#"<w:p><w:r><w:pict><v:shape {attributes}>{inner}<v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"#,
            para("", &run("", marker))
        )
    };
    let beside = |object: &str| format!("<w:p>{}{object}{}</w:p>", run("", "1"), run("", "00"));
    let fields = |inner: &str| format!("<w:p>{inner}</w:p>");
    let begin = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#;
    let instr = |text: &str| {
        format!(r#"<w:r><w:instrText xml:space="preserve">{text}</w:instrText></w:r>"#)
    };
    let separate = r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#;
    let end = r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#;
    let result = |text: &str| format!("<w:r><w:t>{text}</w:t></w:r>");
    let styled_table = |style: &str, look: &str, rows: &[Vec<String>]| {
        table(&format!(r#"<w:tblStyle w:val="{style}"/>{look}"#), rows)
    };
    let black_shading = r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#;

    type Row = (&'static str, Docx, &'static str, Vec<(u32, &'static str)>);
    let rows: Vec<Row> = vec![
        // Shapes.
        (
            "VML display:none",
            Docx::new(&vml(r#"style="position:absolute;display:none" fillcolor="white""#, "", "x")),
            "",
            vec![(2, "hidden_shape")],
        ),
        (
            "a hidden wps:cNvPr",
            Docx::new(&anchored(
                "",
                white_fill,
                "x",
            ).replace("<wps:spPr>", r#"<wps:cNvPr id="2" name="s" hidden="1"/><wps:spPr>"#)),
            "",
            vec![(2, "hidden_shape")],
        ),
        (
            "an unparseable docPr hidden",
            Docx::new(&anchored(r#"<wp:docPr id="1" name="b" hidden="maybe"/>"#, white_fill, "x")),
            "",
            vec![(2, "undecided")],
        ),
        (
            "simplePos off the page",
            Docx::new(&anchored(
                r#"<wp:simplePos x="40000000" y="0"/>"#,
                white_fill,
                "x",
            ).replace("<wp:anchor>", r#"<wp:anchor simplePos="1">"#)),
            "",
            vec![(2, "off_page")],
        ),
        (
            "off the page on the negative side",
            Docx::new(&anchored(
                r#"<wp:positionV relativeFrom="page"><wp:posOffset>-40000000</wp:posOffset></wp:positionV><wp:extent cx="914400" cy="914400"/>"#,
                white_fill,
                "x",
            )),
            "",
            vec![(2, "off_page")],
        ),
        (
            "an offset that does not parse",
            Docx::new(&anchored(
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>far</wp:posOffset></wp:positionH>"#,
                white_fill,
                "x",
            )),
            "",
            vec![(2, "undecided")],
        ),
        // A position written twice: which Word takes is not known.
        (
            "an offset written twice",
            Docx::new(&anchored(
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>40000000</wp:posOffset><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
                white_fill,
                "x",
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a position written twice",
            Docx::new(&anchored(
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>40000000</wp:posOffset></wp:positionH><wp:positionH relativeFrom="page"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
                white_fill,
                "x",
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a simple position written twice",
            Docx::new(&anchored(
                r#"<wp:simplePos x="40000000" y="0"/><wp:simplePos x="0" y="0"/>"#,
                white_fill,
                "x",
            ).replace("<wp:anchor>", r#"<wp:anchor simplePos="1">"#)),
            "",
            vec![(2, "undecided")],
        ),
        (
            "an inline shape is never off the page",
            Docx::new(&inline(white_fill, "x").replace(
                "<wp:inline>",
                r#"<wp:inline><wp:positionH relativeFrom="page"><wp:posOffset>40000000</wp:posOffset></wp:positionH>"#,
            )),
            "x",
            vec![],
        ),
        ("a pattern fill", Docx::new(&inline("<a:pattFill/>", "x")), "", vec![(2, "unknown_paint")]),
        ("a picture fill", Docx::new(&inline("<a:blipFill/>", "x")), "", vec![(2, "unknown_paint")]),
        ("a group fill", Docx::new(&inline("<a:grpFill/>", "x")), "", vec![(2, "unknown_paint")]),
        (
            "two fills, the last none",
            Docx::new(&inline(&format!("{white_fill}<a:noFill/>"), "x")),
            "",
            vec![(2, "unknown_paint")],
        ),
        (
            "two colours in one fill",
            Docx::new(&inline(
                r#"<a:solidFill><a:srgbClr val="000000"/><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
                "x",
            )),
            "",
            vec![(2, "unknown_paint")],
        ),
        (
            "VML opacity",
            Docx::new(&vml(r#"fillcolor="white""#, r#"<v:fill opacity="0.5"/>"#, "x")),
            "",
            vec![(2, "unknown_paint")],
        ),
        (
            "VML filled off, floating",
            Docx::new(&vml(r#"style="position:absolute" filled="f""#, "", "x")),
            "",
            vec![(2, "unknown_paint")],
        ),
        // VML's own booleans: an inline box filled `f` shows the paragraph it sits in, one
        // filled `t` its colour.
        (
            "VML filled f, inline",
            Docx::new(&vml(r#"filled="f""#, "", "x")),
            "x",
            vec![],
        ),
        (
            "VML filled t",
            Docx::new(
                &vml(r#"filled="t" fillcolor="black""#, "", "x")
                    .replace("<w:rPr></w:rPr>", &format!("<w:rPr>{BLACK}</w:rPr>")),
            ),
            "",
            vec![(2, "low_contrast")],
        ),
        (
            "style fill 0 is no fill",
            Docx::new(&inline("", "x").replace(
                "</wps:spPr>",
                r#"</wps:spPr><wps:style><a:fillRef idx="0"/></wps:style>"#,
            )),
            "x",
            vec![],
        ),
        (
            "another style fill is the theme's, unread",
            Docx::new(&inline("", "x").replace(
                "</wps:spPr>",
                r#"</wps:spPr><wps:style><a:fillRef idx="1"/></wps:style>"#,
            )),
            "",
            vec![(2, "unknown_paint")],
        ),
        (
            "two font colours",
            Docx::new(&inline(white_fill, "x").replace(
                "</wps:spPr>",
                r#"</wps:spPr><wps:style><a:fontRef idx="minor"><a:srgbClr val="000000"/></a:fontRef><a:fontRef idx="minor"/></wps:style>"#,
            )),
            "",
            vec![(2, "unknown_paint")],
        ),
        (
            "a text colour transform",
            Docx::new(&inline(white_fill, "x").replace(
                "</wps:spPr>",
                r#"</wps:spPr><wps:style><a:fontRef idx="minor"><a:srgbClr val="000000"><a:lumMod val="10000"/></a:srgbClr></a:fontRef></wps:style>"#,
            )),
            "",
            vec![(2, "unknown_paint")],
        ),
        (
            "text in no shape",
            Docx::new(&format!(
                "<w:p><w:r><w:pict><w:txbxContent>{}</w:txbxContent></w:pict></w:r></w:p>",
                para("", &run("", "x"))
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "text in a shape's extensions",
            Docx::new(&inline(
                &format!(
                    r#"{white_fill}<a:extLst><a:ext uri="{{00000000-0000-0000-0000-000000000000}}">{}</a:ext></a:extLst>"#,
                    unknown("note")
                ),
                "x",
            )),
            "x",
            vec![],
        ),
        (
            "a shape property in undecided markup",
            Docx::new(&inline(&unknown(white_fill), "x")),
            "",
            vec![(2, "unknown_namespace")],
        ),
        (
            "a fill after the text box",
            Docx::new(&vml(r#"fillcolor="white""#, "", "x").replace(
                "</v:textbox>",
                r#"</v:textbox><v:fill color="black"/>"#,
            )),
            "error",
            vec![],
        ),
        (
            "shape properties after the text box",
            Docx::new(&inline(white_fill, "x").replace("</wps:wsp>", "<wps:spPr/></wps:wsp>")),
            "error",
            vec![],
        ),
        (
            "a shape style after the text box",
            Docx::new(&inline(white_fill, "x").replace("</wps:wsp>", "<wps:style/></wps:wsp>")),
            "error",
            vec![],
        ),
        // Where shapes and their text stand.
        (
            "a shape in a group in a group",
            Docx::new(&inline(white_fill, "x").replace("<wps:wsp>", r#"<wpg:wgp xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><wpg:grpSp><wps:wsp>"#).replace("</wps:wsp>", "</wps:wsp></wpg:grpSp></wpg:wgp>")),
            "x",
            vec![],
        ),
        (
            "a shape in a canvas",
            Docx::new(&inline(white_fill, "x").replace("<wps:wsp>", r#"<wpc:wpc xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas"><wps:wsp>"#).replace("</wps:wsp>", "</wps:wsp></wpc:wpc>")),
            "x",
            vec![],
        ),
        (
            "a VML shape directly in another",
            Docx::new(&vml(r#"fillcolor="white""#, "", "x").replace("<v:textbox>", r#"<v:rect fillcolor="white"><v:textbox>"#).replace("</v:textbox>", "</v:textbox></v:rect>")),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a graphic outside a frame",
            Docx::new(&inline(white_fill, "x").replace(
                "</wps:spPr>",
                &format!(
                    r#"</wps:spPr><a:graphic><a:graphicData><wps:wsp><wps:spPr>{white_fill}</wps:spPr><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic>"#,
                    para("", &run("", "y"))
                ),
            )),
            "x",
            vec![(2, "undecided")],
        ),
        (
            "graphic data outside a graphic",
            Docx::new(&inline(white_fill, "x").replace("<a:graphic>", "").replace("</a:graphic>", "")),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a shape's text box in its frame",
            Docx::new(&format!(
                "<w:p><w:r><w:drawing><wp:inline><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wp:inline></w:drawing></w:r></w:p>",
                para("", &run("", "x"))
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a VML text box in a group",
            Docx::new(&format!(
                r#"<w:p><w:r><w:pict><v:group fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:group></w:pict></w:r></w:p>"#,
                para("", &run("", "x"))
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a VML shape inside a VML group's fill",
            Docx::new(&format!(
                r#"<w:p><w:r><w:pict><v:group style="position:absolute;width:100pt;height:100pt" coordsize="21600,21600"><v:fill><v:shape fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></v:fill></v:group></w:pict></w:r></w:p>"#,
                para("", &run("", "x"))
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a DrawingML shape directly in a VML group",
            Docx::new(&format!(
                r#"<w:p><w:r><w:pict><v:group style="position:absolute;width:100pt;height:100pt" coordsize="21600,21600"><wps:wsp><wps:spPr>{white_fill}</wps:spPr><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp></v:group></w:pict></w:r></w:p>"#,
                para("", &run("", "x"))
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a VML text box in a DrawingML shape",
            Docx::new(
                &inline(white_fill, "x")
                    .replace("<wps:txbx>", "<v:textbox>")
                    .replace("</wps:txbx>", "</v:textbox>"),
            ),
            "",
            vec![(2, "undecided")],
        ),
        // A shape's text is one story: one written twice is markup Word does not lay out as the
        // reader would read it.
        (
            "a second wps:txbx in a shape",
            Docx::new(&inline(white_fill, "x").replace(
                "</wps:txbx>",
                &format!(
                    "</wps:txbx><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx>",
                    para("", &run("", "y"))
                ),
            )),
            "error",
            vec![],
        ),
        (
            "a second story in one wps:txbx",
            Docx::new(&inline(white_fill, "x").replace(
                "</wps:txbx>",
                &format!("<w:txbxContent>{}</w:txbxContent></wps:txbx>", para("", &run("", "y"))),
            )),
            "error",
            vec![],
        ),
        (
            "a second v:textbox in a shape",
            Docx::new(&vml(r#"fillcolor="white""#, "", "x").replace(
                "</v:textbox>",
                &format!(
                    "</v:textbox><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox>",
                    para("", &run("", "y"))
                ),
            )),
            "error",
            vec![],
        ),
        (
            "a second story in one v:textbox",
            Docx::new(&vml(r#"fillcolor="white""#, "", "x").replace(
                "</v:textbox>",
                &format!("<w:txbxContent>{}</w:txbxContent></v:textbox>", para("", &run("", "y"))),
            )),
            "error",
            vec![],
        ),
        (
            "two shapes in a group, a story each",
            Docx::new(&inline(white_fill, "x").replace(
                "<wps:wsp>",
                &format!(
                    r#"<wpg:wgp xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><wps:wsp><wps:spPr>{white_fill}</wps:spPr><wps:txbx><w:txbxContent>{}</w:txbxContent></wps:txbx></wps:wsp><wps:wsp>"#,
                    para("", &run("", "w"))
                ),
            ).replace("</wps:wsp></a:graphicData>", "</wps:wsp></wpg:wgp></a:graphicData>")),
            "w\nx",
            vec![],
        ),
        // A text box in a paragraph outside any cell of its table.
        (
            "a box whose host is in no cell",
            Docx::new(&format!(
                "<w:tbl><w:tr>{}</w:tr></w:tbl>",
                inline(white_fill, "x")
            )),
            "",
            vec![(2, "undecided")],
        ),
        // The host paragraph's cell gives it its table style's paragraph shading.
        (
            "an inline box's host takes its cell's table level",
            Docx::new(&styled_table(
                "Shaded",
                "",
                &[vec![cell(
                    "",
                    &format!(
                        "<w:p>{}</w:p>",
                        shape_box("inline", "", "", &para("", &run(BLACK, "x")))
                    ),
                )]],
            ))
            .styles(&table_style(
                "Shaded",
                &format!("<w:pPr>{black_shading}</w:pPr>"),
            )),
            "",
            vec![(2, "low_contrast")],
        ),
        // Gaps.
        ("a note mark", Docx::new(&beside(r#"<w:r><w:footnoteReference w:id="1"/></w:r>"#)), "1\u{FFFC}00", vec![]),
        ("a page number", Docx::new(&beside("<w:r><w:pgNum/></w:r>")), "1\u{FFFC}00", vec![]),
        ("ruby text", Docx::new(&beside("<w:r><w:ruby/></w:r>")), "1\u{FFFC}00", vec![]),
        (
            "an embedded object",
            Docx::new(&beside("<w:r><w:object/></w:r>")),
            "1\u{FFFC}00",
            vec![],
        ),
        (
            "a picture after a run's text",
            Docx::new(&format!(
                "<w:p><w:r><w:t>1</w:t><w:drawing/></w:r>{}</w:p>",
                run("", "250")
            )),
            "1\u{FFFC}250",
            vec![],
        ),
        (
            "a run holding only dropped text",
            Docx::new(&beside(&format!("<w:r>{}</w:r>", unknown("<w:t>2</w:t>")))),
            "1\u{FFFC}00",
            vec![(1, "unknown_namespace")],
        ),
        (
            "a Symbol font control character",
            Docx::new(&beside(
                r#"<w:r><mc:AlternateContent xmlns:w16se="http://schemas.microsoft.com/office/word/2015/wordml/symex"><mc:Choice Requires="w16se"><w16se:symEx w16se:font="Symbol" w16se:char="0007"/></mc:Choice></mc:AlternateContent></w:r>"#,
            )),
            "1\u{FFFC}00",
            vec![],
        ),
        // Fields.
        (
            "a field end in dropped markup",
            Docx::new(&fields(&format!(
                "{begin}{}{separate}{}<w:r>{}</w:r>{}{end}",
                instr(" REF x "),
                result("r"),
                unknown(r#"<w:fldChar w:fldCharType="end"/>"#),
                result("after")
            ))),
            "r\u{FFFC}",
            vec![(1, "unknown_namespace")],
        ),
        (
            "an unknown field character",
            Docx::new(&fields(&format!(
                r#"{begin}{}<w:r><w:fldChar w:fldCharType="bogus"/></w:r>{separate}{}{end}"#,
                instr(" REF x "),
                result("r")
            ))),
            "",
            vec![(1, "unknown_namespace")],
        ),
        (
            "a second separate",
            Docx::new(&fields(&format!(
                "{begin}{}{separate}{}{separate}{}{end}",
                instr(" REF x "),
                result("a"),
                result("b")
            ))),
            "a\u{FFFC}",
            vec![(1, "unknown_namespace")],
        ),
        (
            "hidden instruction text",
            Docx::new(&fields(&format!(
                "{begin}<w:r>{}</w:r>{separate}{}{end}",
                unknown(r#"<w:instrText> REF x </w:instrText>"#),
                result("r")
            ))),
            "",
            vec![(1, "unknown_namespace")],
        ),
        (
            "markup inside an instruction",
            Docx::new(&fields(&format!(
                "{begin}<w:r><w:instrText> REF<w:b/> x </w:instrText></w:r>{separate}{}{end}",
                result("r")
            ))),
            "",
            vec![(1, "unknown_namespace")],
        ),
        // Settings.
        (
            "updateFields where Word may not read it",
            Docx::new(&fields(&format!("{begin}{}{separate}{}{end}", instr(" REF x "), result("r"))))
                .settings(&unknown(r#"<w:updateFields w:val="0"/>"#)),
            "",
            vec![(1, "field_result")],
        ),
        (
            "updateFields written twice, on then off",
            Docx::new(&fields(&format!("{begin}{}{separate}{}{end}", instr(" REF x "), result("r"))))
                .settings(r#"<w:updateFields/><w:updateFields w:val="0"/>"#),
            "",
            vec![(1, "field_result")],
        ),
        (
            "updateFields written off twice",
            Docx::new(&fields(&format!("{begin}{}{separate}{}{end}", instr(" REF x "), result("r"))))
                .settings(r#"<w:updateFields w:val="0"/><w:updateFields w:val="0"/>"#),
            "r",
            vec![],
        ),
        (
            "displayBackgroundShape where Word may not read it",
            Docx::from_document(&document_with(
                r#"<w:background w:color="000000"/>"#,
                &para("", &run(WHITE, "x")),
            ))
            .settings(&unknown("<w:displayBackgroundShape/>")),
            "",
            vec![(1, "low_contrast")],
        ),
        (
            "displayBackgroundShape written twice, off then on",
            Docx::from_document(&document_with(
                r#"<w:background w:color="000000"/>"#,
                &para("", &run(WHITE, "x")),
            ))
            .settings(r#"<w:displayBackgroundShape w:val="0"/><w:displayBackgroundShape/>"#),
            "",
            vec![(1, "low_contrast")],
        ),
        (
            "a second page colour",
            Docx::from_document(&document_with(
                r#"<w:background w:color="000000"/><w:background w:color="FFFFFF"/>"#,
                &para("", &run(BLACK, "x")),
            ))
            .settings("<w:displayBackgroundShape/>"),
            "",
            vec![(1, "low_contrast")],
        ),
        (
            "a scheme mapping written twice, two ways",
            Docx::new(&para(
                "",
                &run(r#"<w:color w:val="FFFFFF" w:themeColor="background1"/>"#, "x"),
            ))
            .theme(&theme(&[]))
            .settings(r#"<w:clrSchemeMapping w:bg1="light1"/><w:clrSchemeMapping w:bg1="dark1"/>"#),
            "",
            vec![(1, "low_contrast")],
        ),
        (
            "a picture at a paragraph's end",
            Docx::new(&format!("<w:p>{}<w:r><w:drawing/></w:r></w:p>", run("", "x"))),
            "x",
            vec![],
        ),
        // Styles and tables.
        (
            "two rPrDefault",
            Docx::new(&para("", &run("", "x"))).styles(
                r#"<w:docDefaults><w:rPrDefault/><w:rPrDefault><w:rPr/></w:rPrDefault></w:docDefaults>"#,
            ),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a style's rPr written twice",
            Docx::new(&para(r#"<w:pStyle w:val="S"/>"#, &run("", "x")))
                .styles(r#"<w:style w:type="paragraph" w:styleId="S"><w:rPr/><w:rPr/></w:style>"#),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a region's tcPr written twice",
            Docx::new(&styled_table("T", "", &[vec![cell("", &para("", &run("", "x")))]]))
                .styles(&table_style(
                    "T",
                    r#"<w:tblStylePr w:type="wholeTable"><w:tcPr/><w:tcPr/></w:tblStylePr>"#,
                )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a region written twice",
            Docx::new(&styled_table("T", "", &[vec![cell("", &para("", &run("", "x")))]]))
                .styles(&table_style(
                    "T",
                    &[region("wholeTable", "FFFFFF"), region("wholeTable", "000000")].concat(),
                )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a second tblPr",
            Docx::new(&table("", &[vec![cell("", &para("", &run("", "x")))]]).replace(
                "</w:tblPr>",
                "</w:tblPr><w:tblPr/>",
            )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a second trPr",
            Docx::new(&table("", &[vec![cell("", &para("", &run("", "x")))]]).replace(
                "<w:tr>",
                "<w:tr><w:trPr/><w:trPr/>",
            )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a second tcPr",
            Docx::new(&table("", &[vec![cell("", &para("", &run("", "x")))]]).replace(
                "</w:tcPr>",
                "</w:tcPr><w:tcPr/>",
            )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a vMerge that does not parse",
            Docx::new(&table("", &[vec![cell(r#"<w:vMerge w:val="bogus"/>"#, &para("", &run("", "x")))]])),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a table outside a block's place",
            Docx::new(&format!(
                "<w:p>{}</w:p>",
                table("", &[vec![cell("", &para("", &run("", "x")))]])
            )),
            "",
            vec![(2, "undecided")],
        ),
        (
            "a row outside a table's place",
            Docx::new(&format!(
                "<w:tbl><w:tblPr/><w:notAnElement><w:tr>{}</w:tr></w:notAnElement></w:tbl>",
                cell("", &para("", &run("", "x")))
            )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a cell outside a row's place",
            Docx::new(&format!(
                "<w:tbl><w:tblPr/><w:tr><w:notAnElement>{}</w:notAnElement></w:tr></w:tbl>",
                cell("", &para("", &run("", "x")))
            )),
            "",
            vec![(1, "undecided")],
        ),
        (
            "a cell's properties after a nested table",
            Docx::new(&format!(
                "<w:tbl><w:tblPr/><w:tr><w:tc>{}<w:tcPr/></w:tc></w:tr></w:tbl>",
                table("", &[vec![cell("", &para("", &run("", "x")))]])
            )),
            "error",
            vec![],
        ),
        (
            "a deleted cell moves the cells after it",
            Docx::new(&styled_table(
                "Columns",
                r#"<w:tblLook w:firstRow="0" w:firstColumn="0" w:noHBand="1"/>"#,
                &[vec![
                    cell("", &para("", &run("", "a"))),
                    cell(r#"<w:cellDel w:id="1" w:author="a"/>"#, &para("", &run("", "b"))),
                    cell("", &para("", &run(BLACK, "c"))),
                ]],
            ))
            .styles(&table_style(
                "Columns",
                &[
                    r#"<w:tblPr><w:tblStyleColBandSize w:val="1"/></w:tblPr>"#,
                    &region("band1Vert", "000000"),
                ]
                .concat(),
            )),
            "a\nc",
            vec![],
        ),
        // Markup compatibility.
        (
            "a choice requiring markup compatibility itself",
            Docx::new(r#"<w:p><mc:AlternateContent><mc:Choice Requires="mc"><w:r><w:t>x</w:t></w:r></mc:Choice></mc:AlternateContent></w:p>"#),
            "x",
            vec![],
        ),
        (
            "alternate content directly in alternate content",
            Docx::new(r#"<w:p><mc:AlternateContent><mc:AlternateContent><mc:Choice Requires="w14"><w:r><w:t>x</w:t></w:r></mc:Choice></mc:AlternateContent></mc:AlternateContent></w:p>"#),
            "",
            vec![(1, "unknown_namespace")],
        ),
        (
            "a choice outside alternate content",
            Docx::new(r#"<w:p><mc:Choice Requires="w14"><w:r><w:t>x</w:t></w:r></mc:Choice></w:p>"#),
            "",
            vec![(1, "unknown_namespace")],
        ),
        (
            "another markup compatibility element",
            Docx::new(r#"<w:p><mc:Other><w:r><w:t>x</w:t></w:r></mc:Other></w:p>"#),
            "",
            vec![(1, "unknown_namespace")],
        ),
        // Parts.
        (
            "a styles part related but missing",
            Docx::new(&para("", &run("", "x"))).related_missing("styles", "styles.xml"),
            "error",
            vec![],
        ),
        (
            "a second body",
            Docx::from_document(&document(&para("", &run("", "x"))).replace(
                "</w:body>",
                "</w:body><w:body/>",
            )),
            "error",
            vec![],
        ),
    ];
    for (case, docx, shown, warned) in rows {
        match docs::extract(MediaKind::Docx, &docx.build(), &limits()) {
            Ok(extracted) => {
                assert_ne!(shown, "error", "{case}: read");
                assert_eq!(text(&extracted), shown, "{case}");
                assert_eq!(hidden(&extracted), warned, "{case}");
            }
            Err(error) => assert_eq!(shown, "error", "{case}: {error:?}"),
        }
    }

    // A header related twice is read once.
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {NAMESPACES}>{}</w:hdr>"#,
        para("", &run("", "header"))
    );
    let twice = Docx::new("")
        .related("header", "header1.xml", header.as_bytes())
        .related_missing("header", "header1.xml");
    assert_eq!(text(&read(&twice)), "header");
}

/// Every namespace the reader understands is one Word writes: a choice requiring it is read.
#[test]
fn docx_every_understood_namespace_is_read() {
    for uri in [
        "http://schemas.microsoft.com/office/word/2010/wordml",
        "http://schemas.microsoft.com/office/word/2012/wordml",
        "http://schemas.microsoft.com/office/word/2015/wordml/symex",
        "http://schemas.microsoft.com/office/word/2016/wordml/cid",
        "http://schemas.microsoft.com/office/word/2018/wordml",
        "http://schemas.microsoft.com/office/word/2018/wordml/cex",
        "http://schemas.microsoft.com/office/word/2020/wordml/sdtdatahash",
        "http://schemas.microsoft.com/office/word/2023/wordml/word16du",
        "http://schemas.microsoft.com/office/word/2024/wordml/sdtformatlock",
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingShape",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas",
        "http://schemas.openxmlformats.org/drawingml/2006/main",
        "http://schemas.openxmlformats.org/drawingml/2006/picture",
        "urn:schemas-microsoft-com:vml",
        "urn:schemas-microsoft-com:office:office",
        "urn:schemas-microsoft-com:office:word",
    ] {
        let body = format!(
            r#"<w:p><mc:AlternateContent xmlns:z="{uri}"><mc:Choice Requires="z"><w:r><w:t>choice</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>fallback</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>"#
        );
        assert_eq!(text(&read_body(&body)), "choice", "{uri}");
    }
}

fn read_with(docx: &Docx, limits: &DocLimits) -> Result<Extracted, DocError> {
    docs::extract(MediaKind::Docx, &docx.build(), limits)
}

/// One table of `rows` rows, each cell holding one run of black text named by its place.
fn black_grid(props: &str, rows: usize, columns: usize) -> String {
    let rows: Vec<Vec<String>> = (0..rows)
        .map(|row| {
            (0..columns)
                .map(|column| cell("", &para("", &run(BLACK, &format!("r{row}c{column}")))))
                .collect()
        })
        .collect();
    table(props, &rows)
}

fn lines(extracted: &Extracted) -> Vec<String> {
    extracted
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

/// Each region, flag and band size of a table style decides what lies beneath its cells; the
/// shaded cells hold black text on black.
#[test]
fn docx_table_style_regions_flags_and_band_sizes() {
    let black = "000000";
    let case = |style: &str, props: &str, rows: usize, columns: usize| {
        let styles = table_style("T", style);
        lines(&read(
            &Docx::new(&black_grid(
                &format!(r#"<w:tblStyle w:val="T"/>{props}"#),
                rows,
                columns,
            ))
            .styles(&styles),
        ))
    };
    let look = |attributes: &str| format!("<w:tblLook {attributes}/>");
    let none = r#"w:firstRow="0" w:lastRow="0" w:firstColumn="0" w:lastColumn="0""#;
    // The style's own cell shading.
    assert!(
        case(
            &format!(r#"<w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{black}"/></w:tcPr>"#),
            "",
            1,
            1
        )
        .is_empty()
    );
    // A region's run properties: white header text on white.
    let styles = table_style(
        "T",
        &format!(r#"<w:tblStylePr w:type="firstRow"><w:rPr>{WHITE}</w:rPr></w:tblStylePr>"#),
    );
    let body = table(
        r#"<w:tblStyle w:val="T"/><w:tblLook w:firstRow="1"/>"#,
        &[
            vec![cell("", &para("", &run("", "header")))],
            vec![cell("", &para("", &run("", "body")))],
        ],
    );
    assert_eq!(text(&read(&Docx::new(&body).styles(&styles))), "body");
    // The last row and the last column, when the look turns them on.
    let regions = [region("lastRow", black), region("lastCol", black)].concat();
    assert_eq!(
        case(
            &regions,
            &look(
                r#"w:firstRow="0" w:firstColumn="0" w:lastRow="1" w:lastColumn="1" w:noHBand="1" w:noVBand="1""#
            ),
            2,
            2
        ),
        ["r0c0"]
    );
    assert_eq!(
        case(
            &regions,
            &look(&format!(r#"{none} w:noHBand="1" w:noVBand="1""#)),
            2,
            2
        )
        .len(),
        4
    );
    // Row bands, and the look that turns them off.
    let rows = region("band1Horz", black);
    assert_eq!(
        case(
            &rows,
            &look(&format!(r#"{none} w:noHBand="0" w:noVBand="1""#)),
            3,
            1
        ),
        ["r1c0"]
    );
    assert_eq!(
        case(
            &rows,
            &look(&format!(r#"{none} w:noHBand="1" w:noVBand="1""#)),
            3,
            1
        )
        .len(),
        3
    );
    // Column bands: the first band is band1Vert, the second band2Vert.
    let columns = [region("band1Vert", black), region("band2Vert", "FFFFFF")].concat();
    assert_eq!(
        case(
            &columns,
            &look(&format!(r#"{none} w:noHBand="1" w:noVBand="0""#)),
            1,
            3
        ),
        ["r0c1"]
    );
    assert_eq!(
        case(
            &columns,
            &look(&format!(r#"{none} w:noHBand="1" w:noVBand="1""#)),
            1,
            3
        )
        .len(),
        3
    );
    // Band sizes of 2.
    let sized = |size: &str, band: &str| {
        format!(
            r#"<w:tblPr><w:{size} w:val="2"/></w:tblPr>{}"#,
            region(band, black)
        )
    };
    assert_eq!(
        case(
            &sized("tblStyleRowBandSize", "band1Horz"),
            &look(&format!(r#"{none} w:noHBand="0" w:noVBand="1""#)),
            3,
            1
        ),
        ["r2c0"]
    );
    assert_eq!(
        case(
            &sized("tblStyleColBandSize", "band1Vert"),
            &look(&format!(r#"{none} w:noHBand="1" w:noVBand="0""#)),
            1,
            3
        ),
        ["r0c2"]
    );
    // Word applies row bands, then column bands: the column band wins where both are on.
    let both = [region("band1Horz", black), region("band1Vert", "FFFFFF")].concat();
    assert_eq!(
        case(
            &both,
            &look(&format!(r#"{none} w:noHBand="0" w:noVBand="0""#)),
            1,
            1
        ),
        ["r0c0"]
    );
    // A row's own look (w:tblPrEx) overrides the table's.
    let styles = table_style("T", &rows);
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblStyle w:val="T"/>{}</w:tblPr><w:tr><w:tblPrEx><w:tblLook {none} w:noHBand="1" w:noVBand="1"/></w:tblPrEx>{}</w:tr><w:tr>{}</w:tr><w:tr>{}</w:tr></w:tbl>"#,
        look(&format!(r#"{none} w:noHBand="0" w:noVBand="1""#)),
        cell("", &para("", &run(BLACK, "ownlook"))),
        cell("", &para("", &run(BLACK, "bandtwo"))),
        cell("", &para("", &run(BLACK, "tablelook"))),
    );
    assert_eq!(
        text(&read(&Docx::new(&body).styles(&styles))),
        "ownlook\nbandtwo"
    );
}

#[test]
fn docx_default_character_and_table_styles_apply() {
    let styles = r#"<w:style w:type="character" w:default="1" w:styleId="Quiet"><w:rPr><w:vanish/></w:rPr></w:style><w:style w:type="character" w:styleId="Loud"/><w:style w:type="table" w:default="1" w:styleId="Dark"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="000000"/></w:tcPr></w:style>"#;
    let body = [
        para("", &run("", "defaultchar")),
        para("", &run(r#"<w:rStyle w:val="Loud"/>"#, "ownchar")),
        table(
            "",
            &[vec![cell(
                "",
                &para(
                    "",
                    &run(
                        &format!(r#"<w:rStyle w:val="Loud"/>{BLACK}"#),
                        "defaulttable",
                    ),
                ),
            )]],
        ),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(styles));
    assert_eq!(text(&extracted), "ownchar");
    assert_eq!(hidden(&extracted), [(1, "vanish"), (3, "low_contrast")]);
}

#[test]
fn docx_style_ids_and_kinds_that_do_not_agree_are_undecided() {
    let styles = concat!(
        r#"<w:style w:type="character" w:styleId="C"><w:rPr><w:b/></w:rPr></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Q"><w:rPr><w:b/></w:rPr></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Cross"><w:basedOn w:val="C"/></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Same"><w:basedOn w:val="Q"/></w:style>"#,
        // The same definition twice is one style.
        r#"<w:style w:type="paragraph" w:styleId="Twice"/>"#,
        r#"<w:style w:type="paragraph" w:styleId="Twice"/>"#,
    );
    let body = [
        para(r#"<w:pStyle w:val="Cross"/>"#, &run("", "crosskind")),
        para(r#"<w:pStyle w:val="Same"/>"#, &run("", "samekind")),
        para(r#"<w:pStyle w:val="Twice"/>"#, &run("", "twice")),
    ]
    .concat();
    let extracted = read(&Docx::new(&body).styles(styles));
    assert_eq!(text(&extracted), "samekind\ntwice");
    assert_eq!(hidden(&extracted), [(1, "undecided")]);
}

#[test]
fn docx_no_highlight_and_automatic_fill_show_the_layer_beneath() {
    let black_cell = |content: &str| {
        table(
            "",
            &[vec![cell(
                r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#,
                content,
            )]],
        )
    };
    let body = [
        black_cell(&para(
            "",
            &run(
                &format!(r#"{BLACK}<w:highlight w:val="none"/>"#),
                "nohighlight",
            ),
        )),
        black_cell(&para(
            r#"<w:shd w:val="clear" w:color="auto" w:fill="auto"/>"#,
            &run(BLACK, "autofill"),
        )),
        para(
            "",
            &run(&format!(r#"{BLACK}<w:highlight w:val="none"/>"#), "onwhite"),
        ),
    ]
    .concat();
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "onwhite");
    assert_eq!(
        hidden(&extracted),
        [(1, "low_contrast"), (2, "low_contrast")]
    );
}

#[test]
fn docx_vanish_first_reading_and_complex_scripts() {
    // docDefaults on, the paragraph style on and the character style off: the exclusive or of
    // the levels set (on, off) hides, though the second reading shows it.
    let styles = concat!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:vanish/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        r#"<w:style w:type="paragraph" w:styleId="POn"><w:rPr><w:vanish/></w:rPr></w:style>"#,
        r#"<w:style w:type="character" w:styleId="COff"><w:rPr><w:vanish w:val="0"/></w:rPr></w:style>"#,
    );
    let body = para(
        r#"<w:pStyle w:val="POn"/>"#,
        &run(r#"<w:rStyle w:val="COff"/>"#, "firstreading"),
    );
    assert_eq!(
        hidden(&read(&Docx::new(&body).styles(styles))),
        [(1, "vanish")]
    );
    // Word sizes a complex-script run (w:cs or w:rtl) by w:szCs, whatever its script.
    let body = paragraphs(&[
        (
            r#"<w:cs/><w:sz w:val="24"/><w:szCs w:val="2"/>"#,
            "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}",
        ),
        (
            r#"<w:sz w:val="24"/><w:szCs w:val="2"/>"#,
            "\u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA}",
        ),
    ]);
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "\u{05E2}\u{05D1}\u{05E8}\u{05D9}\u{05EA}");
    assert_eq!(hidden(&extracted), [(1, "tiny")]);
}

#[test]
fn docx_contrast_exactly_at_the_floor_is_visible() {
    let grey = colour::parse_hex("767676").unwrap();
    let floor = colour::contrast_ratio(grey, Rgb::WHITE);
    let limits = DocLimits {
        min_contrast: floor,
        ..limits()
    };
    let docx = Docx::new(&paragraphs(&[(r#"<w:color w:val="767676"/>"#, "atfloor")]));
    assert_eq!(text(&read_with(&docx, &limits).unwrap()), "atfloor");
    let limits = DocLimits {
        min_contrast: floor + 0.01,
        ..limits
    };
    assert_eq!(text(&read_with(&docx, &limits).unwrap()), "");
}

#[test]
fn docx_paragraph_ids_and_external_relationships() {
    let body = [
        r#"<w:p w14:paraId="XYZ"><w:r><w:t>short</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="1A2B3C4D5"><w:r><w:t>long</w:t></w:r></w:p>"#,
        r#"<w:p w14:paraId="1A2B3C4G"><w:r><w:t>nothex</w:t></w:r></w:p>"#,
    ]
    .concat();
    for block in read_body(&body).blocks {
        assert!(
            matches!(&block.locator, Locator::DocxParagraph { para_id: None, .. }),
            "{block:?}"
        );
    }
    // A styles relationship whose target is External is never followed, though a part of that
    // name is in the package.
    let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml" TargetMode="External"/></Relationships>"#;
    let white_normal = styles(&format!(
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr>{WHITE}</w:rPr></w:style>"#
    ));
    let docx = Docx::new(&para("", &run("", "external")))
        .entry("word/_rels/document.xml.rels", rels.as_bytes())
        .entry("word/styles.xml", white_normal.as_bytes());
    assert_eq!(text(&read(&docx)), "external");
}

#[test]
fn docx_structure_word_cannot_lay_out_is_refused() {
    let extract = |body: &str| docs::extract(MediaKind::Docx, &Docx::new(body).build(), &limits());
    for (case, body, error) in [
        (
            "a cell's properties after its first paragraph",
            "<w:tbl><w:tr><w:tc><w:p/><w:tcPr/></w:tc></w:tr></w:tbl>".to_owned(),
            DocError::Malformed("docx_structure"),
        ),
        (
            "a table's properties after its first row",
            "<w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr><w:tblPr/></w:tbl>".to_owned(),
            DocError::Malformed("docx_structure"),
        ),
        (
            "a row's properties after its first cell",
            "<w:tbl><w:tr><w:tc><w:p/></w:tc><w:trPr/></w:tr></w:tbl>".to_owned(),
            DocError::Malformed("docx_structure"),
        ),
        (
            "a table look that does not parse",
            r#"<w:tbl><w:tblPr><w:tblLook w:firstRow="maybe"/></w:tblPr></w:tbl>"#.to_owned(),
            DocError::Malformed("docx_table"),
        ),
        (
            "a hexadecimal table look that does not parse",
            r#"<w:tbl><w:tblPr><w:tblLook w:val="zz"/></w:tblPr></w:tbl>"#.to_owned(),
            DocError::Malformed("docx_table"),
        ),
    ] {
        assert_eq!(extract(&body), Err(error), "{case}");
    }
    let bad_band = Docx::new("").styles(&table_style(
        "T",
        r#"<w:tblPr><w:tblStyleRowBandSize w:val="two"/></w:tblPr>"#,
    ));
    assert_eq!(
        read_with(&bad_band, &limits()),
        Err(DocError::Malformed("docx_table"))
    );
    let late_page = Docx::from_document(&format!(
        r#"<?xml version="1.0"?><w:document {NAMESPACES}><w:body/><w:background w:color="000000"/></w:document>"#
    ));
    assert_eq!(
        read_with(&late_page, &limits()),
        Err(DocError::Malformed("docx_structure"))
    );
    // Fields nest at most max_xml_depth deep.
    let shallow = DocLimits {
        max_xml_depth: 8,
        ..limits()
    };
    let begins = |n: usize| {
        Docx::new(&format!(
            "<w:p>{}</w:p>",
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#.repeat(n)
        ))
    };
    assert!(read_with(&begins(8), &shallow).is_ok());
    assert_eq!(
        read_with(&begins(9), &shallow),
        Err(DocError::TooComplex("field_depth"))
    );
    // No package relationships at all.
    let bare = zip(&[("word/document.xml", document("").as_bytes())], true);
    assert_eq!(
        docs::extract(MediaKind::Docx, &bare, &limits()),
        Err(DocError::Malformed("docx_rels"))
    );
}

#[test]
fn blocks_and_warnings_are_capped() {
    let two = DocLimits {
        max_blocks: 2,
        ..limits()
    };
    let visible = Docx::new(&paragraphs(&[("", "a"), ("", "b"), ("", "c")]));
    assert_eq!(
        read_with(&visible, &two),
        Err(DocError::TooComplex("blocks"))
    );
    let hidden_three = Docx::new(&paragraphs(&[(WHITE, "a"), (WHITE, "b"), (WHITE, "c")]));
    assert_eq!(
        read_with(&hidden_three, &two),
        Err(DocError::TooComplex("warnings"))
    );
    assert_eq!(
        docs::extract(MediaKind::Text, b"a\n\nb\n\nc", &two),
        Err(DocError::TooComplex("blocks"))
    );
    // A table's paragraphs wait for its end, and are counted as they are judged.
    let in_table = Docx::new(&table(
        "",
        &[vec![cell(
            "",
            &paragraphs(&[("", "a"), ("", "b"), ("", "c")]),
        )]],
    ));
    assert_eq!(
        read_with(&in_table, &two),
        Err(DocError::TooComplex("blocks"))
    );
}

/// Random hex in an XML comment: bytes that do not compress, so a flood passes the ratio cap.
fn incompressible(len: usize) -> String {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut out = String::from("<!--");
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push_str(&format!("{state:016x}"));
    }
    out.push_str("-->");
    out
}

#[test]
fn xml_elements_are_one_budget_for_the_package() {
    // A plain flood never reaches the element cap: the ratio cap stops it.
    let flood = |elements: usize| "<w:p/>".repeat(elements);
    let over = limits().max_xml_elements as usize + 1;
    let plain = Docx::new(&flood(over)).build();
    assert_eq!(
        docs::extract(MediaKind::Docx, &plain, &limits()),
        Err(DocError::TooComplex("zip_ratio"))
    );
    // Padded with bytes that do not compress, it passes the ratio cap and meets the element cap.
    let document = format!(
        "{}{}",
        document(&flood(over)),
        incompressible(over * 6 / 40)
    );
    let padded = Docx::from_document(&document).build();
    assert!(padded.len() as u64 <= limits().max_input_bytes);
    assert_eq!(
        docs::extract(MediaKind::Docx, &padded, &limits()),
        Err(DocError::TooComplex("xml_elements"))
    );
    // Two parts each under a budget of 1,000, together over it.
    let small = DocLimits {
        max_xml_elements: 1_000,
        ..limits()
    };
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {NAMESPACES}>{}</w:hdr>"#,
        flood(600)
    );
    assert!(read_with(&Docx::new(&flood(600)), &small).is_ok());
    assert!(
        read_with(
            &Docx::new("").related("header", "header1.xml", header.as_bytes()),
            &small
        )
        .is_ok()
    );
    assert_eq!(
        read_with(
            &Docx::new(&flood(600)).related("header", "header1.xml", header.as_bytes()),
            &small
        ),
        Err(DocError::TooComplex("xml_elements"))
    );
    // Elements with content draw on the budget as empty ones do.
    let open_flood = |elements: usize| "<w:p></w:p>".repeat(elements);
    assert!(read_with(&Docx::new(&open_flood(900)), &small).is_ok());
    assert_eq!(
        read_with(&Docx::new(&open_flood(1_000)), &small),
        Err(DocError::TooComplex("xml_elements"))
    );
    // The theme draws on the package's budget too.
    let themed = theme(&[]).replace(
        "</a:themeElements>",
        &format!(
            "<a:fontScheme name=\"x\">{}</a:fontScheme></a:themeElements>",
            "<a:b/>".repeat(600)
        ),
    );
    assert!(read_with(&Docx::new("").theme(&themed), &small).is_ok());
    assert_eq!(
        read_with(&Docx::new(&flood(600)).theme(&themed), &small),
        Err(DocError::TooComplex("xml_elements"))
    );
    assert_eq!(DocLimits::default().max_xml_elements, 2_000_000);
}

#[test]
fn runs_styles_and_relationships_are_capped() {
    let runs = |n: usize| {
        stored(&Docx::new(&format!(
            "<w:p>{}</w:p>",
            "<w:r><w:t>x</w:t></w:r>".repeat(n)
        )))
    };
    assert!(docs::extract(MediaKind::Docx, &runs(65_536), &limits()).is_ok());
    assert_eq!(
        docs::extract(MediaKind::Docx, &runs(65_537), &limits()),
        Err(DocError::TooComplex("paragraph_runs"))
    );
    let styles = |n: usize| {
        let defined: String = (0..n)
            .map(|at| format!(r#"<w:style w:styleId="s{at}"/>"#))
            .collect();
        stored(&Docx::new("").styles(&defined))
    };
    assert!(docs::extract(MediaKind::Docx, &styles(65_536), &limits()).is_ok());
    assert_eq!(
        docs::extract(MediaKind::Docx, &styles(65_537), &limits()),
        Err(DocError::TooComplex("styles"))
    );
    let header = format!(r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr {NAMESPACES}/>"#);
    let headers = |n: usize| {
        (1..n).fold(
            Docx::new("").related("header", "header1.xml", header.as_bytes()),
            |docx, _| docx.related_missing("header", "header1.xml"),
        )
    };
    assert!(read_with(&headers(2_000), &limits()).is_ok());
    assert_eq!(
        read_with(&headers(2_001), &limits()),
        Err(DocError::TooComplex("relationships"))
    );
    // Style ids the styles part defines are at most 256 bytes; a longer one named in the body
    // names no style.
    let id = |len: usize| "s".repeat(len);
    for (styles, expected) in [
        (format!(r#"<w:style w:styleId="{}"/>"#, id(256)), Ok(())),
        (
            format!(r#"<w:style w:styleId="{}"/>"#, id(257)),
            Err(DocError::TooComplex("style_id")),
        ),
        (
            format!(
                r#"<w:style w:styleId="s"><w:basedOn w:val="{}"/></w:style>"#,
                id(257)
            ),
            Err(DocError::TooComplex("style_id")),
        ),
    ] {
        assert_eq!(
            read_with(&Docx::new("").styles(&styles), &limits()).map(drop),
            expected
        );
    }
    let long_reference = format!(r#"<w:rStyle w:val="{}"/>"#, id(4_000));
    assert_eq!(
        text(&read(
            &Docx::new(&para("", &run(&long_reference, "named"))).styles("")
        )),
        "named"
    );
    // A field opened in a simple field counts toward the field depth, as fldChar fields do.
    let shallow = DocLimits {
        max_xml_depth: 16,
        ..limits()
    };
    let begins = r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#.repeat(16);
    assert_eq!(
        read_with(
            &Docx::new(&format!(
                r#"<w:p>{begins}<w:fldSimple w:instr=" PAGE "/></w:p>"#
            )),
            &shallow
        ),
        Err(DocError::TooComplex("field_depth"))
    );
    // A table's queued paragraphs are capped at what they could still show or warn of.
    let few = DocLimits {
        max_blocks: 10,
        ..limits()
    };
    let hidden_rows = table(
        "",
        &[vec![cell(
            "",
            &para("", &run("<w:vanish/>", "x")).repeat(21),
        )]],
    );
    assert_eq!(
        read_with(&Docx::new(&hidden_rows), &few),
        Err(DocError::TooComplex("blocks"))
    );
    // Relationships the reader never follows are not kept, however many.
    let hyperlinks = (0..5_000).fold(Docx::new(""), |docx, _| {
        docx.related_missing(
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink",
            "https://example.com",
        )
    });
    assert!(read_with(&hyperlinks, &limits()).is_ok());
}

#[test]
fn xml_guard_table() {
    for (xml, error) in [
        (&b"<a><b>"[..], DocError::Malformed("xml_structure")),
        (b"<a/><b/>", DocError::Malformed("xml_structure")),
        (b"<a/>text", DocError::Malformed("xml_structure")),
        (b"", DocError::Malformed("xml_structure")),
        (b"<!doctype a><a/>", DocError::Malformed("doctype")),
        (
            b"<!DOCTYPE a [<!ENTITY e 'x'>]><a>&e;</a>",
            DocError::Malformed("doctype"),
        ),
        (b"<a>&foo;</a>", DocError::Malformed("xml_entity")),
        (b"<a>&#1;</a>", DocError::Malformed("xml_entity")),
        (b"<a b='&#1;'/>", DocError::Malformed("xml_entity")),
        (b"<a b='&foo;'/>", DocError::Malformed("xml_entity")),
        (b"<a>\x01</a>", DocError::Malformed("xml_char")),
        (b"<a></a></b>", DocError::Malformed("xml_syntax")),
        (b"<p:a/>", DocError::Malformed("xml_namespace")),
        (b"<a b='1' b='2'/>", DocError::Malformed("xml_attribute")),
        (
            b"\xFF\xFE<\x00a\x00/\x00>\x00",
            DocError::Malformed("xml_encoding"),
        ),
        (
            b"<?xml version='1.0' encoding='UTF-16'?><a/>",
            DocError::Malformed("xml_encoding"),
        ),
        (b"<a>\xFF</a>", DocError::Malformed("xml_encoding")),
        (b"<a><b></a></b>", DocError::Malformed("xml_syntax")),
        (b"<a x:y='1'/>", DocError::Malformed("xml_namespace")),
        (b"<a><!-- a -- b --></a>", DocError::Malformed("xml_syntax")),
        (b"<\x00a\x00/\x00>\x00", DocError::Malformed("xml_encoding")),
    ] {
        assert_eq!(
            xml_guard::scan(xml, &limits(), None, &mut budget()),
            Err(error),
            "{xml:?}"
        );
    }
    let mut flood = String::from("<a");
    for at in 0..200 {
        flood.push_str(&format!(" xmlns:n{at}='urn:{at}'"));
    }
    flood.push_str("/>");
    assert_eq!(
        xml_guard::scan(flood.as_bytes(), &limits(), None, &mut budget()),
        Err(DocError::TooComplex("xml_namespaces"))
    );
    // Attributes, namespace declarations among them, are capped per element.
    let attributes = |n: usize| {
        let list: String = (0..n).map(|at| format!(" a{at}=''")).collect();
        format!("<a xmlns:p='urn:p'{list}/>")
    };
    assert!(xml_guard::scan(attributes(255).as_bytes(), &limits(), None, &mut budget()).is_ok());
    assert_eq!(
        xml_guard::scan(attributes(256).as_bytes(), &limits(), None, &mut budget()),
        Err(DocError::TooComplex("xml_attributes"))
    );
    let stats = xml_guard::scan(
        b"\xEF\xBB\xBF<x:r xmlns:x='u'><x:c/><c/><x:d><x:c/></x:d></x:r>",
        &limits(),
        Some(b"c"),
        &mut budget(),
    )
    .unwrap();
    assert_eq!((stats.elements, stats.counted), (5, 3));

    let extracted = read_body(&para(
        "",
        &[run("", "A &amp; B"), run("", " &#x0E01;&#3585;")].concat(),
    ));
    assert_eq!(text(&extracted), "A & B กก");
    let spaced = read_body(
        r#"<w:p><w:r><w:t xml:space="preserve"> lead</w:t></w:r><w:r><w:t xml:space="preserve">trail </w:t></w:r><w:r><w:t>end</w:t></w:r></w:p>"#,
    );
    assert_eq!(text(&spaced), " leadtrail end");
}

fn package_with(entries: &[RawEntry]) -> Vec<u8> {
    raw_zip(entries, b"")
}

/// A ZIP64 end record, its locator, and an end record whose fields all carry the ZIP64 markers,
/// for an archive whose ZIP64 record starts at `at`.
fn zip64_record(entries: u64, size: u64, offset: u64, at: u64) -> Vec<u8> {
    let mut out = b"PK\x06\x06".to_vec();
    out.extend_from_slice(&44u64.to_le_bytes());
    out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for field in [entries, entries, size, offset] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(b"PK\x06\x07");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out
}

/// `zip` (with no comment) ending in ZIP64 records instead, its directory size off by `lie`.
fn to_zip64(zip: &[u8], lie: u64) -> Vec<u8> {
    let end = zip.len() - 22;
    let field = |at: usize| u32::from_le_bytes(zip[end + at..end + at + 4].try_into().unwrap());
    let entries = u64::from(u16::from_le_bytes([zip[end + 10], zip[end + 11]]));
    let mut out = zip[..end].to_vec();
    out.extend_from_slice(&zip64_record(
        entries,
        u64::from(field(12)) + lie,
        u64::from(field(16)),
        end as u64,
    ));
    out.extend_from_slice(&end_record(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0));
    out
}

#[test]
fn zip_strictness_table() {
    let honest = zip(&[("a.xml", b"<a/>"), ("b.xml", b"<b/>")], true);
    assert!(open_package(&honest, &limits()).is_ok());

    let claims = end_record(60_000, 0, 0, 0);
    assert_eq!(
        open_package(&claims, &limits()).unwrap_err(),
        DocError::TooComplex("zip_entries")
    );

    // A ZIP64 end record claiming 100,000 entries, followed before the parse.
    let mut zip64 = Vec::new();
    zip64.extend_from_slice(b"PK\x06\x06");
    zip64.extend_from_slice(&44u64.to_le_bytes());
    zip64.extend_from_slice(&[45, 0, 45, 0]);
    zip64.extend_from_slice(&[0; 8]);
    zip64.extend_from_slice(&100_000u64.to_le_bytes());
    zip64.extend_from_slice(&100_000u64.to_le_bytes());
    zip64.extend_from_slice(&0u64.to_le_bytes());
    zip64.extend_from_slice(&0u64.to_le_bytes());
    zip64.extend_from_slice(b"PK\x06\x07");
    zip64.extend_from_slice(&0u32.to_le_bytes());
    zip64.extend_from_slice(&0u64.to_le_bytes());
    zip64.extend_from_slice(&1u32.to_le_bytes());
    zip64.extend_from_slice(&end_record(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0));
    assert_eq!(
        open_package(&zip64, &limits()).unwrap_err(),
        DocError::TooComplex("zip_entries")
    );

    let mut prefixed = b"junk".to_vec();
    prefixed.extend_from_slice(&honest);

    let decoy = raw_zip(
        &[RawEntry::stored("a.xml", b"<a/>")],
        &[end_record(1, 0, 0, 0), b"padding".to_vec()].concat(),
    );

    let mut overlapping = package_with(&[
        RawEntry::stored("a.xml", b"<a/>"),
        RawEntry::stored("b.xml", b"<b/>"),
    ]);
    let centrals = central_headers(&overlapping);
    let first_local = local_header(&overlapping, centrals[0]);
    set_u32(&mut overlapping, centrals[1] + 42, first_local as u32);

    let flagged = |flags: u16| {
        package_with(&[RawEntry {
            flags,
            ..RawEntry::stored("a.xml", b"<a/>")
        }])
    };
    let bzip2 = package_with(&[RawEntry {
        method: 12,
        ..RawEntry::stored("a.xml", b"<a/>")
    }]);
    let short = package_with(&[RawEntry {
        size: 14,
        ..RawEntry::stored("a.xml", b"<a/>")
    }]);
    // One entry with a Unicode Path extra field (a second name) in both headers.
    let unicode_path = {
        let name = b"a.xml";
        let extra = [
            &0x7075u16.to_le_bytes()[..],
            &9u16.to_le_bytes(),
            &[1],
            &crc32fast::hash(name).to_le_bytes(),
            b"b.xml"[..4].as_ref(),
        ]
        .concat();
        let data = b"<a/>";
        let crc = crc32fast::hash(data);
        let mut out = Vec::new();
        out.extend_from_slice(b"PK\x03\x04");
        for field in [20u16, 0, 0, 0, 0x21] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for field in [crc, 4, 4] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&extra);
        out.extend_from_slice(data);
        let directory = out.len();
        out.extend_from_slice(b"PK\x01\x02");
        for field in [20u16, 20, 0, 0, 0, 0x21] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for field in [crc, 4, 4] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for field in [name.len() as u16, extra.len() as u16, 0, 0, 0] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&extra);
        let directory_len = out.len() - directory;
        out.extend_from_slice(&end_record(1, directory_len as u32, directory as u32, 0));
        out
    };

    // The directory's size in the end record against where its headers end.
    let end = honest.len() - 22;
    let size = u32::from_le_bytes(honest[end + 12..end + 16].try_into().unwrap());
    let sized = |size: u32| {
        let mut bytes = honest.clone();
        set_u32(&mut bytes, end + 12, size);
        bytes
    };
    let junk_gap = |covered: bool| {
        let mut bytes = honest.clone();
        bytes.splice(end..end, [0u8; 12]);
        if covered {
            set_u32(&mut bytes, end + 12 + 12, size + 12);
        }
        bytes
    };
    // Three headers in the directory, the end record counting two.
    let mut phantom = package_with(&[
        RawEntry::stored("a.xml", b"<a/>"),
        RawEntry::stored("b.xml", b"<b/>"),
        RawEntry::stored("word/document.xml", b"<evil/>"),
    ]);
    let phantom_end = phantom.len() - 22;
    set_u16(&mut phantom, phantom_end + 8, 2);
    set_u16(&mut phantom, phantom_end + 10, 2);
    let mut after_end = honest.clone();
    after_end.push(0);
    let mut disk = honest.clone();
    set_u16(&mut disk, end + 4, 1);
    let zip64_true = to_zip64(&honest, 0);
    assert!(open_package(&zip64_true, &limits()).is_ok());
    // An entry comment is part of its header: the directory still ends where the record says.
    let mut commented = package_with(&[RawEntry::stored("a.xml", b"<a/>")]);
    let header = central_headers(&commented)[0];
    commented.splice(header + 46 + 5..header + 46 + 5, *b"note!");
    set_u16(&mut commented, header + 32, 5);
    let commented_end = commented.len() - 22;
    let directory_len = u32::from_le_bytes(
        commented[commented_end + 12..commented_end + 16]
            .try_into()
            .unwrap(),
    );
    set_u32(&mut commented, commented_end + 12, directory_len + 5);
    assert!(open_package(&commented, &limits()).is_ok());
    // The count is neither its marker nor the ZIP64 record's 100,000.
    let mut size_marker = zip64_record(100_000, 0, 0, 0);
    size_marker.extend_from_slice(&[end_record(1, 0xFFFF_FFFF, 0, 0)].concat());
    let corrupt = package_with(&[RawEntry {
        name: b"a.xml".to_vec(),
        method: 8,
        flags: 0,
        crc: 0,
        compressed: vec![0xFF; 8],
        size: 10,
    }]);

    let cases: Vec<(&str, Vec<u8>, DocError)> = vec![
        (
            "bytes before the archive",
            prefixed,
            DocError::Malformed("zip_directory"),
        ),
        (
            "a decoy end record in the comment",
            decoy,
            DocError::Malformed("zip_end_record"),
        ),
        (
            "two byte-equal names",
            package_with(&[
                RawEntry::stored("a.xml", b"1"),
                RawEntry::stored("a.xml", b"2"),
            ]),
            DocError::Malformed("zip_duplicate_name"),
        ),
        (
            "a non-ASCII name",
            package_with(&[RawEntry::stored("wörd.xml", b"x")]),
            DocError::Malformed("zip_name"),
        ),
        (
            "a backslash",
            package_with(&[RawEntry::stored("word\\document.xml", b"x")]),
            DocError::Malformed("zip_name"),
        ),
        (
            "a control character",
            package_with(&[RawEntry::stored("a\u{1}.xml", b"x")]),
            DocError::Malformed("zip_name"),
        ),
        (
            "a leading slash",
            package_with(&[RawEntry::stored("/a.xml", b"x")]),
            DocError::Malformed("zip_name"),
        ),
        (
            "a Unicode Path field",
            unicode_path,
            DocError::Malformed("zip_name"),
        ),
        (
            "an overlapping entry",
            overlapping,
            DocError::Malformed("zip_header_mismatch"),
        ),
        ("flag bit 6 only", flagged(0x0040), DocError::Encrypted),
        ("flag bit 13 only", flagged(0x2000), DocError::Encrypted),
        ("bzip2", bzip2, DocError::Malformed("zip_method")),
        (
            "shorter than declared",
            short,
            DocError::Malformed("zip_entry_size"),
        ),
        (
            "a directory size of 0",
            sized(0),
            DocError::Malformed("zip_directory"),
        ),
        (
            "a directory size 7 too large",
            sized(size + 7),
            DocError::Malformed("zip_directory"),
        ),
        (
            "a directory size 5 too small",
            sized(size - 5),
            DocError::Malformed("zip_directory"),
        ),
        (
            "junk before the end record",
            junk_gap(false),
            DocError::Malformed("zip_directory"),
        ),
        (
            "junk the directory size covers",
            junk_gap(true),
            DocError::Malformed("zip_directory"),
        ),
        (
            "a header past the counted ones",
            phantom,
            DocError::Malformed("zip_directory"),
        ),
        (
            "a ZIP64 record with a lying directory size",
            to_zip64(&honest, 7),
            DocError::Malformed("zip_directory"),
        ),
        (
            "a byte after the end record",
            after_end,
            DocError::Malformed("zip_end_record"),
        ),
        ("a second disk", disk, DocError::Malformed("zip_end_record")),
        (
            "a count that is neither its marker nor the ZIP64 count",
            size_marker,
            DocError::Malformed("zip_end_record"),
        ),
        (
            "a corrupt deflate stream",
            corrupt,
            DocError::Malformed("zip_data"),
        ),
    ];
    for (case, bytes, error) in cases {
        assert_eq!(
            open_package(&bytes, &limits()).unwrap_err(),
            error,
            "{case}"
        );
    }

    let mut streamed = zip::ZipWriter::new_stream(Vec::new());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::DEFLATE)
        .last_modified_time(zip::DateTime::default());
    streamed.start_file("word/document.xml", options).unwrap();
    std::io::Write::write_all(&mut streamed, b"<a>streamed</a>").unwrap();
    let streamed = streamed.finish().unwrap().into_inner();
    assert_eq!(
        open_package(&streamed, &limits())
            .unwrap()
            .part("WORD/document.xml"),
        Some(&b"<a>streamed</a>"[..])
    );
}

/// `zip` with `tail` as its last central header's comment, the directory size grown to match.
fn with_last_comment(zip: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut out = zip.to_vec();
    let header = *central_headers(&out).last().unwrap();
    let name = usize::from(u16::from_le_bytes([out[header + 28], out[header + 29]]));
    let extra = usize::from(u16::from_le_bytes([out[header + 30], out[header + 31]]));
    let at = header + 46 + name + extra;
    out.splice(at..at, tail.iter().copied());
    set_u16(&mut out, header + 32, tail.len() as u16);
    let end = out.len() - 22;
    let size = u32::from_le_bytes(out[end + 12..end + 16].try_into().unwrap());
    set_u32(&mut out, end + 12, size + tail.len() as u32);
    out
}

#[test]
fn zip64_locators_are_followed_wherever_they_stand() {
    let honest = zip(&[("a.xml", b"<a/>"), ("b.xml", b"<b/>")], true);
    let end = honest.len() - 22;
    let field = |at: usize| u32::from_le_bytes(honest[end + at..end + at + 4].try_into().unwrap());
    let (size, offset) = (field(12), field(16));
    // ZIP64 records naming the honest directory, then end records with the given 32-bit fields.
    let zip64 = |entries: u16, size32: u32, offset32: u32| {
        let mut out = honest[..end].to_vec();
        out.extend_from_slice(&zip64_record(
            2,
            u64::from(size),
            u64::from(offset),
            end as u64,
        ));
        out.extend_from_slice(&end_record(entries, size32, offset32, 0));
        out
    };
    // Each 32-bit field is its marker or the ZIP64 value: the size alone carries the marker.
    let size_only = zip64(2, 0xFFFF_FFFF, offset);
    assert_eq!(
        open_package(&size_only, &limits())
            .unwrap()
            .names()
            .collect::<Vec<_>>(),
        ["a.xml", "b.xml"]
    );
    // A locator hidden in the last entry's comment, with no marker anywhere: Python's zipfile
    // and libarchive follow it to another directory, so the guard follows it too.
    let record_at = honest.len() - 22;
    let hidden_locator = with_last_comment(
        &honest,
        &zip64_record(1, 46 + 5, u64::from(offset), record_at as u64),
    );
    // Markers set, and the locator names a record that is not the one right before it.
    let mut displaced = zip64(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF);
    let record = displaced.len() - 22 - 20 - 56;
    let copy = displaced[record..record + 56].to_vec();
    displaced.splice(record + 56..record + 56, copy);
    // The count carries the marker; the 32-bit size and offset name another directory.
    let other_size = zip64(0xFFFF, size - 46 - 5, offset);
    let mut long_record = zip64(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF);
    let record = long_record.len() - 22 - 20 - 56;
    set_u32(&mut long_record, record + 4, 100);
    let mut far_disk = zip64(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF);
    let locator = far_disk.len() - 22 - 20;
    set_u32(&mut far_disk, locator + 4, 1);
    // A marker with no locator before the end record names no ZIP64 record.
    let mut marker_alone = honest.clone();
    set_u32(&mut marker_alone, end + 12, 0xFFFF_FFFF);
    let mut disk_counts = zip64(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF);
    let record = disk_counts.len() - 22 - 20 - 56;
    set_u32(&mut disk_counts, record + 24, 1);
    for (case, bytes) in [
        ("a locator in an entry comment", hidden_locator),
        ("a locator naming an earlier record", displaced),
        ("32-bit fields naming another directory", other_size),
        ("a ZIP64 record with extensible data", long_record),
        ("the ZIP64 record on another disk", far_disk),
        ("entries on this disk not the total", disk_counts),
        ("a size marker with no locator", marker_alone),
    ] {
        assert_eq!(
            open_package(&bytes, &limits()).unwrap_err(),
            DocError::Malformed("zip_end_record"),
            "{case}"
        );
    }
}

#[test]
fn local_header_times_need_not_match_and_names_are_bounded() {
    let honest = zip(&[("a.xml", b"<a/>"), ("b.xml", b"<b/>")], true);
    // Writers set the local header's DOS time apart from the central one's; it is no content.
    let mut retimed = honest.clone();
    let local = local_header(&retimed, central_headers(&honest)[1]);
    set_u16(&mut retimed, local + 10, 0x1234);
    set_u16(&mut retimed, local + 12, 0x5678);
    let package = open_package(&retimed, &limits()).unwrap();
    assert_eq!(package.part("b.xml"), Some(&b"<b/>"[..]));
    // Every other field still has to agree.
    let mut recrc = honest.clone();
    let crc = u32::from_le_bytes(recrc[local + 14..local + 18].try_into().unwrap());
    set_u32(&mut recrc, local + 14, crc ^ 1);
    assert_eq!(
        open_package(&recrc, &limits()).unwrap_err(),
        DocError::Malformed("zip_header_mismatch")
    );
    for (len, accepted) in [(256, true), (257, false)] {
        let name = format!("{}.xml", "n".repeat(len - 4));
        let bytes = zip(&[(name.as_str(), b"<a/>")], false);
        match open_package(&bytes, &limits()) {
            Ok(_) => assert!(accepted, "{len}"),
            Err(error) => {
                assert!(!accepted, "{len}");
                assert_eq!(error, DocError::Malformed("zip_name"));
            }
        }
    }
}

#[test]
fn to_stored_zip_is_stored_only_and_round_trips() {
    let docx = Docx::new(&para("", &run("", "stored")))
        .theme(&theme(&[]))
        .build();
    let package = open_package(&docx, &limits()).unwrap();
    let stored = package.to_stored_zip();
    let mut archive = zip::ZipArchive::new(Cursor::new(&stored)).unwrap();
    for at in 0..archive.len() {
        let entry = archive.by_index_data(at).unwrap();
        assert_eq!(entry.compression(), zip::CompressionMethod::STORE);
        assert_eq!(entry.size(), entry.compressed_size());
    }
    archive.validate_header_consistency().unwrap();
    let again = open_package(&stored, &limits()).unwrap();
    assert_eq!(again, package);
    assert_eq!(
        again.names().collect::<Vec<_>>(),
        package.names().collect::<Vec<_>>()
    );
    assert_eq!(package.to_stored_zip(), stored);
    assert_eq!(
        docs::extract(MediaKind::Docx, &stored, &limits()).unwrap(),
        docs::extract(MediaKind::Docx, &docx, &limits()).unwrap()
    );
}

#[test]
fn hostile_packages_fail_bounded() {
    let gib = 1u64 << 30;
    let zeros = zeros_stream(gib);
    let bomb = |declared: u32| {
        package_with(&[RawEntry {
            name: b"word/document.xml".to_vec(),
            method: 8,
            flags: 0,
            crc: 0,
            compressed: zeros.clone(),
            size: declared,
        }])
    };
    let many: Vec<RawEntry> = (0..2_001)
        .map(|at| RawEntry::stored(&format!("e{at}"), b""))
        .collect();
    let mut lying = zip(&[("a.xml", b"<a>some text</a>")], false);
    let centrals = central_headers(&lying);
    let local = local_header(&lying, centrals[0]);
    set_u32(&mut lying, local + 22, 3);
    let mut two_ends = package_with(&[RawEntry::stored("a.xml", b"<a/>")]);
    let end = two_ends.len() - 22;
    let copy = two_ends[end..].to_vec();
    two_ends.splice(end..end, copy);
    let encrypted = package_with(&[RawEntry {
        flags: 1,
        ..RawEntry::stored("word/document.xml", b"x")
    }]);
    let cases: Vec<(&str, Vec<u8>, DocError)> = vec![
        (
            "a gibibyte of zeros, declared",
            bomb(u32::MAX),
            DocError::TooComplex("unzipped_bytes"),
        ),
        (
            "a gibibyte of zeros, declared 1 MiB",
            bomb(1 << 20),
            DocError::TooComplex("zip_entry_size"),
        ),
        (
            "2,001 entries",
            package_with(&many),
            DocError::TooComplex("zip_entries"),
        ),
        (
            "names equal under case folding",
            zip(
                &[
                    ("word/document.xml", b"<a/>"),
                    ("Word/Document.xml", b"<b/>"),
                ],
                true,
            ),
            DocError::Malformed("zip_duplicate_name"),
        ),
        (
            "a lying local header",
            lying,
            DocError::Malformed("zip_header_mismatch"),
        ),
        (
            "two end records",
            two_ends,
            DocError::Malformed("zip_end_record"),
        ),
        ("an encrypted entry", encrypted, DocError::Encrypted),
        (
            "a .. name",
            package_with(&[RawEntry::stored("word/../evil.xml", b"x")]),
            DocError::Malformed("zip_name"),
        ),
    ];
    for (case, bytes, error) in cases {
        assert_eq!(
            docs::extract(MediaKind::Docx, &bytes, &limits()),
            Err(error),
            "{case}"
        );
    }

    // The running total stops inflation at the cap: 1 MiB entries (each spared the ratio check)
    // under an 8 MiB budget inflate eight entries, never a ninth.
    let mib = vec![0u8; 1 << 20];
    let compressed = deflate(&mib);
    let entries: Vec<RawEntry> = (0..64)
        .map(|at| RawEntry {
            name: format!("e{at}").into_bytes(),
            method: 8,
            flags: 0,
            crc: crc32fast::hash(&mib),
            compressed: compressed.clone(),
            size: mib.len() as u32,
        })
        .collect();
    let small = DocLimits {
        max_unzipped_bytes: 8 << 20,
        ..limits()
    };
    let before = package::inflated_entries();
    assert_eq!(
        open_package(&package_with(&entries), &small).unwrap_err(),
        DocError::TooComplex("unzipped_bytes")
    );
    assert_eq!(package::inflated_entries() - before, 8);

    let doctype = Docx::from_document(&format!(
        r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;">]><w:document xmlns:w="{W}"><w:body><w:p><w:r><w:t>&b;</w:t></w:r></w:p></w:body></w:document>"#
    ));
    assert_eq!(
        docs::extract(MediaKind::Docx, &doctype.build(), &limits()),
        Err(DocError::Malformed("doctype"))
    );
    let nested = |depth: usize| {
        let inner = depth - 2;
        Docx::from_document(&format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="{W}"><w:body>{}{}</w:body></w:document>"#,
            "<w:sdt>".repeat(inner),
            "</w:sdt>".repeat(inner)
        ))
        .build()
    };
    assert!(docs::extract(MediaKind::Docx, &nested(256), &limits()).is_ok());
    assert_eq!(
        docs::extract(MediaKind::Docx, &nested(257), &limits()),
        Err(DocError::TooComplex("xml_depth"))
    );
    let mut too_big = Docx::new("").build();
    too_big.resize(limits().max_input_bytes as usize + 1, 0);
    assert_eq!(
        docs::extract(MediaKind::Docx, &too_big, &limits()),
        Err(DocError::TooLarge("input"))
    );
}

#[test]
fn ratio_cap_spares_small_sparse_entries() {
    let entry = |size: usize| {
        let data = vec![0u8; size];
        let compressed = deflate_with_ratio(&data, 400);
        assert!((size / 401..=size / 400).contains(&compressed.len()));
        package_with(&[RawEntry {
            name: b"sparse.bin".to_vec(),
            method: 8,
            flags: 0,
            crc: crc32fast::hash(&data),
            compressed,
            size: size as u32,
        }])
    };
    let small = open_package(&entry(500 << 10), &limits()).unwrap();
    assert_eq!(small.part("sparse.bin").map(<[u8]>::len), Some(500 << 10));
    assert_eq!(
        open_package(&entry(2 << 20), &limits()).unwrap_err(),
        DocError::TooComplex("zip_ratio")
    );
}

#[test]
fn malformed_inputs_fail_malformed() {
    let docx = Docx::new(&para("", &run("", "x"))).build();
    let truncated = &docx[..docx.len() / 2];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let random: Vec<u8> = (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    for bytes in [truncated, &random[..], b"PK\x03\x04", b""] {
        assert!(matches!(
            docs::extract(MediaKind::Docx, bytes, &limits()),
            Err(DocError::Malformed(_))
        ));
    }
}

#[test]
fn guard_loops_stop_at_the_job_deadline() {
    let entries: Vec<RawEntry> = (0..2_000)
        .map(|at| RawEntry::stored(&format!("e{at:04}"), b"x"))
        .collect();
    let bytes = package_with(&entries);
    assert!(open_package(&bytes, &limits()).is_ok());
    let before = package::inflated_entries();
    let result = deadline::scope(
        Instant::now() + Duration::from_secs(3600),
        None,
        u64::MAX,
        || {
            // The end record's search window takes 64 checks; then the entries are read.
            deadline::expire_after_checks(200);
            open_package(&bytes, &limits())
        },
    );
    assert_eq!(result.unwrap_err(), DocError::Timeout);
    let inflated = package::inflated_entries() - before;
    assert!((1..2_000).contains(&inflated), "{inflated}");
}

/// `docx` as a stored zip, so a large repetitive part passes the ratio cap.
fn stored(docx: &Docx) -> Vec<u8> {
    let entries = docx.entries();
    zip(
        &entries
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
        false,
    )
}

/// `f` inside a job scope whose deadline passes at check number `checks + 1`.
fn after_checks<T>(checks: u32, f: impl FnOnce() -> Result<T, DocError>) -> Result<T, DocError> {
    deadline::scope(
        Instant::now() + Duration::from_secs(3600),
        None,
        u64::MAX,
        || {
            deadline::expire_after_checks(checks);
            f()
        },
    )
}

/// Every reader loop checks the deadline. Each row's input makes one loop's checks far more than
/// all the others', and the deadline passes halfway through them: without that loop's check the
/// read would end before it.
#[test]
fn reader_loops_stop_at_the_job_deadline() {
    // One entry of 4 MiB: 64 chunks, against the end record's window (the last 64 KiB of the
    // file, 64 checks) and a handful of others.
    let zeros = vec![0u8; 4 << 20];
    let one_big = package_with(&[RawEntry {
        name: b"big.bin".to_vec(),
        method: 8,
        flags: 0,
        crc: crc32fast::hash(&zeros),
        compressed: deflate_with_ratio(&zeros, 50),
        size: zeros.len() as u32,
    }]);
    // A 65,535-byte comment: the end record's search window takes 64 checks.
    let long_comment = raw_zip(&[RawEntry::stored("a.xml", b"<a/>")], &[b' '; 65_535]);
    let many_elements = format!("<a>{}</a>", "<b/>".repeat(200_000));
    let blank_lines = "\n".repeat(200_000);
    let paragraphs = "a\n\n".repeat(20_000);
    let one_line = "a".repeat(200_000);
    let mut utf16 = vec![0xFF, 0xFE];
    utf16.extend(
        " ".repeat(200_000)
            .encode_utf16()
            .flat_map(u16::to_le_bytes),
    );
    let empty_runs = stored(&Docx::new(&format!(
        "<w:p>{}</w:p>",
        "<w:r/>".repeat(200_000)
    )));
    let visible = stored(&Docx::new(&para("", &run("", "x")).repeat(20_000)));
    // One paragraph of 60,000 runs: the walk makes about 740 checks, the judge's run loop 234.
    let many_runs = stored(&Docx::new(&format!(
        "<w:p>{}</w:p>",
        "<w:r><w:t>x</w:t></w:r>".repeat(60_000)
    )));
    // A paragraph 80 tables deep: what each enclosing cell puts beneath it is worked out once,
    // with a check per cell.
    let deep = Docx::new(&format!(
        "{}{}{}",
        "<w:tbl><w:tr><w:tc>".repeat(80),
        para("", &run("", "x")),
        "</w:tc></w:tr></w:tbl>".repeat(80)
    ))
    .build();
    // 30,000 runs each in a character style of its own: one merge, with a check, per style.
    let styled_runs = stored(
        &Docx::new(&format!(
            "<w:p>{}</w:p>",
            (0..30_000)
                .map(|at| format!(
                    r#"<w:r><w:rPr><w:rStyle w:val="c{at}"/></w:rPr><w:t>x</w:t></w:r>"#
                ))
                .collect::<String>()
        ))
        .styles(
            &(0..30_000)
                .map(|at| format!(r#"<w:style w:type="character" w:styleId="c{at}"/>"#))
                .collect::<String>(),
        ),
    );
    // Coloured runs: the colours beneath each are worked out, with a check, per run.
    let coloured_runs = stored(&Docx::new(&format!(
        "<w:p>{}</w:p>",
        r#"<w:r><w:rPr><w:color w:val="000000"/></w:rPr><w:t>x</w:t></w:r>"#.repeat(30_000)
    )));
    let themed = theme(&[]).replace(
        "</a:themeElements>",
        &format!(
            "<a:fontScheme name=\"x\">{}</a:fontScheme></a:themeElements>",
            "<a:b/>".repeat(200_000)
        ),
    );
    let prolog = stored(&Docx::from_document(&document("").replacen(
        "<w:document",
        &format!("{}<w:document", "<!--c-->".repeat(200_000)),
        1,
    )));
    type Read<'a> = Box<dyn Fn() -> Result<(), DocError> + 'a>;
    let rows: Vec<(&str, u32, Read)> = vec![
        (
            "inflate's 64 KiB chunks",
            100,
            Box::new(|| open_package(&one_big, &limits()).map(drop)),
        ),
        (
            "the end record's window",
            30,
            Box::new(|| open_package(&long_comment, &limits()).map(drop)),
        ),
        (
            "xml_guard::scan's events",
            100,
            Box::new(|| {
                xml_guard::scan(many_elements.as_bytes(), &limits(), None, &mut budget()).map(drop)
            }),
        ),
        (
            "the text reader's lines",
            100,
            Box::new(|| {
                docs::extract(MediaKind::Text, blank_lines.as_bytes(), &limits()).map(drop)
            }),
        ),
        (
            "the text reader's paragraphs",
            1_000,
            Box::new(|| docs::extract(MediaKind::Text, paragraphs.as_bytes(), &limits()).map(drop)),
        ),
        (
            "split_pieces' graphemes",
            100,
            Box::new(|| docs::extract(MediaKind::Text, one_line.as_bytes(), &limits()).map(drop)),
        ),
        (
            "UTF-16 decoding",
            100,
            Box::new(|| docs::extract(MediaKind::Text, &utf16, &limits()).map(drop)),
        ),
        (
            "the DOCX walk",
            300,
            Box::new(|| docs::extract(MediaKind::Docx, &empty_runs, &limits()).map(drop)),
        ),
        (
            "DOCX paragraphs",
            2_000,
            Box::new(|| docs::extract(MediaKind::Docx, &visible, &limits()).map(drop)),
        ),
        (
            "the judge's runs",
            850,
            Box::new(|| docs::extract(MediaKind::Docx, &many_runs, &limits()).map(drop)),
        ),
        (
            "the links beneath a paragraph",
            40,
            Box::new(|| docs::extract(MediaKind::Docx, &deep, &limits()).map(drop)),
        ),
        (
            "the style merges",
            5_000,
            Box::new(|| docs::extract(MediaKind::Docx, &styled_runs, &limits()).map(drop)),
        ),
        (
            "the colours beneath each run",
            10_000,
            Box::new(|| docs::extract(MediaKind::Docx, &coloured_runs, &limits()).map(drop)),
        ),
        (
            "the theme's scheme walk",
            300,
            Box::new(|| ThemeColours::parse(themed.as_bytes(), &limits(), &mut budget()).map(drop)),
        ),
        (
            "the main part's prolog",
            590,
            Box::new(|| docs::extract(MediaKind::Docx, &prolog, &limits()).map(drop)),
        ),
    ];
    for (case, checks, read) in rows {
        assert_eq!(read(), Ok(()), "{case} reads");
        assert_eq!(
            after_checks(checks, &read),
            Err(DocError::Timeout),
            "{case}"
        );
    }
}

#[test]
fn deadline_scope_does_not_leak_into_the_next_job() {
    assert_eq!(deadline::check(), Ok(()));
    let panicked = std::panic::catch_unwind(|| {
        deadline::scope(Instant::now(), None, u64::MAX, || {
            assert_eq!(deadline::check(), Err(DocError::Timeout));
            panic!("job failed");
        })
    });
    assert!(panicked.is_err());
    assert_eq!(deadline::check(), Ok(()));
    let early: Result<(), DocError> = deadline::scope(Instant::now(), None, u64::MAX, || {
        deadline::check()?;
        unreachable!("the deadline has passed")
    });
    assert_eq!(early, Err(DocError::Timeout));
    assert_eq!(deadline::check(), Ok(()));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = deadline::scope(
        Instant::now() + Duration::from_secs(60),
        Some(cancel),
        u64::MAX,
        deadline::check,
    );
    assert_eq!(cancelled, Err(DocError::Cancelled));
    assert_eq!(deadline::check(), Ok(()));
}

fn pool(workers: usize, call_timeout: Duration) -> Arc<DocPool> {
    Arc::new(
        DocPool::new(
            workers,
            DocLimits {
                call_timeout,
                ..limits()
            },
        )
        .unwrap(),
    )
}

/// A job that marks `started`, then waits on `barrier`.
fn blocker(
    started: &Arc<AtomicBool>,
    barrier: &Arc<Barrier>,
) -> impl FnOnce(&DocLimits) -> Result<(), DocError> + Send + 'static {
    let (started, barrier) = (started.clone(), barrier.clone());
    move |_| {
        started.store(true, Ordering::SeqCst);
        barrier.wait();
        Ok(())
    }
}

async fn until(flag: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !flag.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the job started");
}

#[tokio::test]
async fn doc_pool_stops_at_the_callers_deadline() {
    let pool = pool(1, Duration::from_secs(30));
    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let released = Arc::new(AtomicBool::new(false));
    let job = {
        let (barrier, released) = (barrier.clone(), released.clone());
        let started = started.clone();
        move |_: &DocLimits| {
            started.store(true, Ordering::SeqCst);
            barrier.wait();
            released.store(true, Ordering::SeqCst);
            Ok(())
        }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1);
    let result = pool
        .run(
            WorkspaceId::new(),
            &CancellationToken::new(),
            Some(deadline),
            job,
        )
        .await;
    assert_eq!(result, Err(DocError::Timeout));
    assert!(!released.load(Ordering::SeqCst));
    // Release the job if a worker started it (one its cancelled token kept from starting never
    // reaches the barrier).
    std::thread::spawn(move || barrier.wait());
}

#[tokio::test]
async fn doc_pool_call_timeout_counts_from_pickup() {
    let pool = pool(1, Duration::from_millis(50));
    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let first = tokio::spawn({
        let (pool, job) = (pool.clone(), blocker(&started, &barrier));
        async move {
            pool.run(WorkspaceId::new(), &CancellationToken::new(), None, job)
                .await
        }
    });
    until(&started).await;
    let second = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.run(WorkspaceId::new(), &CancellationToken::new(), None, |_| {
                // A job checks its deadline, which counts from its own pickup.
                deadline::check()?;
                Ok(42)
            })
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(first.await.unwrap(), Err(DocError::CallTimeout));
    barrier.wait();
    assert_eq!(second.await.unwrap(), Ok(42));
}

/// A job that checks its deadline until it passes, as every reader does.
fn checking(_: &DocLimits) -> Result<(), DocError> {
    loop {
        deadline::check()?;
        std::thread::yield_now();
    }
}

#[tokio::test]
async fn a_job_that_sees_its_deadline_reports_which_one_passed() {
    // The pool's own cap came first: `CallTimeout`, which Task 6 caches.
    let pool = pool(1, Duration::from_millis(50));
    for deadline in [
        None,
        Some(tokio::time::Instant::now() + Duration::from_secs(60)),
    ] {
        assert_eq!(
            pool.run(
                WorkspaceId::new(),
                &CancellationToken::new(),
                deadline,
                checking
            )
            .await,
            Err(DocError::CallTimeout)
        );
    }
    // The caller's deadline came first: `Timeout`, which it never caches.
    let pool = self::pool(1, Duration::from_secs(30));
    let deadline = tokio::time::Instant::now() + Duration::from_millis(50);
    assert_eq!(
        pool.run(
            WorkspaceId::new(),
            &CancellationToken::new(),
            Some(deadline),
            checking
        )
        .await,
        Err(DocError::Timeout)
    );
}

#[tokio::test]
async fn a_panic_clears_its_threads_wedge_mark() {
    let pool = pool(2, Duration::from_millis(10));
    // Both jobs wait for each other, so they run on the two threads at once.
    let barrier = Arc::new(Barrier::new(2));
    let panicking = |barrier: Arc<Barrier>| {
        let pool = pool.clone();
        tokio::spawn(async move {
            pool.run(
                WorkspaceId::new(),
                &CancellationToken::new(),
                None,
                move |_| {
                    barrier.wait();
                    panic!("reader bug")
                },
            )
            .await
        })
    };
    let (one, two) = (panicking(barrier.clone()), panicking(barrier));
    assert_eq!(one.await.unwrap(), Err::<(), _>(DocError::Internal));
    assert_eq!(two.await.unwrap(), Err::<(), _>(DocError::Internal));
    // Well past three call timeouts, with every thread idle.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!pool.wedged());
}

#[tokio::test]
async fn dropping_run_cancels_a_queued_or_running_job() {
    let pool = pool(1, Duration::from_secs(30));
    let workspace = WorkspaceId::new();
    let started = Arc::new(AtomicBool::new(false));
    let (report, stopped) = std::sync::mpsc::channel();
    let cancel = CancellationToken::new();
    let running = pool.run(workspace, &cancel, None, {
        let started = started.clone();
        move |_| {
            started.store(true, Ordering::SeqCst);
            loop {
                if let Err(error) = deadline::check() {
                    report.send(error).unwrap();
                    return Err::<(), _>(error);
                }
                std::thread::yield_now();
            }
        }
    });
    let mut running = Box::pin(running);
    tokio::time::timeout(Duration::from_secs(30), async {
        while !started.load(Ordering::SeqCst) {
            tokio::select! {
                _ = &mut running => panic!("the job only ends when cancelled"),
                () = tokio::time::sleep(Duration::from_millis(1)) => {}
            }
        }
    })
    .await
    .unwrap();
    drop(running);
    assert_eq!(
        stopped.recv_timeout(Duration::from_secs(30)),
        Ok(DocError::Cancelled)
    );
    assert_eq!(
        pool.run(workspace, &CancellationToken::new(), None, |_| Ok(7))
            .await,
        Ok(7)
    );

    let (blocked, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let holder = tokio::spawn({
        let (pool, job) = (pool.clone(), blocker(&blocked, &barrier));
        async move {
            pool.run(WorkspaceId::new(), &CancellationToken::new(), None, job)
                .await
        }
    });
    until(&blocked).await;
    let ran = Arc::new(AtomicBool::new(false));
    let mut queued = Box::pin(pool.run(workspace, &cancel, None, {
        let ran = ran.clone();
        move |_| {
            ran.store(true, Ordering::SeqCst);
            Ok(())
        }
    }));
    assert!(futures::poll!(queued.as_mut()).is_pending());
    drop(queued);
    barrier.wait();
    assert_eq!(holder.await.unwrap(), Ok(()));
    assert_eq!(
        pool.run(workspace, &CancellationToken::new(), None, |_| Ok(8))
            .await,
        Ok(8)
    );
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn one_workspace_cannot_hold_every_pool_thread() {
    let pool = pool(2, Duration::from_secs(30));
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let first = tokio::spawn({
        let (pool, job) = (pool.clone(), blocker(&started, &barrier));
        async move { pool.run(a, &CancellationToken::new(), None, job).await }
    });
    until(&started).await;
    let ran = Arc::new(AtomicBool::new(false));
    let cancel = CancellationToken::new();
    let mut second = Box::pin(pool.run(a, &cancel, None, {
        let ran = ran.clone();
        move |_| {
            ran.store(true, Ordering::SeqCst);
            Ok(())
        }
    }));
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert_eq!(
        pool.run(b, &CancellationToken::new(), None, |_| Ok(3))
            .await,
        Ok(3)
    );
    assert!(!ran.load(Ordering::SeqCst));
    barrier.wait();
    assert_eq!(first.await.unwrap(), Ok(()));
    assert_eq!(second.await, Ok(()));
    assert!(ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn doc_pool_runs_at_most_workers_jobs() {
    let pool = pool(1, Duration::from_secs(30));
    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let first = tokio::spawn({
        let (pool, job) = (pool.clone(), blocker(&started, &barrier));
        async move {
            pool.run(WorkspaceId::new(), &CancellationToken::new(), None, job)
                .await
        }
    });
    until(&started).await;
    let ran = Arc::new(AtomicBool::new(false));
    let cancel = CancellationToken::new();
    let mut second = Box::pin(pool.run(WorkspaceId::new(), &cancel, None, {
        let ran = ran.clone();
        move |_| {
            ran.store(true, Ordering::SeqCst);
            Ok(())
        }
    }));
    assert!(futures::poll!(second.as_mut()).is_pending());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!ran.load(Ordering::SeqCst));
    barrier.wait();
    assert_eq!(first.await.unwrap(), Ok(()));
    assert_eq!(second.await, Ok(()));
    assert!(ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn doc_pool_timeout_returns_and_keeps_the_thread_busy() {
    let pool = pool(1, Duration::from_millis(50));
    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    assert_eq!(
        pool.run(
            WorkspaceId::new(),
            &CancellationToken::new(),
            None,
            blocker(&started, &barrier)
        )
        .await,
        Err(DocError::CallTimeout)
    );
    let ran = Arc::new(AtomicBool::new(false));
    let cancel = CancellationToken::new();
    let mut second = Box::pin(pool.run(WorkspaceId::new(), &cancel, None, {
        let ran = ran.clone();
        move |_| {
            ran.store(true, Ordering::SeqCst);
            Ok(5)
        }
    }));
    assert!(futures::poll!(second.as_mut()).is_pending());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert!(!ran.load(Ordering::SeqCst));
    barrier.wait();
    assert_eq!(second.await, Ok(5));
}

#[tokio::test]
async fn doc_pool_cancel_while_queued_starts_nothing() {
    let pool = pool(1, Duration::from_secs(30));
    let (started, barrier) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let holder = tokio::spawn({
        let (pool, job) = (pool.clone(), blocker(&started, &barrier));
        async move {
            pool.run(WorkspaceId::new(), &CancellationToken::new(), None, job)
                .await
        }
    });
    until(&started).await;
    let cancel = CancellationToken::new();
    let ran = Arc::new(AtomicBool::new(false));
    let queued = tokio::spawn({
        let (pool, cancel, ran) = (pool.clone(), cancel.clone(), ran.clone());
        async move {
            pool.run(WorkspaceId::new(), &cancel, None, move |_| {
                ran.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
        }
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    let queued = tokio::time::timeout(Duration::from_secs(30), queued)
        .await
        .expect("a cancelled queued run returns while the thread is still busy");
    assert_eq!(queued.unwrap(), Err(DocError::Cancelled));
    barrier.wait();
    assert_eq!(holder.await.unwrap(), Ok(()));
    assert_eq!(
        pool.run(WorkspaceId::new(), &CancellationToken::new(), None, |_| Ok(
            1
        ))
        .await,
        Ok(1)
    );
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn doc_pool_panic_is_internal() {
    let pool = pool(1, Duration::from_secs(30));
    let panicked = pool
        .run(
            WorkspaceId::new(),
            &CancellationToken::new(),
            None,
            |_| -> Result<(), DocError> { panic!("reader bug") },
        )
        .await;
    assert_eq!(panicked, Err(DocError::Internal));
    // The same thread takes the next job; a lost thread would leave it queued past its deadline.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    assert_eq!(
        pool.run(
            WorkspaceId::new(),
            &CancellationToken::new(),
            Some(deadline),
            |_| Ok(2)
        )
        .await,
        Ok(2)
    );
}

#[inline(never)]
fn recurse(depth: u32) -> u64 {
    let frame = std::hint::black_box([depth as u8; 256]);
    if depth == 0 {
        u64::from(frame[0])
    } else {
        recurse(depth - 1) + u64::from(frame[255])
    }
}

#[tokio::test]
async fn doc_pool_deep_recursion_fits_the_stack() {
    let pool = pool(1, Duration::from_secs(60));
    let result = pool
        .run(WorkspaceId::new(), &CancellationToken::new(), None, |_| {
            Ok(recurse(100_000))
        })
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn wedged_since_waits_for_every_thread() {
    let pool = pool(2, Duration::from_millis(10));
    let barrier = Arc::new(Barrier::new(3));
    let (first, second) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let run = |started: &Arc<AtomicBool>| {
        let (pool, job) = (pool.clone(), blocker(started, &barrier));
        tokio::spawn(async move {
            pool.run(WorkspaceId::new(), &CancellationToken::new(), None, job)
                .await
        })
    };
    let one = run(&first);
    until(&first).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // One thread is still idle: not wedged, however long the other has run.
    assert_eq!(pool.wedged_since(), None);
    let before_second = Instant::now();
    let two = run(&second);
    until(&second).await;
    let after_second = Instant::now();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !pool.wedged() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let since = pool.wedged_since().unwrap();
    let three = Duration::from_millis(30);
    assert!(since >= before_second + three && since <= after_second + three);
    // Releasing the threads ends the wedge.
    barrier.wait();
    assert_eq!(one.await.unwrap(), Err(DocError::CallTimeout));
    assert_eq!(two.await.unwrap(), Err(DocError::CallTimeout));
    tokio::time::timeout(Duration::from_secs(30), async {
        while pool.wedged() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn watchdog_exit_decision_table() {
    let t0 = Instant::now();
    let after = Duration::from_secs(30);
    assert!(!watchdog::should_exit(None, t0, after));
    assert!(!watchdog::should_exit(
        Some(t0),
        t0 + Duration::from_secs(29),
        after
    ));
    assert!(watchdog::should_exit(
        Some(t0),
        t0 + Duration::from_secs(30),
        after
    ));
    assert!(watchdog::should_exit(
        Some(t0),
        t0 + Duration::from_secs(31),
        after
    ));
}

struct Fake(Option<Instant>);

impl WedgeSource for Fake {
    fn wedged_since(&self) -> Option<Instant> {
        self.0
    }
}

type Exit = Arc<dyn Fn(i32) + Send + Sync>;

fn recorder() -> (Arc<Mutex<Vec<i32>>>, Exit) {
    let codes = Arc::new(Mutex::new(Vec::new()));
    let exit: Exit = {
        let codes = codes.clone();
        Arc::new(move |code| codes.lock().unwrap().push(code))
    };
    (codes, exit)
}

#[test]
fn wedged_pool_calls_the_exit_hook() {
    let t0 = Instant::now();
    let (codes, exit) = recorder();
    let mut watchdog = Watchdog::new(Arc::new(Fake(Some(t0))), Duration::from_secs(30), exit);
    assert!(!watchdog.tick(t0 + Duration::from_secs(29)));
    assert!(codes.lock().unwrap().is_empty());
    assert!(watchdog.tick(t0 + Duration::from_secs(30)));
    assert_eq!(*codes.lock().unwrap(), [70]);
    assert!(!watchdog.tick(t0 + Duration::from_secs(31)));
    assert_eq!(*codes.lock().unwrap(), [70]);

    let (codes, exit) = recorder();
    let mut healthy = Watchdog::new(Arc::new(Fake(None)), Duration::from_secs(30), exit);
    for seconds in [0, 30, 3_600] {
        assert!(!healthy.tick(t0 + Duration::from_secs(seconds)));
    }
    assert!(codes.lock().unwrap().is_empty());
}

#[test]
fn watchdog_spawn_returns_after_the_first_fire() {
    let (codes, exit) = recorder();
    let watchdog = Watchdog::new(Arc::new(Fake(Some(Instant::now()))), Duration::ZERO, exit);
    watchdog::spawn(watchdog, Duration::from_millis(10))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(*codes.lock().unwrap(), [70]);
}

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/corpus");
const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/docs/MANIFEST.md"
);

/// The fuzz seeds: (target, file, what it is, bytes). Small, varied and made from this test
/// support, so the corpus is reproducible and reviewable.
fn seeds() -> Vec<(&'static str, &'static str, &'static str, Vec<u8>)> {
    let simple = Docx::new(&para("", &run("", "seed"))).build();
    let stored = open_package(&simple, &limits()).unwrap().to_stored_zip();
    let mut streamed = zip::ZipWriter::new_stream(Vec::new());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::DEFLATE)
        .last_modified_time(zip::DateTime::default());
    streamed.start_file("word/document.xml", options).unwrap();
    std::io::Write::write_all(
        &mut streamed,
        document(&para("", &run("", "streamed"))).as_bytes(),
    )
    .unwrap();
    let streamed = streamed.finish().unwrap().into_inner();
    let mut zip64 = b"PK\x06\x06".to_vec();
    zip64.extend_from_slice(&44u64.to_le_bytes());
    zip64.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    zip64.extend_from_slice(&[0; 32]);
    zip64.extend_from_slice(b"PK\x06\x07");
    zip64.extend_from_slice(&[0; 12]);
    zip64.extend_from_slice(&1u32.to_le_bytes());
    zip64.extend_from_slice(&end_record(0xFFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0));
    let mut two_ends = raw_zip(&[RawEntry::stored("a.xml", b"<a/>")], b"");
    let end = two_ends.len() - 22;
    let copy = two_ends[end..].to_vec();
    two_ends.splice(end..end, copy);
    let notes = |root: &str, item: &str, marker: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:{root} {NAMESPACES}><w:{item} w:id="1"><w:p><w:r><w:t>{marker}</w:t></w:r></w:p></w:{item}></w:{root}>"#
        )
    };
    let styles = concat!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>"#,
        r#"<w:style w:type="paragraph" w:styleId="Quiet"><w:basedOn w:val="Normal"/><w:rPr><w:vanish/></w:rPr></w:style>"#,
        r#"<w:style w:type="character" w:styleId="Loud"><w:rPr><w:vanish/></w:rPr></w:style>"#,
        r#"<w:style w:type="table" w:styleId="Grid"><w:tblPr><w:tblStyleRowBandSize w:val="1"/></w:tblPr><w:tblStylePr w:type="firstRow"><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="1F3864"/></w:tcPr></w:tblStylePr><w:tblStylePr w:type="band1Horz"><w:rPr><w:color w:val="FFFFFF"/></w:rPr></w:tblStylePr></w:style>"#,
    );
    let grid = table(
        r#"<w:tblStyle w:val="Grid"/><w:tblLook w:val="04A0"/>"#,
        &[
            vec![
                cell("", &para("", &run(WHITE, "head"))),
                cell("", &para("", &run("", "x"))),
            ],
            vec![
                cell(
                    r#"<w:shd w:val="pct25" w:color="auto" w:fill="FFFFFF"/>"#,
                    &para("", &run("", "body")),
                ),
                cell("", &para("", &run("", "y"))),
            ],
        ],
    );
    let alternate = format!(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps">{}</mc:Choice><mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"#,
        text_box(&para("", &run("", "box"))),
        para("", &run("", "box"))
    );
    let math = r#"xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math""#;
    let unknown = r#"xmlns:x="urn:example:unknown""#;
    let delimiter = format!(
        r#"<m:oMath {math}><m:d><m:dPr><m:begChr m:val="2"/></m:dPr><m:e/></m:d></m:oMath>"#
    );
    let unread = [
        format!("<w:p>{}{delimiter}{}</w:p>", run("", "Budget: 1"), run("", "00 baht")),
        format!(
            r#"<w:p><w:r><w:t>1</w:t><x:a {unknown} x:b="2"/><w:t>00</w:t><w:r>{delimiter}</w:r></w:r></w:p>"#
        ),
        format!(r#"<x:note {unknown}>2</x:note>"#),
        format!(
            "<w:p>{}<w:r><w:delText>2</w:delText></w:r><w:notAnElement>3</w:notAnElement>{}</w:p>",
            run("", "Payment: 1"),
            run("", "00 baht")
        ),
        format!(
            r#"<w:p><w:r><w:pict><v:shapetype id="t" fillcolor="white"><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shapetype><v:shape type="t" style="position:absolute"/></w:pict></w:r></w:p>"#,
            para("", &run("", "template"))
        ),
        format!(
            "<w:p>{}</w:p>",
            shape_box(
                "anchor",
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>3<!-- -->0000000</wp:posOffset></wp:positionH>"#,
                r#"<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
                &para("", &run("", "offpage"))
            )
        ),
        format!(r#"<m:oMathPara {math}><m:oMath><m:r><m:t>x</m:t></m:r></m:oMath></m:oMathPara>"#),
    ]
    .concat();
    let docx = |docx: Docx| docx.build();
    vec![
        ("package", "docx-deflated.zip", "a one-paragraph DOCX, deflated", simple.clone()),
        ("package", "docx-stored.zip", "the same package through to_stored_zip", stored),
        ("package", "empty.zip", "an empty archive: one end record", end_record(0, 0, 0, 0)),
        ("package", "zip64-empty.zip", "an empty archive with ZIP64 end records", zip64),
        ("package", "data-descriptor.zip", "an entry with a data descriptor", streamed),
        ("package", "two-end-records.zip", "a second end record before the real one", two_ends),
        (
            "package",
            "duplicate-names.zip",
            "two names equal under ASCII case folding",
            zip(&[("a.xml", b"<a/>"), ("A.XML", b"<b/>")], true),
        ),
        (
            "package",
            "encrypted-flag.zip",
            "an entry with the encryption flag",
            raw_zip(&[RawEntry { flags: 1, ..RawEntry::stored("a.xml", b"<a/>") }], b""),
        ),
        (
            "docx",
            "tab-break.docx",
            "tabs and breaks in a run",
            docx(Docx::new(r#"<w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t><w:br/><w:t>c</w:t></w:r></w:p>"#)),
        ),
        ("docx", "table-styles.docx", "a table style, regions, band size and pct shading", docx(Docx::new(&grid).styles(styles))),
        (
            "docx",
            "tracked.docx",
            "insertions, deletions, moves and a field",
            docx(Docx::new(concat!(
                r#"<w:p><w:ins w:id="1" w:author="a"><w:r><w:t>in</w:t></w:r></w:ins><w:del w:id="2" w:author="a"><w:r><w:delText>out</w:delText></w:r></w:del>"#,
                r#"<w:moveFrom w:id="3" w:author="a"><w:r><w:t>moved</w:t></w:r></w:moveFrom><w:moveTo w:id="4" w:author="a"><w:r><w:t>moved</w:t></w:r></w:moveTo>"#,
                r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
            ))),
        ),
        (
            "docx",
            "hidden-runs.docx",
            "vanished, white, tiny and highlighted runs",
            docx(Docx::new(&paragraphs(&[
                ("<w:vanish/>", "v"),
                (WHITE, "w"),
                (r#"<w:sz w:val="6"/><w:szCs w:val="2"/><w:cs/>"#, "ไทย"),
                (r#"<w:highlight w:val="black"/><w:color w:val="000000"/>"#, "h"),
                ("<w:specVanish/>", "s"),
            ]))),
        ),
        (
            "docx",
            "styles.docx",
            "default, based-on, character and paragraph styles",
            docx(Docx::new(&[para(r#"<w:pStyle w:val="Quiet"/>"#, &run(r#"<w:rStyle w:val="Loud"/>"#, "toggled")), para("", &run("", "plain"))].concat()).styles(styles)),
        ),
        (
            "docx",
            "theme.docx",
            "a theme, its scheme mapping, a page colour and themed shading",
            docx(
                Docx::from_document(&document_with(
                    r#"<w:background w:color="FFFFFF" w:themeColor="background1"/>"#,
                    &[
                        para(r#"<w:shd w:val="clear" w:fill="FFFFFF" w:themeFill="text1" w:themeFillTint="99"/>"#, &run(WHITE, "a")),
                        para("", &run(r#"<w:color w:val="000000" w:themeColor="accent1" w:themeShade="BF"/>"#, "b")),
                    ]
                    .concat(),
                ))
                .theme(&theme(&[]))
                .settings(r#"<w:displayBackgroundShape/><w:clrSchemeMapping w:bg1="light1" w:t1="dark1" w:bg2="light2" w:t2="dark2"/>"#),
            ),
        ),
        ("docx", "alternate-content.docx", "a text box in mc:Choice and mc:Fallback", docx(Docx::new(&alternate))),
        (
            "docx",
            "shapes.docx",
            "text boxes with fills, a hidden shape, a VML shapetype and a nested table",
            docx(Docx::new(&[
                shape_box("inline", "", r#"<a:solidFill><a:schemeClr val="lt1"/></a:solidFill>"#, &para("", &run(BLACK, "inline"))),
                shape_box("anchor", r#"<wp:positionH relativeFrom="page"><wp:posOffset>914400</wp:posOffset></wp:positionH><wp:extent cx="914400" cy="457200"/><wp:docPr id="2" name="Hidden" hidden="1"/>"#, "<a:noFill/>", &para("", &run("", "hidden"))),
                format!(r##"<w:p><w:r><w:pict><v:shapetype id="t" filled="f"/><v:shape type="#t" style="position:absolute;margin-left:10pt"><v:fill color="#000000"/><v:textbox><w:txbxContent>{}</w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>"##, para("", &run(WHITE, "vml"))),
                table("", &[vec![cell(r#"<w:shd w:val="clear" w:color="auto" w:fill="000000"/>"#, &table("", &[vec![cell("", &para("", &run(WHITE, "nested")))]]))]]),
            ].concat())),
        ),
        (
            "docx",
            "fields.docx",
            "fields Word recomputes, a hyperlink, a data-bound control and symbols",
            docx(Docx::new(&[
                format!("<w:p>{}{}</w:p>", field(" PAGE ", "1"), field(r#" HYPERLINK "https://example.com" "#, "link")),
                r#"<w:p><w:fldSimple w:instr=" SET x 1"><w:r><w:t>1</w:t></w:r></w:fldSimple><w:r><w:t>a</w:t><w:noBreakHyphen/><w:sym w:font="Symbol" w:char="F02D"/><w:cr/><w:ptab w:relativeTo="margin" w:alignment="right" w:leader="none"/></w:r></w:p>"#.to_owned(),
                r#"<w:sdt><w:sdtPr><w:dataBinding w:xpath="/a" w:storeItemID="{00000000-0000-0000-0000-000000000000}"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>bound</w:t></w:r></w:p></w:sdtContent></w:sdt>"#.to_owned(),
            ].concat()).settings("<w:updateFields/>")),
        ),
        (
            "docx",
            "uncertain.docx",
            "a choice the reader does not understand, undecided properties and tracked table changes",
            docx(Docx::new(&[
                r#"<w:p><w:r><w:rPr><mc:AlternateContent xmlns:a14="http://schemas.microsoft.com/office/drawing/2010/main"><mc:Choice Requires="a14"><w:vanish/></mc:Choice><mc:Fallback/></mc:AlternateContent></w:rPr><w:t>x</w:t></w:r><w:r><w:rPr><w:webHidden/></w:rPr><w:t>2</w:t></w:r><w:r><w:t>y</w:t></w:r></w:p>"#.to_owned(),
                r#"<w:tbl><w:tr><w:trPr><w:del w:id="1" w:author="a"/></w:trPr><w:tc><w:p><w:r><w:t>gone</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p><w:r><w:t>merged</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_owned(),
            ].concat()).settings(r#"<w:clrSchemeMapping w:bg1="light1" w:t1="dark1" w:bg2="light2" w:t2="dark2" w:accent1="light1" w:accent2="accent2" w:accent3="accent3" w:accent4="accent4" w:accent5="accent5" w:accent6="accent6" w:hyperlink="hyperlink" w:followedHyperlink="followedHyperlink"/>"#)),
        ),
        (
            "docx",
            "unread.docx",
            "equations and an unknown element with an attribute between runs, in a run and between paragraphs, text Word would not read there, a shapetype's text box and an offset in pieces",
            docx(Docx::new(&unread)),
        ),
        (
            "docx",
            "entities.docx",
            "entity and character references",
            docx(Docx::new(&para("", &run("", "A &amp; B &#x0E01; &lt;x&gt; &quot;q&quot; &apos;")))),
        ),
        (
            "docx",
            "notes.docx",
            "a header, a footer, footnotes and endnotes",
            docx(
                Docx::new(&para("", &run("", "main")))
                    .related("header", "header1.xml", notes("hdr", "sdt", "header").as_bytes())
                    .related("footer", "footer1.xml", notes("ftr", "sdt", "footer").as_bytes())
                    .related("footnotes", "footnotes.xml", notes("footnotes", "footnote", "note").as_bytes())
                    .related("endnotes", "endnotes.xml", notes("endnotes", "endnote", "end").as_bytes()),
            ),
        ),
        (
            "docx",
            "strict.docx",
            "a Strict OOXML main part",
            docx(Docx::from_document(&document(&para("", &run("", "strict"))).replace(W, STRICT_W))),
        ),
        ("text", "thai.txt", "Thai with a zero-width space and SARA AM as U+0E4D U+0E32", "กำ\u{200B}ไร ก\u{E4D}\u{E32}ไร\n\nสอง".as_bytes().to_vec()),
        ("text", "crlf.txt", "CRLF line ends and a blank line", b"one\r\ntwo\r\n\r\nthree\r\n".to_vec()),
        ("text", "utf16le.txt", "UTF-16 LE with a byte-order mark", [vec![0xFF, 0xFE], "ประชุม\n".encode_utf16().flat_map(u16::to_le_bytes).collect()].concat()),
        ("text", "utf16be.txt", "UTF-16 BE with a byte-order mark", [vec![0xFE, 0xFF], "ประชุม\n".encode_utf16().flat_map(u16::to_be_bytes).collect()].concat()),
        ("text", "bom-only.txt", "a UTF-8 byte-order mark alone", vec![0xEF, 0xBB, 0xBF]),
        ("text", "odd-utf16.txt", "UTF-16 with an odd byte count", vec![0xFF, 0xFE, b'a']),
        ("text", "lone-surrogate.txt", "UTF-16 with a lone surrogate", vec![0xFF, 0xFE, 0x00, 0xD8]),
        ("text", "nul.txt", "a NUL character", b"a\0b".to_vec()),
        ("xlsx", "minimal.xlsx", "one sheet of one inline string", xlsx(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>seed</t></is></c></row>"#)),
        ("xlsx", "styled.xlsx", "styles, shared strings with runs, a theme, number formats and white, tiny and empty-format cells", {
            let mut styles = StyleSet::new();
            let white = styles.with_font(r#"<sz val="11"/><color theme="0"/>"#);
            let tiny = styles.with_font(r#"<sz val="3"/>"#);
            let millions = styles.with_format("#,##0,,");
            let blank = styles.with_format(";;;");
            let black = styles.solid("FF000000");
            let on_black = styles.xf(0, 1, black);
            Xlsx::new()
                .styles(&styles.xml())
                .theme(&theme(&[]))
                .shared_strings(&["<t>shared</t>", r#"<r><t>รายได้ </t></r><r><rPr><color rgb="FFFFFFFF"/></rPr><t>9,999</t></r><rPh sb="0" eb="1"><t>hint</t></rPh>"#])
                .sheet("Sheet1", &format!(
                    r#"<sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" s="{white}"><v>1</v></c><c r="D1" s="{tiny}"><v>2</v></c><c r="E1" s="{millions}"><v>1250000000</v></c><c r="F1" s="{blank}"><v>3</v></c><c r="G1" s="{on_black}"><v>4</v></c></row></sheetData>"#
                ))
                .build()
        }),
        ("xlsx", "hidden.xlsx", "hidden and very hidden sheets, a hidden row, columns hidden in both tag forms, and a merged range", Xlsx::new()
            .sheet("Visible", r#"<cols><col min="2" max="2" hidden="1"></col><col min="4" max="5" hidden="1"/></cols><sheetData><row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="D1"><v>3</v></c><c r="F1"><v>4</v></c><c r="G1"><v>5</v></c></row><row r="2" hidden="1"><c r="A2"><v>6</v></c></row></sheetData><mergeCells count="1"><mergeCell ref="F1:G1"/></mergeCells>"#)
            .sheet_state("Hidden", "hidden", r#"<sheetData><row r="1"><c r="A1"><v>7</v></c></row></sheetData>"#)
            .sheet_state("VeryHidden", "veryHidden", r#"<sheetData><row r="1"><c r="A1"><v>8</v></c></row></sheetData>"#)
            .build()),
        ("xlsx", "formulas.xlsx", "a 1904 workbook that recalculates on load, with shared formulas, every cell type and escapes", Xlsx::new()
            .workbook_head(r#"<workbookPr date1904="1"/>"#)
            .workbook_tail(r#"<calcPr calcId="124519" fullCalcOnLoad="1"/>"#)
            .sheet("S_x0031_", r#"<sheetData><row r="1"><c r="A1"><v>1</v></c><c r="B1"><f t="shared" ref="B1:B2" si="0">A1*2</f><v>2</v></c><c r="C1" t="str"><f>"a"&amp;"_x000D_"</f><v>a_x000D_</v></c><c r="D1" t="b"><v>1</v></c><c r="E1" t="e"><v>#N/A</v></c><c r="F1" t="d"><v>2024-01-31</v></c></row><row r="2"><c r="A2"><v>2</v></c><c r="B2"><f t="shared" si="0"/><v>4</v></c><c><f>NOW()</f></c></row></sheetData>"#)
            .build()),
        ("xlsx", "prefixed.xlsx", "SpreadsheetML under a prefix, with an external link and macros the reader warns about", Xlsx::new()
            .sheet("S", "")
            .workbook_xml(&format!(r#"<?xml version="1.0"?><x:workbook xmlns:x="{SML}" xmlns:r="{RELATIONSHIPS}"><x:sheets><x:sheet name="S" sheetId="1" r:id="rIdS0"/></x:sheets></x:workbook>"#))
            .related("externalLink", "externalLinks/externalLink1.xml", Some(format!(r#"<?xml version="1.0"?><externalLink xmlns="{SML}"/>"#).as_bytes()))
            .entry("xl/vbaProject.bin", b"\xD0\xCF")
            .build_with_sheet_xml(0, &format!(r#"<?xml version="1.0"?><x:worksheet xmlns:x="{SML}"><x:sheetData><x:row r="1"><x:c r="A1" t="inlineStr"><x:is><x:t>prefixed</x:t></x:is></x:c></x:row></x:sheetData></x:worksheet>"#))),
        ("xlsx", "strict.xlsx", "a Strict SpreadsheetML workbook", Xlsx::new()
            .sheet("S", r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#)
            .workbook_xml(&format!(r#"<?xml version="1.0"?><workbook xmlns="{STRICT_SML}" xmlns:r="{RELATIONSHIPS}"><sheets><sheet name="S" sheetId="1" r:id="rIdS0"/></sheets></workbook>"#))
            .build()),
    ]
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
#[ignore = "rewrites fuzz/corpus and tests/fixtures/docs/MANIFEST.md"]
fn regenerate_fuzz_seeds() {
    let mut rows = String::new();
    for target in ["package", "docx", "text", "xlsx"] {
        let dir = format!("{CORPUS}/{target}");
        if std::path::Path::new(&dir).exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
    }
    for (target, file, what, bytes) in seeds() {
        std::fs::write(format!("{CORPUS}/{target}/{file}"), &bytes).unwrap();
        rows.push_str(&format!(
            "| `{target}/{file}` | `{}` | {what} |\n",
            sha256(&bytes)
        ));
    }
    let manifest = std::fs::read_to_string(MANIFEST).unwrap();
    let (head, _) = manifest
        .split_once("| Seed | SHA-256 | What |\n|---|---|---|\n")
        .expect("the manifest's seed table");
    std::fs::write(
        MANIFEST,
        format!("{head}| Seed | SHA-256 | What |\n|---|---|---|\n{rows}"),
    )
    .unwrap();
}

#[test]
fn fuzz_seeds_match_the_manifest() {
    let manifest = std::fs::read_to_string(MANIFEST).unwrap();
    let mut listed = std::collections::BTreeMap::new();
    for line in manifest.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        listed.insert(
            cells[1].trim_matches('`').to_owned(),
            cells[2].trim_matches('`').to_owned(),
        );
    }
    let mut committed = std::collections::BTreeMap::new();
    for target in ["package", "docx", "text", "xlsx"] {
        let files: Vec<_> = std::fs::read_dir(format!("{CORPUS}/{target}"))
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        assert!(!files.is_empty(), "{target} has seeds");
        for file in files {
            let name = format!("{target}/{}", file.file_name().to_string_lossy());
            committed.insert(name, sha256(&std::fs::read(file.path()).unwrap()));
        }
    }
    assert_eq!(committed, listed);
}
