//! Buddhist-Era, Common-Era and fiscal dates, and their comparison by meaning.

use std::ops::Range;

use chrono::NaiveDate;

use crate::found::{
    View, char_len, check_len, digits_end, has_at, is_digit, is_latin_letter, is_thai,
};
use crate::{Flags, Found, TooLong};

/// The era a year was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Era {
    /// Buddhist Era, พ.ศ.: CE + 543.
    Be,
    /// Common Era, ค.ศ.
    Ce,
    /// Rattanakosin era, ร.ศ.: BE − 2324.
    Rattanakosin,
    /// Only for the Chula Sakarat era (จ.ศ.), which KatWork does not convert.
    Unknown,
}

/// What kind of year a period is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PeriodKind {
    Calendar,
    Fiscal,
    Academic,
    Unknown,
}

/// A date or period, with every year in CE except a `Year` of era `Unknown`, whose year is the
/// number as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateValue {
    Day {
        date: NaiveDate,
        era: Era,
    },
    Month {
        year: i32,
        month: u32,
        era: Era,
    },
    Year {
        year: i32,
        era: Era,
        period: PeriodKind,
    },
    /// A fiscal year or quarter: `label` is the CE year in which the fiscal year ends.
    Period {
        kind: PeriodKind,
        label: i32,
        start: NaiveDate,
        end: NaiveDate,
    },
}

/// What the scanners know about the document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ctx {
    /// The document's date when known; two-digit years are read in the century nearest it.
    /// `None` uses today's UTC date.
    pub anchor: Option<NaiveDate>,
}

/// How two dates or periods relate in meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PeriodCmp {
    /// The same day, month, year or period.
    Same,
    /// The same kind of value, another day, month, year or period.
    Different,
    /// The same years in two known kinds, such as a fiscal and a calendar year.
    KindDiffers,
    /// One side does not say which kind of year it is.
    Ambiguous,
    /// Values that do not compare: different granularities, or an unconverted era.
    Incomparable,
}

/// BE = CE + 543.
const BE_OFFSET: i32 = 543;
/// Rattanakosin year N is BE N + 2324.
const RATTANAKOSIN_OFFSET: i32 = 2324;
/// The Thai year began on 1 April before BE 2484 (1941).
const FIRST_JANUARY_YEAR: i32 = 2484;

const CONF_MARKED: f32 = 0.9;
const CONF_NUMERIC: f32 = 0.6;
const CONF_FISCAL: f32 = 0.85;
const CONF_ACADEMIC: f32 = 0.5;
const CONF_OTHER_ERA: f32 = 0.5;
const CONF_TWO_DIGIT: f32 = 0.6;
const CONF_PRE_1941: f32 = 0.7;

// Markers, as normalize_th writes them.
const PI: &str = "\u{0E1B}\u{0E35}"; // ปี
const FISCAL: &str =
    "\u{0E1B}\u{0E35}\u{0E07}\u{0E1A}\u{0E1B}\u{0E23}\u{0E30}\u{0E21}\u{0E32}\u{0E13}"; // ปีงบประมาณ
const FISCAL_SHORT: &str = "\u{0E1B}\u{0E35}\u{0E07}\u{0E1A}"; // ปีงบ
const QUARTER: &str = "\u{0E44}\u{0E15}\u{0E23}\u{0E21}\u{0E32}\u{0E2A}"; // ไตรมาส
const THI: &str = "\u{0E17}\u{0E35}\u{0E48}"; // ที่
const KHONG: &str = "\u{0E02}\u{0E2D}\u{0E07}"; // ของ

/// Year markers other than the fiscal ones, longest first.
const YEAR_MARKERS: [(&str, PeriodKind); 8] = [
    (
        "\u{0E23}\u{0E2D}\u{0E1A}\u{0E1B}\u{0E35}\u{0E1A}\u{0E31}\u{0E0D}\u{0E0A}\u{0E35}",
        PeriodKind::Unknown,
    ), // รอบปีบัญชี
    (
        "\u{0E1B}\u{0E35}\u{0E01}\u{0E32}\u{0E23}\u{0E28}\u{0E36}\u{0E01}\u{0E29}\u{0E32}",
        PeriodKind::Academic,
    ), // ปีการศึกษา
    (
        "\u{0E1B}\u{0E35}\u{0E1B}\u{0E0F}\u{0E34}\u{0E17}\u{0E34}\u{0E19}",
        PeriodKind::Calendar,
    ), // ปีปฏิทิน
    (
        "\u{0E1B}\u{0E35}\u{0E1A}\u{0E31}\u{0E0D}\u{0E0A}\u{0E35}",
        PeriodKind::Unknown,
    ), // ปีบัญชี
    (
        "\u{0E1B}\u{0E35}\u{0E20}\u{0E32}\u{0E29}\u{0E35}",
        PeriodKind::Unknown,
    ), // ปีภาษี
    (PI, PeriodKind::Unknown),
    ("calendar year", PeriodKind::Calendar),
    ("cy", PeriodKind::Calendar),
];

