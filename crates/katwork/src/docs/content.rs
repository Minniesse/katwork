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
// The figure scan refuses a text over `MAX_SCAN_CODE_POINTS`; no content can reach it, so
// `uncited_claims` never fails on checked content and the renderers' `TooLong` mapping is a
// defence only.
const _: () = assert!(MAX_CHARS <= katwork_thai::MAX_SCAN_CODE_POINTS);
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
        .filter(|&c| c != '\r' && c != '\u{7F}')
        .map(|c| match c {
            // Gaps a reader sees inside a figure ("1 000") that the scanner would not read as
            // one: figure, thin and narrow no-break spaces, the Hangul fillers, blank Braille.
            '\n' | '\t' | '\u{2007}' | '\u{2009}' | '\u{202F}' | '\u{3164}' | '\u{FFA0}'
            | '\u{2800}' => ' ',
            '\u{0}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}' => '\u{FFFD}',
            c => c,
        })
        .collect()
}

/// A character that reorders the text around it, refused in a writer's content (code
/// `bidi_control`): the bidi embeddings, overrides, isolates and marks, which `match_key`
/// drops (a claim `\u{202E}12\u{202C}` would match "12" while a reader sees "21"), and every
/// letter of bidi class R or AL (Hebrew, Arabic and the other right-to-left scripts), beside
/// which a sign or digit moves onto a claim (a Hebrew maqaf `־` reads as a hyphen). Thai and
/// English need none of them.
pub fn is_bidi_control(c: char) -> bool {
    use unicode_bidi::BidiClass;
    is_control(c) || matches!(unicode_bidi::bidi_class(c), BidiClass::R | BidiClass::AL)
}

/// The bidi embeddings, overrides, isolates and marks.
fn is_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// Counts what content costs against its caps.
struct Budget {
    chars: usize,
    pieces: usize,
}

impl Budget {
    fn text(&mut self, text: &mut String) -> Result<(), ContentError> {
        if text.chars().any(is_bidi_control) {
            return Err(ContentError("bidi_control"));
        }
        *text = clean_text(text);
        self.chars = self.chars.saturating_add(text.chars().count());
        if self.chars > MAX_CHARS {
            return Err(ContentError("content_too_long"));
        }
        Ok(())
    }

    fn piece(&mut self) -> Result<(), ContentError> {
        self.pieces_of(1)
    }

