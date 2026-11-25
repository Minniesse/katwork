//! Which formula cells Excel recalculates when it opens a workbook in automatic mode, though the
//! current engine saved it: every formula holding a volatile function or marked `ca`, and every
//! formula that depends on one, through cells, ranges, other sheets and defined names. Excel
//! recalculates volatile cells and all their dependents at every recalculation, opening a
//! workbook in automatic mode included, so what such a cell shows is not its cache.
//!
//! Fail closed: a reference the reader does not follow (a structured or external reference, a
//! 3-D range, an unknown sheet or name, a name that names itself) counts as depending on a
//! volatile cell whenever the workbook holds one.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use super::{DocError, DocLimits, MAX_COLUMN, MAX_ROW, Merge, SheetData, check};

/// Functions Excel recalculates at every recalculation, whatever their arguments changed.
const VOLATILE: &[&str] = &[
    "NOW",
    "TODAY",
    "RAND",
    "RANDBETWEEN",
    "RANDARRAY",
    "OFFSET",
    "INDIRECT",
    "CELL",
    "INFO",
];

/// Prefixes Excel writes before a newer function's name in a file.
const FUNCTION_PREFIXES: &[&str] = &["_xlfn.", "_xlws."];

/// A `LET` or `LAMBDA` parameter, as Excel writes it in a file: not a defined name.
const PARAMETER_PREFIX: &str = "_xlpm.";

/// A defined name nested in another deeper than this counts as a reference not followed.
const MAX_NAME_DEPTH: u32 = 32;

/// Work units (formula visits, reference lookups, range scans) per cell `max_cells` allows.
const WORK_PER_CELL: u64 = 64;

/// References kept per cell `max_cells` allows.
const REFS_PER_CELL: u64 = 8;

/// Steps between two deadline checks.
const STEPS_PER_CHECK: u64 = 1_024;

/// A `definedName` of the workbook part.
pub(super) struct DefinedName {
    pub name: String,
    /// The index, in the workbook's `sheets`, of the sheet it belongs to.
    pub local: Option<usize>,
    pub text: String,
}

/// What a formula's text depends on.
#[derive(Clone, Default)]
struct Deps {
    volatile: bool,
    /// A reference the reader does not follow.
    unknown: bool,
    /// Ranges, as (sheet index, range).
    areas: Vec<(usize, Merge)>,
}

impl Deps {
    fn absorb(&mut self, other: &Deps) {
        self.volatile |= other.volatile;
        self.unknown |= other.unknown;
        self.areas.extend_from_slice(&other.areas);
    }
}

/// Where a formula's text is read: its sheet (none for a workbook-wide name, whose unqualified
/// references are relative to whatever sheet is active) and the shift its relative references
/// take (a shared formula's child carries its master's text). A defined name's relative
/// references follow the active cell, which no file states: not followed.
#[derive(Clone, Copy)]
struct Place {
    sheet: Option<usize>,
    shift: Option<(i64, i64)>,
}

enum NameState {
    Unread,
    Reading,
    Read(Deps),
}

struct Resolver<'a> {
    /// Sheet names, lower-cased (Excel compares them whatever their case), to their index.
    sheets: HashMap<String, usize>,
    /// (sheet or none, upper-cased name) to the index of the name.
    names: HashMap<(Option<usize>, String), usize>,
    defined: &'a [DefinedName],
    undecided: bool,
    states: Vec<NameState>,
    work: Work,
}

/// Counts work and kept references against their caps, and checks the deadline.
struct Work {
    steps: u64,
    max_steps: u64,
    refs: u64,
    max_refs: u64,
}

impl Work {
    fn step(&mut self, units: u64) -> Result<(), DocError> {
        let before = self.steps;
        self.steps += units;
        if self.steps > self.max_steps {
            return Err(DocError::TooComplex("formula_dependencies"));
        }
        if before / STEPS_PER_CHECK != self.steps / STEPS_PER_CHECK {
            check("dependents")?;
        }
        Ok(())
    }

