//! What a Kat writes into a document: structured content with claims (SP2a Task 9). Trust core.
//!
//! The grammar has no field for a page, cell, file, link or image: a Kat names a claim only by
//! its text, its kind and the citation ids `cite` returned, and the code that renders the file
//! decides where each claim goes. `uncited_claims` reads every figure and date a Kat wrote
//! outside a claim straight from this content, so no renderer decides what is recorded.
use std::collections::HashSet;

use katwork_thai::{NumberRole, TooLong, find_dates, find_numbers, match_key};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    evidence::{
        claim::{ClaimKind, Derive, MAX_CLAIM_CHARS, NewClaim},
        scan_ctx,
    },
    ids::CitationId,
};

/// Top-level blocks of one document.
pub const MAX_BLOCKS: usize = 2_000;
/// Characters (code points) of all the text of one document or workbook: titles, text runs,
/// claims, list items, cells, sheet names and number strings, each counted once.
pub const MAX_CHARS: usize = 200_000;
/// Inlines, list items and table cells of one document. Empty ones cost no characters, so the
/// character cap alone does not bound them.
pub const MAX_PIECES: usize = 20_000;
/// Citations one claim may name.
pub const MAX_CITE: usize = 8;
/// Columns of one table (Word's own limit).
pub const MAX_TABLE_COLUMNS: usize = 63;
/// Sheets of one workbook.
pub const MAX_SHEETS: usize = 16;
/// Cells of one workbook, `null` cells included.
pub const MAX_CELLS: usize = 50_000;
/// Excel's grid.
pub const MAX_ROWS: usize = 1_048_576;
pub const MAX_COLUMNS: usize = 16_384;
/// Excel's longest cell string.
pub const MAX_CELL_CHARS: usize = 32_767;
/// Excel's longest sheet name.
pub const MAX_SHEET_NAME_CHARS: usize = 31;
/// The longest `{"number": ..}` string.
pub const MAX_NUMBER_CHARS: usize = 64;
/// A number with more significant digits than this is written as text: an `f64` holds it no
/// more.
pub const MAX_NUMBER_DIGITS: usize = 15;
/// Claims, cited and uncited, one version may record: a workbook's 50,000 number cells are each
/// an uncited claim, and each claim is a row the verification pass checks.
pub const MAX_CLAIMS: usize = 60_000;
/// Code points of context kept on each side of an uncited figure.
pub const UNCITED_CONTEXT: usize = 32;

/// A document: an optional title and its blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub blocks: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Heading {
        /// 1 to 3.
        level: u8,
        text: Vec<Inline>,
    },
    Paragraph {
        text: Vec<Inline>,
    },
    List {
        ordered: bool,
        items: Vec<Vec<Inline>>,
    },
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

/// A run of text, or a claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Inline {
    Text(String),
    Claim(ClaimInline),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimInline {
    pub claim: ClaimSpec,
}

/// A claim as a Kat names it. There is no field for where it is: the renderer decides that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSpec {
    /// 1 to 500 code points.
    pub text: String,
    pub kind: ClaimKind,
    /// 1 to 8 distinct citation ids of the Kat's job.
    pub cite: Vec<CitationId>,
    /// Only on a `number` claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive: Option<Derive>,
}

/// One sheet of a workbook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetSpec {
    /// 1 to 31 characters, none of `[ ] : * ? / \`, no leading or trailing apostrophe, unique
    /// in the workbook ignoring case, and not `History`.
    pub name: String,
    pub rows: Vec<Vec<CellSpec>>,
}

/// `null`, a string, `{"number": "<decimal>"}` or `{"claim": ClaimSpec}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CellSpec {
    Empty,
    Text(String),
    Number(NumberCell),
    Claim(ClaimInline),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberCell {
    /// `-?(0|[1-9][0-9]*)(\.[0-9]+)?`
    pub number: String,
}

/// A writer's validated input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrittenContent {
    Doc(DocContent),
    Sheets(Vec<SheetSpec>),
}

/// A claim at the place a writer put it. `location` is `bookmark:kw-claim-<n>` (a cited DOCX
/// claim), `cell:<Sheet>!<A1>` (an XLSX claim, cited or not), `page:<n>` (a cited PDF claim),
/// `block:<n>` (an uncited figure of a DOCX or PDF: `n` the 1-based index of its top-level
/// block, 0 for the title) or `sheet:<Sheet>` (an uncited figure in a sheet's name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedClaim {
    pub ordinal: u32,
    pub kind: ClaimKind,
    pub text: String,
    pub location: String,
    pub cite: Vec<CitationId>,
    pub derive: Option<Derive>,
    pub cited: bool,
}

