//! A `SourceRef` names a block of a file's extracted text, and optionally a quote inside it, as
//! a W3C Web Annotation `SpecificResource` (ADR-0007). Every offset counts code points of the
//! block's own text.
use serde::{Deserialize, Deserializer, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use super::instruction::block_instruction_like;
use crate::{
    artifacts::{ArtifactRole, store::VersionView},
    docs::{Block, CellFacts, ExtractorId, Locator, PageInfo, TextLayer},
    ids::ArtifactId,
};

pub const CONFORMS_PDF: &str = "https://www.rfc-editor.org/rfc/rfc8118";
pub const CONFORMS_CELL: &str = "https://www.rfc-editor.org/rfc/rfc7111";
pub const CONFORMS_BLOCK: &str = "urn:katwork:selector:block:v1";
pub const CONFORMS_SHEET: &str = "urn:katwork:selector:xlsx-sheet:v1";
pub const CONFORMS_DOCX: &str = "urn:katwork:selector:docx-paragraph:v1";
pub const CONFORMS_MEDIA: &str = "http://www.w3.org/TR/media-frags/";
/// The piece of a split block (text, PDF, XLSX), linked after the block only when above 0. A
/// DOCX paragraph names its piece in its own value.
pub const CONFORMS_PIECE: &str = "urn:katwork:selector:piece:v1";

/// The context a narrowed quote carries on each side, in code points.
const CONTEXT: usize = 32;

/// The chains `validate` accepts, outer to inner. A new source type adds a row.
const SHAPES: &[&[&str]] = &[
    &[CONFORMS_PDF, CONFORMS_BLOCK],
    &[CONFORMS_PDF, CONFORMS_BLOCK, CONFORMS_PIECE],
    &[CONFORMS_SHEET, CONFORMS_CELL],
    &[CONFORMS_SHEET, CONFORMS_CELL, CONFORMS_PIECE],
    &[CONFORMS_DOCX],
    &[CONFORMS_BLOCK],
    &[CONFORMS_BLOCK, CONFORMS_PIECE],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceType {
    SpecificResource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    #[serde(rename = "type")]
    pub kind: ResourceType,
    pub source: String,
    pub sha256: String,
    pub extractor: ExtractorRef,
    pub quality: Quality,
    pub selector: Selector,
    #[serde(default)]
    pub boxes: Vec<RefBox>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_context: Option<String>,
    /// A redacted cell (`{"redacted": true}`, Task 30) loads as absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_cell"
    )]
    pub cell: Option<CellFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractorRef {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quality {
    pub generated: bool,
    pub hidden: bool,
    pub instruction_like: bool,
    pub suspect_text_layer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum Selector {
    FragmentSelector {
        conforms_to: String,
        value: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refined_by: Vec<Selector>,
    },
    TextPositionSelector {
        start: u32,
        end: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refined_by: Vec<Selector>,
    },
    TextQuoteSelector {
        exact: String,
        prefix: String,
        suffix: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refined_by: Vec<Selector>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RefBox {
    pub page: u32,
    pub rect: [f32; 4],
}

fn lenient_cell<'de, D: Deserializer<'de>>(d: D) -> Result<Option<CellFacts>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

fn fragment(conforms_to: &str, value: String) -> Selector {
    Selector::FragmentSelector {
        conforms_to: conforms_to.into(),
        value,
        refined_by: vec![],
    }
}

/// Postgres text holds no U+0000.
fn clean(text: &str) -> String {
    text.replace('\0', "\u{FFFD}")
}

/// `%`, `;`, `=` and control characters are percent-encoded in a selector value's names; the
/// rest stays raw, so Thai names stay readable.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | ';' | '=') || c.is_control() {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A selector value's `key=value` pairs, split on `;`, in order.
fn pairs(value: &str) -> Option<Vec<(&str, &str)>> {
    value.split(';').map(|pair| pair.split_once('=')).collect()
}

fn number(text: &str) -> Option<u32> {
    // Canonical decimal only: no sign, no leading zero.
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    text.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
}

/// The parsed value of one chain link.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    Page(u32),
    Block(u32),
    Piece(u32),
    Sheet { name: String },
    Cell { row: u32, col: u32 },
    Docx { part: String, p: u32, piece: u32 },
}

fn parse_link(conforms_to: &str, value: &str) -> Option<Link> {
    let pairs = pairs(value)?;
    let one = |key: &str| match pairs.as_slice() {
        [(k, v)] if *k == key => number(v),
        _ => None,
    };
    match conforms_to {
        CONFORMS_PDF => one("page").filter(|&n| n >= 1).map(Link::Page),
        CONFORMS_BLOCK => one("ordinal").map(Link::Block),
        CONFORMS_PIECE => one("piece").filter(|&n| n >= 1).map(Link::Piece),
        CONFORMS_SHEET => match pairs.as_slice() {
            [("sheetId", id), ("name", name)] => {
                number(id)?;
                Some(Link::Sheet {
                    name: decode(name)?,
                })
            }
            _ => None,
        },
        CONFORMS_CELL => {
            let [("cell", cell)] = pairs.as_slice() else {
                return None;
            };
            let (row, col) = cell.split_once(',')?;
            let (row, col) = (number(row)?, number(col)?);
            (row >= 1 && col >= 1).then_some(Link::Cell { row, col })
        }
        CONFORMS_DOCX => {
            let (part, p, para, piece) = match pairs.as_slice() {
                [("part", part), ("p", p), ("piece", piece)] => (part, p, None, piece),
                [("part", part), ("p", p), ("paraId", id), ("piece", piece)] => {
                    (part, p, Some(id), piece)
                }
                _ => return None,
            };
            if para.is_some_and(|id| id.is_empty()) {
                return None;
            }
            Some(Link::Docx {
                part: decode(part)?,
                p: number(p)?,
                piece: number(piece)?,
            })
        }
        _ => None,
    }
}

impl SourceRef {
    pub fn source_uri(artifact_id: ArtifactId, version: i32) -> String {
        format!("urn:katwork:artifact:{artifact_id}:v{version}")
    }

    /// The chain down to the block link, with no refinement.
    pub fn for_block(
        version: &VersionView,
        role: ArtifactRole,
        extractor: ExtractorId,
        block: &Block,
        page: Option<&PageInfo>,
    ) -> SourceRef {
        let piece = |link: Selector| -> Selector {
            if block.piece == 0 {
                return link;
            }
            let Selector::FragmentSelector {
                conforms_to, value, ..
            } = link
            else {
                unreachable!("a block link is a fragment");
            };
            Selector::FragmentSelector {
                conforms_to,
                value,
                refined_by: vec![fragment(CONFORMS_PIECE, format!("piece={}", block.piece))],
            }
        };
        let (selector, page_number) = match &block.locator {
            Locator::Text { .. } => (
                piece(fragment(
                    CONFORMS_BLOCK,
                    format!("ordinal={}", block.ordinal),
                )),
                None,
            ),
            Locator::PdfPage { page } => (
                Selector::FragmentSelector {
                    conforms_to: CONFORMS_PDF.into(),
                    value: format!("page={page}"),
                    refined_by: vec![piece(fragment(
                        CONFORMS_BLOCK,
                        format!("ordinal={}", block.ordinal),
                    ))],
                },
                Some(*page),
            ),
            Locator::DocxParagraph {
                part,
                ordinal,
                para_id,
            } => {
                let para = para_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .map(|id| format!("paraId={};", encode(&clean(id))))
                    .unwrap_or_default();
                (
                    fragment(
                        CONFORMS_DOCX,
                        format!(
                            "part={};p={ordinal};{para}piece={}",
                            encode(&clean(part)),
                            block.piece
                        ),
                    ),
                    None,
                )
            }
            Locator::XlsxCell {
                sheet_id,
                sheet,
                row,
                col,
            } => (
                Selector::FragmentSelector {
                    conforms_to: CONFORMS_SHEET.into(),
                    value: format!("sheetId={sheet_id};name={}", encode(&clean(sheet))),
                    refined_by: vec![piece(fragment(CONFORMS_CELL, format!("cell={row},{col}")))],
                },
                None,
            ),
        };
        SourceRef {
            kind: ResourceType::SpecificResource,
            source: Self::source_uri(version.artifact_id, version.version),
            sha256: version.sha256.clone(),
            extractor: ExtractorRef {
                name: extractor.name.into_owned(),
                version: extractor.version,
            },
            quality: Quality {
                generated: role == ArtifactRole::Output,
                hidden: block.hidden.is_some(),
                instruction_like: block_instruction_like(block).is_some(),
                suspect_text_layer: suspect_reason(block, page).is_some(),
            },
            selector,
            boxes: block
                .lines
                .iter()
                .map(|line| RefBox {
                    page: line.page,
                    rect: line.rect,
                })
                .collect(),
            unit_context: block.unit_context.as_deref().map(clean),
            cell: block.cell.as_ref().map(clean_cell),
            page: page_number,
        }
    }

    /// Refines the block link with `[TextQuoteSelector, TextPositionSelector]` over
    /// `start..end` (code points of `block.text`), clamped to the text and widened to whole
    /// grapheme clusters; keeps the boxes of the lines that overlap it.
    pub fn narrow(&self, block: &Block, start: u32, end: u32) -> SourceRef {
        let chars: Vec<char> = block.text.chars().collect();
        // Grapheme boundaries in code points, 0 and the length included.
        let mut bounds = vec![0usize];
        let mut count = 0;
        for grapheme in block.text.graphemes(true) {
            count += grapheme.chars().count();
            bounds.push(count);
        }
        let len = chars.len();
        let end = (end as usize).min(len);
        let start = (start as usize).min(end);
        let start = *bounds.iter().rev().find(|&&b| b <= start).unwrap_or(&0);
        let end = *bounds.iter().find(|&&b| b >= end).unwrap_or(&len);
        let take = |range: std::ops::Range<usize>| -> String { chars[range].iter().collect() };
        let refinement = vec![
            Selector::TextQuoteSelector {
                exact: take(start..end),
                prefix: take(start.saturating_sub(CONTEXT)..start),
                suffix: take(end..(end + CONTEXT).min(len)),
                refined_by: vec![],
            },
            Selector::TextPositionSelector {
                start: start as u32,
                end: end as u32,
                refined_by: vec![],
            },
        ];
        let mut narrowed = self.clone();
        *block_link(&mut narrowed.selector) = refinement;
        narrowed.boxes = block
            .lines
            .iter()
            .filter(|line| (line.start as usize) < end && (line.end as usize) > start)
            .map(|line| RefBox {
                page: line.page,
                rect: line.rect,
            })
            .collect();
        narrowed
    }

    /// `"<file name> · <locator>"`, in the plan's English literals, for the model and the log;
    /// the web localises from the structured ref instead (Task 29). `file_name` is the
    /// artifact's full name, folders included.
    pub fn display(&self, file_name: &str) -> String {
        let links = self.links().unwrap_or_default();
        let locator = match links.as_slice() {
            [Link::Page(page), Link::Block(_), ..] => format!("p. {page}"),
            [Link::Sheet { name }, Link::Cell { row, col }, ..] => {
                format!("{name}!{}{row}", column_letters(*col))
            }
            [Link::Docx { part, p, .. }] => match part_label(part) {
                Some(label) => format!("¶ {p} · {label}"),
                None => format!("¶ {p}"),
            },
            [Link::Block(ordinal), ..] => format!("¶ {ordinal}"),
            _ => "?".into(),
        };
        format!("{file_name} · {locator}")
    }

    /// The chain's fragment links, parsed, outer to inner.
    fn links(&self) -> Result<Vec<Link>, &'static str> {
        let mut links = Vec::new();
        let mut node = &self.selector;
        loop {
            let Selector::FragmentSelector {
                conforms_to,
                value,
                refined_by,
            } = node
            else {
                return Err("chain_not_fragment");
            };
            links.push(parse_link(conforms_to, value).ok_or("bad_selector_value")?);
            match refined_by.as_slice() {
                [next @ Selector::FragmentSelector { .. }] => node = next,
                _ => return Ok(links),
            }
        }
    }

    /// A known `conformsTo` at each link, a chain shape this file type has, and, when
    /// narrowed, exactly `[TextQuoteSelector, TextPositionSelector]` in that order.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !is_sha256(&self.sha256) {
            return Err("bad_sha256");
        }
        let links = self.links()?;
        let mut shape = Vec::new();
        let mut node = &self.selector;
        while let Selector::FragmentSelector {
            conforms_to,
            refined_by,
            ..
        } = node
        {
            shape.push(conforms_to.as_str());
            match refined_by.as_slice() {
                [next @ Selector::FragmentSelector { .. }] => node = next,
                [] => break,
                [
                    Selector::TextQuoteSelector {
                        refined_by: quote, ..
                    },
                    Selector::TextPositionSelector {
                        start,
                        end,
                        refined_by: position,
                    },
                ] => {
                    if !quote.is_empty() || !position.is_empty() {
                        return Err("alternative_refined");
                    }
                    if start >= end {
                        return Err("empty_position");
                    }
                    break;
                }
                _ => return Err("bad_refinement"),
            }
        }
        if !SHAPES.contains(&shape.as_slice()) {
            return Err("unknown_chain");
        }
        let page_link = links.iter().find_map(|link| match link {
            Link::Page(page) => Some(*page),
            _ => None,
        });
        if self.page != page_link {
            return Err("page_mismatch");
        }
        Ok(())
    }

    /// `validate`, then the narrowing checked against the block's text: the position lies
    /// inside it, `exact` is the text at the position, and `prefix` and `suffix` are the text
    /// around it.
    pub fn validate_against(&self, block_text: &str) -> Result<(), &'static str> {
        self.validate()?;
        let mut node = &self.selector;
        loop {
            match node {
                Selector::FragmentSelector { refined_by, .. } => match refined_by.as_slice() {
                    [next @ Selector::FragmentSelector { .. }] => node = next,
                    [] => return Ok(()),
                    [
                        Selector::TextQuoteSelector {
                            exact,
                            prefix,
                            suffix,
                            ..
                        },
                        Selector::TextPositionSelector { start, end, .. },
                    ] => {
                        let chars: Vec<char> = block_text.chars().collect();
                        let (start, end) = (*start as usize, *end as usize);
                        if end > chars.len() {
                            return Err("position_outside_block");
                        }
                        let at = |range: std::ops::Range<usize>| -> String {
                            chars[range].iter().collect()
                        };
                        if at(start..end) != *exact {
                            return Err("quote_not_at_position");
                        }
                        if !at(0..start).ends_with(prefix.as_str()) {
                            return Err("prefix_not_in_block");
                        }
                        if !at(end..chars.len()).starts_with(suffix.as_str()) {
                            return Err("suffix_not_in_block");
                        }
                        return Ok(());
                    }
                    _ => return Err("bad_refinement"),
                },
                _ => return Err("chain_not_fragment"),
            }
        }
    }
}