/// Thai era markers, written before the year.
const THAI_ERAS: [(&str, Era); 8] = [
    ("\u{0E1E}.\u{0E28}.", Era::Be),           // พ.ศ.
    ("\u{0E1E}.\u{0E28}", Era::Be),            // พ.ศ
    ("\u{0E1E}\u{0E28}.", Era::Be),            // พศ.
    ("\u{0E04}.\u{0E28}.", Era::Ce),           // ค.ศ.
    ("\u{0E04}.\u{0E28}", Era::Ce),            // ค.ศ
    ("\u{0E04}\u{0E28}.", Era::Ce),            // คศ.
    ("\u{0E23}.\u{0E28}.", Era::Rattanakosin), // ร.ศ.
    ("\u{0E08}.\u{0E28}.", Era::Unknown),      // จ.ศ.
];

/// Latin era markers, before or after the year, as they must be written in the input: in
/// capitals, because `be` and `ad` are English words.
const LATIN_ERAS: [(&str, &str, Era); 4] = [
    ("b.e.", "B.E.", Era::Be),
    ("be", "BE", Era::Be),
    ("ad", "AD", Era::Ce),
    ("ce", "CE", Era::Ce),
];

/// Thai month names in full and truncated, as normalize_th writes them.
const THAI_MONTHS: [(&str, &str); 12] = [
    (
        "\u{0E21}\u{0E01}\u{0E23}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E21}\u{0E01}\u{0E23}\u{0E32}",
    ), // มกราคม มกรา
    (
        "\u{0E01}\u{0E38}\u{0E21}\u{0E20}\u{0E32}\u{0E1E}\u{0E31}\u{0E19}\u{0E18}\u{0E4C}",
        "\u{0E01}\u{0E38}\u{0E21}\u{0E20}\u{0E32}",
    ), // กุมภาพันธ์ กุมภา
    (
        "\u{0E21}\u{0E35}\u{0E19}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E21}\u{0E35}\u{0E19}\u{0E32}",
    ), // มีนาคม มีนา
    (
        "\u{0E40}\u{0E21}\u{0E29}\u{0E32}\u{0E22}\u{0E19}",
        "\u{0E40}\u{0E21}\u{0E29}\u{0E32}",
    ), // เมษายน เมษา
    (
        "\u{0E1E}\u{0E24}\u{0E29}\u{0E20}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E1E}\u{0E24}\u{0E29}\u{0E20}\u{0E32}",
    ), // พฤษภาคม พฤษภา
    (
        "\u{0E21}\u{0E34}\u{0E16}\u{0E38}\u{0E19}\u{0E32}\u{0E22}\u{0E19}",
        "\u{0E21}\u{0E34}\u{0E16}\u{0E38}\u{0E19}\u{0E32}",
    ), // มิถุนายน มิถุนา
    (
        "\u{0E01}\u{0E23}\u{0E01}\u{0E0E}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E01}\u{0E23}\u{0E01}\u{0E0E}\u{0E32}",
    ), // กรกฎาคม กรกฎา
    (
        "\u{0E2A}\u{0E34}\u{0E07}\u{0E2B}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E2A}\u{0E34}\u{0E07}\u{0E2B}\u{0E32}",
    ), // สิงหาคม สิงหา
    (
        "\u{0E01}\u{0E31}\u{0E19}\u{0E22}\u{0E32}\u{0E22}\u{0E19}",
        "\u{0E01}\u{0E31}\u{0E19}\u{0E22}\u{0E32}",
    ), // กันยายน กันยา
    (
        "\u{0E15}\u{0E38}\u{0E25}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E15}\u{0E38}\u{0E25}\u{0E32}",
    ), // ตุลาคม ตุลา
    (
        "\u{0E1E}\u{0E24}\u{0E28}\u{0E08}\u{0E34}\u{0E01}\u{0E32}\u{0E22}\u{0E19}",
        "\u{0E1E}\u{0E24}\u{0E28}\u{0E08}\u{0E34}\u{0E01}\u{0E32}",
    ), // พฤศจิกายน พฤศจิกา
    (
        "\u{0E18}\u{0E31}\u{0E19}\u{0E27}\u{0E32}\u{0E04}\u{0E21}",
        "\u{0E18}\u{0E31}\u{0E19}\u{0E27}\u{0E32}",
    ), // ธันวาคม ธันวา
];

/// The two parts of each abbreviated Thai month: ม.ค. is (ม, ค).
const THAI_ABBREVIATIONS: [(&str, &str); 12] = [
    ("\u{0E21}", "\u{0E04}"),         // ม.ค.
    ("\u{0E01}", "\u{0E1E}"),         // ก.พ.
    ("\u{0E21}\u{0E35}", "\u{0E04}"), // มี.ค.
    ("\u{0E40}\u{0E21}", "\u{0E22}"), // เม.ย.
    ("\u{0E1E}", "\u{0E04}"),         // พ.ค.
    ("\u{0E21}\u{0E34}", "\u{0E22}"), // มิ.ย.
    ("\u{0E01}", "\u{0E04}"),         // ก.ค.
    ("\u{0E2A}", "\u{0E04}"),         // ส.ค.
    ("\u{0E01}", "\u{0E22}"),         // ก.ย.
    ("\u{0E15}", "\u{0E04}"),         // ต.ค.
    ("\u{0E1E}", "\u{0E22}"),         // พ.ย.
    ("\u{0E18}", "\u{0E04}"),         // ธ.ค.
];

