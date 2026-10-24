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
            ExtractWarning::HiddenContent { ordinal, why } => Some((*ordinal, *why)),
            _ => None,
        })
        .collect()
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
    let colours = ThemeColours::parse(theme(&[]).as_bytes(), &limits()).unwrap();
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
        ThemeColours::parse(b"<!DOCTYPE a><a/>", &limits()),
        Err(DocError::Malformed("doctype"))
    );
}

#[test]
fn extract_refuses_kinds_whose_readers_come_later() {
    let workbook = xlsx(r#"<row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row>"#);
    assert_eq!(
        docs::extract(MediaKind::Xlsx, &workbook, &limits()),
        Err(DocError::Unsupported("xlsx"))
    );
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
        Locator::DocxParagraph { part, .. } if part == "word/main.xml"
    ));
}

#[test]
fn docx_tracked_insertion_in_deletion_out_instr_text_out() {
    let body = r#"<w:p><w:ins w:id="1" w:author="a" w:date="2026-10-24T00:00:00Z"><w:r><w:t>inserted</w:t></w:r></w:ins><w:del w:id="2" w:author="a" w:date="2026-10-24T00:00:00Z"><w:r><w:delText>deleted</w:delText></w:r><w:r><w:t>deletedtoo</w:t></w:r></w:del><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>result</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let shown = text(&read_body(body));
    assert!(
        shown.contains("inserted") && shown.contains("result"),
        "{shown}"
    );
    for absent in ["deleted", "PAGE"] {
        assert!(!shown.contains(absent), "{shown}");
    }
}

#[test]
fn docx_field_instruction_text_is_excluded() {
    let body = r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:t>HYPERLINK "x"</w:t></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>shown</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t> after</w:t></w:r></w:p>"#;
    assert_eq!(text(&read_body(body)), "shown after");
}

#[test]
fn docx_hidden_text_excluded_and_warned() {
    let extracted = read_body(&para(
        "",
        &[run("", "seen "), run("<w:vanish/>", "secret")].concat(),
    ));
    assert_eq!(text(&extracted), "seen ");
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
        table_style("Bands", &region("band1Horz", "000000")),
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

fn text_box(content: &str) -> String {
    format!(
        r#"<w:r><w:drawing><wp:anchor><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:txbx><w:txbxContent>{content}</w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>"#
    )
}

#[test]
fn docx_textbox_paragraph_is_its_own_block() {
    let body = format!(
        "<w:p>{}{}{}</w:p>",
        run("", "hostbefore"),
        text_box(&para("", &run("", "boxed"))),
        run("", "hostafter")
    );
    let extracted = read_body(&body);
    assert_eq!(text(&extracted), "hostbeforehostafter\nboxed");
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
        text_box(&para("", &run("", "boxtext"))),
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
    assert_eq!(text(&extracted), "prefixed");
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
                notes("hdr", "sdt", "header text").as_bytes(),
            )
            .entry("word/vbaProject.bin", b"macros"),
    );
    let parts: Vec<(&str, &str)> = extracted
        .blocks
        .iter()
        .map(|block| match &block.locator {
            Locator::DocxParagraph { part, .. } => (part.as_str(), block.text.as_str()),
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
    assert_eq!(
        hidden(&extracted),
        [
            (1, "vanish"),
            (2, "tiny"),
            (3, "low_contrast"),
            (4, "low_contrast"),
            (5, "low_contrast")
        ]
    );
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
    ] {
        assert_eq!(xml_guard::scan(xml, &limits(), None), Err(error), "{xml:?}");
    }
    let mut flood = String::from("<a");
    for at in 0..200 {
        flood.push_str(&format!(" xmlns:n{at}='urn:{at}'"));
    }
    flood.push_str("/>");
    assert_eq!(
        xml_guard::scan(flood.as_bytes(), &limits(), None),
        Err(DocError::TooComplex("xml_namespaces"))
    );
    let stats = xml_guard::scan(
        b"\xEF\xBB\xBF<x:r xmlns:x='u'><x:c/><c/><x:d><x:c/></x:d></x:r>",
        &limits(),
        Some(b"c"),
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

    let cases: Vec<(&str, Vec<u8>, DocError)> = vec![
        (
            "bytes before the archive",
            prefixed,
            DocError::Malformed("zip"),
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
    let result = deadline::scope(Instant::now() + Duration::from_secs(3600), None, || {
        deadline::expire_after_checks(10);
        open_package(&bytes, &limits())
    });
    assert_eq!(result.unwrap_err(), DocError::Timeout);
    let inflated = package::inflated_entries() - before;
    assert!((1..2_000).contains(&inflated), "{inflated}");
}

#[test]
fn deadline_scope_does_not_leak_into_the_next_job() {
    assert_eq!(deadline::check(), Ok(()));
    let panicked = std::panic::catch_unwind(|| {
        deadline::scope(Instant::now(), None, || {
            assert_eq!(deadline::check(), Err(DocError::Timeout));
            panic!("job failed");
        })
    });
    assert!(panicked.is_err());
    assert_eq!(deadline::check(), Ok(()));
    let early: Result<(), DocError> = deadline::scope(Instant::now(), None, || {
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
    for target in ["package", "docx", "text"] {
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
    for target in ["package", "docx", "text"] {
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