    fn keep(&mut self, refs: usize) -> Result<(), DocError> {
        self.refs += refs as u64;
        if self.refs > self.max_refs {
            return Err(DocError::TooComplex("formula_dependencies"));
        }
        Ok(())
    }
}

/// The cells, as (sheet index, row, column), that hold a recalculated formula or lie in the
/// range of a recalculated array formula or data table. `sheets` pairs each worksheet read with
/// its index in the workbook's `sheets`; `sheet_names` lists them all, in that order.
pub(super) fn volatile_cells(
    sheet_names: &[&str],
    sheets: &[(usize, &SheetData)],
    defined: &[DefinedName],
    undecided: bool,
    limits: &DocLimits,
) -> Result<HashSet<(usize, u32, u32)>, DocError> {
    let cap = u64::from(limits.max_cells).max(1);
    let mut resolver = Resolver {
        sheets: sheet_names
            .iter()
            .enumerate()
            .map(|(at, name)| (name.to_lowercase(), at))
            .collect(),
        names: defined
            .iter()
            .enumerate()
            .map(|(at, name)| ((name.local, name.name.to_uppercase()), at))
            .collect(),
        defined,
        undecided,
        states: defined.iter().map(|_| NameState::Unread).collect(),
        work: Work {
            steps: 0,
            max_steps: WORK_PER_CELL * cap,
            refs: 0,
            max_refs: REFS_PER_CELL * cap,
        },
    };

    // Every formula and what it depends on; the walk stops here if none is volatile.
    struct Formula {
        at: (usize, u32, u32),
        deps: Deps,
    }
    let mut formulas = Vec::new();
    let mut seeded = false;
    for &(sheet, data) in sheets {
        for (at, cell) in data.cells.iter().enumerate() {
            if !cell.has_formula {
                continue;
            }
            if at % 64 == 63 {
                check("formulas")?;
            }
            let place = Place {
                sheet: Some(sheet),
                shift: Some(match cell.master {
                    Some((row, col)) => (
                        i64::from(cell.row) - i64::from(row),
                        i64::from(cell.col) - i64::from(col),
                    ),
                    None => (0, 0),
                }),
            };
            let mut deps = match &cell.formula {
                Some(text) => resolver.read(text, place, 0)?,
                None => Deps::default(),
            };
            deps.volatile |= cell.ca;
            deps.unknown |= cell.data_table;
            resolver.work.keep(deps.areas.len())?;
            seeded |= deps.volatile;
            formulas.push(Formula {
                at: (sheet, cell.row, cell.col),
                deps,
            });
        }
    }
    let mut tainted = Tainted::default();
    if !seeded {
        return Ok(tainted.cells);
    }

    // Array formulas and data tables, by the cell holding their `f`, and every cell read.
    let mut ranges: HashMap<(usize, u32, u32), Vec<Merge>> = HashMap::new();
    let mut cells: HashMap<usize, BTreeSet<(u32, u32)>> = HashMap::new();
    for &(sheet, data) in sheets {
        for &(range, (row, col)) in &data.formula_ranges {
            ranges.entry((sheet, row, col)).or_default().push(range);
        }
        cells
            .entry(sheet)
            .or_default()
            .extend(data.cells.iter().map(|cell| (cell.row, cell.col)));
    }

    // Formulas by the single cells they read, and the ranges they read.
    let mut readers: HashMap<(usize, u32, u32), Vec<usize>> = HashMap::new();
    let mut areas: Vec<(usize, Merge, usize)> = Vec::new();
    let mut work = resolver.work;
    for (id, formula) in formulas.iter().enumerate() {
        for &(sheet, area) in &formula.deps.areas {
            work.step(1)?;
            if area.top == area.bottom && area.left == area.right {
                readers
                    .entry((sheet, area.top, area.left))
                    .or_default()
                    .push(id);
            } else {
                areas.push((sheet, area, id));
            }
        }
    }
    areas.sort_by_key(|&(sheet, area, _)| (sheet, area.top, area.left));

    let mut done = vec![false; formulas.len()];
    let mut queue = VecDeque::new();
    for (id, formula) in formulas.iter().enumerate() {
        if formula.deps.volatile || formula.deps.unknown {
            done[id] = true;
            tainted.add(formula.at, &ranges, &cells, &mut queue, &mut work)?;
        }
    }
    let spread = |done: &mut [bool],
                  tainted: &mut Tainted,
                  queue: &mut VecDeque<(usize, u32, u32)>,
                  work: &mut Work|
     -> Result<(), DocError> {
        while let Some(at) = queue.pop_front() {
            work.step(1)?;
            for &id in readers.get(&at).into_iter().flatten() {
                work.step(1)?;
                if !std::mem::replace(&mut done[id], true) {
                    tainted.add(formulas[id].at, &ranges, &cells, queue, work)?;
                }
            }
        }
        Ok(())
    };
    spread(&mut done, &mut tainted, &mut queue, &mut work)?;
    // Ranges: each pass checks every range whose formula is not yet recalculated, in reading
    // order, spreading at once from each it finds; a pass that finds none ends the walk.
    loop {
        let mut found = false;
        for &(sheet, area, id) in &areas {
            if done[id] {
                continue;
            }
            if tainted.within(sheet, area, &mut work)? {
                done[id] = true;
                found = true;
                tainted.add(formulas[id].at, &ranges, &cells, &mut queue, &mut work)?;
                spread(&mut done, &mut tainted, &mut queue, &mut work)?;
            }
        }
        if !found {
            break;
        }
    }
    Ok(tainted.cells)
}

