//! Which formula cells Excel recalculates when it opens a workbook, though the current engine
//! saved it: every formula holding a volatile function or marked `ca`, and every formula that
//! depends on one, through cells, ranges, other sheets and defined names. Excel recalculates
//! volatile cells and all their dependents at every recalculation, opening a workbook in
//! automatic mode included, so what such a cell shows is not its cache. A workbook's own
//! `calcMode` exempts nothing: Excel keeps the mode of the first workbook it opened.
//!
//! Fail closed: a reference the reader does not follow (a structured or external reference, a
//! 3-D range, an unknown sheet or name, a name that names itself) counts as depending on a
//! volatile cell whenever the workbook holds one.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use super::{DocError, DocLimits, MAX_COLUMN, MAX_ROW, Merge, SheetData, check};

/// Functions Excel recalculates at every recalculation, whatever their arguments changed, or
/// may (`SUMIF` for some arguments; `RTD`, `WEBSERVICE`, `STOCKHISTORY` and the `CUBE`
/// functions fetch from outside the workbook): failing closed, all count as volatile. Every
/// `CUBE` function is, by its prefix.
const VOLATILE: &[&str] = &[
    "CELL",
    "INDIRECT",
    "INFO",
    "NOW",
    "OFFSET",
    "RAND",
    "RANDARRAY",
    "RANDBETWEEN",
    "RTD",
    "STOCKHISTORY",
    "SUMIF",
    "TODAY",
    "WEBSERVICE",
];

fn is_volatile(name: &str) -> bool {
    name.starts_with("CUBE") || VOLATILE.contains(&name)
}