/// The innermost fragment link's refinements.
fn block_link(selector: &mut Selector) -> &mut Vec<Selector> {
    let Selector::FragmentSelector { refined_by, .. } = selector else {
        unreachable!("a chain starts with a fragment");
    };
    if matches!(refined_by.as_slice(), [Selector::FragmentSelector { .. }]) {
        return block_link(&mut refined_by[0]);
    }
    refined_by
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn clean_cell(cell: &CellFacts) -> CellFacts {
    let c = |s: &Option<String>| s.as_deref().map(clean);
    CellFacts {
        cached: c(&cell.cached),
        displayed: c(&cell.displayed),
        formula: c(&cell.formula),
        number_format: c(&cell.number_format),
        row_header: c(&cell.row_header),
        col_header: c(&cell.col_header),
        header_unit: c(&cell.header_unit),
        ..cell.clone()
    }
}

/// Why a block's text may differ from what a person sees: the block's own reason, else its
/// page's text layer. A PDF block whose page is unknown is suspect (fail closed).
pub fn suspect_reason(block: &Block, page: Option<&PageInfo>) -> Option<&'static str> {
    if let Some(reason) = block.suspect {
        return Some(reason);
    }
    match (page.map(|p| p.text_layer), &block.locator) {
        (Some(TextLayer::Suspect(reason)), _) => Some(reason),
        (Some(TextLayer::Absent), _) => Some("no_text_layer"),
        (None, Locator::PdfPage { .. }) => Some("page_unknown"),
        _ => None,
    }
}