const ENGLISH_MONTHS: [(&str, &str); 12] = [
    ("january", "jan"),
    ("february", "feb"),
    ("march", "mar"),
    ("april", "apr"),
    ("may", "may"),
    ("june", "jun"),
    ("july", "jul"),
    ("august", "aug"),
    ("september", "sep"),
    ("october", "oct"),
    ("november", "nov"),
    ("december", "dec"),
];

/// A date the scanner read, in normalized code points.
pub(crate) struct Hit {
    pub(crate) range: Range<usize>,
    value: DateValue,
    conf: f32,
    flags: Flags,
    alts: Vec<(DateValue, f32)>,
}

/// What `find_numbers` needs from the dates: their spans, the day numbers written before a
/// month name with no year, and every month name.
pub(crate) struct Scan {
    pub(crate) hits: Vec<Hit>,
    pub(crate) day_parts: Vec<Range<usize>>,
    pub(crate) months: Vec<Range<usize>>,
}

/// Finds dates and periods in `text`.
///
/// A four-digit year in 2400–2600 is BE and in 1800–2100 CE (`ERA_INFERRED`); with an explicit
/// era marker (พ.ศ., ค.ศ., ร.ศ., จ.ศ. and their spellings before the year; `BE`, `B.E.`, `AD`,
/// `CE` in capitals before or after it) any year from 1000 to 9999 is read in that era. A year
/// is a date only with a marker (ปี and its compounds, `FY`, `CY`, `calendar year`, an era), a
/// month name or a numeric day and month; `2,567` with a separator never is. A two-digit year
/// is BE 25yy or CE 20yy, whichever is nearer the anchor in days (BE on a tie), with the other
/// in `alts`. Thai month names (full, abbreviated with or without dots, truncated) and English
/// ones count only as whole tokens next to a day or a year. Numeric dates are day first
/// (`d/m/y`, `d.m.y`, `d-m-y`), or year first as `yyyy-mm-dd` and `yyyy/mm/dd`. A date is
/// checked after conversion to CE, so 29 February needs a CE leap year.
///
/// A fiscal year runs 1 October to 30 September and is named by the year it ends in, which is
/// the Thai government's year (Budget Procedure Act B.E. 2561, section 4); a company's fiscal
/// year need not follow it. Its quarters start in October. A quarter with no fiscal year beside
/// it is not read.
///
/// Confidence: 0.9 for a marked four-digit year or a date with a month name, and for a numeric
/// date that is year first or whose day is above 12; 0.6 for other numeric dates and every
/// two-digit year; 0.85 for fiscal years and quarters; 0.5 for academic years and the ร.ศ. and
/// จ.ศ. eras; at most 0.7 for a BE year before 2484 (`PRE_1941`). The lowest rule that applies
/// wins.
///
/// The text is read through [`crate::normalize_th`], and Latin era markers are matched in the
/// original capitals. Spans are code points of `text`; a text longer than
/// [`crate::MAX_SCAN_CODE_POINTS`] is refused whole.
pub fn find_dates(text: &str, ctx: &Ctx) -> Result<Vec<Found<DateValue>>, TooLong> {
    check_len(text)?;
    let view = View::new(text);
    let scan = scan(&view, anchor(ctx));
    Ok(scan
        .hits
        .into_iter()
        .map(|hit| view.found(&hit.range, hit.value, hit.alts, hit.conf, hit.flags))
        .collect())
}

pub(crate) fn anchor(ctx: &Ctx) -> NaiveDate {
    ctx.anchor
        .unwrap_or_else(|| chrono::Utc::now().date_naive())
}

pub(crate) fn scan(view: &View, anchor: NaiveDate) -> Scan {
    let mut scanner = Scanner {
        view,
        t: &view.chars,
        anchor,
        out: Scan {
            hits: Vec::new(),
            day_parts: Vec::new(),
            months: Vec::new(),
        },
    };
    let mut at = 0;
    while at < scanner.t.len() {
        at = scanner.at(at).unwrap_or(at + 1);
    }
    let mut at = 0;
    while at < scanner.t.len() {
        match scanner.month_at(at) {
            Some((_, end)) => {
                scanner.out.months.push(at..end);
                at = end;
            }
            None => at += 1,
        }
    }
    scanner.out
}

struct Scanner<'v> {
    view: &'v View,
    t: &'v [char],
    anchor: NaiveDate,
    out: Scan,
}

/// A year as written, with the marker that gave its era.
#[derive(Clone, Copy)]
struct YearToken {
    written: i32,
    digits: usize,
    end: usize,
}

/// A reading of a year in one pattern: its value, confidence, flags and the other reading.
struct Reading {
    value: DateValue,
    conf: f32,
    flags: Flags,
    alts: Vec<(DateValue, f32)>,
}

