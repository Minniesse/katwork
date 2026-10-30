//! The PDF reader (Task 5): its guard against hostile files, its visibility rules, and its
//! lines and blocks. Every hostile file is built here, by `support/pdf_build.rs`.
#[path = "support/pdf_build.rs"]
mod pdf_build;

use katwork::docs::{
    self, Block, DocError, DocLimits, ExtractWarning, Extracted, HiddenWhy, Locator, MediaKind,
    TextLayer,
};
use lopdf::dictionary;
use pdf_build::*;

fn limits() -> DocLimits {
    DocLimits::default()
}

fn read_with(bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    docs::extract(MediaKind::Pdf, bytes, limits)
}

fn read(bytes: &[u8]) -> Extracted {
    read_with(bytes, &limits()).unwrap()
}

fn error(bytes: &[u8]) -> DocError {
    read_with(bytes, &limits()).unwrap_err()
}

fn visible(extracted: &Extracted) -> Vec<&str> {
    extracted
        .blocks
        .iter()
        .filter(|block| block.hidden.is_none())
        .map(|block| block.text.as_str())
        .collect()
}

fn hidden(extracted: &Extracted) -> Vec<(&str, HiddenWhy)> {
    extracted
        .blocks
        .iter()
        .filter_map(|block| block.hidden.map(|why| (block.text.as_str(), why)))
        .collect()
}

/// The block whose text is exactly `text`.
fn block<'a>(extracted: &'a Extracted, text: &str) -> &'a Block {
    extracted
        .blocks
        .iter()
        .find(|block| block.text == text)
        .unwrap_or_else(|| panic!("no block {text:?} in {:?}", texts(extracted)))
}

fn texts(extracted: &Extracted) -> Vec<(String, Option<HiddenWhy>, Option<&'static str>)> {
    extracted
        .blocks
        .iter()
        .map(|block| (block.text.clone(), block.hidden, block.suspect))
        .collect()
}

/// Asserts each `(text, verdict)`: `None` for a visible block with no suspect.
fn assert_verdicts(extracted: &Extracted, expected: &[(&str, Option<HiddenWhy>)]) {
    for (text, why) in expected {
        let block = block(extracted, text);
        assert_eq!(block.hidden, *why, "{text:?} in {:?}", texts(extracted));
    }
}

/// One line per case, 20 pt apart from y = 280 down, so each 10 pt line is its own block.
/// `{y}` is the case's baseline and `{low}` 4 pt below it, where a 16 pt tall cover over 10 pt
/// text starts.
fn lines_at(cases: &[&str]) -> String {
    lines_from(280, cases)
}

fn lines_from(top: i32, cases: &[&str]) -> String {
    cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let y = top - 20 * index as i32;
            case.replace("{y}", &y.to_string())
                .replace("{low}", &(y - 4).to_string())
        })
        .collect()
}

#[test]
fn pdf_lines_have_pages_and_user_space_rects() {
    let mut doc = Doc::new();
    doc.size = (200.0, 200.0);
    doc.page(
        &[
            text(10.0, 20.0, 170.0, "First line of a paragraph"),
            text(10.0, 20.0, 158.0, "and its second line"),
            text(10.0, 20.0, 100.0, "A new paragraph"),
        ]
        .concat(),
        "",
    );
    doc.page(&text(12.0, 30.0, 40.0, "Second page"), "");
    let extracted = read(&doc.build());
    assert_eq!(extracted.extractor, docs::ExtractorId::PDF);
    assert_eq!(docs::PDF, docs::ExtractorId::PDF);
    assert_eq!(
        visible(&extracted),
        vec![
            "First line of a paragraph\nand its second line",
            "A new paragraph",
            "Second page"
        ]
    );
    assert_eq!(extracted.pages.len(), 2);
    for page in &extracted.pages {
        assert_eq!(page.media_box, [0.0, 0.0, 200.0, 200.0]);
        assert_eq!(page.crop_box, page.media_box);
        assert_eq!(page.text_layer, TextLayer::Ok);
    }
    let paragraph = &extracted.blocks[0];
    assert_eq!(paragraph.locator, Locator::PdfPage { page: 1 });
    assert_eq!(paragraph.lines.len(), 2);
    assert_eq!((paragraph.lines[0].start, paragraph.lines[0].end), (0, 25));
    assert_eq!((paragraph.lines[1].start, paragraph.lines[1].end), (26, 45));
    for block in &extracted.blocks {
        let Locator::PdfPage { page } = block.locator else {
            panic!()
        };
        let media = extracted.pages[page as usize - 1].media_box;
        for line in &block.lines {
            assert_eq!(line.page, page);
            let [x0, y0, x1, y1] = line.rect;
            assert!(
                media[0] <= x0
                    && x0 < x1
                    && x1 <= media[2]
                    && media[1] <= y0
                    && y0 < y1
                    && y1 <= media[3],
                "{line:?}"
            );
        }
    }
    // The first line's box: x from 20, baseline 170, from −0.2 to 0.8 of 10 pt.
    let first = paragraph.lines[0].rect;
    assert_eq!((first[0], first[1], first[3]), (20.0, 168.0, 178.0));
    let ordinals: Vec<u32> = extracted.blocks.iter().map(|block| block.ordinal).collect();
    assert_eq!(ordinals, vec![1, 2, 3]);
}

/// A page with an optional-content setup: `/Off` off by default, `/On` on, `/View` on but not
/// viewed, `/AllOn` and `/Ve` membership dictionaries.
fn optional_doc() -> (Doc, String) {
    let mut doc = Doc::new();
    let off = doc.add("<< /Type /OCG /Name (off) >>");
    let on = doc.add("<< /Type /OCG /Name (on) >>");
    let view = doc.add("<< /Type /OCG /Name (view) /Usage << /View << /ViewState /OFF >> >> >>");
    let all_on = doc.add(format!(
        "<< /Type /OCMD /OCGs [{off} 0 R {on} 0 R] /P /AllOn >>"
    ));
    let ve = doc.add(format!(
        "<< /Type /OCMD /OCGs [{on} 0 R] /VE [/And {on} 0 R] >>"
    ));
    let empty = doc.add("<< /Type /OCMD /OCGs [null] >>");
    doc.catalog(&format!(
        "/OCProperties << /OCGs [{off} 0 R {on} 0 R {view} 0 R] /D << /ON [{on} 0 R {view} 0 R] /OFF [{off} 0 R] \
         /AS [<< /Event /View /OCGs [{view} 0 R] /Category [/View] >>] >> >>"
    ));
    let properties = format!(
        "/Properties << /Off {off} 0 R /On {on} 0 R /View {view} 0 R /AllOn {all_on} 0 R /Ve {ve} 0 R /Empty {empty} 0 R >>"
    );
    (doc, properties)
}

#[test]
fn pdf_hidden_text_is_flagged() {
    let (mut doc, properties) = optional_doc();
    let transparent = doc.add("<< /Type /ExtGState /ca 0 >>");
    doc.size = (300.0, 300.0);
    let content = lines_at(&[
        "q BT 3 Tr /F1 10 Tf 10 {y} Td (MODETHREE) Tj ET Q\n",
        "q BT 7 Tr /F1 10 Tf 10 {y} Td (MODESEVEN) Tj ET Q\n",
        "/OC /Off BDC BT /F1 10 Tf 10 {y} Td (OCGOFF) Tj ET EMC\n",
        "BT /F1 10 Tf 250 {y} Td (OUTSIDECROP) Tj ET\n",
        "q 0 0 50 50 re W n BT /F1 10 Tf 10 {y} Td (CLIPPED) Tj ET Q\n",
        "q /G0 gs BT /F1 10 Tf 10 {y} Td (TRANSPARENT) Tj ET Q\n",
        "BT /F1 0.5 Tf 10 {y} Td (HALFPOINT) Tj ET\n",
        "q BT /F1 12 Tf 10 Tz 10 {y} Td (SQUEEZED) Tj ET Q\n",
        "/OC /View BDC BT /F1 10 Tf 10 {y} Td (VIEWSTATE) Tj ET EMC\n",
        "/OC /AllOn BDC BT /F1 10 Tf 10 {y} Td (ALLON) Tj ET EMC\n",
        "/OC /Ve BDC BT /F1 10 Tf 10 {y} Td (EXPRESSION) Tj ET EMC\n",
        "/OC /On BDC BT /F1 10 Tf 10 {y} Td (SHOWN) Tj ET EMC\n",
        "/OC /Empty BDC BT /F1 10 Tf 10 {y} Td (NOGROUPS) Tj ET EMC\n",
    ]);
    doc.page_with(
        "/CropBox [0 0 200 300]",
        &content,
        &format!("/ExtGState << /G0 {transparent} 0 R >> {properties}"),
    );
    let extracted = read(&doc.build());
    assert_verdicts(
        &extracted,
        &[
            ("MODETHREE", Some(HiddenWhy::InvisibleMode)),
            ("MODESEVEN", Some(HiddenWhy::InvisibleMode)),
            ("OCGOFF", Some(HiddenWhy::OptionalContentOff)),
            ("OUTSIDECROP", Some(HiddenWhy::OutsideCropBox)),
            ("CLIPPED", Some(HiddenWhy::Clipped)),
            ("TRANSPARENT", Some(HiddenWhy::Transparent)),
            ("HALFPOINT", Some(HiddenWhy::Tiny)),
            ("SQUEEZED", Some(HiddenWhy::Tiny)),
            ("VIEWSTATE", Some(HiddenWhy::OptionalContentOff)),
            ("ALLON", Some(HiddenWhy::OptionalContentOff)),
            ("EXPRESSION", Some(HiddenWhy::OptionalContentOff)),
            ("SHOWN", None),
            ("NOGROUPS", None),
        ],
    );
    assert!(
        extracted
            .blocks
            .iter()
            .filter(|block| block.hidden.is_none())
            .all(|block| block.suspect.is_none())
    );
    let warned: Vec<&str> = extracted
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            ExtractWarning::HiddenContent {
                at: Locator::PdfPage { page: 1 },
                why,
            } => Some(*why),
            _ => None,
        })
        .collect();
    assert_eq!(warned.len(), 11);
    assert!(
        warned.contains(&"invisible_mode")
            && warned.contains(&"optional_content_off")
            && warned.contains(&"tiny")
    );
}

#[test]
fn pdf_hidden_characters_leave_their_visible_line() {
    let mut doc = Doc::new();
    doc.page(
        "BT /F1 10 Tf 10 150 Td (Entity A revenue ) Tj 1 g (9,999) Tj 0 g ( baht) Tj ET\n\
         BT /F1 10 Tf 10 100 Td (1) Tj 1 g (x) Tj 0 g (250) Tj ET\n",
        "",
    );
    let extracted = read(&doc.build());
    assert_eq!(visible(&extracted), vec!["Entity A revenue baht", "1 250"]);
    assert!(!visible(&extracted).join("").contains('9'));
    assert_eq!(
        hidden(&extracted),
        vec![
            ("9,999", HiddenWhy::LowContrast),
            ("x", HiddenWhy::LowContrast)
        ]
    );
    // The hidden block comes right after its visible one.
    let ordinals: Vec<(u32, bool)> = extracted
        .blocks
        .iter()
        .map(|block| (block.ordinal, block.hidden.is_some()))
        .collect();
    assert_eq!(ordinals, vec![(1, false), (2, true), (3, false), (4, true)]);
}

#[test]
fn pdf_white_tiny_and_covered_text_is_hidden() {
    let mut doc = Doc::new();
    doc.size = (300.0, 300.0);
    let image = doc.add_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[0]);
    let shading = doc.add("<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>");
    let translucent = doc.add("<< /Type /ExtGState /ca 0.95 >>");
    let compatible = doc.add("<< /Type /ExtGState /ca 0.95 /BM /Compatible >>");
    let half = doc.add("<< /Type /ExtGState /ca 0.5 >>");
    let faint = doc.add("<< /Type /ExtGState /ca 0.15 >>");
    let content = lines_at(&[
        "1 g BT /F1 10 Tf 10 {y} Td (WHITE) Tj ET 0 g\n",
        "0.972549 0.972549 0.972549 rg BT /F1 10 Tf 10 {y} Td (NEARWHITE) Tj ET 0 g\n",
        "BT /F1 3.5 Tf 10 {y} Td (SMALLTEXT) Tj ET\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERBLACK) Tj ET 0 0 0 rg 5 {low} 120 16 re f 0 g\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERIMAGE) Tj ET q 120 0 0 16 5 {low} cm /Im Do Q\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERSHADING) Tj ET q 5 {low} 120 16 re W n /Sh sh Q\n",
        "BT /F1 10 Tf 10 {y} Td (VEILED) Tj ET q /T gs 1 g 5 {low} 120 16 re f Q\n",
        "BT /F1 10 Tf 10 {y} Td (COMPATIBLE) Tj ET q /C gs 1 g 5 {low} 120 16 re f Q\n",
        "q /F gs 0.5019608 g BT /F1 10 Tf 10 {y} Td (FAINTGRAY) Tj ET Q\n",
        "BT /F1 10 Tf 10 {y} Td (BLACK) Tj ET\n",
        "q 0 g 5 {low} 120 16 re f Q 1 g BT /F1 10 Tf 10 {y} Td (WHITEONBLACK) Tj ET 0 g\n",
        "BT /F1 4 Tf 10 {y} Td (FOURPOINT) Tj ET\n",
        "BT /F1 10 Tf 10 {y} Td (HALFVEIL) Tj ET q /H gs 1 g 5 {low} 120 16 re f Q\n",
    ]);
    doc.page(
        &content,
        &format!(
            "/XObject << /Im {image} 0 R >> /Shading << /Sh {shading} 0 R >> \
             /ExtGState << /T {translucent} 0 R /C {compatible} 0 R /H {half} 0 R /F {faint} 0 R >>"
        ),
    );
    let extracted = read(&doc.build());
    assert_verdicts(
        &extracted,
        &[
            ("WHITE", Some(HiddenWhy::LowContrast)),
            ("NEARWHITE", Some(HiddenWhy::LowContrast)),
            ("SMALLTEXT", Some(HiddenWhy::Tiny)),
            ("UNDERBLACK", Some(HiddenWhy::Covered)),
            ("UNDERIMAGE", Some(HiddenWhy::Covered)),
            ("UNDERSHADING", Some(HiddenWhy::Covered)),
            ("VEILED", Some(HiddenWhy::LowContrast)),
            ("COMPATIBLE", Some(HiddenWhy::LowContrast)),
            ("FAINTGRAY", Some(HiddenWhy::LowContrast)),
            ("BLACK", None),
            ("WHITEONBLACK", None),
            ("FOURPOINT", None),
            ("HALFVEIL", None),
        ],
    );
    for text in ["BLACK", "WHITEONBLACK", "FOURPOINT", "HALFVEIL"] {
        assert_eq!(block(&extracted, text).suspect, None, "{text}");
    }
}

