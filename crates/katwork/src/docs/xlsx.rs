//! KatWork's XLSX reader (`xlsx-cached` version 1): one block per non-empty cell of every sheet,
//! in `<sheets>` order, then row, then column, holding the value Excel shows from the cell's
//! cached value, with the cell's facts and whether a person can see it.
//!
//! The package guard inflates and checks every entry first; then every XML part is scanned, the
//! worksheets' `<c>` elements and the shared strings' `<si>` elements counted against their caps,
//! before any model is built. Parts are read by namespace and local name, never by prefix, and
//! SpreadsheetML's cell markup anywhere but its place in the schema is refused, so the reader
//! never reads another workbook than Excel would. Where what Excel draws cannot be decided, a cell
//! or a rich-text run counts as hidden.
use std::{
    borrow::Cow,
    cell::RefCell,
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BinaryHeap, HashMap, HashSet},
};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use quick_xml::{
    NsReader,
    events::{BytesStart, Event},
    name::{QName, ResolveResult},
};

use super::{
    Block, CellFacts, CellKind, DocError, DocLimits, ExtractWarning, Extracted, ExtractorId,
    HiddenWhy, Locator, WorkbookFacts,
    colour::{self, Rgb, SchemeSlot, ThemeColours},
    deadline, numfmt,
    package::{self, Package},
    replace_nul, split_pieces,
    xml_guard::{self, ElementBudget},
};

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const STRICT: &str = "http://purl.oclc.org/ooxml/";
const PACKAGE_RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const RELATIONSHIP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
/// The namespace of the `r:id` attributes that name a relationship.
const RELATIONSHIP_ID: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MS_RELATIONSHIP: &str = "http://schemas.microsoft.com/office/2006/relationships/";

/// Excel's limits ("Excel specifications and limits"): rows, columns, characters in a cell, in a
/// formula, in a number format and in a sheet's name.
const MAX_ROW: u32 = 1_048_576;
const MAX_COLUMN: u32 = 16_384;
const MAX_CELL_CHARS: usize = 32_767;
const MAX_FORMULA_CHARS: usize = 8_192;
const MAX_FORMAT_CHARS: usize = 255;
const MAX_SHEET_NAME_CHARS: usize = 31;
/// The narrowest column (in characters of `width`) that shows its cells.
const MIN_COLUMN_WIDTH: f64 = 1.0;
/// `_xHHHH_` is seven characters for one: text this many times a cap, in bytes, cannot fit it.
const ESCAPED_BYTES: usize = 7 * 4;
/// A column header stands in one of a sheet's first rows.
const HEADER_ROWS: u32 = 5;
/// The size of a font that states none, when the default font states none either.
const DEFAULT_FONT_PT: f64 = 11.0;
/// The colours one place may be drawn in: past this many, what Excel draws is left undecided.
const MAX_CANDIDATES: usize = 64;
/// Cells given headers between two checks of the job's deadline and memory budget.
const BLOCKS_PER_CHECK: usize = 64;
/// Header bytes copied into blocks between two checks: a step past the memory budget is at most
/// this and one block's copies.
const HEADER_BYTES_PER_CHECK: usize = 1 << 20;

/// The standard 64-entry palette that `indexed` colours name (ECMA-376 Part 1 §18.8.27).
const PALETTE: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
    0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
    0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
    0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
    0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
    0x333333,
];
/// `indexed` 64 and 65: the system's foreground and background, black and white by default.
const SYSTEM_FOREGROUND: u32 = 64;
const SYSTEM_BACKGROUND: u32 = 65;

/// How a cell looks, as the hidden rule reads it: whether its sheet, row or column is hidden,
/// whether its number format's section for its value is empty (`numfmt::section_is_empty`), and
/// its font's size and colour against its fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellLook {
    pub sheet_hidden: bool,
    pub row_hidden: bool,
    pub col_hidden: bool,
    pub empty_format: bool,
    pub font_pt: f32,
    pub font: Rgb,
    pub fill: Rgb,
}

/// Why a person cannot see a cell that looks like `look`, in the rule's order: its row, its
/// column, its sheet, an empty number-format section, a font under `min_visible_pt`, a font whose
/// contrast with the fill is under `min_contrast`. A size or contrast that is not a number hides.
pub fn hidden_why(look: &CellLook, limits: &DocLimits) -> Option<HiddenWhy> {
    if look.row_hidden {
        Some(HiddenWhy::HiddenRow)
    } else if look.col_hidden {
        Some(HiddenWhy::HiddenColumn)
    } else if look.sheet_hidden {
        Some(HiddenWhy::HiddenSheet)
    } else if look.empty_format {
        Some(HiddenWhy::EmptyFormat)
    } else if look.font_pt.is_nan() || look.font_pt < limits.min_visible_pt {
        Some(HiddenWhy::Tiny)
    } else if colour::contrast_ratio(look.font, look.fill)
        .partial_cmp(&limits.min_contrast)
        .is_none_or(Ordering::is_lt)
    {
        Some(HiddenWhy::LowContrast)
    } else {
        None
    }
}

/// Sets `row_header`, `col_header`, `header_unit` and `unit_context` of one sheet's cell blocks
/// from their visible neighbours: the row header is the nearest visible, non-empty text cell to
/// the left in the same row; the column header the nearest one above in the same column within
/// the sheet's first five rows; the header unit a scale or unit stated in parentheses or after a
/// colon in the column header, else the row header; the unit context, for every cell of the
/// sheet, the text of the first visible cell of rows 1 to 5, row by row, whose trimmed text
/// starts with `หน่วย` or, in any case, `unit`. A hidden cell supplies none of them. The pieces of
/// a long cell are joined as its text. Blocks with no cell facts are left as they are.
///
/// Inside a job scope it stops early once a check of the job's deadline and memory budget fails,
/// leaving the rest unset; the reader then fails the read with that check's error. Outside one
/// every check passes.
pub fn fill_headers(blocks: &mut [Block]) {
    let _ = headers(blocks);
}

/// `fill_headers`, failing with the first check that fails.
fn headers(blocks: &mut [Block]) -> Result<(), DocError> {
    let mut order: Vec<(u32, u32, u32, usize)> = blocks
        .iter()
        .enumerate()
        .filter_map(|(at, block)| match (&block.locator, &block.cell) {
            (Locator::XlsxCell { row, col, .. }, Some(_)) => Some((*row, *col, block.piece, at)),
            _ => None,
        })
        .collect();
    order.sort_unstable();
    // The cells, in row then column order: their place, blocks, kind, visibility and text.
    let mut cells: Vec<HeaderCell> = Vec::new();
    for (row, col, _, at) in order {
        let block = &blocks[at];
        match cells.last_mut() {
            Some(cell) if (cell.row, cell.col) == (row, col) => {
                cell.blocks.push(at);
                cell.text.push_str(&block.text);
            }
            _ => cells.push(HeaderCell {
                row,
                col,
                blocks: vec![at],
                text: block.text.clone(),
                header: block.hidden.is_none()
                    && block
                        .cell
                        .as_ref()
                        .is_some_and(|cell| cell.kind == CellKind::Text),
                visible: block.hidden.is_none(),
            }),
        }
    }
    for cell in &mut cells {
        cell.header &= !cell.text.trim().is_empty();
    }
    let unit_context = cells
        .iter()
        .find(|cell| cell.row <= HEADER_ROWS && cell.visible && states_unit_context(&cell.text))
        .map(|cell| cell.text.clone());
    // Each column's header cells in its first rows, by row.
    let mut column_headers: BTreeMap<u32, Vec<(u32, usize)>> = BTreeMap::new();
    for (at, cell) in cells.iter().enumerate() {
        if cell.header && cell.row <= HEADER_ROWS {
            column_headers
                .entry(cell.col)
                .or_default()
                .push((cell.row, at));
        }
    }
    let mut units: HashMap<usize, Option<String>> = HashMap::new();
    let mut unit_of = |at: usize, cells: &[HeaderCell]| {
        units
            .entry(at)
            .or_insert_with(|| header_unit(&cells[at].text))
            .clone()
    };
    let mut row_header: Option<(u32, usize)> = None;
    // Bytes of headers copied since the last check: a long header copied into many pieces.
    let mut copied = 0usize;
    for at in 0..cells.len() {
        if at % BLOCKS_PER_CHECK == BLOCKS_PER_CHECK - 1 {
            check("header_cells")?;
        }
        let (row, col) = (cells[at].row, cells[at].col);
        let left = row_header
            .filter(|(header_row, _)| *header_row == row)
            .map(|(_, header)| header);
        let above = column_headers.get(&col).and_then(|headers| {
            headers
                .iter()
                .take_while(|(header_row, _)| *header_row < row)
                .last()
                .map(|(_, header)| *header)
        });
        let unit = above
            .and_then(|header| unit_of(header, &cells))
            .or_else(|| left.and_then(|header| unit_of(header, &cells)));
        let bytes = left.map_or(0, |header| cells[header].text.len())
            + above.map_or(0, |header| cells[header].text.len())
            + unit.as_ref().map_or(0, String::len)
            + unit_context.as_ref().map_or(0, String::len);
        for &block in &cells[at].blocks {
            if copied >= HEADER_BYTES_PER_CHECK {
                check("header_bytes")?;
                copied = 0;
            }
            let block = &mut blocks[block];
            if let Some(facts) = &mut block.cell {
                facts.row_header = left.map(|header| cells[header].text.clone());
                facts.col_header = above.map(|header| cells[header].text.clone());
                facts.header_unit = unit.clone();
            }
            block.unit_context = unit_context.clone();
            copied += bytes;
        }
        if cells[at].header {
            row_header = Some((row, at));
        }
    }
    Ok(())
}

struct HeaderCell {
    row: u32,
    col: u32,
    blocks: Vec<usize>,
    text: String,
    /// Visible, text, and not blank: it may be a header.
    header: bool,
    visible: bool,
}

fn states_unit_context(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("หน่วย")
        || text
            .get(..4)
            .is_some_and(|start| start.eq_ignore_ascii_case("unit"))
}

/// The scale or unit a header states inside parentheses or after a colon, as in `รายได้
/// (ล้านบาท)`, `Revenue (THB million)` or `งบลงทุน: ล้านบาท`: the first such text from which
/// `find_numbers` of `"1 "` and the text reads a scale or a unit for that `1`.
fn header_unit(header: &str) -> Option<String> {
    let mut candidates = Vec::new();
    let mut rest = header;
    while let Some(open) = rest.find('(') {
        let inner = &rest[open + 1..];
        let Some(close) = inner.find(')') else {
            break;
        };
        candidates.push(&inner[..close]);
        rest = &inner[close + 1..];
    }
    if let Some((_, after)) = header.split_once(':') {
        candidates.push(after);
    }
    // Two-digit years are read around a fixed day, so a read never depends on today's date.
    let ctx = katwork_thai::Ctx {
        anchor: chrono::NaiveDate::from_ymd_opt(2024, 1, 1),
    };
    candidates
        .into_iter()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .find(|text| {
            katwork_thai::find_numbers(&format!("1 {text}"), &ctx).is_ok_and(|found| {
                found.iter().any(|number| {
                    number.start == 0 && (number.value.scale.0 > 0 || number.value.unit.is_some())
                })
            })
        })
        .map(str::to_owned)
}

/// What a read did, in order, for tests: the stage hooks of the plan's umya tests, which became
/// this reader's (founder decision R38).
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// `xml_guard::scan` passed this part; `counted` is its `<c>` elements for a worksheet, its
    /// `<si>` elements for the shared strings.
    Scanned { part: String, counted: u64 },
    /// Every cap checked, the reader began building its model.
    Model,
    /// The reader read this part's bytes (their CRC-32).
    Read { part: String, crc: u32 },
    /// One of the reader's loops checked the job's deadline and memory budget, at this site.
    Check(&'static str),
}

thread_local! {
    static TRACE: RefCell<Option<Vec<Stage>>> = const { RefCell::new(None) };
}

/// Runs `f` recording the stages of every read on this thread; returns them beside its result.
#[doc(hidden)]
pub fn trace<T>(f: impl FnOnce() -> T) -> (T, Vec<Stage>) {
    let outer = TRACE.replace(Some(Vec::new()));
    let result = f();
    let stages = TRACE.replace(outer).unwrap_or_default();
    (result, stages)
}

fn record(stage: impl FnOnce() -> Stage) {
    TRACE.with_borrow_mut(|trace| {
        if let Some(trace) = trace {
            trace.push(stage());
        }
    });
}

/// `deadline::check`, from the reader's loop at `site`.
fn check(site: &'static str) -> Result<(), DocError> {
    record(|| Stage::Check(site));
    deadline::check()
}

fn record_read(part: &str, xml: &[u8]) {
    record(|| Stage::Read {
        part: part.to_owned(),
        crc: crc32fast::hash(xml),
    });
}

pub(super) fn read(bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    let package = package::open_package(bytes, limits)?;
    let mut budget = ElementBudget::new(limits);
    let parts = Parts::find(&package, limits, &mut budget)?;
    prescan(&package, &parts, limits, &mut budget)?;
    record(|| Stage::Model);

    let workbook_xml = package.part(&parts.main).unwrap_or_default();
    record_read(&parts.main, workbook_xml);
    let workbook = Workbook::read(workbook_xml)?;
    let sheets = parts.sheets(&package, &workbook)?;

    let mut warnings = Warnings::new(limits.max_blocks);
    let content = parts.content(&package, limits)?;
    for (found, warning) in [
        (
            content.vba_project,
            ExtractWarning::ActiveContentIgnored {
                what: "vba_project",
            },
        ),
        (
            content.macro_sheet,
            ExtractWarning::ActiveContentIgnored {
                what: "macro_sheet",
            },
        ),
        (
            content.external_link,
            ExtractWarning::EmbeddedContentIgnored {
                what: "external_link",
            },
        ),
        (
            content.ole_object,
            ExtractWarning::EmbeddedContentIgnored { what: "ole_object" },
        ),
        (
            content.activex,
            ExtractWarning::EmbeddedContentIgnored { what: "activex" },
        ),
    ] {
        if found {
            warnings.once(warning)?;
        }
    }
    if workbook.full_calc_on_load {
        warnings.push(ExtractWarning::FullCalcOnLoad)?;
    }

    let theme = match &parts.theme {
        Some(name) => {
            let xml = package.part(name).unwrap_or_default();
            let theme = ThemeColours::parse(xml, limits, &mut budget)?;
            record(|| Stage::Scanned {
                part: name.clone(),
                counted: 0,
            });
            record_read(name, xml);
            Some(theme)
        }
        None => None,
    };
    let styles = match &parts.styles {
        Some(name) => {
            let xml = package.part(name).unwrap_or_default();
            record_read(name, xml);
            Styles::read(xml)?
        }
        None => Styles::default(),
    };
    let strings = match &parts.shared_strings {
        Some(name) => {
            let xml = package.part(name).unwrap_or_default();
            record_read(name, xml);
            read_shared_strings(xml)?
        }
        None => Vec::new(),
    };

    let judge = Judge {
        styles: &styles,
        theme: theme.as_ref(),
        limits,
        default_pt: styles.default_pt(),
        looks: RefCell::default(),
        paints: RefCell::default(),
    };
    let mut out = Output {
        limits,
        strings: &strings,
        date1904: workbook.date1904,
        full_calc_on_load: workbook.full_calc_on_load,
        blocks: Vec::new(),
        warnings,
        ordinal: 0,
    };
    for sheet in &sheets {
        let SheetKind::Worksheet(part) = &sheet.kind else {
            continue;
        };
        let xml = package.part(part).unwrap_or_default();
        record_read(part, xml);
        let data = read_sheet(xml, &strings, limits)?;
        let start = out.blocks.len();
        out.sheet(&judge, sheet, &data)?;
        headers(&mut out.blocks[start..])?;
        check("sheet")?;
    }
    Ok(Extracted {
        extractor: ExtractorId::XLSX,
        blocks: out.blocks,
        pages: Vec::new(),
        warnings: out.warnings.list,
        workbook: Some(WorkbookFacts {
            full_calc_on_load: workbook.full_calc_on_load,
        }),
    })
}