impl Scanner<'_> {
    fn get(&self, at: usize) -> Option<char> {
        self.t.get(at).copied()
    }

    /// `at` with one space skipped.
    fn space(&self, at: usize) -> usize {
        if self.get(at) == Some(' ') {
            at + 1
        } else {
            at
        }
    }

    fn at(&mut self, at: usize) -> Option<usize> {
        let c = self.get(at)?;
        if is_digit(c) {
            return self.day_first(at).or_else(|| self.year_then_era(at));
        }
        self.quarter_first(at)
            .or_else(|| self.fiscal_first(at))
            .or_else(|| self.marked_year(at))
            .or_else(|| self.era_first(at))
            .or_else(|| self.month_first(at))
    }

    fn push(&mut self, range: Range<usize>, reading: Reading) {
        self.out.hits.push(Hit {
            range,
            value: reading.value,
            conf: reading.conf,
            flags: reading.flags,
            alts: reading.alts,
        });
    }

    /// A Latin word at `at`: no letter right before it.
    fn latin_start(&self, at: usize) -> bool {
        at.checked_sub(1)
            .and_then(|prev| self.get(prev))
            .is_none_or(|prev| !is_latin_letter(prev))
    }

    /// No Latin letter at `at`.
    fn latin_end(&self, at: usize) -> bool {
        self.get(at).is_none_or(|next| !is_latin_letter(next))
    }

    /// A digit run that starts at `at` and is not part of a longer figure.
    fn number(&self, at: usize) -> Option<(u32, usize, usize)> {
        if !self.get(at).is_some_and(is_digit) {
            return None;
        }
        let before = at.checked_sub(1).and_then(|prev| self.get(prev));
        let before_that = at.checked_sub(2).and_then(|prev| self.get(prev));
        if before.is_some_and(is_digit)
            || (matches!(before, Some(',' | '.')) && before_that.is_some_and(is_digit))
        {
            return None;
        }
        let (value, end) = self.field(at, 1..=4)?;
        Some((value, end - at, end))
    }

    /// A run of digits at `at` whose length is in `len`: its value and where it ends.
    fn field(&self, at: usize, len: std::ops::RangeInclusive<usize>) -> Option<(u32, usize)> {
        let end = digits_end(self.t, at);
        if !len.contains(&(end - at)) {
            return None;
        }
        Some((
            self.t[at..end]
                .iter()
                .fold(0, |value, c| value * 10 + c.to_digit(10).unwrap_or(0)),
            end,
        ))
    }

    /// A year of `digits` digits at `at` that no thousands separator or decimal point continues.
    fn year(&self, at: usize, digits: &[usize]) -> Option<YearToken> {
        let (value, len, end) = self.number(at)?;
        if !digits.contains(&len) {
            return None;
        }
        let after = self.get(end);
        let continued = |sep: char, count: usize| {
            after == Some(sep)
                && digits_end(self.t, end + 1) - (end + 1) >= count
                && self.get(end + 1).is_some_and(is_digit)
        };
        if continued(',', 3) || continued('.', 1) {
            return None;
        }
        Some(YearToken {
            written: i32::try_from(value).ok()?,
            digits: len,
            end,
        })
    }

    /// A Thai era marker at `at`, with the position after it and one space.
    fn thai_era(&self, at: usize) -> Option<(Era, usize)> {
        THAI_ERAS
            .iter()
            .find(|(marker, _)| has_at(self.t, at, marker))
            .map(|(marker, era)| (*era, self.space(at + char_len(marker))))
    }

    /// A Latin era marker at `at`, written in capitals in the input.
    fn latin_era(&self, at: usize) -> Option<(Era, usize)> {
        if !self.latin_start(at) {
            return None;
        }
        LATIN_ERAS.iter().find_map(|(marker, written, era)| {
            let end = at + char_len(marker);
            (has_at(self.t, at, marker)
                && self.latin_end(end)
                && self.view.written(&(at..end)) == *written)
                .then_some((*era, end))
        })
    }

    /// An era marker of either script before a year.
    fn era_before(&self, at: usize) -> Option<(Era, usize)> {
        self.thai_era(at)
            .or_else(|| self.latin_era(at).map(|(era, end)| (era, self.space(end))))
    }

    /// An optional era marker and then a year, from `at`.
    fn era_and_year(&self, at: usize) -> Option<(Option<Era>, YearToken)> {
        if let Some((era, after)) = self.era_before(at) {
            let digits: &[usize] = if matches!(era, Era::Rattanakosin | Era::Unknown) {
                &[1, 2, 3, 4]
            } else {
                &[2, 4]
            };
            return self.year(after, digits).map(|year| (Some(era), year));
        }
        self.year(at, &[2, 4]).map(|year| (None, year))
    }

    /// Reads `year` in its era and builds the value with it; `None` when it is not a year.
    fn read(
        &self,
        year: YearToken,
        era: Option<Era>,
        base: f32,
        build: impl Fn(i32, Era) -> Option<DateValue>,
    ) -> Option<Reading> {
        let mut flags = Flags::default();
        let mut conf = base;
        let (ce, era, be) = match era {
            Some(Era::Rattanakosin) => {
                let be = year.written + RATTANAKOSIN_OFFSET;
                flags |= Flags::OTHER_ERA;
                conf = conf.min(CONF_OTHER_ERA);
                (be - BE_OFFSET, Era::Rattanakosin, Some(be))
            }
            Some(Era::Unknown) => return None,
            Some(era) if year.digits == 2 => {
                flags |= Flags::TWO_DIGIT_YEAR;
                conf = conf.min(CONF_TWO_DIGIT);
                match era {
                    Era::Be => (
                        2500 + year.written - BE_OFFSET,
                        era,
                        Some(2500 + year.written),
                    ),
                    _ => (2000 + year.written, era, None),
                }
            }
            Some(era) if (1000..=9999).contains(&year.written) => match era {
                Era::Be => (year.written - BE_OFFSET, era, Some(year.written)),
                _ => (year.written, era, None),
            },
            Some(_) => return None,
            None if year.digits == 2 => return self.two_digit(year, base, build),
            None if (2400..=2600).contains(&year.written) => {
                flags |= Flags::ERA_INFERRED;
                (year.written - BE_OFFSET, Era::Be, Some(year.written))
            }
            None if (1800..=2100).contains(&year.written) => {
                flags |= Flags::ERA_INFERRED;
                (year.written, Era::Ce, None)
            }
            None => return None,
        };
        if be.is_some_and(|be| be < FIRST_JANUARY_YEAR) {
            flags |= Flags::PRE_1941;
            conf = conf.min(CONF_PRE_1941);
        }
        Some(Reading {
            value: build(ce, era)?,
            conf,
            flags,
            alts: Vec::new(),
        })
    }

    /// A two-digit year: BE 25yy or CE 20yy, whichever is nearer the anchor in days, BE on a tie.
    fn two_digit(
        &self,
        year: YearToken,
        base: f32,
        build: impl Fn(i32, Era) -> Option<DateValue>,
    ) -> Option<Reading> {
        let be = build(2500 + year.written - BE_OFFSET, Era::Be);
        let ce = build(2000 + year.written, Era::Ce);
        let (value, other) = match (be, ce) {
            (Some(be), Some(ce)) if self.distance(&ce) < self.distance(&be) => (ce, Some(be)),
            (Some(be), ce) => (be, ce),
            (None, Some(ce)) => (ce, None),
            (None, None) => return None,
        };
        let conf = base.min(CONF_TWO_DIGIT);
        Some(Reading {
            value,
            conf,
            flags: Flags::TWO_DIGIT_YEAR | Flags::ERA_INFERRED,
            alts: other.map(|other| (other, 1.0 - conf)).into_iter().collect(),
        })
    }

    /// Days from the anchor to the nearest day of `value`.
    fn distance(&self, value: &DateValue) -> i64 {
        let Some((first, last)) = days_of(value) else {
            return i64::MAX;
        };
        if self.anchor < first {
            (first - self.anchor).num_days()
        } else if self.anchor > last {
            (self.anchor - last).num_days()
        } else {
            0
        }
    }

    /// `ไตรมาส N [ของ] <fiscal year>` and `QN FY <year>`.
    fn quarter_first(&mut self, at: usize) -> Option<usize> {
        let after_marker = if has_at(self.t, at, QUARTER) {
            let mut next = at + char_len(QUARTER);
            if has_at(self.t, next, THI) {
                next += char_len(THI);
            }
            self.space(next)
        } else if self.get(at) == Some('q') && self.latin_start(at) {
            at + 1
        } else {
            return None;
        };
        let quarter = self.quarter_number(after_marker)?;
        let mut next = self.space(after_marker + 1);
        if has_at(self.t, next, KHONG) {
            next = self.space(next + char_len(KHONG));
        }
        let (era, year) = self.fiscal_year(next)?;
        let reading = self.read(year, era, CONF_FISCAL, |ce, _| quarter_period(ce, quarter))?;
        self.push(at..year.end, with_flags(reading, Flags::FISCAL));
        Some(year.end)
    }

    fn quarter_number(&self, at: usize) -> Option<u32> {
        let c = self.get(at)?;
        let quarter = c.to_digit(10).filter(|q| (1..=4).contains(q))?;
        (!self.get(at + 1).is_some_and(is_digit)).then_some(quarter)
    }

    /// `ปีงบประมาณ`, `ปีงบ` or `FY`, an optional era marker, and a year.
    fn fiscal_year(&self, at: usize) -> Option<(Option<Era>, YearToken)> {
        let after = if has_at(self.t, at, FISCAL) {
            self.space(at + char_len(FISCAL))
        } else if has_at(self.t, at, FISCAL_SHORT) {
            self.space(at + char_len(FISCAL_SHORT))
        } else if has_at(self.t, at, "fy") && self.latin_start(at) {
            self.space(at + 2)
        } else {
            return None;
        };
        self.era_and_year(after)
    }

    /// A fiscal year, and a quarter written after it: `ปีงบประมาณ 2569 ไตรมาส 4`.
    fn fiscal_first(&mut self, at: usize) -> Option<usize> {
        let (era, year) = self.fiscal_year(at)?;
        let after = self.space(year.end);
        if has_at(self.t, after, QUARTER) {
            let mut next = after + char_len(QUARTER);
            if has_at(self.t, next, THI) {
                next += char_len(THI);
            }
            let next = self.space(next);
            if let Some(quarter) = self.quarter_number(next) {
                if let Some(reading) =
                    self.read(year, era, CONF_FISCAL, |ce, _| quarter_period(ce, quarter))
                {
                    self.push(at..next + 1, with_flags(reading, Flags::FISCAL));
                }
                return Some(next + 1);
            }
        }
        if let Some(reading) = self.read(year, era, CONF_FISCAL, |ce, _| fiscal_period(ce)) {
            self.push(at..year.end, with_flags(reading, Flags::FISCAL));
        }
        Some(year.end)
    }

    /// `ปี`, `ปีปฏิทิน`, `ปีการศึกษา`, `ปีบัญชี`, `ปีภาษี`, `รอบปีบัญชี`, `calendar year` or
    /// `CY`, an optional era marker, and a year.
    fn marked_year(&mut self, at: usize) -> Option<usize> {
        let (marker, period) = YEAR_MARKERS
            .iter()
            .find(|(marker, _)| has_at(self.t, at, marker))?;
        if is_latin_letter(marker.chars().next()?) && !self.latin_start(at) {
            return None;
        }
        let (era, year) = self.era_and_year(self.space(at + char_len(marker)))?;
        let base = if *period == PeriodKind::Academic {
            CONF_ACADEMIC
        } else {
            CONF_MARKED
        };
        let reading = if era == Some(Era::Unknown) {
            chula(year)
        } else {
            self.read(year, era, base, |ce, era| {
                Some(DateValue::Year {
                    year: ce,
                    era,
                    period: *period,
                })
            })?
        };
        self.push(at..year.end, reading);
        Some(year.end)
    }

    /// An era marker and a year: `พ.ศ. 2567`, `ร.ศ. 130`, `จ.ศ. 1387`, `B.E. 2567`, `AD 2024`.
    fn era_first(&mut self, at: usize) -> Option<usize> {
        self.era_before(at)?;
        let (era, year) = self.era_and_year(at)?;
        let reading = if era == Some(Era::Unknown) {
            chula(year)
        } else {
            self.read(year, era, CONF_MARKED, |ce, era| {
                Some(DateValue::Year {
                    year: ce,
                    era,
                    period: PeriodKind::Unknown,
                })
            })?
        };
        self.push(at..year.end, reading);
        Some(year.end)
    }

    /// A year and a Latin era marker after it: `2567 B.E.`, `2024 AD`.
    fn year_then_era(&mut self, at: usize) -> Option<usize> {
        let year = self.year(at, &[2, 4])?;
        let (era, end) = self.latin_era(self.space(year.end))?;
        let reading = self.read(year, Some(era), CONF_MARKED, |ce, era| {
            Some(DateValue::Year {
                year: ce,
                era,
                period: PeriodKind::Unknown,
            })
        })?;
        self.push(at..end, reading);
        Some(end)
    }

    /// A day, then a month name or a numeric month, then a year.
    fn day_first(&mut self, at: usize) -> Option<usize> {
        let (day, digits, end) = self.number(at)?;
        if matches!(self.get(end), Some('/' | '.' | '-')) {
            return self.numeric(at);
        }
        if digits > 2 {
            return None;
        }
        let month_at = self.space(end);
        let (month, month_end) = self.month_at(month_at)?;
        let english = self.get(month_at).is_some_and(is_latin_letter);
        let year_at = self.space(month_end);
        let year = if english {
            self.year(year_at, &[2, 4]).map(|year| (None, year))
        } else {
            self.era_and_year(year_at)
        };
        let Some((era, year)) = year else {
            self.out.day_parts.push(at..end);
            return Some(month_end);
        };
        if era == Some(Era::Unknown) {
            return Some(year.end);
        }
        if let Some(reading) = self.read(year, era, CONF_MARKED, |ce, era| {
            NaiveDate::from_ymd_opt(ce, month, day).map(|date| DateValue::Day { date, era })
        }) {
            self.push(at..year.end, reading);
        }
        Some(year.end)
    }

    /// `d/m/y`, `d.m.y` and `d-m-y` with a two- or four-digit year, or `yyyy-mm-dd` and
    /// `yyyy/mm/dd`.
    fn numeric(&mut self, at: usize) -> Option<usize> {
        let (first, first_digits, first_end) = self.number(at)?;
        let sep = self.get(first_end)?;
        let (second, second_end) = self.field(first_end + 1, 1..=2)?;
        if self.get(second_end) != Some(sep) {
            return None;
        }
        let third_at = second_end + 1;
        let third_end = digits_end(self.t, third_at);
        let continued = matches!(self.get(third_end), Some('/' | '.' | '-' | ','))
            && self.get(third_end + 1).is_some_and(is_digit);
        if continued {
            return None;
        }
        let (third, _) = self.field(third_at, 1..=4)?;
        let third_digits = third_end - third_at;
        let (day, month, year, base) =
            if first_digits == 4 && sep != '.' && (1..=2).contains(&third_digits) {
                (
                    third,
                    second,
                    YearToken {
                        written: i32::try_from(first).ok()?,
                        digits: 4,
                        end: first_end,
                    },
                    CONF_MARKED,
                )
            } else if (1..=2).contains(&first_digits) && matches!(third_digits, 2 | 4) {
                let base = if first > 12 {
                    CONF_MARKED
                } else {
                    CONF_NUMERIC
                };
                (
                    first,
                    second,
                    YearToken {
                        written: i32::try_from(third).ok()?,
                        digits: third_digits,
                        end: third_end,
                    },
                    base,
                )
            } else {
                return None;
            };
        if let Some(reading) = self.read(year, None, base, |ce, era| {
            NaiveDate::from_ymd_opt(ce, month, day).map(|date| DateValue::Day { date, era })
        }) {
            self.push(at..third_end, reading);
        }
        Some(third_end)
    }

    /// A month name, then an optional era marker and a year; or, in English, a day and a year.
    fn month_first(&mut self, at: usize) -> Option<usize> {
        let (month, month_end) = self.month_at(at)?;
        let next = self.space(month_end);
        if self.get(at).is_some_and(is_latin_letter) {
            if let Some((day, _, day_end)) = self.number(next).filter(|(_, digits, _)| *digits <= 2)
            {
                let mut year_at = day_end;
                if self.get(year_at) == Some(',') {
                    year_at += 1;
                }
                let year_at = self.space(year_at);
                return match self.year(year_at, &[4]) {
                    Some(year) => {
                        if let Some(reading) = self.read(year, None, CONF_MARKED, |ce, era| {
                            NaiveDate::from_ymd_opt(ce, month, day)
                                .map(|date| DateValue::Day { date, era })
                        }) {
                            self.push(at..year.end, reading);
                        }
                        Some(year.end)
                    }
                    None => {
                        self.out.day_parts.push(next..day_end);
                        Some(day_end)
                    }
                };
            }
            let year = self.year(next, &[4])?;
            let reading = self.read(year, None, CONF_MARKED, |ce, era| {
                Some(DateValue::Month {
                    year: ce,
                    month,
                    era,
                })
            })?;
            self.push(at..year.end, reading);
            return Some(year.end);
        }
        let (era, year) = self.era_and_year(next)?;
        if era == Some(Era::Unknown) {
            return Some(year.end);
        }
        if let Some(reading) = self.read(year, era, CONF_MARKED, |ce, era| {
            Some(DateValue::Month {
                year: ce,
                month,
                era,
            })
        }) {
            self.push(at..year.end, reading);
        }
        Some(year.end)
    }

    /// A month name at `at` as a whole token, longest alias first: its month and end.
    pub(crate) fn month_at(&self, at: usize) -> Option<(u32, usize)> {
        let c = self.get(at)?;
        if is_latin_letter(c) {
            if !self.latin_start(at) {
                return None;
            }
            return (1..)
                .zip(ENGLISH_MONTHS)
                .find_map(|(month, (full, short))| {
                    let mut found = None;
                    for alias in [full, "sept", short] {
                        if alias == "sept" && month != 9 {
                            continue;
                        }
                        if has_at(self.t, at, alias) {
                            let mut end = at + char_len(alias);
                            if alias != full && self.get(end) == Some('.') {
                                end += 1;
                            }
                            if self.latin_end(end) {
                                found = Some((month, end));
                                break;
                            }
                        }
                    }
                    found
                });
        }
        if !is_thai(c) {
            return None;
        }
        let thai_end = |end: usize| self.get(end).is_none_or(|next| !is_thai(next));
        for (month, (full, truncated)) in (1..).zip(THAI_MONTHS) {
            for name in [full, truncated] {
                let end = at + char_len(name);
                if has_at(self.t, at, name) && thai_end(end) {
                    return Some((month, end));
                }
            }
        }
        let before = at.checked_sub(1).and_then(|prev| self.get(prev));
        if before.is_some_and(|prev| is_thai(prev) || prev == '.') {
            return None;
        }
        for (month, (first, second)) in (1..).zip(THAI_ABBREVIATIONS) {
            if let Some(end) = self.abbreviation(at, first, second)
                && thai_end(end)
            {
                return Some((month, end));
            }
        }
        None
    }

    /// `ม.ค.`, `ม.ค`, `ม. ค.`, `ม. ค` or `มค` at `at`: where it ends.
    fn abbreviation(&self, at: usize, first: &str, second: &str) -> Option<usize> {
        if !has_at(self.t, at, first) {
            return None;
        }
        let mut next = at + char_len(first);
        if self.get(next) == Some('.') {
            next = self.space(next + 1);
            if !has_at(self.t, next, second) {
                return None;
            }
            next += char_len(second);
            if self.get(next) == Some('.') {
                next += 1;
            }
            return Some(next);
        }
        has_at(self.t, next, second).then_some(next + char_len(second))
    }
}