#[test]
fn pdf_unknown_paint_is_hidden() {
    let mut doc = Doc::new();
    let pattern = doc.add("<< /Type /Pattern /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >> >>");
    let image = doc.add_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[255]);
    let shading = doc.add("<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>");
    let masked = doc.add("<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 0 0 R >> >>");
    let multiply = doc.add("<< /Type /ExtGState /BM /Multiply >>");
    let screen = doc.add("<< /Type /ExtGState /BM /Screen >>");
    let half = doc.add("<< /Type /ExtGState /ca 0.5 >>");
    let normal = doc.add("<< /Type /ExtGState /BM /Normal >>");
    let compatible = doc.add("<< /Type /ExtGState /BM /Compatible >>");
    let content = lines_at(&[
        "q /Pattern cs /P0 scn BT /F1 10 Tf 10 {y} Td (PATTERNFILL) Tj ET Q\n",
        "q /Lab cs 100 0 0 sc BT /F1 10 Tf 10 {y} Td (LABTEXT) Tj ET Q\n",
        "q /SM gs BT /F1 10 Tf 10 {y} Td (SOFTMASK) Tj ET Q\n",
        "q 0 g 5 {low} 150 16 re f /MU gs 1 g BT /F1 10 Tf 10 {y} Td (MULTIPLIED) Tj ET Q\n",
        "BT /F1 10 Tf 10 {y} Td (SCREENED) Tj ET q /SC gs 0.5 g 5 {low} 150 16 re f Q\n",
        "BT /F1 10 Tf 10 {y} Td (HALFIMAGE) Tj ET q /HA gs 150 0 0 16 5 {low} cm /Im Do Q\n",
        "BT /F1 10 Tf 10 {y} Td (HALFSHADING) Tj ET q /HA gs 5 {low} 150 16 re W n /Sh sh Q\n",
        "q /NO gs BT /F1 10 Tf 10 {y} Td (NORMALBLEND) Tj ET Q\n",
        "q /CO gs BT /F1 10 Tf 10 {y} Td (COMPATIBLEBLEND) Tj ET Q\n",
    ]);
    doc.page(
        &content,
        &format!(
            "/Pattern << /P0 {pattern} 0 R >> /ColorSpace << /Lab [/Lab << /WhitePoint [0.95 1 1.09] >>] >> \
             /XObject << /Im {image} 0 R >> /Shading << /Sh {shading} 0 R >> \
             /ExtGState << /SM {masked} 0 R /MU {multiply} 0 R /SC {screen} 0 R /HA {half} 0 R /NO {normal} 0 R /CO {compatible} 0 R >>"
        ),
    );
    let extracted = read(&doc.build());
    let unknown = Some(HiddenWhy::UnknownPaint);
    assert_verdicts(
        &extracted,
        &[
            ("PATTERNFILL", unknown),
            ("LABTEXT", unknown),
            ("SOFTMASK", unknown),
            ("MULTIPLIED", unknown),
            ("SCREENED", unknown),
            ("HALFIMAGE", unknown),
            ("HALFSHADING", unknown),
            ("NORMALBLEND", None),
            ("COMPATIBLEBLEND", None),
        ],
    );
}

#[test]
fn pdf_colour_spaces_resolve() {
    let mut doc = Doc::new();
    let icc = |doc: &mut Doc, n: u32| doc.add_stream(&format!("/N {n}"), b"");
    let (gray, rgb, cmyk) = (icc(&mut doc, 1), icc(&mut doc, 3), icc(&mut doc, 4));
    let type4 = doc.add_stream(
        "/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1 0 1]",
        b"{ pop 0 0 0 0 }",
    );
    let content = lines_at(&[
        "q /G1 cs 1 sc BT /F1 10 Tf 10 {y} Td (ICCGRAY) Tj ET Q\n",
        "q /G3 cs 1 1 1 sc BT /F1 10 Tf 10 {y} Td (ICCRGB) Tj ET Q\n",
        "q /G4 cs 0 0 0 0 sc BT /F1 10 Tf 10 {y} Td (ICCCMYK) Tj ET Q\n",
        "q /IX cs 1 sc BT /F1 10 Tf 10 {y} Td (INDEXED) Tj ET Q\n",
        "q /SP cs 1 sc BT /F1 10 Tf 10 {y} Td (SEPARATION) Tj ET Q\n",
        "q /NO cs 1 sc BT /F1 10 Tf 10 {y} Td (NONECOLORANT) Tj ET Q\n",
        "q /DN cs 1 sc BT /F1 10 Tf 10 {y} Td (POSTSCRIPT) Tj ET Q\n",
        "q /IX cs 0 sc BT /F1 10 Tf 10 {y} Td (INDEXEDBLACK) Tj ET Q\n",
        "q /SP cs BT /F1 10 Tf 10 {y} Td (FULLTINT) Tj ET Q\n",
        "BT /F1 10 Tf 10 {y} Td (NOCOLOUR) Tj ET\n",
    ]);
    doc.page(
        &content,
        &format!(
            "/ColorSpace << /G1 [/ICCBased {gray} 0 R] /G3 [/ICCBased {rgb} 0 R] /G4 [/ICCBased {cmyk} 0 R] \
             /IX [/Indexed /DeviceRGB 1 <000000FFFFFF>] \
             /SP [/Separation /Spot /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 0] /N 1 >>] \
             /NO [/Separation /None /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>] \
             /DN [/DeviceN [/Cyan] /DeviceCMYK {type4} 0 R] >>"
        ),
    );
    let extracted = read(&doc.build());
    let low = Some(HiddenWhy::LowContrast);
    assert_verdicts(
        &extracted,
        &[
            ("ICCGRAY", low),
            ("ICCRGB", low),
            ("ICCCMYK", low),
            ("INDEXED", low),
            ("SEPARATION", low),
            ("NONECOLORANT", Some(HiddenWhy::InvisibleMode)),
            ("POSTSCRIPT", Some(HiddenWhy::UnknownPaint)),
            ("INDEXEDBLACK", None),
            ("FULLTINT", low),
            ("NOCOLOUR", None),
        ],
    );
}

#[test]
fn pdf_stroke_modes_use_the_stroke_colour() {
    let mut doc = Doc::new();
    let content = lines_at(&[
        "q 1 G 0 g BT 1 Tr /F1 10 Tf 10 {y} Td (STROKEWHITE) Tj ET Q\n",
        "q 0 G 1 g BT 2 Tr /F1 10 Tf 10 {y} Td (FILLWHITE) Tj ET Q\n",
        "q 1 G 1 g BT 2 Tr /F1 10 Tf 10 {y} Td (BOTHWHITE) Tj ET Q\n",
    ]);
    doc.page(&content, "");
    let extracted = read(&doc.build());
    assert_verdicts(
        &extracted,
        &[
            ("STROKEWHITE", Some(HiddenWhy::LowContrast)),
            ("FILLWHITE", None),
            ("BOTHWHITE", Some(HiddenWhy::LowContrast)),
        ],
    );
}

#[test]
fn pdf_inline_images_are_areas() {
    let mut doc = Doc::new();
    let content = lines_at(&[
        "BT /F1 10 Tf 10 {y} Td (COVERED) Tj ET q 150 0 0 16 5 {low} cm BI /W 1 /H 1 /BPC 8 /CS /G ID \x00 EI Q\n",
        "q 150 0 0 16 5 {low} cm BI /W 1 /H 1 /BPC 8 /CS /G ID \x00 EI Q BT /F1 10 Tf 10 {y} Td (OVERPARSED) Tj ET\n",
        "q 150 0 0 16 5 {low} cm BI /W 1 /H 1 /BPC 8 /CS /G /F /AHx ID ff> EI Q BT /F1 10 Tf 10 {y} Td (OVERFILTERED) Tj ET\n",
        "q 150 0 0 16 5 {low} cm BI /W 1 /H 1 /BPC 8 /CS /Cs1 ID \x00 EI Q BT /F1 10 Tf 10 {y} Td (OVERNAMED) Tj ET\n",
        "BT /F1 10 Tf 10 {y} Td (COVEREDFILTERED) Tj ET q 150 0 0 16 5 {low} cm BI /W 1 /H 1 /BPC 8 /CS /G /F /AHx ID ff> EI Q\n",
    ]);
    doc.page(&content, "/ColorSpace << /Cs1 /DeviceGray >>");
    let extracted = read(&doc.build());
    assert_verdicts(
        &extracted,
        &[
            ("COVERED", Some(HiddenWhy::Covered)),
            ("OVERPARSED", None),
            ("OVERFILTERED", None),
            ("OVERNAMED", None),
            ("COVEREDFILTERED", Some(HiddenWhy::Covered)),
        ],
    );
    for text in ["OVERPARSED", "OVERFILTERED", "OVERNAMED"] {
        assert_eq!(
            block(&extracted, text).suspect,
            Some("unknown_background"),
            "{text}"
        );
    }
}

#[test]
fn pdf_text_over_an_image_or_shading_is_suspect() {
    let mut doc = Doc::new();
    doc.size = (300.0, 300.0);
    let image = doc.add_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[255]);
    let shading = doc.add("<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>");
    let screen = doc.add("<< /Type /ExtGState /BM /Screen >>");
    let faint = doc.add("<< /Type /ExtGState /ca 0.01 >>");
    let content = [
        // A white image over the top half (y 150 to 300).
        "q 300 0 0 150 0 150 cm /Im Do Q\n".to_owned(),
        lines_from(280, &[
            "1 g BT /F1 10 Tf 10 {y} Td (WHITEONIMAGE) Tj ET 0 g\n",
            "BT /F1 10 Tf 10 {y} Td (BLACKONIMAGE) Tj ET\n",
        ]),
        // An empty clip inside q … Q ends at its Q, and hides nothing after it.
        "q 0 0 0 0 re W n Q\n".to_owned(),
        lines_from(130, &[
            "q 5 {low} 150 16 re W n /Sh sh Q BT /F1 10 Tf 10 {y} Td (OVERSHADING) Tj ET\n",
            "q 1 g 5 {low} 150 16 re f /SC gs 0 g 5 {low} 150 16 re f Q 1 g BT /F1 10 Tf 10 {y} Td (SCREENWHITE) Tj ET 0 g\n",
            "q 0 g 5 {low} 150 16 re 20 {y} 100 4 re f* Q 1 g BT /F1 10 Tf 30 {y} Td (FRAMEHOLE) Tj ET 0 g\n",
        ]),
    ]
    .concat();
    doc.page(
        &content,
        &format!(
            "/XObject << /Im {image} 0 R >> /Shading << /Sh {shading} 0 R >> /ExtGState << /SC {screen} 0 R /FA {faint} 0 R >>"
        ),
    );
    // Shapes whose bounding box is not what they paint, each on a page of its own.
    let shapes = [
        (
            "TRIANGLE",
            "q 0 g 0 0 m 200 0 l 0 200 l h f Q 1 g BT /F1 10 Tf 120 150 Td (TRIANGLE) Tj ET",
        ),
        (
            "BOWTIE",
            "q 0 g 20 20 m 180 180 l 180 20 l 20 180 l h f Q 1 g BT /F1 10 Tf 60 100 Td (BOWTIE) Tj ET",
        ),
        (
            "CURVED",
            "q 0 g 20 20 m 100 60 180 20 v 140 100 180 180 v 100 140 20 180 v 60 100 20 20 v f Q 1 g BT /F1 10 Tf 25 25 Td (CURVED) Tj ET",
        ),
        (
            "DIAMOND",
            "q 0 g 0.7071 0.7071 -0.7071 0.7071 100 0 cm 0 0 141 141 re f Q 1 g BT /F1 10 Tf 5 5 Td (DIAMOND) Tj ET",
        ),
        (
            "SKEWED",
            "q 0 g 1 0 1 1 100 100 cm 0 0 50 50 re f Q 1 g BT /F1 10 Tf 100 140 Td (SKEWED) Tj ET",
        ),
        (
            "ROTATEDCLIP",
            "q 0.7071 0.7071 -0.7071 0.7071 100 0 cm 0 0 141 141 re W n 1 0 0 1 0 0 cm 0 g -100 -100 400 400 re f Q 1 g BT /F1 10 Tf 90 100 Td (ROTATEDCLIP) Tj ET",
        ),
        (
            "FAINTBLACK",
            "q /FA gs 0 g 0 0 200 200 re f Q 1 g BT /F1 10 Tf 20 100 Td (FAINTBLACK) Tj ET",
        ),
        (
            "QUARTERTURN",
            "q 0 g 0 1 -1 0 200 0 cm 0 0 200 200 re f Q 1 g BT /F1 10 Tf 20 100 Td (QUARTERTURN) Tj ET",
        ),
    ];
    for (_, content) in &shapes {
        doc.page(content, &format!("/ExtGState << /FA {faint} 0 R >>"));
    }
    let extracted = read(&doc.build());
    for text in [
        "WHITEONIMAGE",
        "BLACKONIMAGE",
        "OVERSHADING",
        "SCREENWHITE",
        "FRAMEHOLE",
        "TRIANGLE",
        "BOWTIE",
        "CURVED",
        "DIAMOND",
        "SKEWED",
        "ROTATEDCLIP",
        "FAINTBLACK",
    ] {
        let block = block(&extracted, text);
        assert_eq!(
            (block.hidden, block.suspect),
            (None, Some("unknown_background")),
            "{text}"
        );
    }
    let turned = block(&extracted, "QUARTERTURN");
    assert_eq!((turned.hidden, turned.suspect), (None, None));
}