impl PlacedClaim {
    pub fn to_new_claim(&self) -> NewClaim {
        NewClaim {
            ordinal: self.ordinal,
            kind: self.kind,
            text: self.text.clone(),
            location: self.location.clone(),
            cited: self.cited,
            derive: self.derive.clone(),
            cite: self.cite.clone(),
        }
    }
}

/// A figure or date written outside every claim. `after_claims` counts the claims before it in
/// document order, so cited and uncited claims number in one sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncitedFigure {
    pub after_claims: u32,
    pub claim: PlacedClaim,
}

/// Why content was refused: the rule's name, never content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContentError(pub &'static str);

/// Text as it may be written into a file and recorded: XML 1.0 refuses C0 controls (other than
/// tab, line feed and carriage return), U+FFFE and U+FFFF, and Postgres refuses NUL, so each
/// becomes U+FFFD; a carriage return is dropped, and a line feed or tab becomes one space,
/// since every inline is one run of one line. Zero-width characters stay: Thai text uses them.
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|&c| c != '\r')
        .map(|c| match c {
            '\n' | '\t' => ' ',
            '\u{0}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}' => '\u{FFFD}',
            c => c,
        })
        .collect()
}

/// Counts what content costs against its caps.
struct Budget {
    chars: usize,
    pieces: usize,
}

impl Budget {
    fn text(&mut self, text: &mut String) -> Result<(), ContentError> {
        *text = clean_text(text);
        self.chars = self.chars.saturating_add(text.chars().count());
        if self.chars > MAX_CHARS {
            return Err(ContentError("content_too_long"));
        }
        Ok(())
    }

    fn piece(&mut self) -> Result<(), ContentError> {
        self.pieces += 1;
        if self.pieces > MAX_PIECES {
            return Err(ContentError("content_too_many_pieces"));
        }
        Ok(())
    }
}

/// Cleans a claim's text and checks it: 1 to 500 code points with something a person can read,
/// 1 to 8 distinct citations, and `derive` only on a `number` claim, indexing `cite`.
fn check_claim(claim: &mut ClaimSpec, budget: &mut Budget) -> Result<(), ContentError> {
    budget.text(&mut claim.text)?;
    let chars = claim.text.chars().count();
    if chars == 0 || chars > MAX_CLAIM_CHARS || match_key(&claim.text).text.is_empty() {
        return Err(ContentError("claim_text"));
    }
    if claim.cite.is_empty() || claim.cite.len() > MAX_CITE {
        return Err(ContentError("claim_cite"));
    }
    let mut seen = HashSet::with_capacity(claim.cite.len());
    if !claim.cite.iter().all(|id| seen.insert(*id)) {
        return Err(ContentError("claim_cite_repeated"));
    }
    if let Some(derive) = &claim.derive
        && (claim.kind != ClaimKind::Number || !derive.indices_in(claim.cite.len()))
    {
        return Err(ContentError("claim_derive"));
    }
    Ok(())
}

fn check_inlines(inlines: &mut [Inline], budget: &mut Budget) -> Result<(), ContentError> {
    for inline in inlines {
        budget.piece()?;
        match inline {
            Inline::Text(text) => budget.text(text)?,
            Inline::Claim(ClaimInline { claim }) => check_claim(claim, budget)?,
        }
    }
    Ok(())
}

/// Cleans a document's text and checks it against the grammar's caps.
pub fn check_doc(content: &mut DocContent) -> Result<(), ContentError> {
    if content.blocks.len() > MAX_BLOCKS {
        return Err(ContentError("content_too_many_blocks"));
    }
    let mut budget = Budget {
        chars: 0,
        pieces: 0,
    };
    if let Some(title) = &mut content.title {
        budget.text(title)?;
    }
    for block in &mut content.blocks {
        match block {
            ContentBlock::Heading { level, text } => {
                if !(1..=3).contains(level) {
                    return Err(ContentError("heading_level"));
                }
                check_inlines(text, &mut budget)?;
            }
            ContentBlock::Paragraph { text } => check_inlines(text, &mut budget)?,
            ContentBlock::List { items, .. } => {
                for item in items {
                    budget.piece()?;
                    check_inlines(item, &mut budget)?;
                }
            }
            ContentBlock::Table { header, rows } => {
                if header.len() > MAX_TABLE_COLUMNS
                    || rows.iter().any(|row| row.len() > MAX_TABLE_COLUMNS)
                {
                    return Err(ContentError("table_too_wide"));
                }
                for cell in header.iter_mut().chain(rows.iter_mut().flatten()) {
                    budget.piece()?;
                    check_inlines(cell, &mut budget)?;
                }
            }
        }
    }
    Ok(())
}