fn with_flags(mut reading: Reading, flags: Flags) -> Reading {
    reading.flags |= flags;
    reading
}

/// `จ.ศ. N`: kept as written, because KatWork does not convert the Chula Sakarat era.
fn chula(year: YearToken) -> Reading {
    Reading {
        value: DateValue::Year {
            year: year.written,
            era: Era::Unknown,
            period: PeriodKind::Unknown,
        },
        conf: CONF_OTHER_ERA,
        flags: Flags::OTHER_ERA,
        alts: Vec::new(),
    }
}

/// The fiscal year named by the CE year it ends in: 1 October of the year before to 30 September.
fn fiscal_period(label: i32) -> Option<DateValue> {
    Some(DateValue::Period {
        kind: PeriodKind::Fiscal,
        label,
        start: NaiveDate::from_ymd_opt(label - 1, 10, 1)?,
        end: NaiveDate::from_ymd_opt(label, 9, 30)?,
    })
}

/// Quarter 1 is October to December of the year before the label, quarters 2 to 4 the
/// following three-month runs of the label year.
fn quarter_period(label: i32, quarter: u32) -> Option<DateValue> {
    let (year, first, last, last_day) = match quarter {
        1 => (label - 1, 10, 12, 31),
        2 => (label, 1, 3, 31),
        3 => (label, 4, 6, 30),
        4 => (label, 7, 9, 30),
        _ => return None,
    };
    Some(DateValue::Period {
        kind: PeriodKind::Fiscal,
        label,
        start: NaiveDate::from_ymd_opt(year, first, 1)?,
        end: NaiveDate::from_ymd_opt(year, last, last_day)?,
    })
}