/// Excel's own worksheet functions that are not volatile, by their names in a file after any
/// `_xlfn.` or `_xlws.` prefix: those of ECMA-376 Part 1 §18.17.7 and the later ones Excel
/// writes with a prefix. Any other function called (an add-in's, a macro's, another
/// workbook's, or one the reader does not know) counts as volatile. Sorted, for a binary
/// search.
const BUILT_IN: &[&str] = &[
    "ABS",
    "ACCRINT",
    "ACCRINTM",
    "ACOS",
    "ACOSH",
    "ACOT",
    "ACOTH",
    "ADDRESS",
    "AGGREGATE",
    "AMORDEGRC",
    "AMORLINC",
    "ANCHORARRAY",
    "AND",
    "ARABIC",
    "AREAS",
    "ARRAYTOTEXT",
    "ASC",
    "ASIN",
    "ASINH",
    "ATAN",
    "ATAN2",
    "ATANH",
    "AVEDEV",
    "AVERAGE",
    "AVERAGEA",
    "AVERAGEIF",
    "AVERAGEIFS",
    "BAHTTEXT",
    "BASE",
    "BESSELI",
    "BESSELJ",
    "BESSELK",
    "BESSELY",
    "BETA.DIST",
    "BETA.INV",
    "BETADIST",
    "BETAINV",
    "BIN2DEC",
    "BIN2HEX",
    "BIN2OCT",
    "BINOM.DIST",
    "BINOM.DIST.RANGE",
    "BINOM.INV",
    "BINOMDIST",
    "BITAND",
    "BITLSHIFT",
    "BITOR",
    "BITRSHIFT",
    "BITXOR",
    "BYCOL",
    "BYROW",
    "CEILING",
    "CEILING.MATH",
    "CEILING.PRECISE",
    "CHAR",
    "CHIDIST",
    "CHIINV",
    "CHISQ.DIST",
    "CHISQ.DIST.RT",
    "CHISQ.INV",
    "CHISQ.INV.RT",
    "CHISQ.TEST",
    "CHITEST",
    "CHOOSE",
    "CHOOSECOLS",
    "CHOOSEROWS",
    "CLEAN",
    "CODE",
    "COLUMN",
    "COLUMNS",
    "COMBIN",
    "COMBINA",
    "COMPLEX",
    "CONCAT",
    "CONCATENATE",
    "CONFIDENCE",
    "CONFIDENCE.NORM",
    "CONFIDENCE.T",
    "CONVERT",
    "CORREL",
    "COS",
    "COSH",
    "COT",
    "COTH",
    "COUNT",
    "COUNTA",
    "COUNTBLANK",
    "COUNTIF",
    "COUNTIFS",
    "COUPDAYBS",
    "COUPDAYS",
    "COUPDAYSNC",
    "COUPNCD",
    "COUPNUM",
    "COUPPCD",
    "COVAR",
    "COVARIANCE.P",
    "COVARIANCE.S",
    "CRITBINOM",
    "CSC",
    "CSCH",
    "CUMIPMT",
    "CUMPRINC",
    "DATE",
    "DATEDIF",
    "DATEVALUE",
    "DAVERAGE",
    "DAY",
    "DAYS",
    "DAYS360",
    "DB",
    "DBCS",
    "DCOUNT",
    "DCOUNTA",
    "DDB",
    "DEC2BIN",
    "DEC2HEX",
    "DEC2OCT",
    "DECIMAL",
    "DEGREES",
    "DELTA",
    "DEVSQ",
    "DGET",
    "DISC",
    "DMAX",
    "DMIN",
    "DOLLAR",
    "DOLLARDE",
    "DOLLARFR",
    "DPRODUCT",
    "DROP",
    "DSTDEV",
    "DSTDEVP",
    "DSUM",
    "DURATION",
    "DVAR",
    "DVARP",
    "EDATE",
    "EFFECT",
    "ENCODEURL",
    "EOMONTH",
    "ERF",
    "ERF.PRECISE",
    "ERFC",
    "ERFC.PRECISE",
    "ERROR.TYPE",
    "EVEN",
    "EXACT",
    "EXP",
    "EXPAND",
    "EXPON.DIST",
    "EXPONDIST",
    "F.DIST",
    "F.DIST.RT",
    "F.INV",
    "F.INV.RT",
    "F.TEST",
    "FACT",
    "FACTDOUBLE",
    "FALSE",
    "FDIST",
    "FILTER",
    "FILTERXML",
    "FIND",
    "FINDB",
    "FINV",
    "FISHER",
    "FISHERINV",
    "FIXED",
    "FLOOR",
    "FLOOR.MATH",
    "FLOOR.PRECISE",
    "FORECAST",
    "FORECAST.ETS",
    "FORECAST.ETS.CONFINT",
    "FORECAST.ETS.SEASONALITY",
    "FORECAST.ETS.STAT",
    "FORECAST.LINEAR",
    "FORMULATEXT",
    "FREQUENCY",
    "FTEST",
    "FV",
    "FVSCHEDULE",
    "GAMMA",
    "GAMMA.DIST",
    "GAMMA.INV",
    "GAMMADIST",
    "GAMMAINV",
    "GAMMALN",
    "GAMMALN.PRECISE",
    "GAUSS",
    "GCD",
    "GEOMEAN",
    "GESTEP",
    "GETPIVOTDATA",
    "GROUPBY",
    "GROWTH",
    "HARMEAN",
    "HEX2BIN",
    "HEX2DEC",
    "HEX2OCT",
    "HLOOKUP",
    "HOUR",
    "HSTACK",
    "HYPERLINK",
    "HYPGEOM.DIST",
    "HYPGEOMDIST",
    "IF",
    "IFERROR",
    "IFNA",
    "IFS",
    "IMABS",
    "IMAGE",
    "IMAGINARY",
    "IMARGUMENT",
    "IMCONJUGATE",
    "IMCOS",
    "IMCOSH",
    "IMCOT",
    "IMCSC",
    "IMCSCH",
    "IMDIV",
    "IMEXP",
    "IMLN",
    "IMLOG10",
    "IMLOG2",
    "IMPOWER",
    "IMPRODUCT",
    "IMREAL",
    "IMSEC",
    "IMSECH",
    "IMSIN",
    "IMSINH",
    "IMSQRT",
    "IMSUB",
    "IMSUM",
    "IMTAN",
    "INDEX",
    "INT",
    "INTERCEPT",
    "INTRATE",
    "IPMT",
    "IRR",
    "ISBLANK",
    "ISERR",
    "ISERROR",
    "ISEVEN",
    "ISFORMULA",
    "ISLOGICAL",
    "ISNA",
    "ISNONTEXT",
    "ISNUMBER",
    "ISO.CEILING",
    "ISODD",
    "ISOMITTED",
    "ISOWEEKNUM",
    "ISPMT",
    "ISREF",
    "ISTEXT",
    "ISTHAIDIGIT",
    "JIS",
    "KURT",
    "LAMBDA",
    "LARGE",
    "LCM",
    "LEFT",
    "LEFTB",
    "LEN",
    "LENB",
    "LET",
    "LINEST",
    "LN",
    "LOG",
    "LOG10",
    "LOGEST",
    "LOGINV",
    "LOGNORM.DIST",
    "LOGNORM.INV",
    "LOGNORMDIST",
    "LOOKUP",
    "LOWER",
    "MAKEARRAY",
    "MAP",
    "MATCH",
    "MAX",
    "MAXA",
    "MAXIFS",
    "MDETERM",
    "MDURATION",
    "MEDIAN",
    "MID",
    "MIDB",
    "MIN",
    "MINA",
    "MINIFS",
    "MINUTE",
    "MINVERSE",
    "MIRR",
    "MMULT",
    "MOD",
    "MODE",
    "MODE.MULT",
    "MODE.SNGL",
    "MONTH",
    "MROUND",
    "MULTINOMIAL",
    "MUNIT",
    "N",
    "NA",
    "NEGBINOM.DIST",
    "NEGBINOMDIST",
    "NETWORKDAYS",
    "NETWORKDAYS.INTL",
    "NOMINAL",
    "NORM.DIST",
    "NORM.INV",
    "NORM.S.DIST",
    "NORM.S.INV",
    "NORMDIST",
    "NORMINV",
    "NORMSDIST",
    "NORMSINV",
    "NOT",
    "NPER",
    "NPV",
    "NUMBERVALUE",
    "OCT2BIN",
    "OCT2DEC",
    "OCT2HEX",
    "ODD",
    "ODDFPRICE",
    "ODDFYIELD",
    "ODDLPRICE",
    "ODDLYIELD",
    "OR",
    "PDURATION",
    "PEARSON",
    "PERCENTILE",
    "PERCENTILE.EXC",
    "PERCENTILE.INC",
    "PERCENTOF",
    "PERCENTRANK",
    "PERCENTRANK.EXC",
    "PERCENTRANK.INC",
    "PERMUT",
    "PERMUTATIONA",
    "PHI",
    "PHONETIC",
    "PI",
    "PIVOTBY",
    "PMT",
    "POISSON",
    "POISSON.DIST",
    "POWER",
    "PPMT",
    "PRICE",
    "PRICEDISC",
    "PRICEMAT",
    "PROB",
    "PRODUCT",
    "PROPER",
    "PV",
    "QUARTILE",
    "QUARTILE.EXC",
    "QUARTILE.INC",
    "QUOTIENT",
    "RADIANS",
    "RANK",
    "RANK.AVG",
    "RANK.EQ",
    "RATE",
    "RECEIVED",
    "REDUCE",
    "REGEXEXTRACT",
    "REGEXREPLACE",
    "REGEXTEST",
    "REPLACE",
    "REPLACEB",
    "REPT",
    "RIGHT",
    "RIGHTB",
    "ROMAN",
    "ROUND",
    "ROUNDBAHTDOWN",
    "ROUNDBAHTUP",
    "ROUNDDOWN",
    "ROUNDUP",
    "ROW",
    "ROWS",
    "RRI",
    "RSQ",
    "SCAN",
    "SEARCH",
    "SEARCHB",
    "SEC",
    "SECH",
    "SECOND",
    "SEQUENCE",
    "SERIESSUM",
    "SHEET",
    "SHEETS",
    "SIGN",
    "SIN",
    "SINGLE",
    "SINH",
    "SKEW",
    "SKEW.P",
    "SLN",
    "SLOPE",
    "SMALL",
    "SORT",
    "SORTBY",
    "SQRT",
    "SQRTPI",
    "STANDARDIZE",
    "STDEV",
    "STDEV.P",
    "STDEV.S",
    "STDEVA",
    "STDEVP",
    "STDEVPA",
    "STEYX",
    "SUBSTITUTE",
    "SUBTOTAL",
    "SUM",
    "SUMIFS",
    "SUMPRODUCT",
    "SUMSQ",
    "SUMX2MY2",
    "SUMX2PY2",
    "SUMXMY2",
    "SWITCH",
    "SYD",
    "T",
    "T.DIST",
    "T.DIST.2T",
    "T.DIST.RT",
    "T.INV",
    "T.INV.2T",
    "T.TEST",
    "TAKE",
    "TAN",
    "TANH",
    "TBILLEQ",
    "TBILLPRICE",
    "TBILLYIELD",
    "TDIST",
    "TEXT",
    "TEXTAFTER",
    "TEXTBEFORE",
    "TEXTJOIN",
    "TEXTSPLIT",
    "THAIDAYOFWEEK",
    "THAIDIGIT",
    "THAIMONTHOFYEAR",
    "THAINUMSOUND",
    "THAINUMSTRING",
    "THAISTRINGLENGTH",
    "THAIYEAR",
    "TIME",
    "TIMEVALUE",
    "TINV",
    "TOCOL",
    "TOROW",
    "TRANSPOSE",
    "TREND",
    "TRIM",
    "TRIMMEAN",
    "TRIMRANGE",
    "TRUE",
    "TRUNC",
    "TTEST",
    "TYPE",
    "UNICHAR",
    "UNICODE",
    "UNIQUE",
    "UPPER",
    "VALUE",
    "VALUETOTEXT",
    "VAR",
    "VAR.P",
    "VAR.S",
    "VARA",
    "VARP",
    "VARPA",
    "VDB",
    "VLOOKUP",
    "VSTACK",
    "WEEKDAY",
    "WEEKNUM",
    "WEIBULL",
    "WEIBULL.DIST",
    "WORKDAY",
    "WORKDAY.INTL",
    "WRAPCOLS",
    "WRAPROWS",
    "XIRR",
    "XLOOKUP",
    "XMATCH",
    "XNPV",
    "XOR",
    "YEAR",
    "YEARFRAC",
    "YIELD",
    "YIELDDISC",
    "YIELDMAT",
    "Z.TEST",
    "ZTEST",
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
    /// The workbook's other sheets (chart, dialog and macro sheets), lower-cased: their cells
    /// are never read, so one may hold the only volatile formula.
    unread: HashMap<String, usize>,
    /// Some formula reads a sheet in `unread`.
    unreadable: bool,
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
    /// More references than `max_refs`: the walk stops following them, and every formula
    /// counts as recalculated if any is volatile.
    overflow: bool,
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

    /// Counts `deps`' references, and drops them once past the cap.
    fn keep(&mut self, deps: &mut Deps) {
        self.refs += deps.areas.len() as u64;
        if self.refs > self.max_refs {
            self.overflow = true;
        }
        if self.overflow {
            deps.areas = Vec::new();
        }
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
        // Only worksheets: a reference into a chart, dialog or macro sheet, whose cells the
        // reader never reads, is not followed.
        sheets: sheets
            .iter()
            .map(|&(at, _)| (sheet_names[at].to_lowercase(), at))
            .collect(),
        unread: sheet_names
            .iter()
            .enumerate()
            .filter(|(at, _)| !sheets.iter().any(|&(read, _)| read == *at))
            .map(|(at, name)| (name.to_lowercase(), at))
            .collect(),
        unreadable: false,
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
            overflow: false,
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
        // A formula whose cached value is empty shows nothing, but is recalculated all the same.
        for (at, cell) in data.cells.iter().chain(&data.silent).enumerate() {
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
            resolver.work.keep(&mut deps);
            seeded |= deps.volatile;
            formulas.push(Formula {
                at: (sheet, cell.row, cell.col),
                deps,
            });
        }
    }
    seeded |= resolver.unreadable;
    let mut tainted = Tainted::default();
    if !seeded {
        return Ok(tainted.cells);
    }
    let overflow = resolver.work.overflow;

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
    let mut work = resolver.work;
    // Past the reference cap the walk cannot tell which formulas depend on a volatile one: all
    // do.
    if overflow {
        let mut queue = VecDeque::new();
        for formula in &formulas {
            tainted.add(formula.at, &ranges, &cells, &mut queue, &mut work)?;
        }
        return Ok(tainted.cells);
    }

    // Formulas by the single cells they read, and the ranges they read.
    let mut readers: HashMap<(usize, u32, u32), Vec<usize>> = HashMap::new();
    let mut areas: Vec<(usize, Merge, usize)> = Vec::new();
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
    // A cell a formula reads, written in the file or not: a recalculated array or data table
    // marks those of its cells.
    for &(sheet, row, col) in readers.keys() {
        cells.entry(sheet).or_default().insert((row, col));
    }

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
        work.step(areas.len() as u64)?;
        areas.retain(|&(_, _, id)| !done[id]);
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
    /// The ranges of recalculated arrays and data tables, kept whole: their cells need not be
    /// written in the file.
    ranges: Vec<(usize, Merge)>,
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
            let mut marked = 0u64;
            if let Some(read) = cells.get(&sheet) {
                for &(row, col) in read.range((range.top, range.left)..=(range.bottom, range.right))
                {
                    work.step(1)?;
                    if (range.left..=range.right).contains(&col) {
                        self.mark((sheet, row, col), queue);
                        marked += 1;
                    }
                }
            }
            // Kept whole only when the file leaves some of its cells out: a range every cell
            // of which is marked costs `within` nothing more.
            let size =
                u64::from(range.bottom - range.top + 1) * u64::from(range.right - range.left + 1);
            if marked < size {
                self.ranges.push((sheet, *range));
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

    /// Whether a recalculated cell or range lies in `area`, cells scanned along its shorter
    /// side.
    fn within(&self, sheet: usize, area: Merge, work: &mut Work) -> Result<bool, DocError> {
        work.step(self.ranges.len() as u64)?;
        if self.ranges.iter().any(|&(on, range)| {
            on == sheet
                && range.top <= area.bottom
                && area.top <= range.bottom
                && range.left <= area.right
                && area.left <= range.right
        }) {
            return Ok(true);
        }
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
    /// The worksheet a `Sheet!` prefix names; none for any other name, noting one the workbook
    /// has but the reader does not read.
    fn sheet(&mut self, name: &str) -> Option<usize> {
        let name = name.to_lowercase();
        let found = self.sheets.get(&name).copied();
        self.unreadable |= found.is_none() && self.unread.contains_key(&name);
        if let Some((first, last)) = name.split_once(':') {
            self.span(first, last);
        }
        found
    }

    /// A 3-D reference from sheet `first` to sheet `last`, in tab order: notes an unread sheet
    /// at either end or between them.
    fn span(&mut self, first: &str, last: &str) {
        let at = |name: &str| {
            let name = name.to_lowercase();
            self.sheets
                .get(&name)
                .or_else(|| self.unread.get(&name))
                .copied()
        };
        let (first_at, last_at) = (at(first), at(last));
        let (low, high) = match (first_at, last_at) {
            (Some(a), Some(b)) => (a.min(b), a.max(b)),
            (Some(a), None) | (None, Some(a)) => (a, a),
            (None, None) => return,
        };
        self.unreadable |= self.unread.values().any(|&at| (low..=high).contains(&at));
    }

    /// What `text` depends on, read at `place`; `depth` counts the names it is read through.
    fn read(&mut self, text: &str, place: Place, depth: u32) -> Result<Deps, DocError> {
        let chars: Vec<char> = text.chars().collect();
        self.work.step(1 + chars.len() as u64 / 64)?;
        let bound = bound_parameters(&chars);
        let mut deps = Deps::default();
        let mut at = 0;
        // The sheet a `Sheet!` prefix just named: none when it names no sheet of the workbook.
        let mut qualified: Option<Option<usize>> = None;
        // A `[1]!` prefix just named another workbook.
        let mut external = false;
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
                        qualified = Some(self.sheet(&name));
                    }
                }
                '[' => {
                    // A structured or external reference; `[1]!F(` calls a function of
                    // another workbook.
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
                    if chars.get(at) == Some(&'!') {
                        at += 1;
                        external = true;
                        continue;
                    }
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
                        qualified = Some(self.sheet(&token));
                        continue;
                    }
                    let sheet = qualified.take();
                    let external = std::mem::take(&mut external);
                    let mut next = at;
                    while chars.get(next).is_some_and(|c| " \r\n\t".contains(*c)) {
                        next += 1;
                    }
                    if chars.get(next) == Some(&'(') {
                        let bound = bound.contains(&token.to_uppercase());
                        self.call(&token, sheet, external, bound, place, depth, &mut deps)?;
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
                    // A `:` no reference took: a range with a function's result or a bracketed
                    // reference at an end, which the walk does not follow.
                    if c == ':' {
                        deps.unknown = true;
                    }
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
                // `First:Last!A1`: a 3-D reference, not followed.
                if chars.get(stop) == Some(&'!') {
                    self.span(&token, &other);
                }
                deps.unknown = true;
                return Ok(stop);
            };
            if chars.get(stop) == Some(&':') {
                deps.unknown = true;
            }
            match (on, place.shift) {
                (Some(on), Some(shift)) => {
                    if let (Some((r0, c0)), Some((r1, c1))) =
                        (a.shifted(shift, false), b.shifted(shift, true))
                    {
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
                    let (Some((r0, c0)), Some((r1, c1))) =
                        (a.shifted((0, 0), false), b.shifted((0, 0), true))
                    else {
                        return Ok(stop);
                    };
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
                    if let Some((row, col)) = a.shifted(shift, false) {
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
                    let Some((row, col)) = a.shifted((0, 0), false) else {
                        return Ok(end);
                    };
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
        {
            return Ok(end);
        }
        // A defined name: the sheet's own first, then the workbook's; a name spelled like a
        // `LET` or `LAMBDA` parameter is looked up too before it is read as one.
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
            _ if upper.starts_with(&PARAMETER_PREFIX.to_uppercase()) => {}
            _ => deps.unknown = true,
        }
        Ok(end)
    }

    /// A function called by `token`: volatile if Excel recalculates it at every recalculation,
    /// or if it is not one of Excel's own (another workbook's, an add-in's or a macro's, which
    /// may be); a defined name called as one (a named `LAMBDA`) is followed.
    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        token: &str,
        sheet: Option<Option<usize>>,
        external: bool,
        bound: bool,
        place: Place,
        depth: u32,
        deps: &mut Deps,
    ) -> Result<(), DocError> {
        let upper = token.to_uppercase();
        if bound {
            // A `LET` or `LAMBDA` parameter this same text binds, to a `LAMBDA` it holds.
            return Ok(());
        }
        let mut name = upper.as_str();
        while let Some(rest) = FUNCTION_PREFIXES
            .iter()
            .find_map(|prefix| name.strip_prefix(&prefix.to_uppercase()))
        {
            name = rest;
        }
        if external || sheet == Some(None) || is_volatile(name) {
            deps.volatile = true;
            return Ok(());
        }
        let on = sheet.flatten().or(place.sheet);
        let found = [on.map(Some), Some(None)]
            .into_iter()
            .flatten()
            .find_map(|scope| self.names.get(&(scope, name.to_owned())).copied());
        match found {
            Some(at) if !self.undecided => {
                let named = self.name(at, depth)?;
                self.work.step(named.areas.len() as u64)?;
                deps.absorb(&named);
            }
            _ if sheet.is_none() && BUILT_IN.binary_search(&name).is_ok() => {}
            _ => deps.volatile = true,
        }
        Ok(())
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
        let mut deps = deps;
        self.work.keep(&mut deps);
        self.states[at] = NameState::Read(deps.clone());
        Ok(deps)
    }
}

/// The `_xlpm.` names, upper-cased, that `chars` writes inside a `LET` or `LAMBDA` call and
/// not in call position (string literals, quoted sheet names and bracketed text left out): the
/// parameters such a call binds.
fn bound_parameters(chars: &[char]) -> HashSet<String> {
    let mut bound = HashSet::new();
    // The function each open parenthesis belongs to: whether it is a `LET` or `LAMBDA`.
    let mut calls: Vec<bool> = Vec::new();
    let mut at = 0;
    while let Some(&c) = chars.get(at) {
        match c {
            '"' | '\'' => {
                at += 1;
                while chars.get(at).is_some_and(|&next| next != c) {
                    at += 1;
                }
                at += 1;
            }
            '[' => {
                let mut open = 0u32;
                while let Some(&c) = chars.get(at) {
                    at += 1;
                    match c {
                        '[' => open += 1,
                        ']' => {
                            open = open.saturating_sub(1);
                            if open == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            '(' => {
                calls.push(false);
                at += 1;
            }
            ')' => {
                calls.pop();
                at += 1;
            }
            c if word(c) => {
                let start = at;
                while chars.get(at).is_some_and(|&c| word(c)) {
                    at += 1;
                }
                let token: String = chars[start..at].iter().collect::<String>().to_uppercase();
                let mut next = at;
                while chars.get(next).is_some_and(|c| " \r\n\t".contains(*c)) {
                    next += 1;
                }
                if chars.get(next) == Some(&'(') {
                    let mut name = token.as_str();
                    while let Some(rest) = FUNCTION_PREFIXES
                        .iter()
                        .find_map(|prefix| name.strip_prefix(&prefix.to_uppercase()))
                    {
                        name = rest;
                    }
                    calls.push(matches!(name, "LET" | "LAMBDA"));
                    at = next + 1;
                } else if token.starts_with(&PARAMETER_PREFIX.to_uppercase())
                    && calls.iter().any(|&binds| binds)
                {
                    bound.insert(token);
                }
            }
            _ => at += 1,
        }
    }
    bound
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

    /// Shifted by `(rows, cols)` where relative; none when that leaves the grid (Excel shows
    /// `#REF!`, which depends on nothing). A whole column's missing row, and a whole row's
    /// missing column, is the grid's first on a range's first end and its last on the `last`.
    fn shifted(self, (rows, cols): (i64, i64), last: bool) -> Option<(u32, u32)> {
        let move_by = |part: Option<(u32, bool)>, by: i64, max: u32| match part {
            None => Some(if last { max } else { 1 }),
            Some((value, true)) => Some(value),
            Some((value, false)) => {
                let moved = i64::from(value) + by;
                (1..=i64::from(max))
                    .contains(&moved)
                    .then(|| u32::try_from(moved).ok())
                    .flatten()
            }
        };
        Some((
            move_by(self.row, rows, MAX_ROW)?,
            move_by(self.col, cols, MAX_COLUMN)?,
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

#[cfg(test)]
mod tests {
    use super::{BUILT_IN, VOLATILE, is_volatile};

    #[test]
    fn function_lists_are_sorted_unique_and_apart() {
        for list in [BUILT_IN, VOLATILE] {
            assert!(list.windows(2).all(|pair| pair[0] < pair[1]), "{list:?}");
        }
        assert!(BUILT_IN.iter().all(|name| !is_volatile(name)));
        // Functions that may refresh from outside the workbook, named one by one.
        for name in [
            "SUMIF",
            "RTD",
            "WEBSERVICE",
            "STOCKHISTORY",
            "CUBEVALUE",
            "CUBESET",
            "CUBEKPIMEMBER",
        ] {
            assert!(is_volatile(name), "{name}");
        }
    }
}