/// Whether `name` is a sheet name Excel accepts (case-insensitive uniqueness is checked by the
/// caller).
fn sheet_name_ok(name: &str) -> bool {
    let chars = name.chars().count();
    (1..=MAX_SHEET_NAME_CHARS).contains(&chars)
        && clean_text(name) == name
        && !name.contains(['[', ']', ':', '*', '?', '/', '\\'])
        && !name.starts_with('\'')
        && !name.ends_with('\'')
        && !name.eq_ignore_ascii_case("history")
}

/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?`, at most `MAX_NUMBER_CHARS` long.
pub fn number_ok(text: &str) -> bool {
    if text.len() > MAX_NUMBER_CHARS {
        return false;
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (int, frac) = match digits.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (digits, None),
    };
    let int_ok = int == "0"
        || (!int.is_empty() && !int.starts_with('0') && int.bytes().all(|b| b.is_ascii_digit()));
    int_ok && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

/// The significant digits of a number string `number_ok` accepted: its digits without leading
/// zeros and without trailing zeros.
pub fn significant_digits(text: &str) -> usize {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    digits.trim_start_matches('0').trim_end_matches('0').len()
}

/// Cleans a workbook's text and checks it: 1 to 16 sheets with names Excel accepts, the grid,
/// at most 50,000 cells and 200,000 characters, and number cells of the decimal grammar.
pub fn check_sheets(sheets: &mut [SheetSpec]) -> Result<(), ContentError> {
    if sheets.is_empty() || sheets.len() > MAX_SHEETS {
        return Err(ContentError("sheet_count"));
    }
    let mut budget = Budget {
        chars: 0,
        pieces: 0,
    };
    let mut names = HashSet::with_capacity(sheets.len());
    let mut cells = 0usize;
    for sheet in sheets {
        if !sheet_name_ok(&sheet.name) || !names.insert(sheet.name.to_lowercase()) {
            return Err(ContentError("sheet_name"));
        }
        budget.chars += sheet.name.chars().count();
        if sheet.rows.len() > MAX_ROWS {
            return Err(ContentError("sheet_too_many_rows"));
        }
        for row in &mut sheet.rows {
            if row.len() > MAX_COLUMNS {
                return Err(ContentError("sheet_too_many_columns"));
            }
            cells += row.len();
            if cells > MAX_CELLS {
                return Err(ContentError("sheet_too_many_cells"));
            }
            for cell in row {
                match cell {
                    CellSpec::Empty => {}
                    CellSpec::Text(text) => {
                        budget.text(text)?;
                        if text.chars().count() > MAX_CELL_CHARS {
                            return Err(ContentError("cell_too_long"));
                        }
                    }
                    CellSpec::Number(NumberCell { number }) => {
                        if !number_ok(number) {
                            return Err(ContentError("cell_number"));
                        }
                        budget.text(number)?;
                    }
                    CellSpec::Claim(ClaimInline { claim }) => check_claim(claim, &mut budget)?,
                }
            }
        }
    }
    Ok(())
}

/// The claims of `content` in document order: blocks, then within a block its inlines, list
/// items and table cells (header first) in order; in a workbook, sheets, then rows, then cells.
pub fn claim_specs(content: &WrittenContent) -> Vec<&ClaimSpec> {
    let mut out = Vec::new();
    match content {
        WrittenContent::Doc(doc) => {
            for block in &doc.blocks {
                for inlines in block_runs(block) {
                    out.extend(inlines.iter().filter_map(|inline| match inline {
                        Inline::Claim(ClaimInline { claim }) => Some(claim),
                        Inline::Text(_) => None,
                    }));
                }
            }
        }
        WrittenContent::Sheets(sheets) => {
            for cell in sheets.iter().flat_map(|s| s.rows.iter().flatten()) {
                if let CellSpec::Claim(ClaimInline { claim }) = cell {
                    out.push(claim);
                }
            }
        }
    }
    out
}

/// A block's sequences of inlines, in document order: its text, each list item, or each table
/// cell (the header row first).
pub fn block_runs(block: &ContentBlock) -> Vec<&[Inline]> {
    match block {
        ContentBlock::Heading { text, .. } | ContentBlock::Paragraph { text } => vec![text],
        ContentBlock::List { items, .. } => items.iter().map(Vec::as_slice).collect(),
        ContentBlock::Table { header, rows } => header
            .iter()
            .chain(rows.iter().flatten())
            .map(Vec::as_slice)
            .collect(),
    }
}

/// The A1 name of a 0-based row and column (`(1, 1)` is `B2`).
pub fn a1(row: usize, col: usize) -> String {
    let mut letters = Vec::new();
    let mut n = col + 1;
    while n > 0 {
        let rem = (n - 1) % 26;
        letters.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    letters.reverse();
    format!(
        "{}{}",
        String::from_utf8(letters).expect("ASCII letters"),
        row + 1
    )
}

/// The location of a cell of sheet `name`.
pub fn cell_location(name: &str, row: usize, col: usize) -> String {
    format!("cell:{name}!{}", a1(row, col))
}

struct Walk {
    after: u32,
    out: Vec<UncitedFigure>,
}

impl Walk {
    /// Every `Quantity` number and every date in `run`, in order of their start, each with up
    /// to `UNCITED_CONTEXT` code points of the run on each side.
    fn scan(&mut self, run: &str, location: &str) -> Result<(), TooLong> {
        if run.is_empty() {
            return Ok(());
        }
        let ctx = scan_ctx();
        let mut found: Vec<(u32, u32, ClaimKind)> = find_numbers(run, &ctx)?
            .into_iter()
            .filter(|f| f.value.role == NumberRole::Quantity)
            .map(|f| (f.start, f.end, ClaimKind::Number))
            .chain(
                find_dates(run, &ctx)?
                    .into_iter()
                    .map(|f| (f.start, f.end, ClaimKind::Date)),
            )
            .collect();
        found.sort_by_key(|&(start, end, _)| (start, end));
        if found.is_empty() {
            return Ok(());
        }
        let bounds = Bounds::new(run);
        for (start, end, kind) in found {
            self.push(kind, window(run, &bounds, start, end), location);
        }
        Ok(())
    }

    fn push(&mut self, kind: ClaimKind, text: String, location: &str) {
        self.out.push(UncitedFigure {
            after_claims: self.after,
            claim: PlacedClaim {
                ordinal: 0,
                kind,
                text,
                location: location.to_owned(),
                cite: vec![],
                derive: None,
                cited: false,
            },
        });
    }

    /// A paragraph's (heading's, item's, cell's) inlines: the adjacent `Text` inlines between
    /// two claims are joined into one run, so a figure split across inlines is still found.
    fn inlines(&mut self, inlines: &[Inline], location: &str) -> Result<(), TooLong> {
        let mut run = String::new();
        for inline in inlines {
            match inline {
                Inline::Text(text) => run.push_str(text),
                Inline::Claim(_) => {
                    self.scan(&run, location)?;
                    run.clear();
                    self.after += 1;
                }
            }
        }
        self.scan(&run, location)
    }
}

/// The grapheme-cluster boundaries of a run: each in code points and in bytes, from 0 to the
/// run's length.
struct Bounds {
    points: Vec<usize>,
    bytes: Vec<usize>,
}

impl Bounds {
    fn new(text: &str) -> Bounds {
        let mut bounds = Bounds {
            points: Vec::new(),
            bytes: Vec::new(),
        };
        let mut at = 0;
        for (byte, grapheme) in text.grapheme_indices(true) {
            bounds.points.push(at);
            bounds.bytes.push(byte);
            at += grapheme.chars().count();
        }
        bounds.points.push(at);
        bounds.bytes.push(text.len());
        bounds
    }
}

/// Code points `start..end` of `run` with up to `UNCITED_CONTEXT` code points on each side, cut
/// at grapheme boundaries, trimmed, and at most `MAX_CLAIM_CHARS` long. Logarithmic in the run's
/// length, so a run of many figures costs no more than its scan.
fn window(run: &str, bounds: &Bounds, start: u32, end: u32) -> String {
    let (start, end) = (start as usize, end as usize);
    let points = &bounds.points;
    // The first boundary at most `UNCITED_CONTEXT` before `start`, and the last at most that
    // far after `end`; a figure's own ends are boundaries.
    let from = points
        .partition_point(|&b| b + UNCITED_CONTEXT < start)
        .min(points.partition_point(|&b| b < start));
    let to = points
        .partition_point(|&b| b <= end + UNCITED_CONTEXT)
        .saturating_sub(1)
        .max(points.partition_point(|&b| b < end));
    let to = to.min(points.len() - 1);
    let text = &run[bounds.bytes[from]..bounds.bytes[to]];
    text.trim().chars().take(MAX_CLAIM_CHARS).collect()
}

/// Every figure and date a Kat wrote outside a claim, as uncited claims, in document order:
/// the title (`block:0`); the `Text` inlines of headings, paragraphs, list items and table cells
/// (`block:<n>`); a sheet's name (`sheet:<Name>`), each non-claim string cell, and every
/// `{"number"}` cell, recorded whole as its decimal string (`cell:<Sheet>!<A1>`). A pure
/// function of the content: what a renderer drew never changes what is recorded.
pub fn uncited_claims(content: &WrittenContent) -> Result<Vec<UncitedFigure>, TooLong> {
    let mut walk = Walk {
        after: 0,
        out: Vec::new(),
    };
    match content {
        WrittenContent::Doc(doc) => {
            if let Some(title) = &doc.title {
                walk.scan(title, "block:0")?;
            }
            for (index, block) in doc.blocks.iter().enumerate() {
                let location = format!("block:{}", index + 1);
                for inlines in block_runs(block) {
                    walk.inlines(inlines, &location)?;
                }
            }
        }
        WrittenContent::Sheets(sheets) => {
            for sheet in sheets {
                walk.scan(&sheet.name, &format!("sheet:{}", sheet.name))?;
                for (r, row) in sheet.rows.iter().enumerate() {
                    for (c, cell) in row.iter().enumerate() {
                        match cell {
                            CellSpec::Empty => {}
                            CellSpec::Text(text) => {
                                walk.scan(text, &cell_location(&sheet.name, r, c))?;
                            }
                            CellSpec::Number(NumberCell { number }) => walk.push(
                                ClaimKind::Number,
                                number.clone(),
                                &cell_location(&sheet.name, r, c),
                            ),
                            CellSpec::Claim(_) => walk.after += 1,
                        }
                    }
                }
            }
        }
    }
    Ok(walk.out)
}

/// The final ordinal of each of `claims` cited claims, numbered in one sequence with `uncited`
/// in document order, from 1.
pub fn cited_ordinals(claims: usize, uncited: &[UncitedFigure]) -> Vec<u32> {
    let mut before = 0usize;
    (0..claims)
        .map(|k| {
            while before < uncited.len() && uncited[before].after_claims as usize <= k {
                before += 1;
            }
            (k + 1 + before) as u32
        })
        .collect()
}

/// The uncited figures with their final ordinals.
pub fn number_uncited(uncited: Vec<UncitedFigure>) -> Vec<PlacedClaim> {
    uncited
        .into_iter()
        .enumerate()
        .map(|(j, figure)| PlacedClaim {
            ordinal: (j + 1) as u32 + figure.after_claims,
            ..figure.claim
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id() -> CitationId {
        CitationId::new()
    }

    fn claim(text: &str) -> Inline {
        Inline::Claim(ClaimInline {
            claim: ClaimSpec {
                text: text.into(),
                kind: ClaimKind::Number,
                cite: vec![id()],
                derive: None,
            },
        })
    }

    fn doc(blocks: Vec<ContentBlock>) -> WrittenContent {
        WrittenContent::Doc(DocContent {
            title: None,
            blocks,
        })
    }

    fn texts(content: &WrittenContent) -> Vec<(String, String, u32)> {
        uncited_claims(content)
            .unwrap()
            .into_iter()
            .map(|f| (f.claim.text, f.claim.location, f.after_claims))
            .collect()
    }

    #[test]
    fn a1_names() {
        assert_eq!(a1(0, 0), "A1");
        assert_eq!(a1(1, 1), "B2");
        assert_eq!(a1(0, 25), "Z1");
        assert_eq!(a1(0, 26), "AA1");
        assert_eq!(a1(0, 701), "ZZ1");
        assert_eq!(a1(0, 702), "AAA1");
        assert_eq!(a1(1_048_575, 16_383), "XFD1048576");
    }

    #[test]
    fn number_grammar() {
        for ok in ["0", "-0", "5", "1250", "-12.50", "0.001", "123456789"] {
            assert!(number_ok(ok), "{ok}");
        }
        for bad in [
            "", "1e5", "+5", ".5", "5.", "007", "1,234", "๑๒", "NaN", "inf", "-", "1.2.3", " 1",
        ] {
            assert!(!number_ok(bad), "{bad}");
        }
        assert!(!number_ok(&"9".repeat(400)));
        assert_eq!(significant_digits("1000"), 1);
        assert_eq!(significant_digits("0.00120"), 2);
        assert_eq!(significant_digits("1234567890123456"), 16);
    }

    #[test]
    fn clean_text_makes_text_writable() {
        assert_eq!(
            clean_text("a\0b\u{1}c\u{B}d\u{FFFE}e\r\nf\tg\u{200B}ก"),
            "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e f g\u{200B}ก"
        );
    }

    #[test]
    fn uncited_figures_split_across_inlines_are_found() {
        let content = doc(vec![ContentBlock::Paragraph {
            text: vec![Inline::Text("งบ 3,45".into()), Inline::Text("0 บาท".into())],
        }]);
        let found = texts(&content);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].0.contains("3,450"), "{found:?}");
        assert_eq!(found[0].1, "block:1");
    }

    #[test]
    fn ordinals_and_years_are_not_uncited_numbers() {
        let content = doc(vec![ContentBlock::Paragraph {
            text: vec![Inline::Text("วาระที่ 3".into())],
        }]);
        assert!(texts(&content).is_empty());
        let years = doc(vec![ContentBlock::Paragraph {
            text: vec![Inline::Text("ปี 2567".into())],
        }]);
        assert!(
            uncited_claims(&years)
                .unwrap()
                .iter()
                .all(|f| f.claim.kind == ClaimKind::Date)
        );
    }

    #[test]
    fn ordinals_interleave_cited_and_uncited() {
        let content = doc(vec![ContentBlock::Paragraph {
            text: vec![
                claim("1"),
                Inline::Text(" งบประมาณ 3,450 บาท ".into()),
                claim("2"),
            ],
        }]);
        let uncited = uncited_claims(&content).unwrap();
        assert_eq!(uncited.len(), 1);
        assert_eq!(uncited[0].after_claims, 1);
        assert_eq!(cited_ordinals(2, &uncited), vec![1, 3]);
        assert_eq!(number_uncited(uncited)[0].ordinal, 2);
        // Two uncited figures before any claim.
        let content = doc(vec![
            ContentBlock::Paragraph {
                text: vec![Inline::Text("ยอด 1,250 บาท และ 3,400 บาท".into())],
            },
            ContentBlock::Paragraph {
                text: vec![claim("x")],
            },
        ]);
        let uncited = uncited_claims(&content).unwrap();
        assert_eq!(uncited.len(), 2);
        assert_eq!(cited_ordinals(1, &uncited), vec![3]);
        let numbered = number_uncited(uncited);
        assert_eq!((numbered[0].ordinal, numbered[1].ordinal), (1, 2));
    }

    #[test]
    fn figures_in_title_lists_tables_cells_and_sheet_names() {
        let content = WrittenContent::Doc(DocContent {
            title: Some("รายงานงบ 9,999 บาท".into()),
            blocks: vec![
                ContentBlock::List {
                    ordered: true,
                    items: vec![vec![Inline::Text("ค่าใช้จ่าย 1,200 บาท".into())]],
                },
                ContentBlock::Table {
                    header: vec![vec![Inline::Text("หัว".into())]],
                    rows: vec![vec![vec![Inline::Text("รวม 4,500 บาท".into())]]],
                },
            ],
        });
        let found: Vec<String> = texts(&content).into_iter().map(|t| t.1).collect();
        assert_eq!(found, vec!["block:0", "block:1", "block:2"]);
        let sheets = WrittenContent::Sheets(vec![SheetSpec {
            name: "งบ 9,999 บาท".into(),
            rows: vec![
                vec![CellSpec::Empty],
                vec![
                    CellSpec::Empty,
                    CellSpec::Number(NumberCell {
                        number: "9999".into(),
                    }),
                ],
                vec![CellSpec::Empty, CellSpec::Text("งบ 3,450 บาท".into())],
            ],
        }]);
        let found: Vec<String> = texts(&sheets).into_iter().map(|t| t.1).collect();
        assert_eq!(
            found,
            vec![
                "sheet:งบ 9,999 บาท",
                "cell:งบ 9,999 บาท!B2",
                "cell:งบ 9,999 บาท!B3"
            ]
        );
    }

    #[test]
    fn uncited_window_cuts_at_graphemes() {
        // A combining mark sits exactly where a code-point cut would split its cluster.
        let left = format!("{}{}", "ก".repeat(2), "กี".repeat(16));
        let run = format!("{left} 1,250 บาท");
        let found = texts(&doc(vec![ContentBlock::Paragraph {
            text: vec![Inline::Text(run.clone())],
        }]));
        assert_eq!(found.len(), 1);
        let text = &found[0].0;
        assert!(text.contains("1,250"));
        assert!(!text.starts_with('\u{0E35}'), "{text}");
        assert!(run.contains(text.as_str()));
        assert!(text.chars().count() <= 5 + 2 * UNCITED_CONTEXT);
    }

    #[test]
    fn grammar_refuses_locators_and_unknown_fields() {
        let cite = id();
        for bad in [
            json!({"blocks":[{"type":"paragraph","text":[{"claim":{"text":"1","kind":"number","cite":[cite],"page":3}}]}]}),
            json!({"blocks":[{"type":"paragraph","text":[{"claim":{"text":"1","kind":"number","cite":[cite]},"page":3}]}]}),
            json!({"blocks":[{"type":"paragraph","text":[],"page":1}]}),
            json!({"blocks":[{"type":"image","src":"x"}]}),
            json!({"blocks":[],"file":"x"}),
            json!({"blocks":[{"type":"paragraph","text":[{"link":"http://x"}]}]}),
        ] {
            assert!(
                serde_json::from_value::<DocContent>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        for bad in [
            json!({"name":"S","rows":[[{"number":"1","cell":"A1"}]]}),
            json!({"name":"S","rows":[[{"formula":"=1"}]]}),
            json!({"name":"S","rows":[[1]]}),
        ] {
            assert!(
                serde_json::from_value::<SheetSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let ok: SheetSpec =
            serde_json::from_value(json!({"name":"S","rows":[[null,"x",{"number":"1"}]]})).unwrap();
        assert_eq!(ok.rows[0][0], CellSpec::Empty);
    }

    #[test]
    fn content_round_trips_through_serde() {
        let cite = id();
        let value = json!({"title":"T","blocks":[
            {"type":"heading","level":1,"text":["H"]},
            {"type":"paragraph","text":["a",{"claim":{"text":"1","kind":"number","cite":[cite],"derive":{"op":"sum","args":[0,0]}}}]},
            {"type":"list","ordered":false,"items":[["x"]]},
            {"type":"table","header":[["h"]],"rows":[[["c"]]]}]});
        let content: DocContent = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&content).unwrap(), value);
    }

    #[test]
    fn claim_bounds() {
        let check = |claim: ClaimSpec| {
            let mut content = DocContent {
                title: None,
                blocks: vec![ContentBlock::Paragraph {
                    text: vec![Inline::Claim(ClaimInline { claim })],
                }],
            };
            check_doc(&mut content)
        };
        let base = ClaimSpec {
            text: "1".into(),
            kind: ClaimKind::Number,
            cite: vec![id()],
            derive: None,
        };
        assert!(check(base.clone()).is_ok());
        let one = id();
        for bad in [
            ClaimSpec {
                text: String::new(),
                ..base.clone()
            },
            ClaimSpec {
                text: "ก".repeat(501),
                ..base.clone()
            },
            ClaimSpec {
                text: " \u{200B} ".into(),
                ..base.clone()
            },
            ClaimSpec {
                cite: vec![],
                ..base.clone()
            },
            ClaimSpec {
                cite: (0..9).map(|_| id()).collect(),
                ..base.clone()
            },
            ClaimSpec {
                cite: vec![one, one],
                ..base.clone()
            },
            ClaimSpec {
                kind: ClaimKind::Quote,
                derive: Some(serde_json::from_value(json!({"op":"sum","args":[0,0]})).unwrap()),
                ..base.clone()
            },
            ClaimSpec {
                cite: vec![id(), id()],
                derive: Some(serde_json::from_value(json!({"op":"sum","args":[0,5]})).unwrap()),
                ..base.clone()
            },
        ] {
            assert!(check(bad.clone()).is_err(), "{bad:?}");
        }
        assert!(
            check(ClaimSpec {
                text: "ก".repeat(500),
                cite: (0..8).map(|_| id()).collect(),
                ..base
            })
            .is_ok()
        );
    }

    #[test]
    fn content_caps() {
        let para = |text: &str| ContentBlock::Paragraph {
            text: vec![Inline::Text(text.into())],
        };
        let mut many = DocContent {
            title: None,
            blocks: (0..=MAX_BLOCKS).map(|_| para("")).collect(),
        };
        assert_eq!(
            check_doc(&mut many),
            Err(ContentError("content_too_many_blocks"))
        );
        let mut long = DocContent {
            title: Some("ก".repeat(MAX_CHARS)),
            blocks: vec![para("x")],
        };
        assert_eq!(check_doc(&mut long), Err(ContentError("content_too_long")));
        let mut cells = DocContent {
            title: None,
            blocks: vec![ContentBlock::Table {
                header: vec![],
                rows: (0..400).map(|_| vec![vec![]; 60]).collect(),
            }],
        };
        assert_eq!(
            check_doc(&mut cells),
            Err(ContentError("content_too_many_pieces"))
        );
        let mut items = DocContent {
            title: None,
            blocks: vec![ContentBlock::List {
                ordered: false,
                items: vec![vec![]; MAX_PIECES + 1],
            }],
        };
        assert_eq!(
            check_doc(&mut items),
            Err(ContentError("content_too_many_pieces"))
        );
        let mut levels = DocContent {
            title: None,
            blocks: vec![ContentBlock::Heading {
                level: 4,
                text: vec![],
            }],
        };
        assert_eq!(check_doc(&mut levels), Err(ContentError("heading_level")));
        levels.blocks = vec![ContentBlock::Heading {
            level: 0,
            text: vec![],
        }];
        assert!(check_doc(&mut levels).is_err());
    }

    #[test]
    fn xlsx_caps() {
        let sheet = |name: &str| SheetSpec {
            name: name.into(),
            rows: vec![],
        };
        let check = |mut sheets: Vec<SheetSpec>| check_sheets(&mut sheets);
        assert!(check(vec![sheet("Sheet1")]).is_ok());
        assert!(check(vec![]).is_err());
        assert!(check((0..17).map(|i| sheet(&format!("S{i}"))).collect()).is_err());
        for bad in [
            "a".repeat(32).as_str(),
            "",
            "a:b",
            "'a",
            "a'",
            "a[1]",
            "x/y",
            "History",
            "history",
            "a\u{1}",
        ] {
            assert!(check(vec![sheet(bad)]).is_err(), "{bad}");
        }
        assert!(check(vec![sheet(&"ก".repeat(31))]).is_ok());
        assert!(check(vec![sheet("A!B")]).is_ok());
        assert!(check(vec![sheet("Data"), sheet("DATA")]).is_err());
        assert!(check(vec![sheet("ข้อมูล"), sheet("ข้อมูล")]).is_err());
        let cells = |rows: Vec<Vec<CellSpec>>| {
            check(vec![SheetSpec {
                name: "S".into(),
                rows,
            }])
        };
        assert!(cells(vec![vec![CellSpec::Empty; 10_000]; 5]).is_ok());
        assert!(
            cells(vec![
                vec![CellSpec::Empty; 10_001],
                vec![CellSpec::Empty; 40_000]
            ])
            .is_err()
        );
        assert_eq!(
            cells(vec![vec![CellSpec::Empty; MAX_COLUMNS + 1]]),
            Err(ContentError("sheet_too_many_columns"))
        );
        assert!(cells(vec![vec![CellSpec::Text("ก".repeat(MAX_CELL_CHARS + 1))]]).is_err());
        assert!(cells(vec![vec![CellSpec::Text("ก".repeat(MAX_CELL_CHARS))]]).is_ok());
        assert!(
            cells(vec![vec![CellSpec::Number(NumberCell {
                number: "1e5".into()
            })]])
            .is_err()
        );
        let mut rows = SheetSpec {
            name: "S".into(),
            rows: vec![],
        };
        rows.rows = (0..=MAX_ROWS).map(|_| vec![]).collect();
        assert_eq!(
            check_sheets(std::slice::from_mut(&mut rows)),
            Err(ContentError("sheet_too_many_rows"))
        );
        // Over the character cap across cells.
        assert!(
            cells(
                (0..7)
                    .map(|_| vec![CellSpec::Text("ก".repeat(30_000))])
                    .collect()
            )
            .is_err()
        );
    }
}