#[test]
fn pdf_transparency_groups_fold_into_their_content() {
    let mut doc = Doc::new();
    doc.size = (300.0, 300.0);
    let state = |doc: &mut Doc, entries: &str| doc.add(format!("<< /Type /ExtGState {entries} >>"));
    let reset = state(&mut doc, "/ca 1 /CA 1 /SMask /None /BM /Normal");
    let zero = state(&mut doc, "/ca 0");
    let mask = state(&mut doc, "/SMask << /Type /Mask /S /Luminosity /G 0 0 R >>");
    let multiply = state(&mut doc, "/BM /Multiply");
    let point3 = state(&mut doc, "/ca 0.3");
    let point01 = state(&mut doc, "/ca 0.01");
    let half = state(&mut doc, "/ca 0.5");
    let veil = state(&mut doc, "/ca 0.95");
    let clear = state(&mut doc, "/ca 0");
    let states = format!(
        "/ExtGState << /R {reset} 0 R /Z {zero} 0 R /M {mask} 0 R /MU {multiply} 0 R /P3 {point3} 0 R /P01 {point01} 0 R /H {half} 0 R /V {veil} 0 R /CL {clear} 0 R >>"
    );
    let group = |doc: &mut Doc, knockout: bool, content: &str, inner: &str| {
        let k = if knockout { "/K true" } else { "" };
        let resources = doc.resources(&format!("{states} {inner}"));
        doc.form(
            &format!("/Group << /S /Transparency {k} >> {resources}"),
            &format!("/R gs {content}"),
        )
    };
    let text_at = |y: i32, word: &str| format!("BT /F1 10 Tf 10 {y} Td ({word}) Tj ET");
    let cover = |y: i32| format!("5 {} 150 16 re f", y - 4);
    let mut page = String::new();
    let mut xobjects = String::new();
    fn add(page: &mut String, xobjects: &mut String, name: &str, outer: &str, form: u32) {
        xobjects.push_str(&format!("/{name} {form} 0 R "));
        page.push_str(&format!("q {outer} /{name} Do Q\n"));
    }
    let g = group(&mut doc, false, &text_at(280, "ZEROGROUP"), "");
    add(&mut page, &mut xobjects, "G1", "/Z gs", g);
    let g = group(&mut doc, false, &text_at(260, "MASKGROUP"), "");
    add(&mut page, &mut xobjects, "G2", "/M gs", g);
    let g = group(&mut doc, false, &text_at(240, "MULTIPLYGROUP"), "");
    add(&mut page, &mut xobjects, "G3", "/MU gs", g);
    let inner = group(&mut doc, false, &text_at(220, "NESTEDTHIRDS"), "");
    let outer = group(
        &mut doc,
        false,
        "/P3 gs /In Do",
        &format!("/XObject << /In {inner} 0 R >>"),
    );
    add(&mut page, &mut xobjects, "G4", "/P3 gs", outer);
    let g = group(&mut doc, false, &format!("0 g {}", cover(200)), "");
    add(&mut page, &mut xobjects, "G5", "/P01 gs", g);
    page.push_str(&format!("1 g {} 0 g\n", text_at(200, "AFTERFAINTGROUP")));
    let g = group(&mut doc, false, &format!("0 g {}", cover(180)), "");
    add(&mut page, &mut xobjects, "G6", "", g);
    page.push_str(&format!("1 g {} 0 g\n", text_at(180, "AFTEROPAQUEGROUP")));
    let g = group(
        &mut doc,
        false,
        &format!("0 g {} 1 g {}", text_at(160, "INHALFGROUP"), cover(160)),
        "",
    );
    add(&mut page, &mut xobjects, "G7", "/H gs", g);
    let g = group(
        &mut doc,
        false,
        &format!(
            "0 g {} q /V gs 1 g {} Q",
            text_at(140, "INHALFGROUPVEIL"),
            cover(140)
        ),
        "",
    );
    add(&mut page, &mut xobjects, "G8", "/H gs", g);
    let g = group(
        &mut doc,
        false,
        &format!("0 g {} 1 g {}", text_at(120, "INOPAQUEGROUP"), cover(120)),
        "",
    );
    add(&mut page, &mut xobjects, "G9", "", g);
    let g = group(
        &mut doc,
        true,
        &format!(
            "0 g {} q /CL gs 1 g {} Q",
            text_at(100, "KNOCKOUT"),
            cover(100)
        ),
        "",
    );
    add(&mut page, &mut xobjects, "G10", "", g);
    let g = group(
        &mut doc,
        false,
        &format!(
            "0 g {} q /CL gs 1 g {} Q",
            text_at(80, "NOKNOCKOUT"),
            cover(80)
        ),
        "",
    );
    add(&mut page, &mut xobjects, "G11", "", g);
    let inner = group(
        &mut doc,
        false,
        &format!("0 g {} 1 g {}", text_at(60, "NESTEDINHALF"), cover(60)),
        "",
    );
    let outer = group(
        &mut doc,
        false,
        "/In Do",
        &format!("/XObject << /In {inner} 0 R >>"),
    );
    add(&mut page, &mut xobjects, "G12", "/H gs", outer);
    let inner = group(
        &mut doc,
        false,
        &format!("0 g {}", text_at(40, "INSIDEKNOCKOUT")),
        "",
    );
    let outer = group(
        &mut doc,
        true,
        &format!("/In Do q /CL gs 1 g {} Q", cover(40)),
        &format!("/XObject << /In {inner} 0 R >>"),
    );
    add(&mut page, &mut xobjects, "G13", "", outer);
    doc.page(&page, &format!("{states} /XObject << {xobjects} >>"));
    let extracted = read(&doc.build());
    let unknown = Some(HiddenWhy::UnknownPaint);
    assert_verdicts(
        &extracted,
        &[
            ("ZEROGROUP", Some(HiddenWhy::Transparent)),
            ("MASKGROUP", unknown),
            ("MULTIPLYGROUP", unknown),
            ("NESTEDTHIRDS", Some(HiddenWhy::Transparent)),
            ("AFTERFAINTGROUP", None),
            ("AFTEROPAQUEGROUP", None),
            ("INHALFGROUP", unknown),
            ("INHALFGROUPVEIL", unknown),
            ("INOPAQUEGROUP", Some(HiddenWhy::Covered)),
            ("KNOCKOUT", unknown),
            ("NOKNOCKOUT", None),
            ("NESTEDINHALF", unknown),
            ("INSIDEKNOCKOUT", unknown),
        ],
    );
    assert_eq!(
        block(&extracted, "AFTERFAINTGROUP").suspect,
        Some("unknown_background")
    );
    assert_eq!(block(&extracted, "AFTEROPAQUEGROUP").suspect, None);
}

/// Each page's blocks, by page number from 1.
fn on_page(
    extracted: &Extracted,
    page: u32,
) -> Vec<(String, Option<HiddenWhy>, Option<&'static str>)> {
    extracted
        .blocks
        .iter()
        .filter(|block| block.locator == Locator::PdfPage { page })
        .map(|block| (block.text.clone(), block.hidden, block.suspect))
        .collect()
}

#[test]
fn pdf_form_xobjects_inherit_the_invoking_state() {
    let (mut doc, properties) = optional_doc();
    let zero = doc.add("<< /Type /ExtGState /ca 0 >>");
    let off = properties
        .split("/Off ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_owned();
    // A Form that sets no colour or state of its own.
    let plain = doc.form("", "BT /F1 48 Tf 20 80 Td (9999) Tj ET");
    let boxed = doc.form("/BBox [0 0 10 10]", "BT /F1 48 Tf 20 80 Td (9999) Tj ET");
    let shifted = doc.form(
        "/Matrix [1 0 0 1 0 100]",
        "BT /F1 48 Tf 20 80 Td (9999) Tj ET",
    );
    let own_oc = doc.form(
        &format!("/OC {off} 0 R"),
        "BT /F1 48 Tf 20 80 Td (9999) Tj ET",
    );
    let hidden_black = doc.form(&format!("/OC {off} 0 R"), "0 g 0 0 300 300 re f");
    let sets_black = doc.form("", "0 g");
    let resources = format!(
        "/ExtGState << /Z {zero} 0 R >> {properties} /XObject << /P {plain} 0 R /B {boxed} 0 R /S {shifted} 0 R \
         /O {own_oc} 0 R /HB {hidden_black} 0 R /SB {sets_black} 0 R >>"
    );
    let cases: &[(&str, Option<HiddenWhy>)] = &[
        ("q 1 g /P Do Q", Some(HiddenWhy::LowContrast)),
        ("q /Z gs /P Do Q", Some(HiddenWhy::Transparent)),
        ("q 0.02 0 0 0.02 0 0 cm /P Do Q", Some(HiddenWhy::Tiny)),
        (
            "q 1 0 0 1 1000 0 cm /P Do Q",
            Some(HiddenWhy::OutsideCropBox),
        ),
        ("q /B Do Q", Some(HiddenWhy::Clipped)),
        ("q 0 g /S Do Q", None),
        (
            "q 0 g /OC /Off BDC /P Do EMC Q",
            Some(HiddenWhy::OptionalContentOff),
        ),
        ("q 0 g /OC /On BDC /P Do EMC Q", None),
        ("q 0 g /O Do Q", Some(HiddenWhy::OptionalContentOff)),
    ];
    for (content, _) in cases {
        doc.page(content, &resources);
    }
    doc.page(
        "/HB Do 1 g BT /F1 10 Tf 20 20 Td (OVERHIDDENBLACK) Tj ET",
        &resources,
    );
    doc.page(
        "1 g /SB Do BT /F1 10 Tf 20 20 Td (AFTERRETURN) Tj ET",
        &resources,
    );
    let extracted = read(&doc.build());
    for (index, (content, why)) in cases.iter().enumerate() {
        assert_eq!(
            on_page(&extracted, index as u32 + 1),
            vec![("9999".to_owned(), *why, None)],
            "{content}"
        );
    }
    assert_verdicts(
        &extracted,
        &[
            ("OVERHIDDENBLACK", Some(HiddenWhy::LowContrast)),
            ("AFTERRETURN", Some(HiddenWhy::LowContrast)),
        ],
    );
    // The Form's /Matrix and the CTM place its text: at 20, 80 + 100, at 48 pt.
    let shown = extracted
        .blocks
        .iter()
        .find(|block| block.locator == Locator::PdfPage { page: 6 })
        .unwrap();
    let rect = shown.lines[0].rect;
    assert_eq!(
        (rect[0], rect[1], rect[3]),
        (20.0, 180.0 - 0.2 * 48.0, 180.0 + 0.8 * 48.0)
    );
}

/// Limits scaled down so a bomb proves its cap in milliseconds: 1 MiB per stream.
fn small_limits() -> DocLimits {
    DocLimits {
        max_stream_bytes: 1 << 20,
        ..DocLimits::default()
    }
}

/// 65 KiB of zlib data that inflates to 64 MiB of zeros, built once per test binary.
fn bomb() -> &'static [u8] {
    static BOMB: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BOMB.get_or_init(|| zeros(64 << 20))
}

/// `read_with` on this thread, with the bytes the capped decoder produced and the walks run.
fn read_counted(bytes: &[u8], limits: &DocLimits) -> (Result<Extracted, DocError>, u64, u64) {
    let (decoded, walks) = (
        docs::pdf_guard::decoded_bytes(),
        docs::pdf::output_doc_calls(),
    );
    let result = read_with(bytes, limits);
    (
        result,
        docs::pdf_guard::decoded_bytes() - decoded,
        docs::pdf::output_doc_calls() - walks,
    )
}

#[test]
fn pdf_flate_bomb_fails_bounded() {
    let mut doc = Doc::new();
    let contents = doc.add(stream("/Filter /FlateDecode", bomb()));
    let (raw, catalog) = {
        doc.page("", "");
        let (mut raw, catalog) = doc.into_raw();
        let page = (1..raw.size())
            .find(|id| raw_has(&raw, *id, "/Type /Page "))
            .unwrap();
        let body = raw_body(&raw, page).replace(
            &format!("/Contents {} 0 R", page - 1),
            &format!("/Contents {contents} 0 R"),
        );
        raw.set(page, body);
        (raw, catalog)
    };
    let limits = small_limits();
    let (result, decoded, walks) = read_counted(&raw.build(catalog, ""), &limits);
    assert_eq!(result.unwrap_err(), DocError::TooComplex("stream_bytes"));
    assert!(decoded <= limits.max_stream_bytes, "{decoded}");
    assert_eq!(walks, 0);
    // Twice compressed, 244 bytes inflate past the cap the same way.
    let mut doc = Doc::new();
    doc.add(stream(
        "/Filter [/FlateDecode /FlateDecode]",
        &flate(bomb()),
    ));
    doc.page("", "");
    assert_eq!(
        read_counted(&doc.build(), &limits).0.unwrap_err(),
        DocError::TooComplex("stream_bytes")
    );
}

fn raw_has(raw: &Raw, id: u32, needle: &str) -> bool {
    raw_body(raw, id).contains(needle)
}

fn raw_body(raw: &Raw, id: u32) -> String {
    let file = raw.build(1, "");
    let header = format!("\n{id} 0 obj\n");
    let at = file
        .windows(header.len())
        .position(|window| window == header.as_bytes())
        .unwrap()
        + header.len();
    let end = at
        + file[at..]
            .windows(7)
            .position(|window| window == b"\nendobj")
            .unwrap();
    String::from_utf8_lossy(&file[at..end]).into_owned()
}

#[tokio::test]
async fn pdf_font_and_cmap_bombs_fail_bounded() {
    let limits = small_limits();
    let font_bomb = {
        let mut doc = Doc::new();
        let file = doc.add(stream("/Filter /FlateDecode", bomb()));
        let descriptor = doc.add(format!(
            "<< /Type /FontDescriptor /FontName /B /Flags 32 /FontFile2 {file} 0 R >>"
        ));
        let font = doc.add(format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /B /FontDescriptor {descriptor} 0 R >>"
        ));
        doc.page(
            "BT /B 10 Tf 10 10 Td (x) Tj ET",
            &format!("/Font << /B {font} 0 R >>"),
        );
        doc.build()
    };
    let cmap_bomb = {
        let mut doc = Doc::new();
        let cmap = doc.add(stream("/Filter /FlateDecode", bomb()));
        let font = doc.add(format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode {cmap} 0 R >>"
        ));
        doc.page(
            "BT /B 10 Tf 10 10 Td (x) Tj ET",
            &format!("/Font << /B {font} 0 R >>"),
        );
        doc.build()
    };
    let pool = docs::DocPool::new(1, limits.clone()).unwrap();
    let workspace = katwork::ids::WorkspaceId::new();
    let cancel = tokio_util::sync::CancellationToken::new();
    for bomb in [font_bomb, cmap_bomb] {
        let outcome = pool
            .run(workspace, &cancel, None, move |limits| {
                let before = docs::pdf_guard::decoded_bytes();
                let result = docs::extract(MediaKind::Pdf, &bomb, limits);
                Ok((result, docs::pdf_guard::decoded_bytes() - before))
            })
            .await
            .unwrap();
        assert_eq!(outcome.0.unwrap_err(), DocError::TooComplex("stream_bytes"));
        assert!(outcome.1 <= limits.max_stream_bytes);
        // The next job on that thread reads a file.
        let mut doc = Doc::new();
        doc.page(&text(10.0, 10.0, 100.0, "after the bomb"), "");
        let next = doc.build();
        let read = pool
            .run(workspace, &cancel, None, move |limits| {
                docs::extract(MediaKind::Pdf, &next, limits)
            })
            .await
            .unwrap();
        assert_eq!(visible(&read), vec!["after the bomb"]);
    }
}

/// A one-page file of exactly `total` objects.
fn objects(total: u32) -> Vec<u8> {
    let mut doc = Doc::new();
    // The catalog, the page tree, two fonts with a descendant and a map, and the page with its
    // content stream are eight.
    for _ in 8..total {
        doc.add("0");
    }
    doc.page("", "");
    doc.build()
}