/// The recalculated cells, also by row and by column on each sheet.
#[derive(Default)]
struct Tainted {
    cells: HashSet<(usize, u32, u32)>,
    rows: HashMap<(usize, u32), BTreeSet<u32>>,
    cols: HashMap<(usize, u32), BTreeSet<u32>>,
}

impl Tainted {
    /// Marks a recalculated formula's cell, and every cell of an array or data table it holds.
    fn add(
        &mut self,
        at: (usize, u32, u32),
        ranges: &HashMap<(usize, u32, u32), Vec<Merge>>,
        cells: &HashMap<usize, BTreeSet<(u32, u32)>>,
        queue: &mut VecDeque<(usize, u32, u32)>,
        work: &mut Work,
    ) -> Result<(), DocError> {
        self.mark(at, queue);
        let (sheet, ..) = at;
        for range in ranges.get(&at).into_iter().flatten() {
            let Some(read) = cells.get(&sheet) else {
                continue;
            };
            for &(row, col) in read.range((range.top, range.left)..=(range.bottom, range.right)) {
                work.step(1)?;
                if (range.left..=range.right).contains(&col) {
                    self.mark((sheet, row, col), queue);
                }
            }
        }
        Ok(())
    }

    fn mark(&mut self, at: (usize, u32, u32), queue: &mut VecDeque<(usize, u32, u32)>) {
        if self.cells.insert(at) {
            let (sheet, row, col) = at;
            self.rows.entry((sheet, row)).or_default().insert(col);
            self.cols.entry((sheet, col)).or_default().insert(row);
            queue.push_back(at);
        }
    }