/// The first and last day a value covers.
fn days_of(value: &DateValue) -> Option<(NaiveDate, NaiveDate)> {
    match *value {
        DateValue::Day { date, .. } => Some((date, date)),
        DateValue::Month { year, month, .. } => {
            let first = NaiveDate::from_ymd_opt(year, month, 1)?;
            let next = if month == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)?
            };
            Some((first, next.pred_opt()?))
        }
        DateValue::Year { year, .. } => Some((
            NaiveDate::from_ymd_opt(year, 1, 1)?,
            NaiveDate::from_ymd_opt(year, 12, 31)?,
        )),
        DateValue::Period { start, end, .. } => Some((start, end)),
    }
}

/// Compares two dates or periods by meaning, never by text: the first rule that applies.
///
/// A value of era `Unknown` is `Incomparable` with anything. `Day` with `Day` and `Month` with
/// `Month` are `Same` or `Different`. `Year` with `Year`: equal years of one kind are `Same`,
/// equal years with one kind `Unknown` `Ambiguous`, equal years of two known kinds
/// `KindDiffers`, other years `Different`. `Period` with `Period`: one kind and the same days
/// `Same`, one kind and other days `Different`, two kinds `KindDiffers`. A calendar `Year` and a
/// fiscal `Period` are `KindDiffers`; a `Year` of kind `Unknown` and a fiscal `Period`
/// `Ambiguous`, whatever the years. Anything else, such as a `Day` and a `Year`, is
/// `Incomparable`.
pub fn compare_periods(a: &DateValue, b: &DateValue) -> PeriodCmp {
    let unknown_era = |value: &DateValue| {
        matches!(
            value,
            DateValue::Day {
                era: Era::Unknown,
                ..
            } | DateValue::Month {
                era: Era::Unknown,
                ..
            } | DateValue::Year {
                era: Era::Unknown,
                ..
            }
        )
    };
    if unknown_era(a) || unknown_era(b) {
        return PeriodCmp::Incomparable;
    }
    let same = |equal: bool| {
        if equal {
            PeriodCmp::Same
        } else {
            PeriodCmp::Different
        }
    };
    match (*a, *b) {
        (DateValue::Day { date: x, .. }, DateValue::Day { date: y, .. }) => same(x == y),
        (
            DateValue::Month {
                year: xy,
                month: xm,
                ..
            },
            DateValue::Month {
                year: yy,
                month: ym,
                ..
            },
        ) => same(xy == yy && xm == ym),
        (
            DateValue::Year {
                year: xy,
                period: xk,
                ..
            },
            DateValue::Year {
                year: yy,
                period: yk,
                ..
            },
        ) => {
            if xy != yy {
                PeriodCmp::Different
            } else if xk == yk {
                PeriodCmp::Same
            } else if xk == PeriodKind::Unknown || yk == PeriodKind::Unknown {
                PeriodCmp::Ambiguous
            } else {
                PeriodCmp::KindDiffers
            }
        }
        (
            DateValue::Period {
                kind: xk,
                start: xs,
                end: xe,
                ..
            },
            DateValue::Period {
                kind: yk,
                start: ys,
                end: ye,
                ..
            },
        ) => {
            if xk != yk {
                PeriodCmp::KindDiffers
            } else {
                same(xs == ys && xe == ye)
            }
        }
        (
            DateValue::Year { period, .. },
            DateValue::Period {
                kind: PeriodKind::Fiscal,
                ..
            },
        )
        | (
            DateValue::Period {
                kind: PeriodKind::Fiscal,
                ..
            },
            DateValue::Year { period, .. },
        ) => match period {
            PeriodKind::Calendar => PeriodCmp::KindDiffers,
            PeriodKind::Unknown => PeriodCmp::Ambiguous,
            _ => PeriodCmp::Incomparable,
        },
        _ => PeriodCmp::Incomparable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_written_as_normalize_th_writes_them() {
        let mut words: Vec<&str> = vec![PI, FISCAL, FISCAL_SHORT, QUARTER, THI, KHONG];
        words.extend(YEAR_MARKERS.iter().map(|(marker, _)| *marker));
        words.extend(THAI_ERAS.iter().map(|(marker, _)| *marker));
        words.extend(
            THAI_MONTHS
                .iter()
                .flat_map(|(full, truncated)| [*full, *truncated]),
        );
        words.extend(
            THAI_ABBREVIATIONS
                .iter()
                .flat_map(|(first, second)| [*first, *second]),
        );
        for word in words {
            assert_eq!(crate::normalize_th(word).text, word, "{word:?}");
        }
    }
}