/// `count` objects; each `/Type /ObjStm` holds one member, `inflate` makes one inflate past
/// `max_objstm_bytes`.
fn object_streams(count: usize, inflate: bool) -> Vec<u8> {
    let mut doc = Doc::new();
    for at in 0..count {
        doc.add(stream(
            "/Type /ObjStm /N 1 /First 6",
            format!("9{at:04} 0 << >>").as_bytes(),
        ));
    }
    if inflate {
        doc.add(stream(
            "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode",
            &zeros(3 << 20),
        ));
    }
    doc.page(&text(10.0, 10.0, 100.0, "x"), "");
    doc.build()
}

#[test]
fn pdf_object_stream_budget_is_enforced_during_load() {
    let limits = limits();
    let (result, _, walks) = read_counted(&object_streams(65, false), &limits);
    assert_eq!(result.unwrap_err(), DocError::TooComplex("object_streams"));
    assert_eq!(walks, 0);
    assert!(read_with(&object_streams(64, false), &limits).is_ok());
    let (result, decoded, walks) = read_counted(&object_streams(0, true), &limits);
    assert_eq!(result.unwrap_err(), DocError::TooComplex("objstm_bytes"));
    assert!(decoded <= limits.max_objstm_bytes);
    assert_eq!(walks, 0);
    // A file of 500,001 objects, past the default cap of 500,000.
    let file = objects(500_001);
    assert!(file.len() as u64 <= limits.max_input_bytes);
    let (result, _, walks) = read_counted(&file, &limits);
    assert_eq!(result.unwrap_err(), DocError::TooComplex("objects"));
    assert_eq!(walks, 0);
    // And at a scaled cap: 1,000 objects read, 1,001 do not.
    let small = DocLimits {
        max_pdf_objects: 1_000,
        ..DocLimits::default()
    };
    assert!(read_with(&objects(1_000), &small).is_ok());
    assert_eq!(
        read_with(&objects(1_001), &small).unwrap_err(),
        DocError::TooComplex("objects")
    );
    // A container that does not decode is malformed, never a page missing its objects.
    let mut doc = Doc::new();
    doc.add(stream(
        "/Type /ObjStm /N 1 /First 4 /Filter /FlateDecode",
        b"\x78\x9c\xff\xff\xff",
    ));
    doc.page("", "");
    assert_eq!(
        read_counted(&doc.build(), &limits).0.unwrap_err(),
        DocError::Malformed("object_stream")
    );
}

#[test]
fn pdf_objects_inside_object_streams_survive_the_filter() {
    let mut doc = Doc::new();
    doc.page(&text(10.0, 10.0, 100.0, "packed font"), "");
    let (raw, catalog) = doc.into_raw();
    // Helvetica (3), the page tree (2) and the catalog (1) live in an object stream.
    let extracted = read(&raw.build_packed(catalog, &[1, 2, 3]));
    assert_eq!(visible(&extracted), vec!["packed font"]);
}

#[test]
fn pdf_streams_with_image_filters_outside_images_are_malformed() {
    let mut doc = Doc::new();
    let file = doc.add(stream("/Filter /DCTDecode", b"\xFF\xD8\xFF"));
    let descriptor = doc.add(format!(
        "<< /Type /FontDescriptor /FontName /B /FontFile2 {file} 0 R >>"
    ));
    let font = doc.add(format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /B /FontDescriptor {descriptor} 0 R >>"
    ));
    doc.page("", &format!("/Font << /B {font} 0 R >>"));
    assert_eq!(error(&doc.build()), DocError::Malformed("stream_filter"));
    let mut doc = Doc::new();
    doc.add(stream("/Filter /NoSuchFilter", b"BT ET"));
    doc.page("", "");
    assert_eq!(error(&doc.build()), DocError::Malformed("stream_filter"));
    // An image keeps its codec and is never decoded.
    let mut doc = Doc::new();
    let image = doc.add(stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /DCTDecode", b"\xFF\xD8"));
    doc.page(
        &format!(
            "q 10 0 0 10 0 0 cm /Im Do Q {}",
            text(10.0, 50.0, 50.0, "beside")
        ),
        &format!("/XObject << /Im {image} 0 R >>"),
    );
    assert_eq!(visible(&read(&doc.build())), vec!["beside"]);
}

#[test]
fn pdf_page_tree_cycle_fails_bounded() {
    let mut doc = Doc::new();
    doc.page(&text(10.0, 10.0, 100.0, "x"), "");
    let (mut raw, catalog) = doc.into_raw();
    let pages = 2;
    raw.set(
        pages,
        format!("<< /Type /Pages /Kids [{pages} 0 R] /Count 1 >>"),
    );
    assert_eq!(
        error(&raw.build(catalog, "")),
        DocError::Malformed("page_tree_cycle")
    );
    // A page whose /Parent is itself, and no media box or resources of its own.
    let mut raw = Raw::new();
    let catalog = raw.reserve();
    let pages = raw.reserve();
    let page = raw.reserve();
    let contents = raw.add(stream("", b"BT /F1 10 Tf (x) Tj ET"));
    raw.set(
        page,
        format!("<< /Type /Page /Parent {page} 0 R /Contents {contents} 0 R >>"),
    );
    raw.set(
        pages,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>"),
    );
    raw.set(catalog, format!("<< /Type /Catalog /Pages {pages} 0 R >>"));
    let extracted = read(&raw.build(catalog, ""));
    assert_eq!(extracted.pages.len(), 1);
    // A kid that does not exist is skipped.
    let mut doc = Doc::new();
    doc.page(&text(10.0, 10.0, 100.0, "kept"), "");
    let (mut raw, catalog) = doc.into_raw();
    let body = raw_body(&raw, 2).replace("/Kids [", "/Kids [999 0 R ");
    raw.set(2, body);
    assert_eq!(visible(&read(&raw.build(catalog, ""))), vec!["kept"]);
}

#[test]
fn pdf_deep_xobject_nesting_fails_bounded() {
    let mut doc = Doc::new();
    let mut inner = doc.form("", "BT /F1 10 Tf 10 10 Td (deep) Tj ET");
    for _ in 1..100 {
        inner = doc.form(
            &doc.resources(&format!("/XObject << /N {inner} 0 R >>")),
            "/N Do",
        );
    }
    doc.page("/N Do", &format!("/XObject << /N {inner} 0 R >>"));
    let (result, _, walks) = read_counted(&doc.build(), &limits());
    assert_eq!(result.unwrap_err(), DocError::TooComplex("xobject_depth"));
    assert_eq!(walks, 0);
    let mut doc = Doc::new();
    let id = doc.raw.reserve();
    doc.raw.set(id, stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /S {id} 0 R >> >>"), b"/S Do"));
    doc.page("/S Do", &format!("/XObject << /S {id} 0 R >>"));
    assert_eq!(error(&doc.build()), DocError::Malformed("xobject_cycle"));
    // Eight levels read.
    let mut doc = Doc::new();
    let mut inner = doc.form("", "BT /F1 10 Tf 10 10 Td (eight) Tj ET");
    for _ in 1..8 {
        inner = doc.form(
            &doc.resources(&format!("/XObject << /N {inner} 0 R >>")),
            "/N Do",
        );
    }
    doc.page("/N Do", &format!("/XObject << /N {inner} 0 R >>"));
    assert_eq!(visible(&read(&doc.build())), vec!["eight"]);
}

#[test]
fn pdf_form_fanout_fails_bounded() {
    // Four levels, each drawing the next 100 times: 10^8 Form runs from a few objects.
    let mut doc = Doc::new();
    let mut inner = doc.form("", "q Q");
    for _ in 0..4 {
        let calls = "/N Do ".repeat(100);
        inner = doc.form(
            &doc.resources(&format!("/XObject << /N {inner} 0 R >>")),
            &calls,
        );
    }
    doc.page("/N Do", &format!("/XObject << /N {inner} 0 R >>"));
    assert_eq!(error(&doc.build()), DocError::TooComplex("content_ops"));
}

#[test]
fn pdf_over_max_pages_is_too_complex() {
    let mut doc = Doc::new();
    for _ in 0..501 {
        doc.page("", "");
    }
    let (result, _, walks) = read_counted(&doc.build(), &limits());
    assert_eq!(result.unwrap_err(), DocError::TooComplex("pages"));
    assert_eq!(walks, 0);
    let mut doc = Doc::new();
    for _ in 0..500 {
        doc.page("", "");
    }
    assert_eq!(read(&doc.build()).pages.len(), 500);
}

#[test]
fn pdf_encrypted_is_encrypted() {
    let mut doc = Doc::new();
    let content = doc.add(stream(
        "/Filter /FlateDecode",
        &flate(b"BT /F1 10 Tf (x) Tj ET"),
    ));
    doc.page("", "");
    let (raw, catalog) = doc.into_raw();
    let encrypt = "/Encrypt << /Filter /Standard /V 1 /R 2 /O <00> /U <00> /P -4 >>";
    let escaped = "/Encr#79pt << /Filter /Standard /V 1 /R 2 /O <00> /U <00> /P -4 >>";
    let _ = content;
    for trailer in [encrypt, escaped] {
        let (result, decoded, walks) = read_counted(&raw.build(catalog, trailer), &limits());
        assert_eq!(result.unwrap_err(), DocError::Encrypted, "{trailer}");
        assert_eq!((decoded, walks), (0, 0), "nothing is decoded: {trailer}");
    }
}

#[test]
fn pdf_encrypted_variants_are_refused_before_any_decode() {
    // A file lopdf itself encrypts (RC4, an empty user password lopdf would open).
    let mut doc = lopdf::Document::with_version("1.7");
    let pages = doc.new_object_id();
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let contents = doc.add_object(lopdf::Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 10 10 Td (secret) Tj ET".to_vec(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages, "Contents" => contents,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
    });
    doc.objects.insert(
        pages,
        lopdf::Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    doc.trailer.set(
        "ID",
        lopdf::Object::Array(vec![
            lopdf::Object::string_literal("0123456789abcdef"),
            lopdf::Object::string_literal("0123456789abcdef"),
        ]),
    );
    let version = lopdf::EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password: "",
        key_length: 128,
        permissions: lopdf::Permissions::all(),
    };
    let state = lopdf::EncryptionState::try_from(version).unwrap();
    doc.encrypt(&state).unwrap();
    let mut encrypted = Vec::new();
    doc.save_to(&mut encrypted).unwrap();
    let (result, decoded, walks) = read_counted(&encrypted, &limits());
    assert_eq!(result.unwrap_err(), DocError::Encrypted);
    assert_eq!((decoded, walks), (0, 0));
}

#[test]
fn pdf_one_lopdf_in_the_lock_file_without_rayon() {
    // The host's packages only: another platform's (Android's, Windows') are never fetched for
    // this build, and --offline cannot fetch them.
    let version = std::process::Command::new(env!("CARGO"))
        .arg("-vV")
        .output()
        .unwrap();
    let host = String::from_utf8_lossy(&version.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .expect("cargo -vV names its host");
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--filter-platform",
            &host,
            "--manifest-path",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let lopdf: Vec<&serde_json::Value> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| package["name"] == "lopdf")
        .collect();
    assert_eq!(lopdf.len(), 1, "exactly one lopdf");
    assert_eq!(lopdf[0]["version"], "0.46.0");
    let id = lopdf[0]["id"].as_str().unwrap();
    let node = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .unwrap();
    let features: Vec<&str> = node["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|feature| feature.as_str().unwrap())
        .collect();
    assert!(!features.contains(&"rayon"), "{features:?}");
    assert!(
        metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|package| package["name"] != "rayon")
    );
}

#[test]
fn pdf_javascript_and_attachments_are_ignored() {
    let mut doc = Doc::new();
    let data = doc.add(stream("/Type /EmbeddedFile", b"attached"));
    let spec = doc.add(format!(
        "<< /Type /Filespec /F (a.txt) /EF << /F {data} 0 R >> >>"
    ));
    doc.catalog(&format!(
        "/OpenAction << /S /JavaScript /JS (app.alert(1)) >> /Names << /EmbeddedFiles << /Names [(a.txt) {spec} 0 R] >> >>"
    ));
    doc.page(&text(10.0, 10.0, 100.0, "page text"), "");
    let extracted = read(&doc.build());
    assert_eq!(visible(&extracted), vec!["page text"]);
    assert_eq!(
        extracted.warnings,
        vec![
            ExtractWarning::ActiveContentIgnored { what: "pdf_script" },
            ExtractWarning::EmbeddedContentIgnored {
                what: "pdf_attachment"
            },
        ]
    );
    // The same anywhere: a link annotation's action, a field's /AA, an XFA form. A plain link
    // and a destination are not active content.
    let mut doc = Doc::new();
    doc.add("<< /Type /Annot /Subtype /Link /A << /S /Launch /F (calc.exe) >> >>");
    doc.add("<< /FT /Tx /AA << /K << /S /JavaScript /JS (x) >> >> >>");
    doc.catalog("/AcroForm << /Fields [] /XFA [(x) 0 0 R] >> /OpenAction [3 0 R /Fit]");
    doc.page(&text(10.0, 10.0, 100.0, "page text"), "");
    let warnings = read(&doc.build()).warnings;
    assert_eq!(
        warnings,
        vec![
            ExtractWarning::ActiveContentIgnored { what: "pdf_script" },
            ExtractWarning::ActiveContentIgnored { what: "pdf_xfa" },
        ]
    );
    let mut doc = Doc::new();
    doc.add("<< /Type /Annot /Subtype /Link /A << /S /URI /URI (https://example.com) >> >>");
    doc.catalog("/OpenAction << /S /GoTo /D [3 0 R /Fit] >>");
    doc.page(&text(10.0, 10.0, 100.0, "page text"), "");
    assert_eq!(read(&doc.build()).warnings, vec![]);
}