// ---------------------------------------------------------------------------------------------
// Parts, relationships and the pre-scan.
// ---------------------------------------------------------------------------------------------

struct Relationship {
    id: String,
    kind: String,
    target: Option<String>,
    external: bool,
    /// The directory of the part whose relationships these are, for relative targets.
    base: String,
}

impl Relationship {
    /// The package part an internal relationship points at, or `None` for an external one, a
    /// missing target or one that climbs above the package root.
    fn internal_target(&self) -> Option<String> {
        if self.external {
            return None;
        }
        let target = self.target.as_deref()?;
        let path = match target.strip_prefix('/') {
            Some(absolute) => absolute.to_owned(),
            None => format!("{}{target}", self.base),
        };
        let mut segments = Vec::new();
        for segment in path.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop()?;
                }
                segment => segments.push(segment),
            }
        }
        (!segments.is_empty()).then(|| segments.join("/"))
    }

    fn is(&self, kind: &str) -> bool {
        self.kind
            .strip_prefix(RELATIONSHIP)
            .or_else(|| self.kind.strip_prefix(MS_RELATIONSHIP))
            == Some(kind)
    }
}

/// Whether the reader follows a relationship of type `kind`; the others are passed over.
fn followed(kind: &str) -> bool {
    kind.starts_with(STRICT)
        || kind.strip_prefix(RELATIONSHIP).is_some_and(|kind| {
            matches!(
                kind,
                "officeDocument"
                    | "worksheet"
                    | "chartsheet"
                    | "dialogsheet"
                    | "styles"
                    | "sharedStrings"
                    | "theme"
                    | "externalLink"
                    | "connections"
                    | "oleObject"
                    | "package"
                    | "control"
            )
        })
        || kind.strip_prefix(MS_RELATIONSHIP).is_some_and(|kind| {
            matches!(
                kind,
                "vbaProject" | "xlMacrosheet" | "xlIntlMacrosheet" | "activeXControlBinary"
            )
        })
}

/// The relationships part of `part` (`""` for the package's own).
fn rels_of(part: &str) -> (String, String) {
    match part.rsplit_once('/') {
        Some((dir, file)) => (format!("{dir}/_rels/{file}.rels"), format!("{dir}/")),
        None if part.is_empty() => ("_rels/.rels".into(), String::new()),
        None => (format!("_rels/{part}.rels"), String::new()),
    }
}

/// The relationships a scanned relationships part lists that the reader follows. An id written
/// twice is malformed, as is a target mode that is neither; more followed relationships than
/// `max_zip_entries` is `TooComplex`: a package has no more parts to point at.
fn relationships(
    xml: &[u8],
    base: &str,
    limits: &DocLimits,
) -> Result<Vec<Relationship>, DocError> {
    let mut reader = xml_guard::reader(xml);
    let (mut depth, mut found, mut ids) = (0u32, Vec::new(), HashSet::new());
    for events in 0u64.. {
        if events % 1024 == 1023 {
            check("relationships")?;
        }
        let (resolved, event) = reader.read_resolved_event().map_err(xml_guard::map_error)?;
        let ours = matches!(&resolved, ResolveResult::Bound(ns) if ns.0 == PACKAGE_RELATIONSHIPS);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if depth == 1 && ours && start.local_name().as_ref() == "Relationship" {
                    let [id, kind, target, mode] = attributes(
                        &reader,
                        start,
                        [
                            (None, "Id"),
                            (None, "Type"),
                            (None, "Target"),
                            (None, "TargetMode"),
                        ],
                        None,
                    )?;
                    if !ids.insert(id.clone().unwrap_or_default()) {
                        return Err(DocError::Malformed("xlsx_rels"));
                    }
                    let kind = kind.unwrap_or_default();
                    if followed(&kind) {
                        if found.len() as u64 >= u64::from(limits.max_zip_entries) {
                            return Err(DocError::TooComplex("relationships"));
                        }
                        let external = match mode.as_deref() {
                            None | Some("Internal") => false,
                            Some("External") => true,
                            Some(_) => return Err(DocError::Malformed("xlsx_rels")),
                        };
                        found.push(Relationship {
                            id: id.unwrap_or_default(),
                            kind,
                            target,
                            external,
                            base: base.to_owned(),
                        });
                    }
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(found)
}

/// The values of `start`'s attributes `names` ((namespace, local name); no namespace for an
/// unprefixed one), entities resolved. An attribute in the namespace `refuse` is refused.
fn attributes<const N: usize>(
    reader: &NsReader<&[u8]>,
    start: &BytesStart,
    names: [(Option<&str>, &str); N],
    refuse: Option<&str>,
) -> Result<[Option<String>; N], DocError> {
    let mut found = [const { None }; N];
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|_| DocError::Malformed("xml_attribute"))?;
        let (resolved, local) = reader.resolver().resolve_attribute(attribute.key);
        let namespace = match resolved {
            ResolveResult::Bound(ns) => Some(ns.0),
            _ => None,
        };
        if namespace.is_some() && namespace == refuse {
            return Err(DocError::Malformed("xlsx_structure"));
        }
        if let Some(at) = names
            .iter()
            .position(|(ns, name)| *ns == namespace && *name == local.as_ref())
        {
            found[at] = Some(xml_guard::attribute_value(&attribute.value)?.into_owned());
        }
    }
    Ok(found)
}

/// The parts the workbook's relationships give roles, found before the pre-scan.
struct Parts {
    /// The workbook part.
    main: String,
    workbook_rels: Vec<Relationship>,
    /// The names of the relationships parts already read.
    rels_read: Vec<String>,
    styles: Option<String>,
    shared_strings: Option<String>,
    theme: Option<String>,
    /// The parts `worksheet` relationships name, ASCII-lowercased.
    worksheets: HashSet<String>,
}

impl Parts {
    fn find(
        package: &Package,
        limits: &DocLimits,
        budget: &mut ElementBudget,
    ) -> Result<Parts, DocError> {
        let mut rels_read = Vec::new();
        let mut read_rels = |part: &str, budget: &mut ElementBudget| {
            let (name, base) = rels_of(part);
            let Some(xml) = package.part(&name) else {
                return Ok(None);
            };
            let stats = xml_guard::scan(xml, limits, None, budget)?;
            record(|| Stage::Scanned {
                part: name.clone(),
                counted: stats.counted,
            });
            record_read(&name, xml);
            rels_read.push(name);
            relationships(xml, &base, limits).map(Some)
        };
        let root = read_rels("", budget)?.ok_or(DocError::Malformed("xlsx_main_part"))?;
        let strict = |rels: &[Relationship]| rels.iter().any(|rel| rel.kind.starts_with(STRICT));
        if strict(&root) {
            return Err(DocError::Unsupported("strict_ooxml"));
        }
        let mut mains = root.iter().filter(|rel| rel.is("officeDocument"));
        let (Some(main), None) = (mains.next(), mains.next()) else {
            return Err(DocError::Malformed("xlsx_main_part"));
        };
        let main = main
            .internal_target()
            .filter(|name| package.part(name).is_some())
            .ok_or(DocError::Malformed("xlsx_main_part"))?;
        let workbook_rels = read_rels(&main, budget)?.unwrap_or_default();
        if strict(&workbook_rels) {
            return Err(DocError::Unsupported("strict_ooxml"));
        }
        // A part of one role comes from the one relationship of its type; a second leaves which
        // one Excel reads undecided.
        let single = |kind: &str| -> Result<Option<String>, DocError> {
            let mut found = workbook_rels.iter().filter(|rel| rel.is(kind));
            match (found.next(), found.next()) {
                (None, _) => Ok(None),
                (Some(_), Some(_)) => Err(DocError::Malformed("xlsx_part_ambiguous")),
                (Some(rel), None) if rel.external => Ok(None),
                (Some(rel), None) => rel
                    .internal_target()
                    .filter(|name| package.part(name).is_some())
                    .map(Some)
                    .ok_or(DocError::Malformed("xlsx_part_missing")),
            }
        };
        let (styles, shared_strings, theme) = (
            single("styles")?,
            single("sharedStrings")?,
            single("theme")?,
        );
        let worksheets: HashSet<String> = workbook_rels
            .iter()
            .filter(|rel| rel.is("worksheet"))
            .filter_map(Relationship::internal_target)
            .filter(|name| package.part(name).is_some())
            .map(|name| name.to_ascii_lowercase())
            .collect();
        // One part in two roles is read two ways.
        let mut roles: HashMap<String, &str> = HashMap::new();
        let named = [
            (Some(&main), "workbook"),
            (styles.as_ref(), "styles"),
            (shared_strings.as_ref(), "sharedStrings"),
            (theme.as_ref(), "theme"),
        ];
        for (name, role) in named
            .into_iter()
            .filter_map(|(name, role)| name.map(|name| (name.to_ascii_lowercase(), role)))
            .chain(worksheets.iter().map(|name| (name.clone(), "worksheet")))
        {
            if roles.insert(name, role).is_some_and(|other| other != role) {
                return Err(DocError::Malformed("xlsx_part_ambiguous"));
            }
        }
        Ok(Parts {
            main,
            workbook_rels,
            rels_read,
            styles,
            shared_strings,
            theme,
            worksheets,
        })
    }

    /// Each `<sheet>` of the workbook with the part its relationship names.
    fn sheets(&self, package: &Package, workbook: &Workbook) -> Result<Vec<Sheet>, DocError> {
        let mut parts = HashSet::new();
        let mut sheets = Vec::new();
        for entry in &workbook.sheets {
            let rel = self
                .workbook_rels
                .iter()
                .find(|rel| rel.id == entry.rel_id)
                .ok_or(DocError::Malformed("xlsx_sheet_part"))?;
            let kind = if rel.is("worksheet") {
                if rel.external {
                    return Err(DocError::Malformed("xlsx_sheet_part"));
                }
                let part = rel
                    .internal_target()
                    .filter(|name| package.part(name).is_some())
                    .ok_or(DocError::Malformed("xlsx_part_missing"))?;
                if !parts.insert(part.to_ascii_lowercase()) {
                    return Err(DocError::Malformed("xlsx_sheet_identity"));
                }
                SheetKind::Worksheet(part)
            } else if rel.is("chartsheet")
                || rel.is("dialogsheet")
                || rel.is("xlMacrosheet")
                || rel.is("xlIntlMacrosheet")
            {
                SheetKind::NoCells
            } else {
                return Err(DocError::Malformed("xlsx_sheet_part"));
            };
            sheets.push(Sheet {
                name: entry.name.clone(),
                id: entry.id,
                hidden: entry.hidden,
                kind,
            });
        }
        Ok(sheets)
    }

    /// The macros, external links and embedded objects (ActiveX controls among them) the package
    /// holds, which the reader never reads: from `xl/vbaProject.bin`, from any part under
    /// `xl/activeX/`, and from every relationships part.
    fn content(&self, package: &Package, limits: &DocLimits) -> Result<Content, DocError> {
        let mut content = Content {
            vba_project: package.part("xl/vbaProject.bin").is_some(),
            activex: package
                .names()
                .any(|name| name.to_ascii_lowercase().starts_with("xl/activex/")),
            ..Content::default()
        };
        content.note(&self.workbook_rels);
        let names: Vec<&str> = package
            .names()
            .filter(|name| {
                name.to_ascii_lowercase().ends_with(".rels")
                    && !self
                        .rels_read
                        .iter()
                        .any(|read| read.eq_ignore_ascii_case(name))
            })
            .collect();
        for name in names {
            let xml = package.part(name).unwrap_or_default();
            record_read(name, xml);
            let base = name
                .rsplit_once("_rels/")
                .map(|(dir, _)| dir.to_owned())
                .unwrap_or_default();
            content.note(&relationships(xml, &base, limits)?);
        }
        Ok(content)
    }
}

#[derive(Default)]
struct Content {
    vba_project: bool,
    macro_sheet: bool,
    external_link: bool,
    ole_object: bool,
    activex: bool,
}

impl Content {
    fn note(&mut self, rels: &[Relationship]) {
        for rel in rels {
            self.vba_project |= rel.is("vbaProject");
            self.macro_sheet |= rel.is("xlMacrosheet") || rel.is("xlIntlMacrosheet");
            self.external_link |= rel.is("externalLink") || rel.is("connections");
            self.ole_object |= rel.is("oleObject") || rel.is("package");
            self.activex |= rel.is("control") || rel.is("activeXControlBinary");
        }
    }
}