    fn pieces_of(&mut self, count: usize) -> Result<(), ContentError> {
        self.pieces = self.pieces.saturating_add(count);
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
                // The renderer pads every row to the widest one, so the grid it draws, not the
                // cells given, is what costs.
                let width = rows
                    .iter()
                    .map(Vec::len)
                    .chain([header.len()])
                    .max()
                    .unwrap_or(0);
                let drawn_rows = rows.len() + usize::from(!header.is_empty());
                budget.pieces_of(width.saturating_mul(drawn_rows))?;
                for cell in header.iter_mut().chain(rows.iter_mut().flatten()) {
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
        if sheet.name.chars().any(is_bidi_control) {
            return Err(ContentError("bidi_control"));
        }
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

/// The location of each claim cell of a workbook, in the order of `claim_specs`.
pub fn claim_cells(sheets: &[SheetSpec]) -> Vec<String> {
    let mut out = Vec::new();
    for sheet in sheets {
        for (r, row) in sheet.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if matches!(cell, CellSpec::Claim(_)) {
                    out.push(cell_location(&sheet.name, r, c));
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
        for (_, _, kind, text) in figures(run, |role| role == NumberRole::Quantity)? {
            self.push(kind, text, location);
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

    /// A paragraph's (heading's, item's, cell's) inlines. The adjacent `Text` inlines between
    /// two claims are joined into one run, so a figure split across inlines is still found.
    /// Then the whole sequence is scanned with the claims' texts in place: a figure whose span
    /// crosses a claim's boundary is what a reader sees (an ordinal or a sign before a claim, a
    /// scale word after it) and no claim says, so it is recorded as uncited too, whatever role
    /// the scanner gives it. Figures are recorded in document order of their start.
    fn inlines(&mut self, inlines: &[Inline], location: &str) -> Result<(), TooLong> {
        let mut joined = String::new();
        let mut joined_len = 0usize;
        // Each claim's code-point range in `joined`, and each text run with its start there.
        let mut claims: Vec<(u32, u32)> = Vec::new();
        let mut runs: Vec<(String, u32)> = Vec::new();
        let mut run = String::new();
        let mut run_start = 0u32;
        for inline in inlines {
            match inline {
                Inline::Text(text) => {
                    run.push_str(text);
                    joined.push_str(text);
                    joined_len += text.chars().count();
                }
                Inline::Claim(ClaimInline { claim }) => {
                    runs.push((std::mem::take(&mut run), run_start));
                    let start = joined_len as u32;
                    joined.push_str(&claim.text);
                    joined_len += claim.text.chars().count();
                    claims.push((start, joined_len as u32));
                    run_start = joined_len as u32;
                }
            }
        }
        runs.push((run, run_start));
        let mut found: Vec<(u32, u32, ClaimKind, String)> = Vec::new();
        for (run, offset) in &runs {
            for (start, end, kind, text) in figures(run, |role| role == NumberRole::Quantity)? {
                found.push((start + offset, end + offset, kind, text));
            }
        }
        if !claims.is_empty() {
            // `claims` is in order and its ranges do not overlap, so both their starts and
            // their ends ascend: a figure crosses a boundary when the first start, or the first
            // end, past its own start lies before its end.
            let crosses = |start: u32, end: u32| {
                let next_start = claims.partition_point(|&(cs, _)| cs <= start);
                let next_end = claims.partition_point(|&(_, ce)| ce <= start);
                claims.get(next_start).is_some_and(|&(cs, _)| cs < end)
                    || claims.get(next_end).is_some_and(|&(_, ce)| ce < end)
            };
            let crossing: Vec<(u32, u32, ClaimKind)> = spans(&joined, |_| true)?
                .into_iter()
                .filter(|&(start, end, _)| crosses(start, end))
                .collect();
            // Windows only for the figures kept.
            if !crossing.is_empty() {
                let bounds = Bounds::new(&joined);
                found.extend(crossing.into_iter().map(|(start, end, kind)| {
                    (start, end, kind, window(&joined, &bounds, start, end))
                }));
            }
        }
        found.sort_by_key(|f| (f.0, f.1));
        let first = self.after;
        for (start, _, kind, text) in found {
            // The claims of this sequence that end before the figure starts.
            let before = claims.partition_point(|&(_, ce)| ce <= start) as u32;
            self.after = first + before;
            self.push(kind, text, location);
        }
        self.after = first + claims.len() as u32;
        Ok(())
    }
}

/// The figures of `run`: every number whose role `keep` accepts and every date, in order of
/// their start, each as its code-point range and kind.
fn spans(
    run: &str,
    keep: impl Fn(NumberRole) -> bool,
) -> Result<Vec<(u32, u32, ClaimKind)>, TooLong> {
    if run.is_empty() {
        return Ok(Vec::new());
    }
    let ctx = scan_ctx();
    let mut found: Vec<(u32, u32, ClaimKind)> = find_numbers(run, &ctx)?
        .into_iter()
        .filter(|f| keep(f.value.role))
        .map(|f| (f.start, f.end, ClaimKind::Number))
        .chain(
            find_dates(run, &ctx)?
                .into_iter()
                .map(|f| (f.start, f.end, ClaimKind::Date)),
        )
        .collect();
    let numerals = unfolded_numerals(run, &found);
    found.extend(
        numerals
            .into_iter()
            .map(|(start, end)| (start, end, ClaimKind::Number)),
    );
    found.sort_by_key(|&(start, end, _)| (start, end));
    Ok(found)
}

/// Runs of numerals the scanners do not read, outside every figure they found: digits that
/// `fold_digit` does not map (math, superscript and other scripts' digits), fractions such as
/// `½`, circled and Roman numerals, and digits carrying a combining mark (`5\u{301}000`). A
/// reader sees a figure in each, so each is recorded whole.
fn unfolded_numerals(run: &str, found: &[(u32, u32, ClaimKind)]) -> Vec<(u32, u32)> {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    let mut out = Vec::new();
    let mut start: Option<(u32, bool)> = None;
    let mut at = 0u32;
    for c in run.chars() {
        let mark = matches!(
            c.general_category(),
            GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
        );
        if c.is_numeric() || (mark && start.is_some()) {
            let odd = mark || katwork_thai::fold_digit(c).is_none();
            start = Some(match start {
                Some((s, seen)) => (s, seen || odd),
                None => (at, odd),
            });
        } else if let Some((s, odd)) = start.take()
            && odd
        {
            out.push((s, at));
        }
        at += 1;
    }
    if let Some((s, true)) = start {
        out.push((s, at));
    }
    out.retain(|&(s, e)| !found.iter().any(|&(fs, fe, _)| fs <= s && e <= fe));
    out
}

/// `spans`, each with its text: up to `UNCITED_CONTEXT` code points of the run on each side.
fn figures(
    run: &str,
    keep: impl Fn(NumberRole) -> bool,
) -> Result<Vec<(u32, u32, ClaimKind, String)>, TooLong> {
    let found = spans(run, keep)?;
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let bounds = Bounds::new(run);
    Ok(found
        .into_iter()
        .map(|(start, end, kind)| (start, end, kind, window(run, &bounds, start, end)))
        .collect())
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
mod tests;