#[test]
fn pdf_text_layer_suspect_cases() {
    let mut doc = Doc::new();
    // A Type 0 font with no ToUnicode map: its codes have no text.
    let descendant = doc.add("<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>");
    let bare = doc.add(format!("<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [{descendant} 0 R] >>"));
    doc.page(
        "BT /X 10 Tf 10 100 Td <00410042> Tj ET",
        &format!("/Font << /X {bare} 0 R >>"),
    );
    // A simple font with no ToUnicode whose text is not ASCII (Latin-1 codes for Thai).
    doc.page("BT /F1 10 Tf 10 100 Td (\\341\\342\\343) Tj ET", "");
    // Replacement characters.
    doc.page(&utext(10.0, 10.0, 100.0, "ab\u{FFFD}\u{FFFD}"), "");
    // An image and nothing else.
    let image = doc.add_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[0]);
    doc.page(
        "q 200 0 0 200 0 0 cm /Im Do Q",
        &format!("/XObject << /Im {image} 0 R >>"),
    );
    // A Thai mark with no base.
    doc.page(&utext(10.0, 10.0, 100.0, "\u{E48}กา"), "");
    doc.page(&utext(10.0, 10.0, 100.0, "กำไร น้ำ"), "");
    let extracted = read(&doc.build());
    let layers: Vec<TextLayer> = extracted.pages.iter().map(|page| page.text_layer).collect();
    assert_eq!(
        layers,
        vec![
            TextLayer::Suspect("tounicode"),
            TextLayer::Suspect("tounicode"),
            TextLayer::Suspect("fffd"),
            TextLayer::Absent,
            TextLayer::Suspect("marks"),
            TextLayer::Ok,
        ]
    );
    assert!(
        extracted
            .warnings
            .contains(&ExtractWarning::SuspectTextLayer {
                page: 1,
                why: "tounicode"
            })
    );
    assert!(
        extracted
            .warnings
            .contains(&ExtractWarning::NoTextLayer { page: 4 })
    );
}

#[test]
fn pdf_searchable_scan_is_absent() {
    let scan = |mode: u32| {
        let mut doc = Doc::new();
        let image = doc.add_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[255]);
        doc.page(
            &format!("q 300 0 0 300 0 0 cm /Im Do Q BT {mode} Tr /F1 10 Tf 20 100 Td (scanned words) Tj ET"),
            &format!("/XObject << /Im {image} 0 R >>"),
        );
        read(&doc.build())
    };
    let invisible = scan(3);
    assert_eq!(invisible.pages[0].text_layer, TextLayer::Absent);
    assert_eq!(
        hidden(&invisible),
        vec![("scanned words", HiddenWhy::InvisibleMode)]
    );
    assert!(visible(&invisible).is_empty());
    let shown = scan(0);
    assert_eq!(shown.pages[0].text_layer, TextLayer::Ok);
    assert_eq!(
        block(&shown, "scanned words").suspect,
        Some("unknown_background")
    );
}

/// A document of every kind the walker reads: Flate content, a Form, an embedded Type 1 font
/// with a ToUnicode map, an ICC profile, an Indexed table, a sampled tint transform, images.
fn every_stream_kind() -> Vec<u8> {
    let mut doc = Doc::new();
    let program = doc.add(stream(
        "/Filter /FlateDecode /Length1 40",
        &flate(b"/Encoding StandardEncoding def eexec"),
    ));
    let to_unicode = doc.add(stream(
        "/Filter /FlateDecode",
        &flate(b"1 beginbfchar <41> <0E01> endbfchar"),
    ));
    let descriptor = doc.add(format!(
        "<< /Type /FontDescriptor /FontName /E /FontFile {program} 0 R >>"
    ));
    let font = doc.add(format!("<< /Type /Font /Subtype /Type1 /BaseFont /E /FontDescriptor {descriptor} 0 R /ToUnicode {to_unicode} 0 R >>"));
    let icc = doc.add(stream("/N 3 /Filter /FlateDecode", &flate(b"profile")));
    let lookup = doc.add(stream("/Filter /ASCIIHexDecode", b"000000 FFFFFF>"));
    let samples = doc.add(stream("/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [2] /BitsPerSample 8 /Filter /FlateDecode", &flate(&[255, 0])));
    let form = doc.add(stream(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Filter /FlateDecode {}",
            doc.resources("")
        ),
        &flate(b"BT /F1 10 Tf 10 30 Td (in a form) Tj ET"),
    ));
    let image = doc.add(stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode", &flate(&[0])));
    let content = doc.add(stream(
        "/Filter /FlateDecode",
        &flate(b"BT /E 10 Tf 10 150 Td (A) Tj ET /C cs 0 0 0 sc BT /F1 10 Tf 10 120 Td (icc) Tj ET /I cs 0 sc BT /F1 10 Tf 10 100 Td (indexed) Tj ET /S cs 1 sc BT /F1 10 Tf 10 80 Td (sampled) Tj ET /Fm Do q 10 0 0 10 150 150 cm /Im Do Q"),
    ));
    let page = doc.page("", &format!(
        "/Font << /E {font} 0 R >> /ColorSpace << /C [/ICCBased {icc} 0 R] /I [/Indexed /DeviceRGB 1 {lookup} 0 R] /S [/Separation /Spot /DeviceGray {samples} 0 R] >> /XObject << /Fm {form} 0 R /Im {image} 0 R >>"
    ));
    let (mut raw, catalog) = doc.into_raw();
    let body = raw_body(&raw, page).replace(
        &format!("/Contents {} 0 R", page - 1),
        &format!("/Contents {content} 0 R"),
    );
    raw.set(page, body);
    raw.build(catalog, "")
}

#[test]
fn pdf_streams_reach_pdf_extract_decoded() {
    pdf_extract::test_hooks::reset();
    let extracted = read(&every_stream_kind());
    assert_eq!(visible(&extracted).concat(), "กiccindexedsampledin a form");
    // pdf-extract read the content, the Form, the font program, the CMap, the lookup table and
    // the samples, and none of them had a /Filter.
    assert!(
        pdf_extract::test_hooks::streams_read() >= 6,
        "{}",
        pdf_extract::test_hooks::streams_read()
    );
    assert_eq!(pdf_extract::test_hooks::filtered_get_contents(), 0);
}

#[test]
fn pdf_extract_never_decompresses() {
    pdf_extract::test_hooks::reset();
    let mut files: Vec<Vec<u8>> = vec![every_stream_kind(), object_streams(3, false)];
    for name in ["thai.pdf", "white.pdf", "gradient.pdf"] {
        files.push(std::fs::read(format!("{FIXTURES}/{name}")).unwrap());
    }
    for file in &files {
        read(file);
    }
    for hostile in [object_streams(65, false), object_streams(0, true)] {
        let _ = read_with(&hostile, &limits());
    }
    assert!(pdf_extract::test_hooks::streams_read() > 0);
    assert_eq!(pdf_extract::test_hooks::filtered_get_contents(), 0);
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pdf");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
}

/// The text a Typst source sets: every line but `#set` rules, and the body of `#text(..)[..]`.
fn typst_text(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.starts_with("#set"))
        .map(|line| match line.strip_prefix("#text(") {
            Some(rest) => rest
                .split_once(")[")
                .map_or("", |(_, body)| body.trim_end_matches(']')),
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Thai text as matching compares it: `normalize_th` (which composes SARA AM), without white
/// space, since Typst breaks Thai lines where the source has none.
fn comparable(text: &str) -> String {
    katwork_thai::normalize_th(text)
        .text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[test]
fn pdf_thai_fixture_text_matches_its_source() {
    let source = std::fs::read_to_string(format!("{FIXTURES}/thai.typ")).unwrap();
    let extracted = read(&fixture("thai.pdf"));
    assert!(hidden(&extracted).is_empty());
    let text = visible(&extracted).join("\n");
    assert_eq!(comparable(&text), comparable(&typst_text(&source)));
    // SARA AM stays one character, and a tone mark over an above vowel keeps its order.
    assert!(
        text.contains("กำไร")
            && text.contains("น\u{E49}ำ")
            && text.contains("ที\u{E48}")
            && text.contains("1,250")
    );
    // The marks gate passes a Typst page: every Thai mark has its consonant.
    assert!(
        extracted
            .pages
            .iter()
            .all(|page| page.text_layer == TextLayer::Ok)
    );
    // A paragraph Typst wrapped over four lines is one block with one line box per line.
    let paragraph = extracted
        .blocks
        .iter()
        .find(|block| block.text.contains("1,250"))
        .unwrap();
    assert_eq!(paragraph.lines.len(), 4);
    assert_eq!(paragraph.text.matches('\n').count(), 3);
}

#[test]
fn typst_white_text_is_low_contrast() {
    let extracted = read(&fixture("white.pdf"));
    assert_eq!(
        visible(&extracted),
        vec!["ข\u{E49}อความท\u{E35}\u{E48}มองเห\u{E47}น visible text"]
    );
    assert_eq!(
        hidden(&extracted),
        vec![
            ("ซ\u{E48}อนส\u{E35}ขาว hidden white", HiddenWhy::LowContrast),
            ("เก\u{E37}อบขาว near white", HiddenWhy::LowContrast),
        ]
    );
}

#[test]
fn typst_gradient_text_is_hidden() {
    let extracted = read(&fixture("gradient.pdf"));
    assert_eq!(hidden(&extracted), vec![("9,999", HiddenWhy::UnknownPaint)]);
    assert!(
        visible(&extracted)
            .iter()
            .all(|text| !text.contains("9,999"))
    );
    assert_eq!(visible(&extracted), vec!["Total revenue"]);
}

const PDF_MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pdf/MANIFEST.md"
);

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `MANIFEST.md`'s rows: the first cell (a path in backticks) and the second (its SHA-256).
fn manifest_rows() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(PDF_MANIFEST)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            (
                cells[1].trim_matches('`').to_owned(),
                cells[2].trim_matches('`').to_owned(),
            )
        })
        .collect()
}

#[test]
fn pdf_fixtures_match_the_manifest() {
    let rows = manifest_rows();
    let mut committed = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(FIXTURES).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != "MANIFEST.md" {
            committed.insert(
                format!("fixtures/pdf/{name}"),
                sha256(&std::fs::read(entry.path()).unwrap()),
            );
        }
    }
    let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/corpus/pdf");
    for entry in std::fs::read_dir(corpus).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        committed.insert(
            format!("fuzz/corpus/pdf/{name}"),
            sha256(&std::fs::read(entry.path()).unwrap()),
        );
    }
    let fonts = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts");
    for name in ["Sarabun-Regular.ttf", "NotoSansThai-Regular.ttf"] {
        committed.insert(
            format!("assets/fonts/{name}"),
            sha256(&std::fs::read(format!("{fonts}/{name}")).unwrap()),
        );
    }
    assert_eq!(committed, rows);
}

/// The `pdf` fuzz target's seeds: ordinary files and the hostile shapes of PdfCraft's and
/// craft-libs' patch notes and of this task's tests, each rebuilt here.
fn seeds() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let simple = {
        let mut doc = Doc::new();
        doc.page(
            &[
                text(10.0, 20.0, 170.0, "First line"),
                text(10.0, 20.0, 158.0, "second line"),
            ]
            .concat(),
            "",
        );
        doc.page(&utext(12.0, 20.0, 100.0, "กำไร 1,250 บาท"), "");
        doc.build()
    };
    let cycle = {
        let mut doc = Doc::new();
        doc.page(&text(10.0, 10.0, 100.0, "x"), "");
        let (mut raw, catalog) = doc.into_raw();
        raw.set(2, "<< /Type /Pages /Kids [2 0 R] /Count 1 >>");
        raw.build(catalog, "")
    };
    let wide = {
        let mut doc = Doc::new();
        let cid =
            doc.add("<< /Type /Font /Subtype /CIDFontType2 /BaseFont /W /W [0 4294967295 500] >>");
        let font = doc.add(format!("<< /Type /Font /Subtype /Type0 /BaseFont /W /Encoding /Identity-H /DescendantFonts [{cid} 0 R] >>"));
        doc.page(
            "BT /W 10 Tf 10 10 Td <00010002> Tj ET",
            &format!("/Font << /W {font} 0 R >>"),
        );
        doc.build()
    };
    let columns = {
        let mut doc = Doc::new();
        let contents = doc.add(stream(
            "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4294967295 >>",
            &flate(b"\x00BT ET"),
        ));
        let page = doc.page("", "");
        let (mut raw, catalog) = doc.into_raw();
        let body = raw_body(&raw, page).replace(
            &format!("/Contents {} 0 R", page - 1),
            &format!("/Contents {contents} 0 R"),
        );
        raw.set(page, body);
        raw.build(catalog, "")
    };
    let bomb_in = |key: &str| {
        let mut doc = Doc::new();
        let data = doc.add(stream("/Filter /FlateDecode", bomb()));
        let (font, descriptor) = match key {
            "ToUnicode" => (
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode {data} 0 R >>"
                ),
                None,
            ),
            _ => (
                String::new(),
                Some(doc.add(format!(
                    "<< /Type /FontDescriptor /FontName /B /{key} {data} 0 R >>"
                ))),
            ),
        };
        let font = match descriptor {
            Some(descriptor) => doc.add(format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /B /FontDescriptor {descriptor} 0 R >>"
            )),
            None => doc.add(font),
        };
        doc.page(
            "BT /B 10 Tf (x) Tj ET",
            &format!("/Font << /B {font} 0 R >>"),
        );
        doc.build()
    };
    let content_bomb = {
        let mut doc = Doc::new();
        doc.add(stream("/Filter /FlateDecode", bomb()));
        doc.page("", "");
        doc.build()
    };
    let range = {
        let mut doc = Doc::new();
        let cmap = doc.add(stream(
            "",
            b"1 beginbfrange <00000000> <FFFFFFFF> <0041> endbfrange",
        ));
        let font = doc.add(format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode {cmap} 0 R >>"
        ));
        doc.page(
            "BT /R 10 Tf 10 10 Td (abc) Tj ET",
            &format!("/Font << /R {font} 0 R >>"),
        );
        doc.build()
    };
    let nesting = {
        let mut doc = Doc::new();
        let cmap = doc.add(stream("", &[b'['; 20_000]));
        let program = doc.add(stream(
            "",
            &[b"/Encoding ".as_slice(), &[b'{'; 20_000]].concat(),
        ));
        let descriptor = doc.add(format!(
            "<< /Type /FontDescriptor /FontName /N /FontFile {program} 0 R >>"
        ));
        let font = doc.add(format!("<< /Type /Font /Subtype /Type1 /BaseFont /N /FontDescriptor {descriptor} 0 R /ToUnicode {cmap} 0 R >>"));
        doc.page(
            "BT /N 10 Tf 10 10 Td (abc) Tj ET",
            &format!("/Font << /N {font} 0 R >>"),
        );
        doc.build()
    };
    let deep = {
        let mut doc = Doc::new();
        let mut inner = doc.form("", "BT /F1 10 Tf 10 10 Td (deep) Tj ET");
        for _ in 1..100 {
            inner = doc.form(
                &doc.resources(&format!("/XObject << /N {inner} 0 R >>")),
                "/N Do",
            );
        }
        doc.page("/N Do", &format!("/XObject << /N {inner} 0 R >>"));
        doc.build()
    };
    let fanout = {
        let mut doc = Doc::new();
        let mut inner = doc.form("", "q Q");
        for _ in 0..4 {
            inner = doc.form(
                &doc.resources(&format!("/XObject << /N {inner} 0 R >>")),
                &"/N Do ".repeat(100),
            );
        }
        doc.page("/N Do", &format!("/XObject << /N {inner} 0 R >>"));
        doc.build()
    };
    let encrypted = {
        let mut doc = Doc::new();
        doc.page(&text(10.0, 10.0, 100.0, "x"), "");
        let (raw, catalog) = doc.into_raw();
        raw.build(
            catalog,
            "/Encr#79pt << /Filter /Standard /V 1 /R 2 /O <00> /U <00> /P -4 >>",
        )
    };
    let packed = {
        let mut doc = Doc::new();
        doc.page(&text(10.0, 10.0, 100.0, "packed"), "");
        let (raw, catalog) = doc.into_raw();
        raw.build_packed(catalog, &[1, 2, 3])
    };
    let groups = {
        let mut doc = Doc::new();
        let state = doc.add("<< /Type /ExtGState /ca 0.5 /BM [/Bogus /Multiply] /SMask /None >>");
        let group = doc.form(
            "/Group << /S /Transparency /K true >>",
            "0 g BT /F1 10 Tf 10 50 Td (in group) Tj ET 1 g 5 46 100 16 re f*",
        );
        let off = doc.add("<< /Type /OCG /Name (off) >>");
        doc.catalog(&format!(
            "/OCProperties << /OCGs [{off} 0 R] /D << /OFF [{off} 0 R] >> >>"
        ));
        doc.page(
            &format!(
                "q /G gs /X Do Q /OC /Off BDC {} EMC q 0 0 50 50 re W n {} Q q 20 0 0 20 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID \x00 EI Q",
                text(10.0, 10.0, 150.0, "off"),
                text(10.0, 10.0, 100.0, "clipped")
            ),
            &format!("/ExtGState << /G {state} 0 R >> /XObject << /X {group} 0 R >> /Properties << /Off {off} 0 R >>"),
        );
        doc.build()
    };
    vec![
        (
            "simple.pdf",
            "two pages of text, Helvetica and an identity-mapped Type 0 font",
            simple,
        ),
        (
            "thai-typst.pdf",
            "the Typst Thai fixture (tests/fixtures/pdf/thai.pdf)",
            fixture("thai.pdf"),
        ),
        (
            "page-tree-cycle.pdf",
            "a Pages node that is its own kid",
            cycle,
        ),
        ("cid-width-range.pdf", "a /W entry 0 4294967295 500", wide),
        (
            "predictor-columns.pdf",
            "a PNG predictor with /Columns 4294967295",
            columns,
        ),
        (
            "content-bomb.pdf",
            "a Flate stream inflating to 64 MiB",
            content_bomb,
        ),
        (
            "font-bomb.pdf",
            "a /FontFile2 inflating to 64 MiB",
            bomb_in("FontFile2"),
        ),
        (
            "cmap-bomb.pdf",
            "a /ToUnicode CMap inflating to 64 MiB",
            bomb_in("ToUnicode"),
        ),
        (
            "cmap-range.pdf",
            "a bfrange from 00000000 to FFFFFFFF",
            range,
        ),
        (
            "cmap-nesting.pdf",
            "20,000 nested brackets in a CMap and a Type 1 program",
            nesting,
        ),
        (
            "object-streams.pdf",
            "65 object streams",
            object_streams(65, false),
        ),
        (
            "objstm-bomb.pdf",
            "an object stream inflating past max_objstm_bytes",
            object_streams(0, true),
        ),
        ("forms-deep.pdf", "100 nested Form XObjects", deep),
        (
            "forms-fanout.pdf",
            "four levels of 100 Form calls each",
            fanout,
        ),
        (
            "encrypted.pdf",
            "an /Encrypt entry spelled /Encr#79pt",
            encrypted,
        ),
        (
            "xref-stream.pdf",
            "an object stream and an xref stream",
            packed,
        ),
        (
            "groups-and-clips.pdf",
            "a knockout group, blend arrays, optional content, a clip and an inline image",
            groups,
        ),
        (
            "every-stream.pdf",
            "Flate content, a Form, a Type 1 program, a CMap, an ICC profile, an Indexed table and a sampled function",
            every_stream_kind(),
        ),
    ]
}

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/corpus/pdf");