    /// Whether a recalculated cell lies in `area`, scanned along its shorter side.
    fn within(&self, sheet: usize, area: Merge, work: &mut Work) -> Result<bool, DocError> {
        if area.bottom - area.top <= area.right - area.left {
            for row in area.top..=area.bottom {
                work.step(1)?;
                if self
                    .rows
                    .get(&(sheet, row))
                    .is_some_and(|cols| cols.range(area.left..=area.right).next().is_some())
                {
                    return Ok(true);
                }
            }
        } else {
            for col in area.left..=area.right {
                work.step(1)?;
                if self
                    .cols
                    .get(&(sheet, col))
                    .is_some_and(|rows| rows.range(area.top..=area.bottom).next().is_some())
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

impl Resolver<'_> {
    /// What `text` depends on, read at `place`; `depth` counts the names it is read through.
    fn read(&mut self, text: &str, place: Place, depth: u32) -> Result<Deps, DocError> {
        let chars: Vec<char> = text.chars().collect();
        self.work.step(1 + chars.len() as u64 / 64)?;
        let mut deps = Deps::default();
        let mut at = 0;
        // The sheet a `Sheet!` prefix just named: none when it names no sheet of the workbook.
        let mut qualified: Option<Option<usize>> = None;
        while let Some(&c) = chars.get(at) {
            match c {
                '"' => {
                    at += 1;
                    while let Some(&c) = chars.get(at) {
                        at += 1;
                        if c == '"' {
                            if chars.get(at) == Some(&'"') {
                                at += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    qualified = None;
                }
                '\'' => {
                    let start = at;
                    let mut name = String::new();
                    at += 1;
                    while let Some(&c) = chars.get(at) {
                        at += 1;
                        if c == '\'' {
                            if chars.get(at) == Some(&'\'') {
                                at += 1;
                            } else {
                                break;
                            }
                        }
                        name.push(c);
                    }
                    if chars.get(at) != Some(&'!') || follows_colon(&chars, start) {
                        deps.unknown = true;
                    } else {
                        at += 1;
                        qualified = Some(self.sheets.get(&name.to_lowercase()).copied());
                    }
                }
                '[' => {
                    // A structured or external reference.
                    deps.unknown = true;
                    let mut open = 0u32;
                    while let Some(&c) = chars.get(at) {
                        at += 1;
                        match c {
                            '[' => open += 1,
                            ']' => {
                                open -= 1;
                                if open == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    qualified = None;
                }
                '#' => {
                    // An error value such as `#REF!` or `#N/A`.
                    at += 1;
                    while chars
                        .get(at)
                        .is_some_and(|c| c.is_alphanumeric() || "/!?_".contains(*c))
                    {
                        at += 1;
                    }
                    qualified = None;
                }
                c if c.is_alphanumeric() || "_\\$".contains(c) => {
                    let start = at;
                    while chars.get(at).is_some_and(|&c| word(c)) {
                        at += 1;
                    }
                    let token: String = chars[start..at].iter().collect();
                    if chars.get(at) == Some(&'!') {
                        if follows_colon(&chars, start) {
                            deps.unknown = true;
                        }
                        at += 1;
                        qualified = Some(self.sheets.get(&token.to_lowercase()).copied());
                        continue;
                    }
                    let sheet = qualified.take();
                    let mut next = at;
                    while chars.get(next) == Some(&' ') {
                        next += 1;
                    }
                    if chars.get(next) == Some(&'(') {
                        let mut name = token.to_uppercase();
                        while let Some(rest) = FUNCTION_PREFIXES
                            .iter()
                            .find_map(|prefix| name.strip_prefix(&prefix.to_uppercase()))
                        {
                            name = rest.to_owned();
                        }
                        deps.volatile |= VOLATILE.contains(&name.as_str());
                        continue;
                    }
                    let sheet = match sheet {
                        Some(Some(sheet)) => Some(sheet),
                        Some(None) => {
                            deps.unknown = true;
                            continue;
                        }
                        None => None,
                    };
                    at = self.reference(&chars, start, at, sheet, place, depth, &mut deps)?;
                }
                _ => {
                    qualified = None;
                    at += 1;
                }
            }
        }
        Ok(deps)
    }

    /// The token `chars[start..end]`, after an optional sheet: a cell, a range, a number, a
    /// boolean, a parameter or a defined name. Returns where reading goes on.
    #[allow(clippy::too_many_arguments)]
    fn reference(
        &mut self,
        chars: &[char],
        start: usize,
        end: usize,
        sheet: Option<usize>,
        place: Place,
        depth: u32,
        deps: &mut Deps,
    ) -> Result<usize, DocError> {
        let token: String = chars[start..end].iter().collect();
        let on = sheet.or(place.sheet);
        // A range: `A1:B2`, `A:C` or `1:3`.
        if chars.get(end) == Some(&':') {
            let second = end + 1;
            let mut stop = second;
            while chars.get(stop).is_some_and(|&c| word(c)) {
                stop += 1;
            }
            let other: String = chars[second..stop].iter().collect();
            let range = match (cell(&token), cell(&other), line(&token), line(&other)) {
                (Some(a), Some(b), ..) => Some((a, b)),
                (_, _, Some(a), Some(b)) => Some((a, b)),
                _ => None,
            };
            let Some((a, b)) = range.filter(|_| chars.get(stop) != Some(&'!')) else {
                deps.unknown = true;
                return Ok(stop);
            };
            if chars.get(stop) == Some(&':') {
                deps.unknown = true;
            }
            match (on, place.shift) {
                (Some(on), Some(shift)) => {
                    if let (Some((r0, c0)), Some((r1, c1))) = (a.shifted(shift), b.shifted(shift)) {
                        deps.areas.push((
                            on,
                            Merge {
                                top: r0.min(r1),
                                left: c0.min(c1),
                                bottom: r0.max(r1),
                                right: c0.max(c1),
                            },
                        ));
                    }
                }
                (Some(on), None) if a.absolute() && b.absolute() => {
                    let (r0, c0) = a.at();
                    let (r1, c1) = b.at();
                    deps.areas.push((
                        on,
                        Merge {
                            top: r0.min(r1),
                            left: c0.min(c1),
                            bottom: r0.max(r1),
                            right: c0.max(c1),
                        },
                    ));
                }
                _ => deps.unknown = true,
            }
            return Ok(stop);
        }
        if let Some(a) = cell(&token) {
            match (on, place.shift) {
                (Some(on), Some(shift)) => {
                    if let Some((row, col)) = a.shifted(shift) {
                        deps.areas.push((
                            on,
                            Merge {
                                top: row,
                                left: col,
                                bottom: row,
                                right: col,
                            },
                        ));
                    }
                }
                (Some(on), None) if a.absolute() => {
                    let (row, col) = a.at();
                    deps.areas.push((
                        on,
                        Merge {
                            top: row,
                            left: col,
                            bottom: row,
                            right: col,
                        },
                    ));
                }
                _ => deps.unknown = true,
            }
            return Ok(end);
        }
        let upper = token.to_uppercase();
        if token.starts_with(|c: char| c.is_ascii_digit() || c == '.')
            || matches!(upper.as_str(), "TRUE" | "FALSE")
            || upper.starts_with(&PARAMETER_PREFIX.to_uppercase())
        {
            return Ok(end);
        }
        // A defined name: the sheet's own first, then the workbook's.
        let found = [on.map(Some), Some(None)]
            .into_iter()
            .flatten()
            .find_map(|scope| self.names.get(&(scope, upper.clone())).copied());
        match found {
            Some(name) if !self.undecided => {
                let named = self.name(name, depth)?;
                self.work.step(named.areas.len() as u64)?;
                deps.absorb(&named);
            }
            _ => deps.unknown = true,
        }
        Ok(end)
    }

    /// What a defined name depends on; a name met again while it is read, or read too deep,
    /// is not followed.
    fn name(&mut self, at: usize, depth: u32) -> Result<Deps, DocError> {
        match &self.states[at] {
            NameState::Read(deps) => return Ok(deps.clone()),
            NameState::Reading => {
                return Ok(Deps {
                    unknown: true,
                    ..Deps::default()
                });
            }
            NameState::Unread => {}
        }
        if depth >= MAX_NAME_DEPTH {
            return Ok(Deps {
                unknown: true,
                ..Deps::default()
            });
        }
        self.states[at] = NameState::Reading;
        let defined = &self.defined[at];
        let place = Place {
            sheet: defined.local,
            shift: None,
        };
        let deps = self.read(&defined.text.clone(), place, depth + 1)?;
        self.work.keep(deps.areas.len())?;
        self.states[at] = NameState::Read(deps.clone());
        Ok(deps)
    }
}

/// A character of a name, a number, a cell reference or a sheet name.
fn word(c: char) -> bool {
    c.is_alphanumeric() || "_.\\?$".contains(c)
}

/// Whether `chars[at]` follows a `:`, as the second sheet of a 3-D reference does.
fn follows_colon(chars: &[char], at: usize) -> bool {
    at > 0 && chars.get(at - 1) == Some(&':')
}

/// One end of a reference: a row, a column, or both, each absolute or relative.
#[derive(Clone, Copy)]
struct End {
    row: Option<(u32, bool)>,
    col: Option<(u32, bool)>,
}

impl End {
    fn absolute(self) -> bool {
        self.row.is_none_or(|(_, abs)| abs) && self.col.is_none_or(|(_, abs)| abs)
    }

    /// (row, column), a whole column's rows from 1 and a whole row's columns from 1 (the other
    /// end gives the last).
    fn at(self) -> (u32, u32) {
        (
            self.row.map_or(1, |(row, _)| row),
            self.col.map_or(1, |(col, _)| col),
        )
    }

    /// Shifted by `(rows, cols)` where relative; none when that leaves the grid (Excel shows
    /// `#REF!`, which depends on nothing). A whole column's missing row reads as the grid's
    /// first or last, as is the other end's.
    fn shifted(self, (rows, cols): (i64, i64)) -> Option<(u32, u32)> {
        let move_by = |part: Option<(u32, bool)>, by: i64, max: u32, whole: u32| match part {
            None => Some(whole),
            Some((value, true)) => Some(value),
            Some((value, false)) => {
                let moved = i64::from(value) + by;
                (1..=i64::from(max))
                    .contains(&moved)
                    .then(|| u32::try_from(moved).ok())
                    .flatten()
            }
        };
        let whole_row = if self.col.is_some() && self.row.is_none() {
            MAX_ROW
        } else {
            1
        };
        let whole_col = if self.row.is_some() && self.col.is_none() {
            MAX_COLUMN
        } else {
            1
        };
        Some((
            move_by(self.row, rows, MAX_ROW, whole_row)?,
            move_by(self.col, cols, MAX_COLUMN, whole_col)?,
        ))
    }
}

/// `$A$1`, `A1` or `a1`: a cell in the grid.
fn cell(token: &str) -> Option<End> {
    let (col, rest) = column_part(token)?;
    let (row, rest) = row_part(rest)?;
    rest.is_empty().then_some(End {
        row: Some(row),
        col: Some(col),
    })
}

/// A whole column (`$A`) or a whole row (`3`), as one end of a range.
fn line(token: &str) -> Option<End> {
    if let Some((col, "")) = column_part(token) {
        return Some(End {
            row: None,
            col: Some(col),
        });
    }
    let (row, rest) = row_part(token)?;
    rest.is_empty().then_some(End {
        row: Some(row),
        col: None,
    })
}

fn column_part(token: &str) -> Option<((u32, bool), &str)> {
    let (abs, rest) = match token.strip_prefix('$') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    if !(1..=3).contains(&letters) {
        return None;
    }
    let col = rest.bytes().take(letters).fold(0u32, |col, letter| {
        col * 26 + u32::from(letter.to_ascii_uppercase() - b'A') + 1
    });
    (col <= MAX_COLUMN).then_some(((col, abs), &rest[letters..]))
}

fn row_part(token: &str) -> Option<((u32, bool), &str)> {
    let (abs, rest) = match token.strip_prefix('$') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 7 || rest.starts_with('0') {
        return None;
    }
    let row: u32 = rest[..digits].parse().ok()?;
    (row <= MAX_ROW).then_some(((row, abs), &rest[digits..]))
}