/// Bijective base 26: 1 is `A`, 26 `Z`, 27 `AA`.
pub fn column_letters(col: u32) -> String {
    let mut col = col;
    let mut letters = Vec::new();
    while col > 0 {
        let rem = (col - 1) % 26;
        letters.push(b'A' + rem as u8);
        col = (col - 1) / 26;
    }
    letters.reverse();
    String::from_utf8(letters).expect("ASCII letters")
}

/// The words a person reads for a DOCX part: none for the main document, else the part's file
/// stem (`footnotes`, `header1`).
pub fn part_label(part: &str) -> Option<&str> {
    let file = part.rsplit('/').next().unwrap_or(part);
    let stem = file.strip_suffix(".xml").unwrap_or(file);
    (stem != "document").then_some(stem)
}

/// The short locator a `read_file` line shows (`Budget!B7`, `p.3`, `¶ 17`).
pub fn block_at(block: &Block) -> String {
    match &block.locator {
        Locator::Text { .. } => format!("¶ {}", block.ordinal),
        Locator::PdfPage { page } => format!("p.{page}"),
        Locator::DocxParagraph { part, ordinal, .. } => docx_at(part, *ordinal),
        Locator::XlsxCell {
            sheet, row, col, ..
        } => format!("{sheet}!{}{row}", column_letters(*col)),
    }
}