#[test]
#[ignore = "rewrites fuzz/corpus/pdf and the seed table of tests/fixtures/pdf/MANIFEST.md"]
fn regenerate_pdf_fuzz_seeds() {
    if std::path::Path::new(CORPUS).exists() {
        std::fs::remove_dir_all(CORPUS).unwrap();
    }
    std::fs::create_dir_all(CORPUS).unwrap();
    let mut rows = String::new();
    for (file, what, bytes) in seeds() {
        std::fs::write(format!("{CORPUS}/{file}"), &bytes).unwrap();
        rows.push_str(&format!(
            "| `fuzz/corpus/pdf/{file}` | `{}` | {what} |\n",
            sha256(&bytes)
        ));
    }
    let manifest = std::fs::read_to_string(PDF_MANIFEST).unwrap();
    let marker = "## Fuzz seeds (`fuzz/corpus/pdf/`)";
    let (head, tail) = manifest
        .split_once(marker)
        .expect("the manifest's seed section");
    let (intro, _) = tail.split_once("|---|---|---|\n").expect("the seed table");
    std::fs::write(
        PDF_MANIFEST,
        format!("{head}{marker}{intro}|---|---|---|\n{rows}"),
    )
    .unwrap();
}

#[test]
fn every_pdf_fuzz_seed_reads_or_fails_bounded() {
    let small = DocLimits {
        max_input_bytes: 1 << 20,
        max_stream_bytes: 1 << 20,
        max_decoded_bytes: 8 << 20,
        max_objstm_bytes: 1 << 20,
        max_pdf_objects: 10_000,
        max_pages: 16,
        max_content_ops: 100_000,
        ..DocLimits::default()
    };
    for (file, _, bytes) in seeds() {
        match read_with(&bytes, &small) {
            Ok(_) | Err(DocError::TooComplex(_) | DocError::Malformed(_) | DocError::Encrypted) => {
            }
            Err(other) => panic!("{file}: {other:?}"),
        }
    }
}

// Revisions, rebuilt cross-reference tables and the load's other edges.

#[test]
fn pdf_freed_object_in_an_object_stream_stays_freed() {
    // Revision 1: the page's /Contents is object 7, an array in an object stream naming the
    // stream that shows OLD. Revision 2 frees object 7; the object stream still holds it.
    let mut doc = Doc::new();
    let array = doc.raw.reserve();
    let page = doc.page(&text(10.0, 10.0, 100.0, "OLD"), "");
    let (mut raw, catalog) = doc.into_raw();
    let content = page - 1;
    raw.set(array, format!("[{content} 0 R]"));
    let body = raw_body(&raw, page).replace(
        &format!("/Contents {content} 0 R"),
        &format!("/Contents {array} 0 R"),
    );
    raw.set(page, body);
    let base = raw.build_packed(catalog, &[3, array]);
    assert_eq!(visible(&read(&base)), vec!["OLD"]);
    // The object stream and the cross-reference stream took the next two numbers.
    let size = raw.size() + 2;
    let updated = append_update(&base, &[], &[array], size, catalog, startxref(&base));
    let extracted = read(&updated);
    assert_eq!(extracted.pages.len(), 1);
    assert!(extracted.blocks.is_empty(), "{:?}", texts(&extracted));
    assert_eq!(extracted.pages[0].text_layer, TextLayer::Ok);
    // The same for an object a classic table listed: revision 2 frees the content stream.
    let mut doc = Doc::new();
    let page = doc.page(&text(10.0, 10.0, 100.0, "OLD"), "");
    let (raw, catalog) = doc.into_raw();
    let base = raw.build(catalog, "");
    let updated = append_update(
        &base,
        &[],
        &[page - 1],
        raw.size(),
        catalog,
        startxref(&base),
    );
    assert!(read(&updated).blocks.is_empty());
}

#[test]
fn pdf_hybrid_reference_files_read_their_stream_entries() {
    // The table marks the catalog, the page tree and the font free; the /XRefStm stream gives
    // them as compressed, and they are in use.
    let mut doc = Doc::new();
    doc.page(&text(10.0, 10.0, 100.0, "hybrid"), "");
    let (raw, catalog) = doc.into_raw();
    let extracted = read(&raw.build_hybrid(catalog, &[1, 2, 3]));
    assert_eq!(visible(&extracted), vec!["hybrid"]);
    assert_eq!(extracted.pages[0].text_layer, TextLayer::Ok);
}

#[test]
fn pdf_incremental_update_reads_the_newest_revision() {
    let mut doc = Doc::new();
    let page = doc.page(&text(10.0, 10.0, 100.0, "OLD"), "");
    let (raw, catalog) = doc.into_raw();
    let base = raw.build(catalog, "");
    assert_eq!(visible(&read(&base)), vec!["OLD"]);
    let replaced = stream("", text(10.0, 10.0, 100.0, "NEW").as_bytes());
    let updated = append_update(
        &base,
        &[(page - 1, replaced)],
        &[],
        raw.size(),
        catalog,
        startxref(&base),
    );
    let extracted = read(&updated);
    assert_eq!(visible(&extracted), vec!["NEW"]);
    assert_eq!(extracted.pages[0].text_layer, TextLayer::Ok);
}

#[test]
fn pdf_reconstructed_xref_marks_pages_suspect() {
    let mut doc = Doc::new();
    doc.page(&text(10.0, 10.0, 100.0, "kept"), "");
    let (raw, catalog) = doc.into_raw();
    let file = raw.build(catalog, "");
    let start = startxref(&file);
    let moved = |by: usize| {
        let tail = format!("startxref\n{start}\n%%EOF\n");
        let mut out = file[..file.len() - tail.len()].to_vec();
        out.extend_from_slice(format!("startxref\n{}\n%%EOF\n", start + by).as_bytes());
        out
    };
    // A few bytes off: lopdf finds the table beside it and reads it as written.
    let near = read(&moved(3));
    assert_eq!(visible(&near), vec!["kept"]);
    assert_eq!(near.pages[0].text_layer, TextLayer::Ok);
    // Far off: lopdf rebuilds the table by scanning the file, so objects may come from an older
    // revision than a viewer shows, and every page is suspect.
    let far = read(&moved(3_000));
    assert_eq!(visible(&far), vec!["kept"]);
    assert_eq!(
        far.pages[0].text_layer,
        TextLayer::Suspect("xref_recovered")
    );
    assert!(far.warnings.contains(&ExtractWarning::SuspectTextLayer {
        page: 1,
        why: "xref_recovered"
    }));
    // An older section's /Prev a few bytes off: lopdf finds the table nearby, but the sections
    // cannot be read again as lopdf read them, so a deleted object could be back.
    let update = append_update(&file, &[], &[], raw.size(), catalog, start + 3);
    let shifted = read(&update);
    assert_eq!(visible(&shifted), vec!["kept"]);
    assert_eq!(
        shifted.pages[0].text_layer,
        TextLayer::Suspect("xref_recovered")
    );
}

#[test]
fn pdf_prev_loop_terminates() {
    let mut doc = Doc::new();
    doc.page(&text(10.0, 10.0, 100.0, "looped"), "");
    let (raw, catalog) = doc.into_raw();
    // The trailer's /Prev names its own table.
    let start = startxref(&raw.build(catalog, ""));
    assert_eq!(
        visible(&read(&raw.build(catalog, &format!("/Prev {start}")))),
        vec!["looped"]
    );
    // 32 sections read; a 33rd is refused before lopdf follows any.
    let mut file = raw.build(catalog, "");
    for _ in 1..32 {
        file = append_update(&file, &[], &[], raw.size(), catalog, startxref(&file));
    }
    assert_eq!(visible(&read(&file)), vec!["looped"]);
    let file = append_update(&file, &[], &[], raw.size(), catalog, startxref(&file));
    let (result, decoded, walks) = read_counted(&file, &limits());
    assert_eq!(result.unwrap_err(), DocError::TooComplex("xref_sections"));
    assert_eq!((decoded, walks), (0, 0));
}

/// A page whose `/Contents` array names `content` (a stream in `doc`) `uses` times.
fn repeated_contents(mut doc: Doc, content: u32, uses: usize) -> Vec<u8> {
    let page = doc.page("", "");
    let (mut raw, catalog) = doc.into_raw();
    let refs = format!("{content} 0 R ").repeat(uses);
    let body = raw_body(&raw, page).replace(
        &format!("/Contents {} 0 R", page - 1),
        &format!("/Contents [{refs}]"),
    );
    raw.set(page, body);
    raw.build(catalog, "")
}

#[test]
fn pdf_repeated_content_refs_are_charged_per_use() {
    // One stream that inflates to 1 MiB: eight uses fit a budget of 8 MiB, nine do not.
    let limits = DocLimits {
        max_stream_bytes: 1 << 20,
        max_decoded_bytes: 8 << 20,
        ..DocLimits::default()
    };
    let file = |uses: usize| {
        let mut doc = Doc::new();
        let content = doc.add(stream("/Filter /FlateDecode", &zeros(1 << 20)));
        repeated_contents(doc, content, uses)
    };
    assert!(read_with(&file(8), &limits).is_ok());
    let (result, decoded, walks) = read_counted(&file(9), &limits);
    assert_eq!(result.unwrap_err(), DocError::TooComplex("decoded_bytes"));
    assert!(
        decoded <= limits.max_stream_bytes,
        "decoded once: {decoded}"
    );
    assert_eq!(walks, 0);
    // At most 1,024 content streams on a page.
    let mut doc = Doc::new();
    let content = doc.add(stream("", b"q Q"));
    assert!(read_with(&repeated_contents(doc, content, 1_024), &limits).is_ok());
    let mut doc = Doc::new();
    let content = doc.add(stream("", b"q Q"));
    assert_eq!(
        read_with(&repeated_contents(doc, content, 1_025), &limits).unwrap_err(),
        DocError::TooComplex("contents")
    );
}

/// The page of `doc` drawing the content stream `content` instead of its own.
fn with_contents(doc: Doc, content: u32) -> Vec<u8> {
    repeated_contents(doc, content, 1)
}

#[test]
fn pdf_indirect_filter_names_are_resolved() {
    let mut doc = Doc::new();
    let name = doc.add("/FlateDecode");
    let names = doc.add(format!("[{name} 0 R]"));
    let content = doc.add(stream(
        &format!("/Filter {names} 0 R"),
        &flate(text(10.0, 10.0, 100.0, "indirect").as_bytes()),
    ));
    assert_eq!(
        visible(&read(&with_contents(doc, content))),
        vec!["indirect"]
    );
    // An image codec named indirectly outside an image is still refused.
    let mut doc = Doc::new();
    let name = doc.add("/DCTDecode");
    let content = doc.add(stream(&format!("/Filter {name} 0 R"), b"\xFF\xD8"));
    assert_eq!(
        error(&with_contents(doc, content)),
        DocError::Malformed("stream_filter")
    );
}

