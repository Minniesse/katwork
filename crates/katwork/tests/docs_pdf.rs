//! The PDF reader (Task 5): its guard against hostile files, its visibility rules, and its
//! lines and blocks. Every hostile file is built here, by `support/pdf_build.rs`.
#[path = "support/pdf_build.rs"]
mod pdf_build;

use katwork::docs::{
    self, Block, DocError, DocLimits, ExtractWarning, Extracted, HiddenWhy, Locator, MediaKind, TextLayer,
};
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
            case.replace("{y}", &y.to_string()).replace("{low}", &(y - 4).to_string())
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
        vec!["First line of a paragraph\nand its second line", "A new paragraph", "Second page"]
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
        let Locator::PdfPage { page } = block.locator else { panic!() };
        let media = extracted.pages[page as usize - 1].media_box;
        for line in &block.lines {
            assert_eq!(line.page, page);
            let [x0, y0, x1, y1] = line.rect;
            assert!(media[0] <= x0 && x0 < x1 && x1 <= media[2] && media[1] <= y0 && y0 < y1 && y1 <= media[3], "{line:?}");
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
    let all_on = doc.add(format!("<< /Type /OCMD /OCGs [{off} 0 R {on} 0 R] /P /AllOn >>"));
    let ve = doc.add(format!("<< /Type /OCMD /OCGs [{on} 0 R] /VE [/And {on} 0 R] >>"));
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
    assert!(extracted.blocks.iter().filter(|block| block.hidden.is_none()).all(|block| block.suspect.is_none()));
    let warned: Vec<&str> = extracted
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            ExtractWarning::HiddenContent { at: Locator::PdfPage { page: 1 }, why } => Some(*why),
            _ => None,
        })
        .collect();
    assert_eq!(warned.len(), 11);
    assert!(warned.contains(&"invisible_mode") && warned.contains(&"optional_content_off") && warned.contains(&"tiny"));
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
    assert_eq!(hidden(&extracted), vec![("9,999", HiddenWhy::LowContrast), ("x", HiddenWhy::LowContrast)]);
    // The hidden block comes right after its visible one.
    let ordinals: Vec<(u32, bool)> = extracted.blocks.iter().map(|block| (block.ordinal, block.hidden.is_some())).collect();
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
    let type4 = doc.add_stream("/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1 0 1]", b"{ pop 0 0 0 0 }");
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
        assert_eq!(block(&extracted, text).suspect, Some("unknown_background"), "{text}");
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
        ("TRIANGLE", "q 0 g 0 0 m 200 0 l 0 200 l h f Q 1 g BT /F1 10 Tf 120 150 Td (TRIANGLE) Tj ET"),
        ("BOWTIE", "q 0 g 20 20 m 180 180 l 180 20 l 20 180 l h f Q 1 g BT /F1 10 Tf 60 100 Td (BOWTIE) Tj ET"),
        ("CURVED", "q 0 g 20 20 m 100 60 180 20 v 140 100 180 180 v 100 140 20 180 v 60 100 20 20 v f Q 1 g BT /F1 10 Tf 25 25 Td (CURVED) Tj ET"),
        ("DIAMOND", "q 0 g 0.7071 0.7071 -0.7071 0.7071 100 0 cm 0 0 141 141 re f Q 1 g BT /F1 10 Tf 5 5 Td (DIAMOND) Tj ET"),
        ("SKEWED", "q 0 g 1 0 1 1 100 100 cm 0 0 50 50 re f Q 1 g BT /F1 10 Tf 100 140 Td (SKEWED) Tj ET"),
        ("ROTATEDCLIP", "q 0.7071 0.7071 -0.7071 0.7071 100 0 cm 0 0 141 141 re W n 1 0 0 1 0 0 cm 0 g -100 -100 400 400 re f Q 1 g BT /F1 10 Tf 90 100 Td (ROTATEDCLIP) Tj ET"),
        ("FAINTBLACK", "q /FA gs 0 g 0 0 200 200 re f Q 1 g BT /F1 10 Tf 20 100 Td (FAINTBLACK) Tj ET"),
        ("QUARTERTURN", "q 0 g 0 1 -1 0 200 0 cm 0 0 200 200 re f Q 1 g BT /F1 10 Tf 20 100 Td (QUARTERTURN) Tj ET"),
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
        assert_eq!((block.hidden, block.suspect), (None, Some("unknown_background")), "{text}");
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
        doc.form(&format!("/Group << /S /Transparency {k} >> {resources}"), &format!("/R gs {content}"))
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
    let outer = group(&mut doc, false, "/P3 gs /In Do", &format!("/XObject << /In {inner} 0 R >>"));
    add(&mut page, &mut xobjects, "G4", "/P3 gs", outer);
    let g = group(&mut doc, false, &format!("0 g {}", cover(200)), "");
    add(&mut page, &mut xobjects, "G5", "/P01 gs", g);
    page.push_str(&format!("1 g {} 0 g\n", text_at(200, "AFTERFAINTGROUP")));
    let g = group(&mut doc, false, &format!("0 g {}", cover(180)), "");
    add(&mut page, &mut xobjects, "G6", "", g);
    page.push_str(&format!("1 g {} 0 g\n", text_at(180, "AFTEROPAQUEGROUP")));
    let g = group(&mut doc, false, &format!("0 g {} 1 g {}", text_at(160, "INHALFGROUP"), cover(160)), "");
    add(&mut page, &mut xobjects, "G7", "/H gs", g);
    let g = group(&mut doc, false, &format!("0 g {} q /V gs 1 g {} Q", text_at(140, "INHALFGROUPVEIL"), cover(140)), "");
    add(&mut page, &mut xobjects, "G8", "/H gs", g);
    let g = group(&mut doc, false, &format!("0 g {} 1 g {}", text_at(120, "INOPAQUEGROUP"), cover(120)), "");
    add(&mut page, &mut xobjects, "G9", "", g);
    let g = group(&mut doc, true, &format!("0 g {} q /CL gs 1 g {} Q", text_at(100, "KNOCKOUT"), cover(100)), "");
    add(&mut page, &mut xobjects, "G10", "", g);
    let g = group(&mut doc, false, &format!("0 g {} q /CL gs 1 g {} Q", text_at(80, "NOKNOCKOUT"), cover(80)), "");
    add(&mut page, &mut xobjects, "G11", "", g);
    let inner = group(&mut doc, false, &format!("0 g {} 1 g {}", text_at(60, "NESTEDINHALF"), cover(60)), "");
    let outer = group(&mut doc, false, "/In Do", &format!("/XObject << /In {inner} 0 R >>"));
    add(&mut page, &mut xobjects, "G12", "/H gs", outer);
    let inner = group(&mut doc, false, &format!("0 g {}", text_at(40, "INSIDEKNOCKOUT")), "");
    let outer = group(&mut doc, true, &format!("/In Do q /CL gs 1 g {} Q", cover(40)), &format!("/XObject << /In {inner} 0 R >>"));
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
    assert_eq!(block(&extracted, "AFTERFAINTGROUP").suspect, Some("unknown_background"));
    assert_eq!(block(&extracted, "AFTEROPAQUEGROUP").suspect, None);
}