/// The locator of a warning's place.
pub fn locator_at(locator: &Locator) -> String {
    match locator {
        Locator::Text { start, end } => format!("chars {start}-{end}"),
        Locator::PdfPage { page } => format!("p.{page}"),
        Locator::DocxParagraph { part, ordinal, .. } => docx_at(part, *ordinal),
        Locator::XlsxCell {
            sheet, row, col, ..
        } => format!("{sheet}!{}{row}", column_letters(*col)),
    }
}

fn docx_at(part: &str, ordinal: u32) -> String {
    match part_label(part) {
        Some(label) => format!("¶ {ordinal} · {label}"),
        None => format!("¶ {ordinal}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters_table() {
        for (col, letters) in [
            (1, "A"),
            (2, "B"),
            (26, "Z"),
            (27, "AA"),
            (702, "ZZ"),
            (703, "AAA"),
            (16_384, "XFD"),
        ] {
            assert_eq!(column_letters(col), letters);
        }
    }

    #[test]
    fn names_round_trip_through_the_value_encoding() {
        for name in ["a;b=c%", "งบ 2567", "tab\there", "word/x;p=1.xml"] {
            assert_eq!(decode(&encode(name)).as_deref(), Some(name));
            assert!(!encode(name).contains(';') && !encode(name).contains('='));
        }
    }
}