#[test]
fn pdf_hostile_predictor_parameters_fail_bounded() {
    // PNG "Up" rows of 8 columns, with /DecodeParms an indirect object.
    let mut plain = text(10.0, 10.0, 100.0, "predicted").into_bytes();
    plain.resize(plain.len().div_ceil(8) * 8, b' ');
    let mut rows = Vec::new();
    let mut above = [0u8; 8];
    for row in plain.chunks(8) {
        rows.push(2);
        rows.extend(
            row.iter()
                .zip(above)
                .map(|(byte, up)| byte.wrapping_sub(up)),
        );
        above.copy_from_slice(row);
    }
    let mut doc = Doc::new();
    let params = doc.add("<< /Predictor 12 /Columns 8 >>");
    let content = doc.add(stream(
        &format!("/Filter /FlateDecode /DecodeParms {params} 0 R"),
        &flate(&rows),
    ));
    assert_eq!(
        visible(&read(&with_contents(doc, content))),
        vec!["predicted"]
    );
    // TIFF predictor 2 over one 8-bit colour: each byte is the difference from the one before.
    let mut deltas = vec![0u8; plain.len()];
    for at in 0..plain.len() {
        deltas[at] = plain[at].wrapping_sub(if at % 8 == 0 { 0 } else { plain[at - 1] });
    }
    let mut doc = Doc::new();
    let content = doc.add(stream(
        "/Filter /FlateDecode /DecodeParms << /Predictor 2 /Columns 8 >>",
        &flate(&deltas),
    ));
    assert_eq!(
        visible(&read(&with_contents(doc, content))),
        vec!["predicted"]
    );
    for params in [
        "/Predictor 12 /Columns 4294967295",
        "/Predictor 12 /Columns 65537",
        "/Predictor 12 /Columns -1",
        "/Predictor 2 /Colors 33",
        "/Predictor 12 /BitsPerComponent 3",
        "/Predictor 12 /Columns 8.5",
        "/Predictor 7",
    ] {
        let mut doc = Doc::new();
        let content = doc.add(stream(
            &format!("/Filter /FlateDecode /DecodeParms << {params} >>"),
            &flate(&rows),
        ));
        let (result, decoded, _) = read_counted(&with_contents(doc, content), &limits());
        assert_eq!(
            result.unwrap_err(),
            DocError::Malformed("stream_predictor"),
            "{params}"
        );
        assert!(decoded <= rows.len() as u64 * 2, "{params}: {decoded}");
    }
}

#[test]
fn pdf_content_ops_cap_counts_executions() {
    // A Form of 100 operators drawn 200 times: 300 operators written, 20,000 executed.
    let limits = DocLimits {
        max_content_ops: 10_000,
        ..DocLimits::default()
    };
    let file = |draws: usize| {
        let mut doc = Doc::new();
        let form = doc.form("", &"q Q ".repeat(50));
        doc.page(
            &"/Fm Do ".repeat(draws),
            &format!("/XObject << /Fm {form} 0 R >>"),
        );
        doc.build()
    };
    assert!(read_with(&file(50), &limits).is_ok());
    assert_eq!(
        read_with(&file(200), &limits).unwrap_err(),
        DocError::TooComplex("content_ops")
    );
}

#[tokio::test]
async fn pdf_cmap_and_type1_bombs_fail_bounded() {
    let font_with = |cmap: &[u8], program: &[u8]| {
        let mut doc = Doc::new();
        let cmap = doc.add(stream("", cmap));
        let program = doc.add(stream("", program));
        let descriptor = doc.add(format!(
            "<< /Type /FontDescriptor /FontName /N /FontFile {program} 0 R >>"
        ));
        let font = doc.add(format!("<< /Type /Font /Subtype /Type1 /BaseFont /N /FontDescriptor {descriptor} 0 R /ToUnicode {cmap} 0 R >>"));
        doc.page(
            "BT /N 10 Tf 10 100 Td (abc) Tj ET",
            &format!("/Font << /N {font} 0 R >>"),
        );
        doc.build()
    };
    // One range over every four-byte code, never expanded.
    let range = font_with(b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfrange <00000000> <FFFFFFFF> <0041> endbfrange", b"");
    // 70,000 ranges and 300,000 single mappings: the parser keeps its caps.
    let many: String = (0..70_000u32)
        .map(|at| format!("<{at:08X}> <{at:08X}> <0041>\n"))
        .collect();
    let ranges = font_with(
        format!("70000 beginbfrange {many} endbfrange").as_bytes(),
        b"",
    );
    let singles: String = (0..300_000u32)
        .map(|at| format!("<{at:06X}> <0041>\n"))
        .collect();
    let singles = font_with(
        format!("300000 beginbfchar {singles} endbfchar").as_bytes(),
        b"",
    );
    // 200,000 nested brackets in a CMap and braces in a Type 1 program, read without recursion.
    let nested = font_with(
        &[b'['; 200_000],
        &[b"/Encoding ".as_slice(), &[b'{'; 200_000]].concat(),
    );
    // Each reads on a pool thread within the call timeout, and the thread lives on.
    let pool = docs::DocPool::new(1, limits()).unwrap();
    let workspace = katwork::ids::WorkspaceId::new();
    let cancel = tokio_util::sync::CancellationToken::new();
    for (what, file) in [
        ("range", range),
        ("ranges", ranges),
        ("singles", singles),
        ("nested", nested),
    ] {
        let extracted = pool
            .run(workspace, &cancel, None, move |limits| {
                docs::extract(MediaKind::Pdf, &file, limits)
            })
            .await
            .unwrap_or_else(|error| panic!("{what}: {error:?}"));
        assert_eq!(extracted.pages.len(), 1, "{what}");
    }
}

// The fork's walker.

#[test]
fn pdf_form_font_names_do_not_alias_the_pages() {
    // The page's /F1 is Helvetica; the Form's own /F1 is the Type 0 font.
    let mut doc = Doc::new();
    let form = doc.form(
        &format!("/Resources << /Font << /F1 {} 0 R >> >>", doc.unicode),
        &format!("BT /F1 10 Tf 10 50 Td {} Tj ET", Doc::utf16("กา")),
    );
    let content = format!(
        "{} /Fm Do {}",
        text(10.0, 10.0, 150.0, "AB"),
        text(10.0, 10.0, 100.0, "CD")
    );
    doc.page(&content, &format!("/XObject << /Fm {form} 0 R >>"));
    assert_eq!(visible(&read(&doc.build())), vec!["AB", "กา", "CD"]);
}

#[test]
fn pdf_cid_width_ranges_are_read_without_expansion() {
    let mut doc = Doc::new();
    let cid = doc.add("<< /Type /Font /Subtype /CIDFontType2 /BaseFont /W /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /W [0 4294967295 500] >>");
    let font = doc.add(format!("<< /Type /Font /Subtype /Type0 /BaseFont /W /Encoding /Identity-H /DescendantFonts [{cid} 0 R] /ToUnicode 4 0 R >>"));
    doc.page(
        "BT /W 10 Tf 10 100 Td <00410042> Tj ET",
        &format!("/Font << /W {font} 0 R >>"),
    );
    let extracted = read(&doc.build());
    assert_eq!(visible(&extracted), vec!["AB"]);
    // Two glyphs 500/1000 of 10 pt wide.
    assert_eq!(extracted.blocks[0].lines[0].rect, [10.0, 98.0, 20.0, 108.0]);
}

#[test]
fn pdf_common_malformations_do_not_panic() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        (
            "a dangling font",
            "/Font << /D 999 0 R >>",
            "BT /D 10 Tf 10 100 Td (dangling) Tj ET",
            &[],
        ),
        (
            "Differences past 255",
            "/Font << /D << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Differences [300 /A 65 /B] >> >> >>",
            "BT /D 10 Tf 10 100 Td (A) Tj ET",
            &["B"],
        ),
        (
            "FirstChar and Widths disagree",
            "/Font << /D << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 /LastChar 70 /Widths [500] >> >>",
            "BT /D 10 Tf 10 100 Td (ABCDEFGH) Tj ET",
            &["ABCDEFGH"],
        ),
        (
            "StandardEncoding",
            "/Font << /D << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /StandardEncoding >> >>",
            "BT /D 10 Tf 10 100 Td (std) Tj ET",
            &["std"],
        ),
        (
            "Identity-H on a simple font",
            "/Font << /D << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /Identity-H >> >>",
            "BT /D 10 Tf 10 100 Td (id) Tj ET",
            &["id"],
        ),
        (
            "Tj before Tf",
            "",
            "BT 10 100 Td (nofont) Tj ET BT /F1 10 Tf 10 50 Td (after) Tj ET",
            &["after"],
        ),
        (
            "cm with five operands",
            "",
            "1 0 0 1 0 cm BT /F1 10 Tf 10 100 Td (five) Tj ET",
            &["five"],
        ),
        (
            "v first",
            "",
            "10 10 20 20 v f BT /F1 10 Tf 10 100 Td (curve) Tj ET",
            &["curve"],
        ),
        (
            "Q without q, EMC without BMC",
            "",
            "Q Q EMC BT /F1 10 Tf 10 100 Td (unbalanced) Tj ET",
            &["unbalanced"],
        ),
        (
            "a missing XObject and shading",
            "",
            "/Nope Do /Nope sh BT /F1 10 Tf 10 100 Td (missing) Tj ET",
            &["missing"],
        ),
        (
            "a Type 0 font without descendants",
            "/Font << /D << /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [] >> >>",
            "BT /D 10 Tf 10 100 Td <0041> Tj ET BT /F1 10 Tf 10 50 Td (after) Tj ET",
            &["after"],
        ),
        (
            "a Type 3 font without procedures",
            "/Font << /D << /Type /Font /Subtype /Type3 /FontMatrix [0.001 0 0 0.001 0 0] /FontBBox [0 0 1 1] /Encoding << /Differences [65 /a] >> /FirstChar 65 /LastChar 65 /Widths [] >> >>",
            "BT /D 10 Tf 10 100 Td (A) Tj ET",
            &["a"],
        ),
        (
            "a ToUnicode that is not a CMap",
            "/Font << /D << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 3 0 R >> >>",
            "BT /D 10 Tf 10 100 Td (plain) Tj ET",
            &["plain"],
        ),
        (
            "operands of the wrong kinds",
            "",
            "(x) (y) Td /F1 (z) Tf 1 Tf BT /F1 10 Tf 10 100 Td [(a) [(b)] 5] TJ (q) Tz ET",
            &["a"],
        ),
        (
            "a negative and a huge font size",
            "",
            "BT /F1 -10 Tf 10 100 Td (neg) Tj /F1 1e30 Tf (huge) Tj ET",
            &[],
        ),
    ];
    for (what, fonts, content, expected) in cases {
        let mut doc = Doc::new();
        doc.page(content, fonts);
        let extracted =
            read_with(&doc.build(), &limits()).unwrap_or_else(|error| panic!("{what}: {error:?}"));
        if !expected.is_empty() {
            assert_eq!(visible(&extracted), *expected, "{what}");
        }
    }
    // A page with no /MediaBox anywhere reads on US Letter.
    let mut doc = Doc::new();
    let page = doc.page(&text(10.0, 10.0, 100.0, "letter"), "");
    let (mut raw, catalog) = doc.into_raw();
    let body = raw_body(&raw, page).replace("/MediaBox [0 0 300 300]", "");
    raw.set(page, body);
    let extracted = read(&raw.build(catalog, ""));
    assert_eq!(extracted.pages[0].media_box, [0.0, 0.0, 612.0, 792.0]);
    assert_eq!(visible(&extracted), vec!["letter"]);
}

#[test]
fn pdf_quote_operators_show_text() {
    let mut doc = Doc::new();
    doc.page(
        "BT /F1 10 Tf 12 TL 10 200 Td (first) Tj (second) ' 1 2 (third) \" ET",
        "",
    );
    assert_eq!(visible(&read(&doc.build())), vec!["first\nsecond\nthird"]);
}

#[test]
fn pdf_even_odd_fill_clears_its_path() {
    // Every painting operator ends its path, even-odd fills too, so the last rectangle is the
    // last area alone and the white text has nothing beneath it.
    let ops = ["f*", "B*", "b*", "f", "F", "B", "b"];
    let mut doc = Doc::new();
    for op in ops {
        doc.page(
            &format!(
                "0 g 0 0 10 10 re {op} 1 g 200 200 50 50 re f \
                 1 g BT /F1 10 Tf 100 100 Td (WHITE) Tj ET"
            ),
            "",
        );
    }
    let extracted = read(&doc.build());
    for (index, op) in ops.iter().enumerate() {
        assert_eq!(
            on_page(&extracted, index as u32 + 1),
            vec![("WHITE".to_owned(), Some(HiddenWhy::LowContrast), None)],
            "{op}"
        );
    }
}

#[test]
fn pdf_type3_widths_follow_the_font_matrix() {
    let mut doc = Doc::new();
    let procedure = doc.add_stream("", b"50 0 d0");
    let font = doc.add(format!(
        "<< /Type /Font /Subtype /Type3 /FontMatrix [0.01 0 0 0.01 0 0] /FontBBox [0 0 100 100] \
         /CharProcs << /a {procedure} 0 R /b {procedure} 0 R >> /Encoding << /Type /Encoding /Differences [65 /a /b] >> \
         /FirstChar 65 /LastChar 66 /Widths [50 50] /Resources << >> >>"
    ));
    doc.page(
        "BT /T3 10 Tf 10 100 Td (ABAB) Tj ET",
        &format!("/Font << /T3 {font} 0 R >>"),
    );
    let extracted = read(&doc.build());
    assert_eq!(visible(&extracted), vec!["abab"]);
    // Each glyph is 50 × 0.01 × 10 = 5 pt wide.
    let rect = extracted.blocks[0].lines[0].rect;
    assert_eq!((rect[0], rect[2]), (10.0, 30.0));
}

// Visibility details.