/// Scans every XML part, and every part a relationship gives a role, before any model is built:
/// the theme part excepted, which `ThemeColours::parse` scans as it reads it, and the two
/// relationships parts already scanned. The `<c>` elements of every worksheet together are held
/// to `max_cells`, and the shared strings' `<si>` elements to `max_shared_strings`.
fn prescan(
    package: &Package,
    parts: &Parts,
    limits: &DocLimits,
    budget: &mut ElementBudget,
) -> Result<(), DocError> {
    let lower = |name: &Option<String>| name.as_ref().map(|name| name.to_ascii_lowercase());
    let (styles, shared, theme) = (
        lower(&parts.styles),
        lower(&parts.shared_strings),
        lower(&parts.theme),
    );
    let main = parts.main.to_ascii_lowercase();
    let mut cells = 0u64;
    for name in package.names() {
        let name_lower = name.to_ascii_lowercase();
        if Some(&name_lower) == theme.as_ref()
            || parts
                .rels_read
                .iter()
                .any(|read| read.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let worksheet = parts.worksheets.contains(&name_lower);
        let strings = Some(&name_lower) == shared.as_ref();
        let role =
            worksheet || strings || name_lower == main || Some(&name_lower) == styles.as_ref();
        let xml = [".xml", ".rels", ".vml"]
            .iter()
            .any(|extension| name_lower.ends_with(extension));
        if !role && !xml {
            continue;
        }
        let count: Option<&[u8]> = if worksheet {
            Some(b"c")
        } else if strings {
            Some(b"si")
        } else {
            None
        };
        let stats = xml_guard::scan(
            package.part(name).unwrap_or_default(),
            limits,
            count,
            budget,
        )?;
        record(|| Stage::Scanned {
            part: name.to_owned(),
            counted: stats.counted,
        });
        if worksheet {
            cells += stats.counted;
            if cells > u64::from(limits.max_cells) {
                return Err(DocError::TooComplex("cells"));
            }
        }
        if strings && stats.counted > u64::from(limits.max_shared_strings) {
            return Err(DocError::TooComplex("shared_strings"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Walking a part by namespace and path.
// ---------------------------------------------------------------------------------------------

/// An open element: whether it is SpreadsheetML's, and its local name when it is one the part's
/// reader knows (else empty).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    sml: bool,
    name: &'static str,
}

enum Node<'x> {
    Start {
        start: BytesStart<'x>,
        open: Open,
        /// A Strict SpreadsheetML (or other Strict OOXML) element.
        strict: bool,
    },
    End(Open),
    Text(Cow<'x, str>),
    /// Where the walk stands, markup compatibility left content the reader cannot read as Excel
    /// may (see `Walk::mc_from`): `held` names the SpreadsheetML elements in it (`""` for one the
    /// part's reader does not know), and `any` is whether it held anything at all.
    Undecided {
        held: Vec<&'static str>,
        any: bool,
    },
    Eof,
}

/// An open element of the walk: one reported (on the path), or markup compatibility's.
enum Frame {
    Reported,
    /// `mc:AlternateContent`: whether a branch was taken, and whether its `mc:Fallback` was
    /// seen.
    Alternate {
        taken: bool,
        fallback: bool,
    },
    /// The branch taken, whose content stands in the alternate's place.
    Branch,
}

const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// A walk over a part `xml_guard::scan` has passed, element by element, with the path of open
/// elements from the root.
struct Walk<'x> {
    reader: NsReader<&'x [u8]>,
    names: &'static [&'static str],
    stack: Vec<Open>,
    frames: Vec<Frame>,
    events: u64,
    /// The element just reported was empty: its end comes next.
    close_next: bool,
    /// From this depth of the path down, markup compatibility applies as Excel applies it: of an
    /// `mc:AlternateContent` the first `mc:Choice` whose `Requires` names only namespaces the
    /// reader understands is read in its place, else the `mc:Fallback`. A choice the reader
    /// cannot take, read before the branch taken, may be Excel's: it is skipped and reported
    /// `Undecided`. So is an element of another namespace that holds SpreadsheetML elements,
    /// which `mc:ProcessContent` can tell Excel to read. Above this depth, those are elements
    /// like any other.
    mc_from: Option<usize>,
}

impl<'x> Walk<'x> {
    fn new(xml: &'x [u8], names: &'static [&'static str]) -> Walk<'x> {
        Walk {
            reader: xml_guard::reader(xml),
            names,
            stack: Vec::new(),
            frames: Vec::new(),
            events: 0,
            close_next: false,
            mc_from: None,
        }
    }

    fn next(&mut self) -> Result<Node<'x>, DocError> {
        if self.close_next {
            self.close_next = false;
            return self.close();
        }
        loop {
            let (resolved, event) = self.event()?;
            let (sml, strict, mc) = match &resolved {
                ResolveResult::Bound(ns) => (ns.0 == SML, ns.0.starts_with(STRICT), ns.0 == MC),
                _ => (false, false, false),
            };
            let compatible = self.mc_from.is_some_and(|depth| self.stack.len() >= depth);
            let empty = matches!(event, Event::Empty(_));
            match event {
                Event::Start(start) | Event::Empty(start) => {
                    if compatible && let Some(node) = self.compatibility(&start, mc, sml, empty)? {
                        match node {
                            Some(node) => return Ok(node),
                            None => continue,
                        }
                    }
                    return Ok(self.open(start, sml, strict, empty));
                }
                Event::End(_) => match self.frames.last() {
                    Some(Frame::Reported) => return self.close(),
                    Some(_) => {
                        self.frames.pop();
                    }
                    None => return Err(DocError::Malformed("xml_structure")),
                },
                // Character data directly inside `mc:AlternateContent` is not content.
                Event::Text(text) => {
                    if !matches!(self.frames.last(), Some(Frame::Alternate { .. })) {
                        return Ok(Node::Text(text.xml10_content()));
                    }
                }
                Event::CData(data) => {
                    if !matches!(self.frames.last(), Some(Frame::Alternate { .. })) {
                        return Ok(Node::Text(data.xml10_content()));
                    }
                }
                Event::GeneralRef(reference) => {
                    let c = xml_guard::resolve_ref(&reference)?;
                    if !matches!(self.frames.last(), Some(Frame::Alternate { .. })) {
                        return Ok(Node::Text(Cow::Owned(c.to_string())));
                    }
                }
                Event::Eof => return Ok(Node::Eof),
                Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
            }
        }
    }

    /// The next event, with a deadline check every 1,024.
    fn event(&mut self) -> Result<(ResolveResult<'_>, Event<'x>), DocError> {
        self.events += 1;
        if self.events.is_multiple_of(1024) {
            check("walk")?;
        }
        self.reader
            .read_resolved_event()
            .map_err(xml_guard::map_error)
    }

    /// Markup compatibility at an element that starts where it applies: `None` when the element
    /// is an ordinary one to report; else what to report instead, if anything.
    #[allow(clippy::option_option)]
    fn compatibility(
        &mut self,
        start: &BytesStart,
        mc: bool,
        sml: bool,
        empty: bool,
    ) -> Result<Option<Option<Node<'x>>>, DocError> {
        let in_alternate = matches!(self.frames.last(), Some(Frame::Alternate { .. }));
        if mc {
            let local = start.local_name();
            match (local.as_ref(), in_alternate) {
                ("AlternateContent", false) => {
                    if !empty {
                        self.frames.push(Frame::Alternate {
                            taken: false,
                            fallback: false,
                        });
                    }
                    return Ok(Some(None));
                }
                ("Choice", true) => {
                    let understood = self.requires_understood(start)?;
                    let Some(Frame::Alternate { taken, fallback }) = self.frames.last_mut() else {
                        return Err(DocError::Malformed("xml_structure"));
                    };
                    if *fallback {
                        return Err(DocError::Malformed("xlsx_structure"));
                    }
                    if *taken {
                        self.skip(empty)?;
                        return Ok(Some(None));
                    }
                    if understood {
                        *taken = true;
                        if !empty {
                            self.frames.push(Frame::Branch);
                        }
                        return Ok(Some(None));
                    }
                    // Excel may take this choice: what it holds is undecided.
                    let (held, any) = self.skip(empty)?;
                    return Ok(Some(any.then_some(Node::Undecided { held, any })));
                }
                ("Fallback", true) => {
                    let Some(Frame::Alternate { taken, fallback }) = self.frames.last_mut() else {
                        return Err(DocError::Malformed("xml_structure"));
                    };
                    if *fallback {
                        return Err(DocError::Malformed("xlsx_structure"));
                    }
                    *fallback = true;
                    if *taken {
                        self.skip(empty)?;
                    } else {
                        *taken = true;
                        if !empty {
                            self.frames.push(Frame::Branch);
                        }
                    }
                    return Ok(Some(None));
                }
                _ => {}
            }
        }
        if mc || !sml || in_alternate {
            // Markup Excel does not read here, or an element of another namespace: what it holds
            // is undecided when it holds SpreadsheetML's markup.
            let own = sml.then(|| self.known(start.local_name().as_ref()));
            let (mut held, _) = self.skip(empty)?;
            if let Some(own) = own
                && !held.contains(&own)
            {
                held.push(own);
            }
            let node = (!held.is_empty()).then_some(Node::Undecided { held, any: true });
            return Ok(Some(node));
        }
        Ok(None)
    }

    /// Whether every prefix a `mc:Choice`'s `Requires` names is bound to a namespace the reader
    /// understands: SpreadsheetML's, or markup compatibility's own.
    fn requires_understood(&self, start: &BytesStart) -> Result<bool, DocError> {
        let [requires] = attributes(&self.reader, start, [(None, "Requires")], None)?;
        let Some(requires) = requires else {
            return Ok(false);
        };
        let mut prefixes = requires.split_ascii_whitespace().peekable();
        if prefixes.peek().is_none() {
            return Ok(false);
        }
        Ok(prefixes.all(|prefix| {
            let name = format!("{prefix}:x");
            !prefix.contains(':')
                && matches!(
                    self.reader.resolver().resolve_element(QName(&name)).0,
                    ResolveResult::Bound(ns) if ns.0 == SML || ns.0 == MC
                )
        }))
    }

    /// Reads past the element that just started, unless it was empty: the names of the
    /// SpreadsheetML elements in it, and whether it held any element or text.
    fn skip(&mut self, empty: bool) -> Result<(Vec<&'static str>, bool), DocError> {
        let (mut held, mut any) = (Vec::new(), false);
        if empty {
            return Ok((held, any));
        }
        let mut depth = 1u32;
        while depth > 0 {
            let (resolved, event) = self.event()?;
            let sml = matches!(&resolved, ResolveResult::Bound(ns) if ns.0 == SML);
            let opens = matches!(event, Event::Start(_));
            match event {
                Event::Start(start) | Event::Empty(start) => {
                    any = true;
                    if sml {
                        let name = self.known(start.local_name().as_ref());
                        if !held.contains(&name) {
                            held.push(name);
                        }
                    }
                    if opens {
                        depth += 1;
                    }
                }
                Event::End(_) => depth -= 1,
                Event::Text(text) => {
                    any |= !text.xml10_content().trim().is_empty();
                }
                Event::CData(_) | Event::GeneralRef(_) => any = true,
                Event::Eof => return Err(DocError::Malformed("xml_structure")),
                Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
            }
        }
        Ok((held, any))
    }

    /// A SpreadsheetML element's local name, when the part's reader knows it (else `""`).
    fn known(&self, local: &str) -> &'static str {
        self.names
            .iter()
            .copied()
            .find(|name| *name == local)
            .unwrap_or("")
    }

    fn open(&mut self, start: BytesStart<'x>, sml: bool, strict: bool, empty: bool) -> Node<'x> {
        let name = if sml {
            self.known(start.local_name().as_ref())
        } else {
            ""
        };
        let open = Open { sml, name };
        self.stack.push(open);
        self.frames.push(Frame::Reported);
        self.close_next = empty;
        Node::Start {
            start,
            open,
            strict,
        }
    }

    /// Ends the innermost reported element.
    fn close(&mut self) -> Result<Node<'x>, DocError> {
        self.frames.pop();
        let open = self
            .stack
            .pop()
            .ok_or(DocError::Malformed("xml_structure"))?;
        Ok(Node::End(open))
    }

    /// Whether the open elements, from the root, are SpreadsheetML's `path`.
    fn at(&self, path: &[&str]) -> bool {
        self.stack.len() == path.len()
            && self
                .stack
                .iter()
                .zip(path)
                .all(|(open, name)| open.sml && open.name == *name)
    }

    /// The element the last reported one stands in.
    fn parent(&self) -> Option<Open> {
        self.stack.len().checked_sub(2).map(|at| self.stack[at])
    }

    /// Unprefixed attributes of a SpreadsheetML element; one in SpreadsheetML's own namespace
    /// is refused, since the schema's attributes are unqualified and a reader that took
    /// `x:hidden` for `hidden` would read another workbook.
    fn attrs<const N: usize>(
        &self,
        start: &BytesStart,
        names: [&str; N],
    ) -> Result<[Option<String>; N], DocError> {
        attributes(
            &self.reader,
            start,
            names.map(|name| (None, name)),
            Some(SML),
        )
    }
}

/// Text that `_xHHHH_` escapes may hold (ECMA-376 Part 1 §22.9.2.19, ST_Xstring): each is the
/// UTF-16 code unit HHHH, two in a row may form a surrogate pair, and `_x005F_` is an underscore,
/// so `_x005F_x0041_` stays `_x0041_`. A lone surrogate, and U+0000 (which no stored text may
/// hold), become U+FFFD.
fn decode_xstring(text: &str) -> Cow<'_, str> {
    if !text.contains("_x") {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let unit_at = |at: usize| -> Option<u16> {
        let escape = bytes.get(at..at + 7)?;
        (escape[0] == b'_'
            && escape[1] == b'x'
            && escape[6] == b'_'
            && escape[2..6].iter().all(u8::is_ascii_hexdigit))
        .then(|| u16::from_str_radix(&text[at + 2..at + 6], 16).ok())
        .flatten()
    };
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        let Some(unit) = unit_at(at) else {
            let c = text[at..]
                .chars()
                .next()
                .expect("a character at a boundary");
            out.push(c);
            at += c.len_utf8();
            continue;
        };
        at += 7;
        let decoded = match unit {
            0xD800..=0xDBFF => match unit_at(at).filter(|low| (0xDC00..=0xDFFF).contains(low)) {
                Some(low) => {
                    at += 7;
                    char::decode_utf16([unit, low]).next().and_then(Result::ok)
                }
                None => None,
            },
            0 => None,
            unit => char::from_u32(u32::from(unit)),
        };
        out.push(decoded.unwrap_or('\u{FFFD}'));
    }
    Cow::Owned(out)
}

/// An `xsd:boolean`: `1` or `true`, `0` or `false`; `None` for anything else.
fn boolean(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// An `xsd:unsignedInt`, white space around it read past.
fn unsigned(value: &str) -> Option<u32> {
    let value = value.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

/// A cell reference, `B3`, as (row, column) from 1, within Excel's grid.
fn cell_ref(text: &str) -> Option<(u32, u32)> {
    let letters = text.bytes().take_while(u8::is_ascii_uppercase).count();
    let digits = &text[letters..];
    if !(1..=3).contains(&letters)
        || digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let col = text
        .bytes()
        .take(letters)
        .fold(0u32, |col, letter| col * 26 + u32::from(letter - b'A') + 1);
    let row: u32 = digits.parse().ok()?;
    ((1..=MAX_ROW).contains(&row) && (1..=MAX_COLUMN).contains(&col)).then_some((row, col))
}

// ---------------------------------------------------------------------------------------------
// The workbook part.
// ---------------------------------------------------------------------------------------------

struct SheetEntry {
    name: String,
    id: u32,
    hidden: bool,
    rel_id: String,
}

struct Workbook {
    date1904: bool,
    full_calc_on_load: bool,
    sheets: Vec<SheetEntry>,
}

const WORKBOOK_NAMES: &[&str] = &["workbook", "workbookPr", "calcPr", "sheets", "sheet"];

impl Workbook {
    /// `workbookPr`, `calcPr` and the sheets, each read only where the schema puts it: an
    /// extension's `workbookPr` is another element and sets nothing.
    fn read(xml: &[u8]) -> Result<Workbook, DocError> {
        let mut walk = Walk::new(xml, WORKBOOK_NAMES);
        let mut workbook = Workbook {
            date1904: false,
            full_calc_on_load: false,
            sheets: Vec::new(),
        };
        let (mut seen, mut ids, mut names) = (HashSet::new(), HashSet::new(), HashSet::new());
        let mut rel_ids = HashSet::new();
        let bad = DocError::Malformed("xlsx_workbook");
        loop {
            let (start, open, strict) = match walk.next()? {
                Node::Start {
                    start,
                    open,
                    strict,
                } => (start, open, strict),
                Node::Eof => break,
                Node::End(_) | Node::Text(_) | Node::Undecided { .. } => continue,
            };
            if walk.stack.len() == 1 {
                if strict {
                    return Err(DocError::Unsupported("strict_ooxml"));
                }
                if open
                    != (Open {
                        sml: true,
                        name: "workbook",
                    })
                {
                    return Err(DocError::Malformed("xlsx_main_part"));
                }
                continue;
            }
            if !open.sml || open.name.is_empty() {
                continue;
            }
            let placed = match open.name {
                "workbookPr" => walk.at(&["workbook", "workbookPr"]),
                "calcPr" => walk.at(&["workbook", "calcPr"]),
                "sheets" => walk.at(&["workbook", "sheets"]),
                "sheet" => walk.at(&["workbook", "sheets", "sheet"]),
                _ => false,
            };
            if !placed {
                return Err(DocError::Malformed("xlsx_structure"));
            }
            if open.name != "sheet" && !seen.insert(open.name) {
                return Err(bad);
            }
            match open.name {
                "workbookPr" => {
                    let [date1904] = walk.attrs(&start, ["date1904"])?;
                    workbook.date1904 = match date1904 {
                        Some(value) => boolean(&value).ok_or(bad)?,
                        None => false,
                    };
                }
                "calcPr" => {
                    let [full] = walk.attrs(&start, ["fullCalcOnLoad"])?;
                    workbook.full_calc_on_load = match full {
                        Some(value) => boolean(&value).ok_or(bad)?,
                        None => false,
                    };
                }
                "sheet" => {
                    let [name, id, state] = walk.attrs(&start, ["name", "sheetId", "state"])?;
                    let [rel_id] = attributes(
                        &walk.reader,
                        &start,
                        [(Some(RELATIONSHIP_ID), "id")],
                        Some(SML),
                    )?;
                    let name = decode_xstring(&name.unwrap_or_default()).into_owned();
                    let length = name.chars().count();
                    if length == 0 || length > MAX_SHEET_NAME_CHARS {
                        return Err(DocError::Malformed("xlsx_sheet_name"));
                    }
                    let id = id
                        .as_deref()
                        .and_then(unsigned)
                        .ok_or(DocError::Malformed("xlsx_sheet_identity"))?;
                    let hidden = match state.as_deref() {
                        None | Some("visible") => false,
                        Some("hidden" | "veryHidden") => true,
                        Some(_) => return Err(DocError::Malformed("xlsx_sheet_state")),
                    };
                    let rel_id = rel_id.ok_or(DocError::Malformed("xlsx_sheet_part"))?;
                    // Excel compares sheet names whatever their case.
                    if !ids.insert(id)
                        || !names.insert(name.to_lowercase())
                        || !rel_ids.insert(rel_id.clone())
                    {
                        return Err(DocError::Malformed("xlsx_sheet_identity"));
                    }
                    workbook.sheets.push(SheetEntry {
                        name,
                        id,
                        hidden,
                        rel_id,
                    });
                }
                _ => {}
            }
        }
        Ok(workbook)
    }
}

enum SheetKind {
    Worksheet(String),
    /// A chart sheet, a dialog sheet or a macro sheet: no cells this reader reads.
    NoCells,
}

struct Sheet {
    name: String,
    id: u32,
    hidden: bool,
    kind: SheetKind,
}

// ---------------------------------------------------------------------------------------------
// Styles.
// ---------------------------------------------------------------------------------------------

/// An attribute: not written, written but not readable, or its value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Attr<T> {
    #[default]
    Absent,
    Bad,
    Value(T),
}

impl<T> Attr<T> {
    fn read(value: Option<String>, parse: impl FnOnce(&str) -> Option<T>) -> Attr<T> {
        match value {
            None => Attr::Absent,
            Some(value) => parse(&value).map_or(Attr::Bad, Attr::Value),
        }
    }
}

/// A `color`, `fgColor` or `bgColor` element's attributes.
#[derive(Debug, Clone, Default)]
struct ColourSpec {
    theme: Attr<u32>,
    rgb: Attr<Rgb>,
    indexed: Attr<u32>,
    auto: Attr<bool>,
    tint: Attr<f64>,
}

impl ColourSpec {
    fn read(walk: &Walk, start: &BytesStart) -> Result<ColourSpec, DocError> {
        let [theme, rgb, indexed, auto, tint] =
            walk.attrs(start, ["theme", "rgb", "indexed", "auto", "tint"])?;
        Ok(ColourSpec {
            theme: Attr::read(theme, unsigned),
            rgb: Attr::read(rgb, colour::parse_hex),
            indexed: Attr::read(indexed, unsigned),
            auto: Attr::read(auto, boolean),
            tint: Attr::read(tint, |value| {
                numfmt::number(value).filter(|tint| (-1.0..=1.0).contains(tint))
            }),
        })
    }
}

/// A font size: points, or written but not readable (a size that hides).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Size {
    Points(f64),
    Bad,
}

impl Size {
    fn read(value: Option<String>) -> Size {
        value
            .as_deref()
            .and_then(numfmt::number)
            .map_or(Size::Bad, Size::Points)
    }

    /// The smaller of two sizes written for one font.
    fn and(self, other: Size) -> Size {
        match (self, other) {
            (Size::Points(a), Size::Points(b)) => Size::Points(a.min(b)),
            _ => Size::Bad,
        }
    }
}

/// A font record, or a rich-text run's properties: the sizes and colours it writes. Written
/// twice, a size counts at its smaller and each colour is a candidate.
#[derive(Debug, Clone, Default)]
struct Font {
    size: Option<Size>,
    colours: Vec<ColourSpec>,
    /// Markup compatibility left what Excel reads for it undecided.
    undecided: bool,
}

impl Font {
    fn add_size(&mut self, size: Size) {
        self.size = Some(self.size.map_or(size, |written| written.and(size)));
    }

    /// A colour it writes, each a candidate; past `MAX_CANDIDATES` (Excel writes one) its colour
    /// is undecided.
    fn add_colour(&mut self, spec: ColourSpec) {
        if self.colours.len() >= MAX_CANDIDATES {
            self.undecided = true;
        } else {
            self.colours.push(spec);
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PatternFill {
    pattern: Option<String>,
    fg: Vec<ColourSpec>,
    bg: Vec<ColourSpec>,
}

#[derive(Debug, Clone, Default)]
struct Fill {
    patterns: Vec<PatternFill>,
    gradients: Vec<Vec<ColourSpec>>,
    undecided: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Xf {
    num_fmt: u32,
    font: u32,
    fill: u32,
    undecided: bool,
}

#[derive(Default)]
struct Styles {
    formats: HashMap<u32, String>,
    fonts: Vec<Font>,
    fills: Vec<Fill>,
    xfs: Vec<Xf>,
    /// `colors/indexedColors`, which replaces the standard palette; an entry that does not
    /// parse is `None`.
    palette: Option<Vec<Option<Rgb>>>,
    /// Markup compatibility left Excel's records undecided: every number format (a choice may
    /// define any id); the fonts, fills and cell formats from these indexes on (Excel's indexes
    /// may differ from the reader's from there); the palette.
    formats_undecided: bool,
    fonts_from: Option<usize>,
    fills_from: Option<usize>,
    xfs_from: Option<usize>,
    palette_undecided: bool,
}

const STYLE_NAMES: &[&str] = &[
    "styleSheet",
    "numFmts",
    "numFmt",
    "fonts",
    "font",
    "sz",
    "color",
    "fills",
    "fill",
    "patternFill",
    "fgColor",
    "bgColor",
    "gradientFill",
    "stop",
    "cellXfs",
    "xf",
    "colors",
    "indexedColors",
    "rgbColor",
];

impl Styles {
    /// The records cells use, each only where the schema puts it: a `numFmt`, `font` or `fill`
    /// of a conditional format's `dxf` is not a cell's. A cell format's `apply*` flags are not
    /// read: Excel draws a cell with its cell format's records whatever they say.
    fn read(xml: &[u8]) -> Result<Styles, DocError> {
        const ROOT: &str = "styleSheet";
        let mut walk = Walk::new(xml, STYLE_NAMES);
        walk.mc_from = Some(1);
        let mut styles = Styles::default();
        let mut seen = HashSet::new();
        let bad = DocError::Malformed("xlsx_styles");
        loop {
            let (start, open, strict) = match walk.next()? {
                Node::Start {
                    start,
                    open,
                    strict,
                } => (start, open, strict),
                Node::Undecided { held, .. } => {
                    let path: Vec<&str> = walk.stack.iter().map(|open| open.name).collect();
                    styles.undecided(&path, &held);
                    continue;
                }
                Node::Eof => break,
                Node::End(_) | Node::Text(_) => continue,
            };
            if walk.stack.len() == 1 {
                if strict {
                    return Err(DocError::Unsupported("strict_ooxml"));
                }
                if open
                    != (Open {
                        sml: true,
                        name: ROOT,
                    })
                {
                    return Err(bad);
                }
                continue;
            }
            if !open.sml || open.name.is_empty() {
                continue;
            }
            if walk.stack.len() == 2 && !seen.insert(open.name) {
                // `numFmts`, `fonts`, `fills`, `cellXfs` and `colors` once each.
                return Err(bad);
            }
            if walk.at(&[ROOT, "numFmts", "numFmt"]) {
                let [id, code] = walk.attrs(&start, ["numFmtId", "formatCode"])?;
                let id = id.as_deref().and_then(unsigned).ok_or(bad)?;
                let code = code.ok_or(bad)?;
                if code.len() > MAX_FORMAT_CHARS * ESCAPED_BYTES {
                    return Err(DocError::TooComplex("number_format"));
                }
                let code = decode_xstring(&code).into_owned();
                if code.chars().count() > MAX_FORMAT_CHARS {
                    return Err(DocError::TooComplex("number_format"));
                }
                if styles.formats.insert(id, code).is_some() {
                    return Err(bad);
                }
            } else if walk.at(&[ROOT, "fonts", "font"]) {
                styles.fonts.push(Font::default());
            } else if walk.at(&[ROOT, "fonts", "font", "sz"]) {
                let [value] = walk.attrs(&start, ["val"])?;
                if let Some(font) = styles.fonts.last_mut() {
                    font.add_size(Size::read(value));
                }
            } else if walk.at(&[ROOT, "fonts", "font", "color"]) {
                let spec = ColourSpec::read(&walk, &start)?;
                if let Some(font) = styles.fonts.last_mut() {
                    font.add_colour(spec);
                }
            } else if walk.at(&[ROOT, "fills", "fill"]) {
                styles.fills.push(Fill::default());
            } else if walk.at(&[ROOT, "fills", "fill", "patternFill"]) {
                let [pattern] = walk.attrs(&start, ["patternType"])?;
                if let Some(fill) = styles.fills.last_mut() {
                    if fill.patterns.len() >= MAX_CANDIDATES {
                        fill.undecided = true;
                    } else {
                        fill.patterns.push(PatternFill {
                            pattern,
                            ..PatternFill::default()
                        });
                    }
                }
            } else if walk.at(&[ROOT, "fills", "fill", "patternFill", "fgColor"])
                || walk.at(&[ROOT, "fills", "fill", "patternFill", "bgColor"])
            {
                let spec = ColourSpec::read(&walk, &start)?;
                if let Some(fill) = styles.fills.last_mut()
                    && let Some(pattern) = fill.patterns.last_mut()
                {
                    if pattern.fg.len() + pattern.bg.len() >= MAX_CANDIDATES {
                        fill.undecided = true;
                    } else if open.name == "fgColor" {
                        pattern.fg.push(spec);
                    } else {
                        pattern.bg.push(spec);
                    }
                }
            } else if walk.at(&[ROOT, "fills", "fill", "gradientFill"]) {
                if let Some(fill) = styles.fills.last_mut() {
                    if fill.gradients.len() >= MAX_CANDIDATES {
                        fill.undecided = true;
                    } else {
                        fill.gradients.push(Vec::new());
                    }
                }
            } else if walk.at(&[ROOT, "fills", "fill", "gradientFill", "stop", "color"]) {
                let spec = ColourSpec::read(&walk, &start)?;
                if let Some(fill) = styles.fills.last_mut()
                    && let Some(gradient) = fill.gradients.last_mut()
                {
                    if gradient.len() >= MAX_CANDIDATES {
                        fill.undecided = true;
                    } else {
                        gradient.push(spec);
                    }
                }
            } else if walk.at(&[ROOT, "cellXfs", "xf"]) {
                let [num_fmt, font, fill] = walk.attrs(&start, ["numFmtId", "fontId", "fillId"])?;
                let index = |value: Option<String>| match value {
                    None => Ok(0),
                    Some(value) => unsigned(&value).ok_or(bad),
                };
                styles.xfs.push(Xf {
                    num_fmt: index(num_fmt)?,
                    font: index(font)?,
                    fill: index(fill)?,
                    undecided: false,
                });
            } else if walk.at(&[ROOT, "colors", "indexedColors"]) {
                if styles.palette.is_some() {
                    return Err(bad);
                }
                styles.palette = Some(Vec::new());
            } else if walk.at(&[ROOT, "colors", "indexedColors", "rgbColor"]) {
                let [rgb] = walk.attrs(&start, ["rgb"])?;
                if let Some(palette) = &mut styles.palette {
                    palette.push(rgb.as_deref().and_then(colour::parse_hex));
                }
            }
        }
        Ok(styles)
    }

    /// Content markup compatibility left undecided, at `path` (the open elements' names), held
    /// these SpreadsheetML elements: the records it may have changed are undecided.
    fn undecided(&mut self, path: &[&str], held: &[&str]) {
        let holds = |name: &str| held.contains(&name);
        match path {
            ["styleSheet"] => {
                self.formats_undecided |= holds("numFmts");
                if holds("fonts") {
                    self.fonts_from = Some(0);
                }
                if holds("fills") {
                    self.fills_from = Some(0);
                }
                if holds("cellXfs") {
                    self.xfs_from = Some(0);
                }
                self.palette_undecided |= holds("colors");
            }
            ["styleSheet", "numFmts", ..] => self.formats_undecided |= !held.is_empty(),
            ["styleSheet", "fonts"] if holds("font") => {
                self.fonts_from.get_or_insert(self.fonts.len());
            }
            ["styleSheet", "fonts", "font", ..] if !held.is_empty() => {
                if let Some(font) = self.fonts.last_mut() {
                    font.undecided = true;
                }
            }
            ["styleSheet", "fills"] if holds("fill") => {
                self.fills_from.get_or_insert(self.fills.len());
            }
            ["styleSheet", "fills", "fill", ..] if !held.is_empty() => {
                if let Some(fill) = self.fills.last_mut() {
                    fill.undecided = true;
                }
            }
            ["styleSheet", "cellXfs"] if holds("xf") => {
                self.xfs_from.get_or_insert(self.xfs.len());
            }
            ["styleSheet", "colors", ..] => {
                self.palette_undecided |= holds("indexedColors") || holds("rgbColor");
            }
            _ => {}
        }
    }

    /// The size of a font that states none: the default font's (font 0), else Excel's 11 pt.
    fn default_pt(&self) -> f64 {
        match self.fonts.first().and_then(|font| font.size) {
            Some(Size::Points(points)) => points,
            Some(Size::Bad) => 0.0,
            None => DEFAULT_FONT_PT,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared and inline strings.
// ---------------------------------------------------------------------------------------------

/// A string of runs (`si` or `is`): its plain text first when it has one, then its runs, in
/// document order; its phonetic hints' text is never part of it.
#[derive(Debug, Clone, Default)]
struct Rst {
    runs: Vec<Run>,
    /// It holds phonetic text (`rPh`), which Excel shows only as a hint, if at all.
    phonetic: bool,
    /// Markup compatibility left its text undecided: Excel may show other text.
    undecided: bool,
}

impl Rst {
    fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }
}

#[derive(Debug, Clone)]
struct Run {
    text: String,
    /// The run's own properties; `None` for plain text or a run with no `rPr`, which takes the
    /// cell's font.
    props: Option<Font>,
    /// Markup compatibility left its text or properties undecided.
    undecided: bool,
}

/// Builds one `si` or `is` from the walk's events below it.
#[derive(Default)]
struct RstBuilder {
    rst: Rst,
    chars: usize,
    /// The run being read.
    run: Option<RunState>,
    /// The `t` being read and its raw text.
    text: Option<String>,
    plain_seen: bool,
    in_phonetic: bool,
}

/// A run being read: its properties, whether its text was seen, and whether markup
/// compatibility left it undecided.
#[derive(Default)]
struct RunState {
    props: Option<Font>,
    seen: bool,
    undecided: bool,
}

impl RstBuilder {
    /// Handles an element opened at `rel`, the path below the `si` or `is`.
    fn open(&mut self, walk: &Walk, start: &BytesStart, rel: &[Open]) -> Result<(), DocError> {
        let structure = DocError::Malformed("xlsx_structure");
        let names: Vec<&str> = rel
            .iter()
            .map(|open| if open.sml { open.name } else { "" })
            .collect();
        match names.as_slice() {
            ["t"] => {
                if self.plain_seen {
                    return Err(structure);
                }
                self.plain_seen = true;
                self.text = Some(String::new());
            }
            ["r"] => self.run = Some(RunState::default()),
            ["r", "rPr"] => match &mut self.run {
                Some(RunState {
                    props: props @ None,
                    ..
                }) => *props = Some(Font::default()),
                _ => return Err(structure),
            },
            ["r", "rPr", "sz"] => {
                let [value] = walk.attrs(start, ["val"])?;
                if let Some(RunState {
                    props: Some(props), ..
                }) = &mut self.run
                {
                    props.add_size(Size::read(value));
                }
            }
            ["r", "rPr", "color"] => {
                let spec = ColourSpec::read(walk, start)?;
                if let Some(RunState {
                    props: Some(props), ..
                }) = &mut self.run
                {
                    props.add_colour(spec);
                }
            }
            ["r", "t"] => match &mut self.run {
                Some(RunState {
                    seen: seen @ false, ..
                }) => {
                    *seen = true;
                    self.text = Some(String::new());
                }
                _ => return Err(structure),
            },
            ["rPh"] => self.in_phonetic = true,
            ["rPh", "t"] => self.text = Some(String::new()),
            _ => {}
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), DocError> {
        if let Some(raw) = &mut self.text {
            raw.push_str(text);
            if raw.len() > MAX_CELL_CHARS * ESCAPED_BYTES {
                return Err(DocError::TooComplex("cell_text"));
            }
        }
        Ok(())
    }

    /// Handles the end of the element at `rel`.
    fn close(&mut self, rel: &[Open]) -> Result<(), DocError> {
        let names: Vec<&str> = rel
            .iter()
            .map(|open| if open.sml { open.name } else { "" })
            .collect();
        match names.as_slice() {
            ["t"] | ["r", "t"] => {
                let text = decode_xstring(&self.text.take().unwrap_or_default()).into_owned();
                self.chars += text.chars().count();
                if self.chars > MAX_CELL_CHARS {
                    return Err(DocError::TooComplex("cell_text"));
                }
                let (props, undecided) = match (names.as_slice(), &self.run) {
                    (["r", "t"], Some(run)) => (run.props.clone(), run.undecided),
                    _ => (None, false),
                };
                self.rst.runs.push(Run {
                    text,
                    props,
                    undecided,
                });
            }
            ["r"] => self.run = None,
            ["rPh", "t"] => {
                let text = self.text.take().unwrap_or_default();
                self.rst.phonetic |= !text.is_empty();
            }
            ["rPh"] => self.in_phonetic = false,
            _ => {}
        }
        Ok(())
    }

    /// Markup compatibility left content undecided at `rel`, the path below the `si` or `is`:
    /// in a run, the run is undecided; in a phonetic hint, nothing that is read; anywhere else,
    /// the string's text.
    fn undecided(&mut self, rel: &[Open]) {
        let names: Vec<&str> = rel
            .iter()
            .map(|open| if open.sml { open.name } else { "" })
            .collect();
        match names.as_slice() {
            ["rPh", ..] => {}
            ["r"] | ["r", "rPr", ..] => {
                if let Some(run) = &mut self.run {
                    run.undecided = true;
                    // Its text may already be in.
                    if run.seen
                        && self.text.is_none()
                        && let Some(last) = self.rst.runs.last_mut()
                    {
                        last.undecided = true;
                    }
                }
            }
            _ => self.rst.undecided = true,
        }
    }
}

/// The paths of the markup of a string of runs, below its `si` or `is`.
fn rst_placed(rel: &[Open]) -> bool {
    let names: Vec<&str> = rel.iter().map(|open| open.name).collect();
    matches!(
        names.as_slice(),
        ["t"] | ["r"] | ["r", "rPr"] | ["r", "t"] | ["rPh"] | ["rPh", "t"]
    )
}

const SST_NAMES: &[&str] = &["sst", "si", "t", "r", "rPr", "rPh", "sz", "color"];
/// The cell markup of a string: anywhere but its place it is refused.
const RST_STRUCTURE: &[&str] = &["t", "r", "rPr", "rPh"];

fn read_shared_strings(xml: &[u8]) -> Result<Vec<Rst>, DocError> {
    let mut walk = Walk::new(xml, SST_NAMES);
    walk.mc_from = Some(1);
    let mut strings: Vec<Rst> = Vec::new();
    let mut current: Option<RstBuilder> = None;
    // Markup compatibility left Excel's strings undecided from this index on.
    let mut undecided_from: Option<usize> = None;
    loop {
        match walk.next()? {
            Node::Undecided { any, .. } => {
                if walk.at(&["sst"]) && any {
                    undecided_from.get_or_insert(strings.len());
                } else if walk.stack.len() >= 2
                    && walk.at_prefix(&["sst", "si"])
                    && let Some(builder) = &mut current
                {
                    builder.undecided(&walk.stack[2..]);
                }
            }
            Node::Start {
                start,
                open,
                strict,
            } => {
                if walk.stack.len() == 1 {
                    if strict {
                        return Err(DocError::Unsupported("strict_ooxml"));
                    }
                    if open
                        != (Open {
                            sml: true,
                            name: "sst",
                        })
                    {
                        return Err(DocError::Malformed("xlsx_shared_strings"));
                    }
                    continue;
                }
                // Text nodes may not hold elements.
                if walk
                    .parent()
                    .is_some_and(|parent| parent.sml && parent.name == "t")
                {
                    return Err(DocError::Malformed("xlsx_structure"));
                }
                if open.sml && open.name == "si" {
                    if !walk.at(&["sst", "si"]) {
                        return Err(DocError::Malformed("xlsx_structure"));
                    }
                    current = Some(RstBuilder::default());
                    continue;
                }
                let rel = walk.stack.get(2..).unwrap_or_default();
                let in_si = walk.stack.len() > 2 && walk.at_prefix(&["sst", "si"]);
                if open.sml && RST_STRUCTURE.contains(&open.name) && !(in_si && rst_placed(rel)) {
                    return Err(DocError::Malformed("xlsx_structure"));
                }
                if in_si && let Some(builder) = &mut current {
                    builder.open(&walk, &start, rel)?;
                }
            }
            Node::Text(text) => {
                if let Some(builder) = &mut current {
                    builder.text(&text)?;
                }
            }
            Node::End(open) => {
                if walk.stack.len() == 1 && open.sml && open.name == "si" {
                    if let Some(builder) = current.take() {
                        strings.push(builder.rst);
                    }
                } else if walk.stack.len() >= 2
                    && let Some(builder) = &mut current
                {
                    let mut rel: Vec<Open> = walk.stack.get(2..).unwrap_or_default().to_vec();
                    rel.push(open);
                    builder.close(&rel)?;
                }
            }
            Node::Eof => {
                if let Some(from) = undecided_from {
                    for string in strings.iter_mut().skip(from) {
                        string.undecided = true;
                    }
                }
                return Ok(strings);
            }
        }
    }
}

impl Walk<'_> {
    /// Whether the open elements start, from the root, with SpreadsheetML's `path`.
    fn at_prefix(&self, path: &[&str]) -> bool {
        self.stack.len() >= path.len()
            && self
                .stack
                .iter()
                .zip(path)
                .all(|(open, name)| open.sml && open.name == *name)
    }
}

// ---------------------------------------------------------------------------------------------
// Worksheets.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Value {
    Number(String),
    Shared(u32),
    Inline(Rst),
    Str(String),
    Bool(bool),
    Error(String),
    Date(String),
}

struct Cell {
    row: u32,
    col: u32,
    /// The cell format its `s` gives (0 when it writes none).
    xf: u32,
    /// It writes no `s`: Excel may draw it in its row's or column's format.
    unstyled: bool,
    /// Its row's format, when the row has `customFormat`.
    row_xf: Option<u32>,
    /// The kind its `t` attribute gives, for a formula with no cached value.
    kind: CellKind,
    row_hidden: bool,
    value: Option<Value>,
    /// The formula's text: the master's for a shared formula's child.
    formula: Option<String>,
    /// The cell holds a formula, whatever its text.
    has_formula: bool,
}

#[derive(Debug, Clone, Copy)]
struct Merge {
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
}

struct SheetData {
    cells: Vec<Cell>,
    columns: Vec<Column>,
    merges: Vec<Merge>,
}

const SHEET_NAMES: &[&str] = &[
    "worksheet",
    "sheetData",
    "row",
    "c",
    "f",
    "v",
    "is",
    "t",
    "r",
    "rPr",
    "rPh",
    "sz",
    "color",
    "cols",
    "col",
    "mergeCells",
    "mergeCell",
];

/// Where SpreadsheetML's cell markup may stand in a worksheet; anywhere else it is refused.
fn sheet_placed(walk: &Walk, name: &str) -> bool {
    const ROW: [&str; 3] = ["worksheet", "sheetData", "row"];
    const CELL: [&str; 4] = ["worksheet", "sheetData", "row", "c"];
    match name {
        "worksheet" => walk.stack.len() == 1,
        "sheetData" | "cols" | "mergeCells" => walk.at(&["worksheet", name]),
        "row" => walk.at(&ROW),
        "c" => walk.at(&CELL),
        "f" | "v" | "is" => walk.at(&["worksheet", "sheetData", "row", "c", name]),
        "col" => walk.at(&["worksheet", "cols", "col"]),
        "mergeCell" => walk.at(&["worksheet", "mergeCells", "mergeCell"]),
        _ => {
            walk.stack.len() > 5
                && walk.at_prefix(&["worksheet", "sheetData", "row", "c", "is"])
                && rst_placed(&walk.stack[5..])
        }
    }
}

/// The cell being read.
struct OpenCell {
    row: u32,
    col: u32,
    xf: u32,
    unstyled: bool,
    row_xf: Option<u32>,
    kind: String,
    value: Option<String>,
    inline: Option<RstBuilder>,
    formula: Option<(String, Option<u32>, bool)>,
    seen_formula: bool,
    seen_value: bool,
}

fn read_sheet(xml: &[u8], strings: &[Rst], limits: &DocLimits) -> Result<SheetData, DocError> {
    let mut walk = Walk::new(xml, SHEET_NAMES);
    let mut data = SheetData {
        cells: Vec::new(),
        columns: Vec::new(),
        merges: Vec::new(),
    };
    let reference = DocError::Malformed("xlsx_cell_reference");
    let structure = DocError::Malformed("xlsx_structure");
    let (mut sheet_data, mut merge_cells) = (false, false);
    // The last row read, and the row being read: its number, hidden, the last column read, and
    // its format when it has `customFormat`.
    let mut last_row = 0u32;
    let mut row: Option<(u32, bool, u32, Option<u32>)> = None;
    let mut cell: Option<OpenCell> = None;
    // Shared formulas' masters, by their `si`.
    let mut masters: HashMap<u32, String> = HashMap::new();
    // The raw text of the `f` or `v` being read.
    let mut raw: Option<String> = None;
    loop {
        match walk.next()? {
            Node::Start {
                start,
                open,
                strict,
            } => {
                if walk.stack.len() == 1 {
                    if strict {
                        return Err(DocError::Unsupported("strict_ooxml"));
                    }
                    if open
                        != (Open {
                            sml: true,
                            name: "worksheet",
                        })
                    {
                        return Err(DocError::Malformed("xlsx_sheet_part"));
                    }
                    continue;
                }
                // Values, formulas and text hold no elements.
                if walk
                    .parent()
                    .is_some_and(|parent| parent.sml && matches!(parent.name, "f" | "v" | "t"))
                {
                    return Err(structure);
                }
                if !open.sml || open.name.is_empty() {
                    continue;
                }
                if ["sz", "color"].contains(&open.name) {
                    // Run properties, read with their run.
                } else if !sheet_placed(&walk, open.name) {
                    return Err(structure);
                }
                match open.name {
                    "sheetData" => {
                        if std::mem::replace(&mut sheet_data, true) {
                            return Err(structure);
                        }
                    }
                    "mergeCells" => {
                        if std::mem::replace(&mut merge_cells, true) {
                            return Err(structure);
                        }
                    }
                    "row" => {
                        let [r, hidden, height, s, custom] =
                            walk.attrs(&start, ["r", "hidden", "ht", "s", "customFormat"])?;
                        let number = match r {
                            Some(r) => unsigned(&r).ok_or(reference)?,
                            None => last_row + 1,
                        };
                        if number <= last_row || !(1..=MAX_ROW).contains(&number) {
                            return Err(reference);
                        }
                        last_row = number;
                        // A hidden flag that does not parse hides it, and so does a height (in
                        // points) under `min_visible_pt`: the row clips any text to less than
                        // tiny text.
                        let min_pt = f64::from(limits.min_visible_pt);
                        let hidden = hidden.is_some_and(|hidden| boolean(&hidden) != Some(false))
                            || height.is_some_and(|height| {
                                numfmt::number(&height).is_none_or(|height| height < min_pt)
                            });
                        // A row's format counts for its cells that write none when it has
                        // `customFormat` (a value that does not parse counts as set).
                        let row_xf = match (s, custom) {
                            (Some(s), Some(custom)) if boolean(&custom) != Some(false) => {
                                Some(unsigned(&s).ok_or(DocError::Malformed("xlsx_style_index"))?)
                            }
                            _ => None,
                        };
                        row = Some((number, hidden, 0, row_xf));
                    }
                    "c" => {
                        let Some((row_number, _, last_col, row_xf)) = row else {
                            return Err(structure);
                        };
                        let [r, s, t] = walk.attrs(&start, ["r", "s", "t"])?;
                        let (cell_row, col) = match r {
                            Some(r) => cell_ref(&r).ok_or(reference)?,
                            None => (row_number, last_col + 1),
                        };
                        if cell_row != row_number || col <= last_col || col > MAX_COLUMN {
                            return Err(reference);
                        }
                        if let Some((_, _, last, _)) = &mut row {
                            *last = col;
                        }
                        let unstyled = s.is_none();
                        let xf = match s {
                            Some(s) => {
                                unsigned(&s).ok_or(DocError::Malformed("xlsx_style_index"))?
                            }
                            None => 0,
                        };
                        let kind = t.unwrap_or_else(|| "n".into());
                        if !matches!(
                            kind.as_str(),
                            "n" | "s" | "str" | "inlineStr" | "b" | "e" | "d"
                        ) {
                            return Err(DocError::Malformed("xlsx_cell_type"));
                        }
                        cell = Some(OpenCell {
                            row: row_number,
                            col,
                            xf,
                            unstyled,
                            row_xf,
                            kind,
                            value: None,
                            inline: None,
                            formula: None,
                            seen_formula: false,
                            seen_value: false,
                        });
                    }
                    "f" => {
                        let open_cell = cell.as_mut().ok_or(structure)?;
                        if std::mem::replace(&mut open_cell.seen_formula, true) {
                            return Err(structure);
                        }
                        let [kind, si] = walk.attrs(&start, ["t", "si"])?;
                        let shared = kind.as_deref() == Some("shared");
                        let si = match (shared, si) {
                            (true, Some(si)) => Some(
                                unsigned(&si).ok_or(DocError::Malformed("xlsx_shared_formula"))?,
                            ),
                            (true, None) => return Err(DocError::Malformed("xlsx_shared_formula")),
                            (false, _) => None,
                        };
                        open_cell.formula = Some((String::new(), si, shared));
                        raw = Some(String::new());
                    }
                    "v" => {
                        let open_cell = cell.as_mut().ok_or(structure)?;
                        if std::mem::replace(&mut open_cell.seen_value, true)
                            || open_cell.kind == "inlineStr"
                        {
                            return Err(structure);
                        }
                        raw = Some(String::new());
                    }
                    "is" => {
                        let open_cell = cell.as_mut().ok_or(structure)?;
                        if open_cell.inline.is_some() || open_cell.kind != "inlineStr" {
                            return Err(structure);
                        }
                        open_cell.inline = Some(RstBuilder::default());
                        walk.mc_from = Some(walk.stack.len());
                    }
                    "col" => {
                        let [min, max, hidden, width, style] =
                            walk.attrs(&start, ["min", "max", "hidden", "width", "style"])?;
                        let bad = DocError::Malformed("xlsx_column");
                        let min = min.as_deref().and_then(unsigned).ok_or(bad)?;
                        let max = max.as_deref().and_then(unsigned).ok_or(bad)?;
                        if min == 0 || min > max || max > MAX_COLUMN {
                            return Err(bad);
                        }
                        // A hidden flag that does not parse hides it, and so does a width under
                        // one character: `width` counts characters of the widest digit with
                        // their padding (ECMA-376 Part 1 §18.3.1.13), so such a column shows no
                        // digit (Excel draws a number there as `#`).
                        let hidden = hidden.is_some_and(|hidden| boolean(&hidden) != Some(false))
                            || width.is_some_and(|width| {
                                numfmt::number(&width).is_none_or(|width| width < MIN_COLUMN_WIDTH)
                            });
                        let style = match style {
                            Some(style) => Some(
                                unsigned(&style).ok_or(DocError::Malformed("xlsx_style_index"))?,
                            ),
                            None => None,
                        };
                        data.columns.push(Column {
                            min,
                            max,
                            hidden,
                            style,
                        });
                    }
                    "mergeCell" => {
                        let [range] = walk.attrs(&start, ["ref"])?;
                        data.merges.push(
                            range
                                .as_deref()
                                .and_then(merge_range)
                                .ok_or(DocError::Malformed("xlsx_merge"))?,
                        );
                    }
                    _ => {
                        // Inline string markup.
                        if let Some(builder) = cell.as_mut().and_then(|cell| cell.inline.as_mut()) {
                            builder.open(&walk, &start, &walk.stack[5..])?;
                        }
                    }
                }
            }
            Node::Text(text) => {
                if let Some(raw) = &mut raw {
                    raw.push_str(&text);
                    if raw.len() > MAX_CELL_CHARS.max(MAX_FORMULA_CHARS) * ESCAPED_BYTES {
                        return Err(DocError::TooComplex("cell_text"));
                    }
                } else if let Some(builder) = cell.as_mut().and_then(|cell| cell.inline.as_mut()) {
                    builder.text(&text)?;
                }
            }
            Node::End(open) => {
                if !open.sml {
                    continue;
                }
                let depth = walk.stack.len();
                match (open.name, depth) {
                    ("row", 2) => row = None,
                    ("f", 4) => {
                        let open_cell = cell.as_mut().ok_or(structure)?;
                        let text = decode_xstring(&raw.take().unwrap_or_default()).into_owned();
                        if text.chars().count() > MAX_FORMULA_CHARS {
                            return Err(DocError::TooComplex("formula"));
                        }
                        if let Some((formula, si, shared)) = &mut open_cell.formula {
                            *formula = match (*shared, *si, text.is_empty()) {
                                (true, Some(si), false) => {
                                    masters.insert(si, text.clone());
                                    text
                                }
                                (true, Some(si), true) => masters
                                    .get(&si)
                                    .cloned()
                                    .ok_or(DocError::Malformed("xlsx_shared_formula"))?,
                                _ => text,
                            };
                        }
                    }
                    ("v", 4) => {
                        let open_cell = cell.as_mut().ok_or(structure)?;
                        open_cell.value = raw.take();
                    }
                    ("is", 4) => walk.mc_from = None,
                    ("c", 3) => {
                        let open_cell = cell.take().ok_or(structure)?;
                        let hidden = row.is_some_and(|(_, hidden, _, _)| hidden);
                        if let Some(cell) = finish_cell(open_cell, hidden, strings)? {
                            data.cells.push(cell);
                        }
                    }
                    _ if depth >= 5 => {
                        if let Some(builder) = cell.as_mut().and_then(|cell| cell.inline.as_mut()) {
                            let mut rel: Vec<Open> = walk.stack[5..].to_vec();
                            rel.push(open);
                            builder.close(&rel)?;
                        }
                    }
                    _ => {}
                }
            }
            Node::Undecided { .. } => {
                if walk.stack.len() >= 5
                    && let Some(builder) = cell.as_mut().and_then(|cell| cell.inline.as_mut())
                {
                    builder.undecided(&walk.stack[5..]);
                }
            }
            Node::Eof => return Ok(data),
        }
    }
}

/// A merged range, `A1:C3` (or one cell), as its corners.
fn merge_range(text: &str) -> Option<Merge> {
    let (first, last) = text.split_once(':').unwrap_or((text, text));
    let ((top, left), (bottom, right)) = (cell_ref(first)?, cell_ref(last)?);
    (top <= bottom && left <= right).then_some(Merge {
        top,
        left,
        bottom,
        right,
    })
}

/// A finished cell's value by its type, checked as Excel stores it; `None` for an empty cell.
fn finish_cell(
    cell: OpenCell,
    row_hidden: bool,
    strings: &[Rst],
) -> Result<Option<Cell>, DocError> {
    // A formula's empty cached value is a value: the cell shows nothing, and its formula's text
    // is not read in its place.
    if cell.formula.is_some()
        && let Some(cached) = &cell.value
    {
        let empty = match cell.kind.as_str() {
            "str" => cached.is_empty(),
            "inlineStr" => false,
            _ => cached
                .trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
                .is_empty(),
        };
        if empty {
            return Ok(None);
        }
    }
    let value = match (cell.kind.as_str(), cell.value, cell.inline) {
        ("inlineStr", _, Some(builder)) => Some(Value::Inline(builder.rst)),
        ("inlineStr", _, None) => None,
        (_, None, _) => None,
        ("n", Some(text), _) => {
            if text.chars().count() > MAX_CELL_CHARS {
                return Err(DocError::TooComplex("cell_text"));
            }
            numfmt::number(&text).ok_or(DocError::Malformed("xlsx_number"))?;
            Some(Value::Number(
                text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
                    .to_owned(),
            ))
        }
        ("s", Some(text), _) => {
            let index = unsigned(&text)
                .filter(|&index| (index as usize) < strings.len())
                .ok_or(DocError::Malformed("xlsx_shared_string"))?;
            Some(Value::Shared(index))
        }
        ("str", Some(text), _) => Some(Value::Str(string(&text)?)),
        ("b", Some(text), _) => Some(Value::Bool(
            match text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n')) {
                "1" => true,
                "0" => false,
                _ => return Err(DocError::Malformed("xlsx_bool")),
            },
        )),
        ("e", Some(text), _) => {
            let text = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
            let error = text.strip_prefix('#').is_some_and(|rest| {
                (1..32).contains(&rest.len())
                    && rest.bytes().all(|b| {
                        b.is_ascii_uppercase() || b.is_ascii_digit() || b"/!?_".contains(&b)
                    })
            });
            if !error {
                return Err(DocError::Malformed("xlsx_error_value"));
            }
            Some(Value::Error(text.to_owned()))
        }
        (_, Some(text), _) => Some(Value::Date(string(&text)?)),
    };
    // A string markup compatibility left undecided is not known to be empty: it keeps a block.
    let value = value.filter(|value| match value {
        Value::Shared(index) => {
            let rst = &strings[*index as usize];
            rst.undecided || !rst.is_empty()
        }
        Value::Inline(rst) => rst.undecided || !rst.is_empty(),
        Value::Str(text) | Value::Date(text) => !text.is_empty(),
        _ => true,
    });
    let has_formula = cell.formula.is_some();
    let formula = cell
        .formula
        .map(|(text, _, _)| text)
        .filter(|text| !text.is_empty());
    if value.is_none() && formula.is_none() {
        return Ok(None);
    }
    let kind = match cell.kind.as_str() {
        "s" | "str" | "inlineStr" => CellKind::Text,
        "b" => CellKind::Bool,
        "e" => CellKind::Error,
        _ => CellKind::Number,
    };
    Ok(Some(Cell {
        row: cell.row,
        col: cell.col,
        xf: cell.xf,
        unstyled: cell.unstyled,
        row_xf: cell.row_xf,
        kind,
        row_hidden,
        value,
        formula,
        has_formula,
    }))
}

/// A `d` value as ISO 8601 writes a date, with or without a time of day and a trailing `Z`:
/// `2024-01-31`, `2024-01-31T12:00`, `2024-01-31T12:00:00.5Z`.
fn iso_date(text: &str) -> Option<NaiveDateTime> {
    let text = text.strip_suffix('Z').unwrap_or(text);
    let (date, time) = match text.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let shape = |text: &str, pattern: &str| {
        text.len() == pattern.len()
            && text.bytes().zip(pattern.bytes()).all(|(byte, expected)| {
                if expected == b'9' {
                    byte.is_ascii_digit()
                } else {
                    byte == expected
                }
            })
    };
    if !shape(date, "9999-99-99") {
        return None;
    }
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let time = match time {
        None => NaiveTime::MIN,
        Some(time) if shape(time, "99:99") => NaiveTime::parse_from_str(time, "%H:%M").ok()?,
        Some(time) => {
            let (whole, fraction) = time.split_once('.').unwrap_or((time, "9"));
            if !shape(whole, "99:99:99")
                || fraction.is_empty()
                || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            NaiveTime::parse_from_str(time, "%H:%M:%S%.f").ok()?
        }
    };
    Some(date.and_time(time))
}

/// A stored string, decoded and held to Excel's 32,767 characters.
fn string(text: &str) -> Result<String, DocError> {
    let text = decode_xstring(text).into_owned();
    if text.chars().count() > MAX_CELL_CHARS {
        return Err(DocError::TooComplex("cell_text"));
    }
    Ok(text)
}

/// Which of a sheet's cells (in row then column order) lie under a merged range other than at
/// its top-left cell, which alone Excel draws. Two ranges that overlap are refused.
fn covered(cells: &[(u32, u32)], merges: &mut [Merge]) -> Result<Vec<bool>, DocError> {
    merges.sort_by_key(|merge| (merge.top, merge.left));
    // The ranges open at a row, by their left column, and when each ends.
    let mut open: BTreeMap<u32, Merge> = BTreeMap::new();
    let mut ends: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let close_before =
        |row: u32, open: &mut BTreeMap<u32, Merge>, ends: &mut BinaryHeap<Reverse<(u32, u32)>>| {
            while let Some(&Reverse((bottom, left))) = ends.peek() {
                if bottom >= row {
                    break;
                }
                ends.pop();
                open.remove(&left);
            }
        };
    for (at, merge) in merges.iter().enumerate() {
        if at % 1024 == 1023 {
            check("merges")?;
        }
        close_before(merge.top, &mut open, &mut ends);
        let before = open.range(..=merge.left).next_back();
        let after = open.range(merge.left..).next();
        if before.is_some_and(|(_, other)| other.right >= merge.left)
            || after.is_some_and(|(&left, _)| left <= merge.right)
        {
            return Err(DocError::Malformed("xlsx_merge"));
        }
        open.insert(merge.left, *merge);
        ends.push(Reverse((merge.bottom, merge.left)));
    }
    open.clear();
    ends.clear();
    let mut next = 0;
    let mut under = Vec::with_capacity(cells.len());
    for (at, &(row, col)) in cells.iter().enumerate() {
        if at % 1024 == 1023 {
            check("merged_cells")?;
        }
        close_before(row, &mut open, &mut ends);
        while let Some(merge) = merges.get(next).filter(|merge| merge.top <= row) {
            next += 1;
            if merge.bottom >= row {
                open.insert(merge.left, *merge);
                ends.push(Reverse((merge.bottom, merge.left)));
            }
        }
        under.push(
            open.range(..=col).next_back().is_some_and(|(_, merge)| {
                col <= merge.right && (row, col) != (merge.top, merge.left)
            }),
        );
    }
    Ok(under)
}

// ---------------------------------------------------------------------------------------------
// Judging cells and making blocks.
// ---------------------------------------------------------------------------------------------

/// The colours a place may be drawn in, or none that can be known (it hides what is drawn on it
/// or in it).
#[derive(Debug, Clone, PartialEq)]
enum Paint {
    Colours(Vec<Rgb>),
    Bad,
}

impl Paint {
    fn union(self, other: Paint) -> Paint {
        match (self, other) {
            (Paint::Colours(mut colours), Paint::Colours(more)) => {
                for colour in more {
                    if !colours.contains(&colour) {
                        colours.push(colour);
                    }
                }
                if colours.len() > MAX_CANDIDATES {
                    Paint::Bad
                } else {
                    Paint::Colours(colours)
                }
            }
            _ => Paint::Bad,
        }
    }
}

/// A font as drawn: its smallest possible size and its possible colours.
#[derive(Debug, Clone)]
struct Look {
    pt: f64,
    paint: Paint,
}

struct Judge<'a> {
    styles: &'a Styles,
    theme: Option<&'a ThemeColours>,
    limits: &'a DocLimits,
    default_pt: f64,
    /// Each font's look and each fill's paint, worked out once.
    looks: RefCell<HashMap<u32, Look>>,
    paints: RefCell<HashMap<u32, Paint>>,
}

/// The share of a pattern fill its foreground covers, for the patterns that are an even mix.
fn coverage(pattern: &str) -> Option<f64> {
    Some(match pattern {
        "gray0625" => 0.0625,
        "gray125" => 0.125,
        "lightGray" => 0.25,
        "mediumGray" => 0.5,
        "darkGray" => 0.75,
        _ => return None,
    })
}

const HATCHES: [&str; 12] = [
    "darkHorizontal",
    "darkVertical",
    "darkDown",
    "darkUp",
    "darkGrid",
    "darkTrellis",
    "lightHorizontal",
    "lightVertical",
    "lightDown",
    "lightUp",
    "lightGrid",
    "lightTrellis",
];

/// `over` laid over `under` at `coverage`, as a fine pattern shows from a distance.
fn blend(over: Rgb, under: Rgb, coverage: f64) -> Rgb {
    let channel = |over: u8, under: u8| {
        (f64::from(over) * coverage + f64::from(under) * (1.0 - coverage)).round() as u8
    };
    Rgb {
        r: channel(over.r, under.r),
        g: channel(over.g, under.g),
        b: channel(over.b, under.b),
    }
}

fn hex(value: u32) -> Rgb {
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

impl Judge<'_> {
    fn xf(&self, index: u32) -> Result<Xf, DocError> {
        if self
            .styles
            .xfs_from
            .is_some_and(|from| index as usize >= from)
        {
            return Ok(Xf {
                undecided: true,
                ..Xf::default()
            });
        }
        match self.styles.xfs.get(index as usize) {
            Some(xf) => Ok(*xf),
            None if index == 0 && self.styles.xfs.is_empty() => Ok(Xf::default()),
            None => Err(DocError::Malformed("xlsx_style_index")),
        }
    }

    /// A font whose record markup compatibility left undecided: its colour cannot be known.
    fn undecided_look(&self) -> Look {
        Look {
            pt: self.default_pt,
            paint: Paint::Bad,
        }
    }

    fn font(&self, index: u32) -> Result<Look, DocError> {
        if let Some(look) = self.looks.borrow().get(&index) {
            return Ok(look.clone());
        }
        let look = self.font_look(index)?;
        self.looks.borrow_mut().insert(index, look.clone());
        Ok(look)
    }

    fn font_look(&self, index: u32) -> Result<Look, DocError> {
        if self
            .styles
            .fonts_from
            .is_some_and(|from| index as usize >= from)
        {
            return Ok(self.undecided_look());
        }
        let font = match self.styles.fonts.get(index as usize) {
            Some(font) => font.clone(),
            None if index == 0 && self.styles.fonts.is_empty() => Font::default(),
            None => return Err(DocError::Malformed("xlsx_style_index")),
        };
        if font.undecided {
            return Ok(self.undecided_look());
        }
        Ok(Look {
            pt: match font.size {
                Some(Size::Points(points)) => points,
                Some(Size::Bad) => 0.0,
                None => self.default_pt,
            },
            paint: self.paint(&font.colours, Rgb::BLACK),
        })
    }

    /// A run's look: its own properties, and where it writes none, the cell's font or the
    /// default's (Excel does not say which a run takes, so it is judged by both).
    fn run(&self, props: Option<&Font>, cell: &Look) -> Look {
        let Some(props) = props else {
            return cell.clone();
        };
        if props.undecided {
            return self.undecided_look();
        }
        Look {
            pt: match props.size {
                Some(Size::Points(points)) => points,
                Some(Size::Bad) => 0.0,
                None => cell.pt.min(self.default_pt),
            },
            paint: if props.colours.is_empty() {
                cell.paint.clone().union(Paint::Colours(vec![Rgb::BLACK]))
            } else {
                self.paint(&props.colours, Rgb::BLACK)
            },
        }
    }

    /// The colours a list of colour elements may be drawn in; none is `automatic`.
    fn paint(&self, specs: &[ColourSpec], automatic: Rgb) -> Paint {
        if specs.is_empty() {
            return Paint::Colours(vec![automatic]);
        }
        specs
            .iter()
            .map(|spec| self.colour(spec, automatic))
            .reduce(Paint::union)
            .unwrap_or(Paint::Bad)
    }

    /// A colour element by Excel's priority, `theme`, then `rgb`, then `indexed`, then `auto`
    /// (`automatic`). A theme colour takes its tint; on an RGB or palette colour a tint gives the
    /// colour with it and without it. Anything Excel would not read is `Bad`.
    fn colour(&self, spec: &ColourSpec, automatic: Rgb) -> Paint {
        let tint = match spec.tint {
            Attr::Absent => 0.0,
            Attr::Value(tint) => tint,
            Attr::Bad => return Paint::Bad,
        };
        let tinted = |colour: Rgb| {
            let mut colours = vec![colour];
            if tint != 0.0 {
                colours.push(colour::apply_tint(colour, tint));
            }
            Paint::Colours(colours)
        };
        match (spec.theme, spec.rgb, spec.indexed, spec.auto) {
            (Attr::Value(index), ..) => {
                let Some(slot) = SchemeSlot::from_xlsx_theme_index(index) else {
                    return Paint::Bad;
                };
                match self.theme {
                    // The theme part's colour in whatever form it writes it; a slot it leaves
                    // undecided is not one Excel's drawing can be known in.
                    Some(theme) => match theme.candidates(slot) {
                        Some(colours) => Paint::Colours(
                            colours
                                .iter()
                                .map(|&colour| colour::apply_tint(colour, tint))
                                .collect(),
                        ),
                        None => Paint::Bad,
                    },
                    // No theme part: the default Office theme's, and the RGB value beside it.
                    None => {
                        let default = ThemeColours::office_default().resolve(slot, tint);
                        let beside = match spec.rgb {
                            Attr::Value(rgb) => vec![colour::apply_tint(rgb, tint)],
                            _ => Vec::new(),
                        };
                        Paint::Colours(vec![default]).union(Paint::Colours(beside))
                    }
                }
            }
            (Attr::Bad, ..) => Paint::Bad,
            (Attr::Absent, Attr::Value(rgb), ..) => tinted(rgb),
            (Attr::Absent, Attr::Bad, ..) => Paint::Bad,
            (Attr::Absent, Attr::Absent, Attr::Value(index), _) => match self.indexed(index) {
                Some(colour) => tinted(colour),
                None => Paint::Bad,
            },
            (Attr::Absent, Attr::Absent, Attr::Bad, _) => Paint::Bad,
            (Attr::Absent, Attr::Absent, Attr::Absent, Attr::Bad) => Paint::Bad,
            (Attr::Absent, Attr::Absent, Attr::Absent, _) => Paint::Colours(vec![automatic]),
        }
    }

    /// A palette colour: the file's palette when it has one, else the standard one; 64 and 65
    /// are the system's foreground and background.
    fn indexed(&self, index: u32) -> Option<Rgb> {
        match index {
            SYSTEM_FOREGROUND => Some(Rgb::BLACK),
            SYSTEM_BACKGROUND => Some(Rgb::WHITE),
            // Past the system colours an index names no colour, however long the palette.
            index if index > SYSTEM_BACKGROUND => None,
            _ if self.styles.palette_undecided => None,
            index => match &self.styles.palette {
                Some(palette) => palette.get(index as usize).copied().flatten(),
                None => PALETTE.get(index as usize).map(|&value| hex(value)),
            },
        }
    }

    /// The colours a cell's fill may show. Fills 0 and 1 are Excel's reserved `none` and
    /// `gray125`, whatever the file writes for them; no fill is white.
    fn fill(&self, index: u32) -> Result<Paint, DocError> {
        if let Some(paint) = self.paints.borrow().get(&index) {
            return Ok(paint.clone());
        }
        let paint = self.fill_paint(index)?;
        self.paints.borrow_mut().insert(index, paint.clone());
        Ok(paint)
    }

    fn fill_paint(&self, index: u32) -> Result<Paint, DocError> {
        let gray125 = blend(Rgb::BLACK, Rgb::WHITE, 0.125);
        match index {
            0 => return Ok(Paint::Colours(vec![Rgb::WHITE])),
            1 => return Ok(Paint::Colours(vec![Rgb::WHITE, gray125])),
            _ => {}
        }
        if self
            .styles
            .fills_from
            .is_some_and(|from| index as usize >= from)
        {
            return Ok(Paint::Bad);
        }
        let fill = self
            .styles
            .fills
            .get(index as usize)
            .ok_or(DocError::Malformed("xlsx_style_index"))?;
        if fill.undecided {
            return Ok(Paint::Bad);
        }
        let mut paint: Option<Paint> = None;
        let mut add = |more: Paint| {
            paint = Some(match paint.take() {
                Some(paint) => paint.union(more),
                None => more,
            })
        };
        for pattern in &fill.patterns {
            add(self.pattern(pattern));
        }
        for stops in &fill.gradients {
            add(if stops.is_empty() {
                Paint::Colours(vec![Rgb::WHITE])
            } else {
                self.paint(stops, Rgb::BLACK)
            });
        }
        Ok(paint.unwrap_or(Paint::Colours(vec![Rgb::WHITE])))
    }

    /// A pattern fill: `solid` shows its foreground; an even pattern its foreground blended over
    /// its background; a hatch both colours; `none` nothing (white). With no pattern written,
    /// what Excel draws is not known: white and both colours. An automatic foreground is black
    /// and an automatic background white, as Excel draws them by default.
    fn pattern(&self, pattern: &PatternFill) -> Paint {
        let fg = || self.paint(&pattern.fg, Rgb::BLACK);
        let bg = || self.paint(&pattern.bg, Rgb::WHITE);
        match pattern.pattern.as_deref() {
            None if pattern.fg.is_empty() && pattern.bg.is_empty() => {
                Paint::Colours(vec![Rgb::WHITE])
            }
            None => Paint::Colours(vec![Rgb::WHITE]).union(fg()).union(bg()),
            Some("none") => Paint::Colours(vec![Rgb::WHITE]),
            Some("solid") => fg(),
            Some(kind) if HATCHES.contains(&kind) => fg().union(bg()),
            Some(kind) => match (coverage(kind), fg(), bg()) {
                (Some(share), Paint::Colours(over), Paint::Colours(under)) => {
                    let mut colours = Vec::new();
                    for &over in &over {
                        for &under in &under {
                            let mixed = blend(over, under, share);
                            if !colours.contains(&mixed) {
                                colours.push(mixed);
                            }
                        }
                    }
                    if colours.len() > MAX_CANDIDATES {
                        Paint::Bad
                    } else {
                        Paint::Colours(colours)
                    }
                }
                _ => Paint::Bad,
            },
        }
    }

    /// Why text in `look` over `fill` cannot be seen, with `base`'s sheet, row, column and
    /// format: each pair of the font's and the fill's colours is judged.
    fn judge(&self, base: &CellLook, look: &Look, fill: &Paint) -> Option<HiddenWhy> {
        let sized = CellLook {
            font_pt: look.pt as f32,
            font: Rgb::BLACK,
            fill: Rgb::WHITE,
            ..*base
        };
        if let Some(why) = hidden_why(&sized, self.limits) {
            return Some(why);
        }
        let (Paint::Colours(fonts), Paint::Colours(fills)) = (&look.paint, fill) else {
            return Some(HiddenWhy::LowContrast);
        };
        fonts.iter().find_map(|&font| {
            fills.iter().find_map(|&fill| {
                hidden_why(
                    &CellLook {
                        font,
                        fill,
                        ..sized
                    },
                    self.limits,
                )
            })
        })
    }

    /// The number format of a cell format: the file's code for its id, else the built-in one;
    /// `None` for `General` and for an id with no code.
    fn format(&self, id: u32) -> Option<&str> {
        self.styles
            .formats
            .get(&id)
            .map(String::as_str)
            .or_else(|| numfmt::builtin(id))
            .filter(|code| !code.eq_ignore_ascii_case("general"))
    }
}

/// Warnings, at most `max_blocks` of them; `ActiveContentIgnored` and `EmbeddedContentIgnored`
/// once per kind.
struct Warnings {
    list: Vec<ExtractWarning>,
    seen: HashSet<&'static str>,
    cap: u32,
}

impl Warnings {
    fn new(cap: u32) -> Warnings {
        Warnings {
            list: Vec::new(),
            seen: HashSet::new(),
            cap,
        }
    }

    fn push(&mut self, warning: ExtractWarning) -> Result<(), DocError> {
        if self.list.len() as u64 >= u64::from(self.cap) {
            return Err(DocError::TooComplex("warnings"));
        }
        self.list.push(warning);
        Ok(())
    }

    fn once(&mut self, warning: ExtractWarning) -> Result<(), DocError> {
        let what = match warning {
            ExtractWarning::ActiveContentIgnored { what }
            | ExtractWarning::EmbeddedContentIgnored { what } => what,
            _ => return self.push(warning),
        };
        if self.seen.insert(what) {
            self.push(warning)?;
        }
        Ok(())
    }
}

fn why_text(why: HiddenWhy) -> &'static str {
    match why {
        HiddenWhy::Tiny => "tiny",
        _ => "low_contrast",
    }
}

struct Output<'l> {
    limits: &'l DocLimits,
    strings: &'l [Rst],
    date1904: bool,
    full_calc_on_load: bool,
    blocks: Vec<Block>,
    warnings: Warnings,
    ordinal: u32,
}

/// A cell judged: its text, facts and hidden reason; for a visible rich-text cell, the reason of
/// its first hidden run; whether its string held phonetic text.
struct Made {
    text: String,
    facts: CellFacts,
    hidden: Option<HiddenWhy>,
    run_why: Option<&'static str>,
    phonetic: bool,
    suspect: Option<&'static str>,
}

impl Output<'_> {
    /// One worksheet's cells as blocks: each cell judged, its hidden runs left out with a
    /// warning, its text split into pieces.
    fn sheet(&mut self, judge: &Judge, sheet: &Sheet, data: &SheetData) -> Result<(), DocError> {
        let columns = columns(data.columns.clone())?;
        let positions: Vec<(u32, u32)> =
            data.cells.iter().map(|cell| (cell.row, cell.col)).collect();
        let mut merges = data.merges.clone();
        let under = covered(&positions, &mut merges)?;
        for (at, cell) in data.cells.iter().enumerate() {
            // A cell's work is bounded (its pieces, its formats, its capped colours): one check
            // each.
            check("cells")?;
            let locator = Locator::XlsxCell {
                sheet_id: sheet.id,
                sheet: sheet.name.clone(),
                row: cell.row,
                col: cell.col,
            };
            let (col_hidden, col_xf) = column(&columns, cell.col)
                .map_or((false, None), |entry| (entry.hidden, entry.style));
            let base = CellLook {
                sheet_hidden: sheet.hidden,
                row_hidden: cell.row_hidden,
                col_hidden,
                empty_format: false,
                font_pt: f32::MAX,
                font: Rgb::BLACK,
                fill: Rgb::WHITE,
            };
            // A cell that writes no format may be drawn in its row's or its column's.
            let mut formats = vec![cell.xf];
            if cell.unstyled {
                for xf in [cell.row_xf, col_xf].into_iter().flatten() {
                    if !formats.contains(&xf) {
                        formats.push(xf);
                    }
                }
            }
            let made = self.cell(judge, cell, &formats, base, under[at])?;
            if made.hidden.is_none() {
                if let Some(why) = made.run_why {
                    self.warnings.push(ExtractWarning::HiddenContent {
                        at: locator.clone(),
                        why,
                    })?;
                }
                if made.phonetic {
                    self.warnings.push(ExtractWarning::HiddenContent {
                        at: locator.clone(),
                        why: "phonetic",
                    })?;
                }
            }
            self.ordinal += 1;
            // A cell whose every run is hidden keeps its block, holding no text.
            let pieces = if made.text.is_empty() {
                std::iter::once(0..0).collect()
            } else {
                split_pieces(&made.text)?
            };
            for (piece, range) in pieces.into_iter().enumerate() {
                if self.blocks.len() as u64 >= u64::from(self.limits.max_blocks) {
                    return Err(DocError::TooComplex("blocks"));
                }
                self.blocks.push(Block {
                    ordinal: self.ordinal,
                    piece: piece as u32,
                    text: replace_nul(&made.text[range]),
                    locator: locator.clone(),
                    lines: Vec::new(),
                    cell: Some(made.facts.clone()),
                    unit_context: None,
                    hidden: made.hidden,
                    suspect: made.suspect,
                });
            }
        }
        Ok(())
    }

    /// Judges one cell drawn in one of `formats` (its own first), with `base`'s sheet, row and
    /// column, and whether a merged range covers it. Its text and facts follow its own format;
    /// it is hidden if any format it may be drawn in hides it.
    fn cell(
        &self,
        judge: &Judge,
        cell: &Cell,
        formats: &[u32],
        base: CellLook,
        covered: bool,
    ) -> Result<Made, DocError> {
        let mut styles = Vec::with_capacity(formats.len());
        for &format in formats {
            let xf = judge.xf(format)?;
            // A cell format markup compatibility left undecided draws in a look not known.
            styles.push(if xf.undecided {
                Drawn {
                    font: judge.undecided_look(),
                    fill: Paint::Bad,
                    format: None,
                    format_undecided: false,
                }
            } else {
                Drawn {
                    font: judge.font(xf.font)?,
                    fill: judge.fill(xf.fill)?,
                    format: judge.format(xf.num_fmt),
                    format_undecided: judge.styles.formats_undecided,
                }
            });
        }
        let number_format = styles.first().and_then(|style| style.format);
        let rst = match &cell.value {
            Some(Value::Shared(index)) => Some(&self.strings[*index as usize]),
            Some(Value::Inline(rst)) => Some(rst),
            _ => None,
        };
        // A string of runs: the text of its visible runs, the first hidden run's reason, and
        // whether every run with text is hidden. A run is judged by its size and colour against
        // the cell's fill, never by the sheet, row, column or format, which judge the cell.
        let runs = match rst {
            Some(rst) => Some(self.runs(judge, rst, &styles)?),
            None => None,
        };
        // A `d` value is the serial number of the date it writes, when that is one Excel holds;
        // anything else is its text, suspect, since what Excel shows for it is not known.
        let date = match &cell.value {
            Some(Value::Date(text)) => Some(
                iso_date(text)
                    .and_then(|date| numfmt::date_to_serial(date, self.date1904))
                    .map(|serial| serial.to_string())
                    .ok_or(text),
            ),
            _ => None,
        };
        let (kind, cached) = match (&cell.value, &runs, &date) {
            (_, Some((text, ..)), _) => (CellKind::Text, Some(text.clone())),
            (_, _, Some(Ok(serial))) => (CellKind::Number, Some(serial.clone())),
            (_, _, Some(Err(text))) => (CellKind::Text, Some((*text).clone())),
            (Some(Value::Number(text)), ..) => (CellKind::Number, Some(text.clone())),
            (Some(Value::Str(text) | Value::Date(text)), ..) => {
                (CellKind::Text, Some(text.clone()))
            }
            (Some(Value::Bool(value)), ..) => (
                CellKind::Bool,
                Some(if *value { "TRUE" } else { "FALSE" }.to_owned()),
            ),
            (Some(Value::Error(text)), ..) => (CellKind::Error, Some(text.clone())),
            (Some(Value::Shared(_) | Value::Inline(_)), None, _) | (None, ..) => (cell.kind, None),
        };
        let empty = |code: Option<&str>| {
            code.is_some_and(|code| match &cached {
                Some(cached) => numfmt::section_is_empty(cached, kind, code),
                // A formula Excel has not calculated: hidden if a value of its kind would be.
                None => {
                    let values: &[&str] = match kind {
                        CellKind::Number => &["1", "-1", "0"],
                        CellKind::Bool => &["TRUE", "FALSE"],
                        CellKind::Text | CellKind::Error => &[""],
                    };
                    values
                        .iter()
                        .any(|value| numfmt::section_is_empty(value, kind, code))
                }
            })
        };
        // The sheet, row and column first, then a merged range over the cell, then each format
        // it may be drawn in: an empty section, then its font against its fill (for a string of
        // runs, every run hidden).
        let hidden = hidden_why(&base, self.limits)
            .or_else(|| covered.then_some(HiddenWhy::Covered))
            .or_else(|| {
                styles.iter().find_map(|style| {
                    if style.format_undecided || empty(style.format) {
                        Some(HiddenWhy::EmptyFormat)
                    } else if runs.is_none() {
                        judge.judge(&base, &style.font, &style.fill)
                    } else {
                        None
                    }
                })
            })
            .or_else(|| match &runs {
                Some((_, first, true)) => {
                    first.map(|(why, _)| why).or(Some(HiddenWhy::LowContrast))
                }
                _ => None,
            });
        let displayed = match (&cached, number_format) {
            (Some(cached), Some(code)) => match kind {
                CellKind::Number => numfmt::display(cached, code),
                CellKind::Text => numfmt::display_text(cached, code),
                CellKind::Bool | CellKind::Error => None,
            },
            (Some(cached), None) if kind == CellKind::Text => Some(cached.clone()),
            _ => None,
        };
        let text = displayed
            .clone()
            .or_else(|| cached.clone())
            .or_else(|| cell.formula.clone())
            .unwrap_or_default();
        let run_why = runs
            .as_ref()
            .and_then(|(_, first, all)| first.filter(|_| !all))
            .map(|(_, why)| why);
        Ok(Made {
            text,
            facts: CellFacts {
                kind,
                cached,
                displayed,
                formula: cell.formula.clone(),
                number_format: number_format.map(str::to_owned),
                date1904: self.date1904,
                hidden: hidden.is_some(),
                row_header: None,
                col_header: None,
                header_unit: None,
                cache_untrusted: self.full_calc_on_load && cell.has_formula,
            },
            hidden,
            run_why,
            phonetic: rst.is_some_and(|rst| rst.phonetic),
            suspect: matches!(date, Some(Err(_))).then_some("xlsx_date_text"),
        })
    }

    /// A string's runs judged against the cell's font and fill in each format it may be drawn
    /// in: a run shows only if it shows in all of them, and never when markup compatibility
    /// left it or its string undecided. The first hidden run's reason comes with the `why` of
    /// its warning.
    fn runs(
        &self,
        judge: &Judge,
        rst: &Rst,
        styles: &[Drawn],
    ) -> Result<(String, Option<(HiddenWhy, &'static str)>, bool), DocError> {
        let plain = CellLook {
            sheet_hidden: false,
            row_hidden: false,
            col_hidden: false,
            empty_format: false,
            font_pt: f32::MAX,
            font: Rgb::BLACK,
            fill: Rgb::WHITE,
        };
        let (mut text, mut first, mut all) = (String::new(), None, true);
        for (at, run) in rst.runs.iter().enumerate() {
            if at % 1024 == 1023 {
                check("runs")?;
            }
            let why = if rst.undecided || run.undecided {
                Some((HiddenWhy::LowContrast, UNKNOWN_NAMESPACE))
            } else {
                styles
                    .iter()
                    .find_map(|style| {
                        judge.judge(
                            &plain,
                            &judge.run(run.props.as_ref(), &style.font),
                            &style.fill,
                        )
                    })
                    .map(|why| (why, why_text(why)))
            };
            match why {
                Some(why) => first = first.or(Some(why)),
                None => {
                    all &= run.text.is_empty();
                    text.push_str(&run.text);
                }
            }
        }
        Ok((text, first, all))
    }
}

/// One format a cell may be drawn in: its font, its fill and its number format, and whether
/// markup compatibility left its number format undecided.
struct Drawn<'s> {
    font: Look,
    fill: Paint,
    format: Option<&'s str>,
    format_undecided: bool,
}

/// The `why` of a run's `HiddenContent` warning when markup compatibility left it undecided.
const UNKNOWN_NAMESPACE: &str = "unknown_namespace";

/// A sheet's `col` entries sorted by column, refusing two that share a column: Excel writes
/// each column once, and which of two entries it would take is not known.
fn columns(mut entries: Vec<Column>) -> Result<Vec<Column>, DocError> {
    entries.sort_unstable_by_key(|entry| entry.min);
    if entries.windows(2).any(|pair| pair[1].min <= pair[0].max) {
        return Err(DocError::Malformed("xlsx_column"));
    }
    Ok(entries)
}

/// A `col` entry: its columns, whether it hides them, and their cell format.
#[derive(Debug, Clone, Copy)]
struct Column {
    min: u32,
    max: u32,
    hidden: bool,
    style: Option<u32>,
}

/// The `col` entry of column `col`, if any.
fn column(entries: &[Column], col: u32) -> Option<Column> {
    let at = entries.partition_point(|entry| entry.min <= col);
    entries
        .get(at.checked_sub(1)?)
        .filter(|entry| (entry.min..=entry.max).contains(&col))
        .copied()
}