/// Each page's blocks, by page number from 1.
fn on_page(extracted: &Extracted, page: u32) -> Vec<(String, Option<HiddenWhy>, Option<&'static str>)> {
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
    let off = properties.split("/Off ").nth(1).unwrap().split(' ').next().unwrap().to_owned();
    // A Form that sets no colour or state of its own.
    let plain = doc.form("", "BT /F1 48 Tf 20 80 Td (9999) Tj ET");
    let boxed = doc.form("/BBox [0 0 10 10]", "BT /F1 48 Tf 20 80 Td (9999) Tj ET");
    let shifted = doc.form("/Matrix [1 0 0 1 0 100]", "BT /F1 48 Tf 20 80 Td (9999) Tj ET");
    let own_oc = doc.form(&format!("/OC {off} 0 R"), "BT /F1 48 Tf 20 80 Td (9999) Tj ET");
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
        ("q 1 0 0 1 1000 0 cm /P Do Q", Some(HiddenWhy::OutsideCropBox)),
        ("q /B Do Q", Some(HiddenWhy::Clipped)),
        ("q 0 g /S Do Q", None),
        ("q 0 g /OC /Off BDC /P Do EMC Q", Some(HiddenWhy::OptionalContentOff)),
        ("q 0 g /OC /On BDC /P Do EMC Q", None),
        ("q 0 g /O Do Q", Some(HiddenWhy::OptionalContentOff)),
    ];
    for (content, _) in cases {
        doc.page(content, &resources);
    }
    doc.page("/HB Do 1 g BT /F1 10 Tf 20 20 Td (OVERHIDDENBLACK) Tj ET", &resources);
    doc.page("1 g /SB Do BT /F1 10 Tf 20 20 Td (AFTERRETURN) Tj ET", &resources);
    let extracted = read(&doc.build());
    for (index, (content, why)) in cases.iter().enumerate() {
        assert_eq!(on_page(&extracted, index as u32 + 1), vec![("9999".to_owned(), *why, None)], "{content}");
    }
    assert_verdicts(
        &extracted,
        &[
            ("OVERHIDDENBLACK", Some(HiddenWhy::LowContrast)),
            ("AFTERRETURN", Some(HiddenWhy::LowContrast)),
        ],
    );
    // The Form's /Matrix and the CTM place its text: at 20, 80 + 100, at 48 pt.
    let shown = extracted.blocks.iter().find(|block| block.locator == Locator::PdfPage { page: 6 }).unwrap();
    let rect = shown.lines[0].rect;
    assert_eq!((rect[0], rect[1], rect[3]), (20.0, 180.0 - 0.2 * 48.0, 180.0 + 0.8 * 48.0));
}