#[test]
fn pdf_text_with_no_colour_operator_is_visible() {
    let mut doc = Doc::new();
    let content = lines_at(&[
        "BT /F1 10 Tf 10 {y} Td (DEFAULTBLACK) Tj ET\n",
        "q /DeviceCMYK cs BT /F1 10 Tf 10 {y} Td (CMYKINITIAL) Tj ET Q\n",
        "q /DeviceRGB cs BT /F1 10 Tf 10 {y} Td (RGBINITIAL) Tj ET Q\n",
        "q /DN cs BT /F1 10 Tf 10 {y} Td (DEVICENINITIAL) Tj ET Q\n",
        "q /SP cs BT /F1 10 Tf 10 {y} Td (SEPARATIONINITIAL) Tj ET Q\n",
        "q 1 0 0 RG BT 1 Tr /F1 10 Tf 10 {y} Td (STROKEDEFAULT) Tj ET Q\n",
    ]);
    doc.page(
        &content,
        "/ColorSpace << /DN [/DeviceN [/Spot] /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>] \
         /SP [/Separation /Spot /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>] >>",
    );
    let extracted = read(&doc.build());
    for text in [
        "DEFAULTBLACK",
        "CMYKINITIAL",
        "RGBINITIAL",
        "DEVICENINITIAL",
        "SEPARATIONINITIAL",
        "STROKEDEFAULT",
    ] {
        assert_eq!(
            (
                block(&extracted, text).hidden,
                block(&extracted, text).suspect
            ),
            (None, None),
            "{text}"
        );
    }
}

#[test]
fn pdf_composite_rounding_boundary() {
    // A white veil at ca 0.91 makes black text E8E8E8 (contrast 1.23, visible); at 0.92,
    // EBEBEB (1.19, hidden). Truncating 234.6 to EA would read 1.20 and show it.
    let mut doc = Doc::new();
    let a = doc.add("<< /Type /ExtGState /ca 0.91 >>");
    let b = doc.add("<< /Type /ExtGState /ca 0.92 >>");
    let content = lines_at(&[
        "BT /F1 10 Tf 10 {y} Td (NINETYONE) Tj ET q /A gs 1 g 5 {low} 150 16 re f Q\n",
        "BT /F1 10 Tf 10 {y} Td (NINETYTWO) Tj ET q /B gs 1 g 5 {low} 150 16 re f Q\n",
    ]);
    doc.page(&content, &format!("/ExtGState << /A {a} 0 R /B {b} 0 R >>"));
    assert_verdicts(
        &read(&doc.build()),
        &[
            ("NINETYONE", None),
            ("NINETYTWO", Some(HiddenWhy::LowContrast)),
        ],
    );
}

#[test]
fn pdf_clip_applies_after_its_painting_operator() {
    // `re W f` paints its rectangle, then clips what follows to it.
    let mut doc = Doc::new();
    doc.page(
        "BT /F1 10 Tf 110 120 Td (UNDERFILL) Tj ET \
         q 0 g 100 100 100 50 re W f BT /F1 10 Tf 10 20 Td (OUTSIDE) Tj ET 1 g BT /F1 10 Tf 110 220 Td (ABOVE) Tj ET \
         BT /F1 10 Tf 104 120 Td (INSIDE) Tj ET Q 0 g BT /F1 10 Tf 10 50 Td (RESTORED) Tj ET",
        "",
    );
    let extracted = read(&doc.build());
    assert_verdicts(
        &extracted,
        &[
            ("UNDERFILL", Some(HiddenWhy::Covered)),
            ("OUTSIDE", Some(HiddenWhy::Clipped)),
            ("ABOVE", Some(HiddenWhy::Clipped)),
            ("INSIDE", None),
            ("RESTORED", None),
        ],
    );
}

#[test]
fn pdf_text_clip_modes_clip_what_follows() {
    let mut doc = Doc::new();
    // Mode 4 fills its glyphs and adds them to the clip, which takes effect at ET.
    doc.page(
        "q BT 4 Tr /F1 10 Tf 10 250 Td (WWWWWWWWWW) Tj ET 0 Tr BT /F1 10 Tf 10 200 Td (BELOW) Tj ET \
         BT /F1 10 Tf 12 250 Td (in) Tj ET Q BT /F1 10 Tf 10 150 Td (AFTERQ) Tj ET",
        "",
    );
    // Mode 7 only clips.
    doc.page("q BT 7 Tr /F1 10 Tf 10 250 Td (CLIPONLY) Tj ET 0 Tr BT /F1 10 Tf 10 200 Td (BELOWSEVEN) Tj ET Q", "");
    let extracted = read(&doc.build());
    assert_verdicts(
        &extracted,
        &[
            ("WWWWWWWWWW", None),
            ("BELOW", Some(HiddenWhy::Clipped)),
            ("in", None),
            ("AFTERQ", None),
            ("CLIPONLY", Some(HiddenWhy::InvisibleMode)),
            ("BELOWSEVEN", Some(HiddenWhy::Clipped)),
        ],
    );
}

#[tokio::test]
async fn pdf_flood_of_areas_fails_bounded() {
    // 20,000 characters, then 100,000 areas drawn after them: one-point squares spread over the
    // page read; page-sized rectangles piled on the text end in TooComplex, never at the call
    // timeout.
    let flood = |spread: bool| {
        let mut content = String::new();
        for row in 0..200 {
            content.push_str(&format!(
                "BT /F1 5 Tf 10 {} Td ({}) Tj ET\n",
                1990 - row * 10,
                "x".repeat(100)
            ));
        }
        for at in 0..100_000u32 {
            let square = if spread {
                format!("{} {} 1 1", (at % 1000) * 2, (at / 1000) * 20)
            } else {
                "0 0 2000 2000".to_owned()
            };
            content.push_str(&format!("{square} re f\n"));
        }
        let mut doc = Doc::new();
        doc.size = (2000.0, 2000.0);
        doc.page(&content, "");
        doc.build()
    };
    let limits = limits();
    let pool = docs::DocPool::new(1, limits.clone()).unwrap();
    let workspace = katwork::ids::WorkspaceId::new();
    let cancel = tokio_util::sync::CancellationToken::new();
    for (spread, file) in [(true, flood(true)), (false, flood(false))] {
        let outcome = pool
            .run(workspace, &cancel, None, move |limits| {
                Ok(docs::extract(MediaKind::Pdf, &file, limits))
            })
            .await
            .expect("no call timeout");
        match (spread, outcome) {
            (true, Ok(extracted)) => assert_eq!(extracted.pages.len(), 1),
            (false, Err(DocError::TooComplex("areas"))) => {}
            (spread, other) => panic!(
                "spread {spread}: {:?}",
                other.map(|extracted| extracted.blocks.len())
            ),
        }
    }
}

#[test]
fn pdf_masked_images_are_not_opaque() {
    let mut doc = Doc::new();
    let mask = doc.add_stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[255]);
    let image = "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray";
    let soft = doc.add_stream(&format!("{image} /SMask {mask} 0 R"), &[0]);
    let keyed = doc.add_stream(&format!("{image} /Mask [0 0]"), &[0]);
    let stencil = doc.add_stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask true",
        &[0],
    );
    let content = lines_at(&[
        "BT /F1 10 Tf 10 {y} Td (UNDERSOFTMASK) Tj ET q 150 0 0 16 5 {low} cm /SM Do Q\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERCOLOURKEY) Tj ET q 150 0 0 16 5 {low} cm /KM Do Q\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERSTENCIL) Tj ET q 150 0 0 16 5 {low} cm /ST Do Q\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERINLINESTENCIL) Tj ET q 150 0 0 16 5 {low} cm BI /W 1 /H 1 /BPC 1 /IM true ID \x00 EI Q\n",
        "q 150 0 0 16 5 {low} cm /SM Do Q BT /F1 10 Tf 10 {y} Td (OVERSOFTMASK) Tj ET\n",
    ]);
    doc.page(
        &content,
        &format!("/XObject << /SM {soft} 0 R /KM {keyed} 0 R /ST {stencil} 0 R >>"),
    );
    let extracted = read(&doc.build());
    let unknown = Some(HiddenWhy::UnknownPaint);
    assert_verdicts(
        &extracted,
        &[
            ("UNDERSOFTMASK", unknown),
            ("UNDERCOLOURKEY", unknown),
            ("UNDERSTENCIL", unknown),
            ("UNDERINLINESTENCIL", unknown),
            ("OVERSOFTMASK", None),
        ],
    );
    assert_eq!(
        block(&extracted, "OVERSOFTMASK").suspect,
        Some("unknown_background")
    );
}

#[test]
fn pdf_optional_content_details() {
    let mut doc = Doc::new();
    let a = doc.add("<< /Type /OCG /Name (a) >>");
    let on = doc.add("<< /Type /OCG /Name (on) >>");
    doc.catalog(&format!(
        "/OCProperties << /OCGs [{a} 0 R {on} 0 R] /D << /BaseState /OFF /ON [{on} 0 R] >> >>"
    ));
    let black = doc.add_stream(&format!("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /OC {a} 0 R"), &[0]);
    let content = lines_at(&[
        "/OC /A BDC BT /F1 10 Tf 10 {y} Td (BASESTATEOFF) Tj ET EMC\n",
        "/OC /On BDC BT /F1 10 Tf 10 {y} Td (LISTEDON) Tj ET EMC\n",
        "/OC /On BDC /OC /A BDC BT /F1 10 Tf 10 {y} Td (INNEROFF) Tj ET EMC EMC\n",
        "/OC /A BDC /OC /On BDC BT /F1 10 Tf 10 {y} Td (OUTEROFF) Tj ET EMC EMC\n",
        "/OC /A BDC EMC BT /F1 10 Tf 10 {y} Td (AFTEREMC) Tj ET\n",
        "/OC << /Type /OCG /Name (direct) >> BDC BT /F1 10 Tf 10 {y} Td (DIRECTLIST) Tj ET EMC\n",
        "BT /F1 10 Tf 10 {y} Td (UNDERHIDDENIMAGE) Tj ET q 150 0 0 16 5 {low} cm /Im Do Q\n",
    ]);
    doc.page(
        &content,
        &format!("/Properties << /A {a} 0 R /On {on} 0 R >> /XObject << /Im {black} 0 R >>"),
    );
    let off = Some(HiddenWhy::OptionalContentOff);
    assert_verdicts(
        &read(&doc.build()),
        &[
            ("BASESTATEOFF", off),
            ("LISTEDON", None),
            ("INNEROFF", off),
            ("OUTEROFF", off),
            ("AFTEREMC", None),
            ("DIRECTLIST", None),
            ("UNDERHIDDENIMAGE", None),
        ],
    );
}

#[test]
fn pdf_effective_size_uses_the_product_matrix() {
    let cases: &[(&str, Option<HiddenWhy>)] = &[
        // Sheared until 3 pt tall, though its y basis vector is 10 long.
        (
            "BT /F1 10 Tf 1 0 10 0.3 20 100 Tm (SIZE) Tj ET",
            Some(HiddenWhy::Tiny),
        ),
        // Italic: sheared, still 10 pt tall.
        ("BT /F1 10 Tf 1 0 0.5 1 20 100 Tm (SIZE) Tj ET", None),
        ("BT /F1 10 Tf 0 1 -1 0 100 50 Tm (SIZE) Tj ET", None),
        (
            "BT /F1 3 Tf 0 1 -1 0 100 50 Tm (SIZE) Tj ET",
            Some(HiddenWhy::Tiny),
        ),
        (
            "BT /F1 10 Tf 0.3 0 0 1 20 100 Tm (SIZE) Tj ET",
            Some(HiddenWhy::Tiny),
        ),
        (
            "q 0.7071 0.7071 -0.7071 0.7071 150 0 cm BT /F1 10 Tf 20 100 Td (SIZE) Tj ET Q",
            None,
        ),
        (
            "q 0.3 0 0 0.3 0 0 cm BT /F1 20 Tf 3 0 0 3 20 100 Tm (SIZE) Tj ET Q",
            None,
        ),
    ];
    let mut doc = Doc::new();
    for (content, _) in cases {
        doc.page(content, "");
    }
    let extracted = read(&doc.build());
    for (index, (content, why)) in cases.iter().enumerate() {
        assert_eq!(
            on_page(&extracted, index as u32 + 1),
            vec![("SIZE".to_owned(), *why, None)],
            "{content}"
        );
    }
}

#[test]
fn pdf_hidden_mark_between_thai_letters_keeps_clusters() {
    // KO KAI, a white MAI HAN-AKAT, then MAI EK and NO NU: the tone mark stays on its base.
    let mut doc = Doc::new();
    doc.page(
        &format!(
            "BT /U 10 Tf 10 100 Td {} Tj 1 g {} Tj 0 g {} Tj ET",
            Doc::utf16("ก"),
            Doc::utf16("\u{E31}"),
            Doc::utf16("\u{E48}น")
        ),
        "",
    );
    let extracted = read(&doc.build());
    assert_eq!(visible(&extracted), vec!["ก\u{E48}น"]);
    assert_eq!(
        hidden(&extracted),
        vec![("\u{E31}", HiddenWhy::LowContrast)]
    );
    assert_eq!(extracted.pages[0].text_layer, TextLayer::Ok);
}

#[test]
fn pdf_line_boxes_follow_block_pieces() {
    // One block of 50 lines of 80 code points, split into pieces of at most 1,500.
    let mut doc = Doc::new();
    doc.size = (600.0, 700.0);
    let content: String = (0..50)
        .map(|line| {
            text(
                10.0,
                10.0,
                650.0 - 12.0 * line as f32,
                &format!("l{line:02} {}", "word ".repeat(15)),
            )
        })
        .collect();
    doc.page(&content, "");
    let extracted = read(&doc.build());
    assert!(extracted.blocks.iter().all(|block| block.ordinal == 1));
    assert!(extracted.blocks.len() >= 3);
    for (index, piece) in extracted.blocks.iter().enumerate() {
        assert_eq!(piece.piece, index as u32);
        let chars: Vec<char> = piece.text.chars().collect();
        assert!(chars.len() <= docs::MAX_BLOCK_CHARS);
        let mut end = 0;
        for line in &piece.lines {
            assert!(
                end <= line.start && line.start < line.end && line.end as usize <= chars.len(),
                "{line:?}"
            );
            end = line.end;
            assert!(!chars[line.start as usize..line.end as usize].contains(&'\n'));
        }
    }
    // The line the first cut falls in has a box in each piece, side by side on one baseline.
    let (before, after) = (
        extracted.blocks[0].lines.last().unwrap(),
        &extracted.blocks[1].lines[0],
    );
    assert_eq!(after.start, 0);
    assert_eq!(
        (before.rect[1], before.rect[3]),
        (after.rect[1], after.rect[3])
    );
    assert!(
        before.rect[2] <= after.rect[0] + 0.01,
        "{before:?} {after:?}"
    );
}

#[test]
fn hidden_why_names_match_serde() {
    use HiddenWhy::*;
    for why in [
        InvisibleMode,
        OptionalContentOff,
        OutsideCropBox,
        Clipped,
        Transparent,
        Tiny,
        LowContrast,
        Covered,
        UnknownPaint,
        HiddenRow,
        HiddenColumn,
        HiddenSheet,
        EmptyFormat,
    ] {
        assert_eq!(
            serde_json::to_value(why).unwrap(),
            serde_json::Value::String(why.as_str().to_owned())
        );
    }
}
