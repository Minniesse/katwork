//! The clean-context rule (a founder decision): a figure or a date reads with confidence 0.8 or
//! more only when it matches a pattern the reader knows exactly and no signal of a range, a list,
//! a fraction, a period or a sign stands near it. Near one, its confidence is capped at
//! [`CONF_CAP`] and the readings the signal suggests go in `alts`, so a check against it is
//! inconclusive, never a pass. A run the reader cannot read at all stays one `Code`.
//!
//! [`SIGNALS`] is the one list of signals; each says how near it must stand ([`Place`]).
//! Joining and opening signals count only next to the reading, with at most one space, and a
//! filler word such as วันที่ or เดือน, between; sign and period signals count within [`NEAR`]
//! code points of it. A signal inside a quantity or a date (the ไตรมาส of `ไตรมาส 2 ปีงบประมาณ
//! 2569`, the ติดลบ of `ติดลบ 5%`) belongs to that reading's own pattern and counts for nothing.

use std::ops::Range;

use crate::found::{has_at, is_latin_letter, is_thai};

/// The highest confidence of a reading with a signal near it: below 0.8, so it is never passed.
pub(crate) const CONF_CAP: f32 = 0.6;

/// The confidence of each reading a signal suggests beside a capped one.
pub(crate) const CONF_OTHER: f32 = 0.4;

/// How far a sign or period signal may stand from a reading, in code points of the normalized
/// text, counted from the reading's unit words: about three Thai words.
pub(crate) const NEAR: usize = 15;

/// What a signal says about the readings near it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The reading may be one end of a range (ถึง, `-`, `until`).
    Range,
    /// The reading may be one item of a list (และ, `,`, `or`).
    List,
    /// The reading may be one term of a fraction or a ratio (`/`).
    Fraction,
    /// A date may name part of its year or month (ไตรมาส, ครึ่งปี, งวด, `Q2`, `H1`). Only for dates
    /// that are not a day.
    Period,
    /// A figure may be negative (ติดลบ, ลบ, `minus`). Only for figures.
    Sign,
}

/// Where a signal must stand to count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Between the reading and another figure or date, next to both: `10-15`, `2567 และ 2568`.
    Joins,
    /// As `Joins`, or right before the reading with nothing before it: `ถึง 31 ธ.ค.`, `until 2026`.
    Leads,
    /// Right before the reading: `ตั้งแต่วันที่ 1 ต.ค.`, `from 2024`.
    Opens,
    /// Within [`NEAR`] code points before the reading: `ติดลบที่ร้อยละ 6.1`.
    Before,
    /// Within [`NEAR`] code points before or after the reading: `งวด 9 เดือนแรกของปี 2568`.
    Around,
}

/// One signal, as `normalize_th` writes it. Latin ones count only as whole words (no Latin letter
/// or digit on either side); ลบ counts only where no Thai letter stands before it (สลบ, หลบ and
/// ผลบวก hold no sign); ต้น, กลาง, ปลาย and สิ้น only before ปี or เดือน.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Signal {
    pub(crate) text: &'static str,
    pub(crate) kind: Kind,
    pub(crate) place: Place,
}

const fn signal(text: &'static str, kind: Kind, place: Place) -> Signal {
    Signal { text, kind, place }
}

/// Every signal the reader knows. A change to the list is a change here and nowhere else.
pub(crate) const SIGNALS: [Signal; 56] = [
    // Ranges.
    signal("\u{0E16}\u{0E36}\u{0E07}", Kind::Range, Place::Leads), // ถึง
    signal(
        "\u{0E08}\u{0E19}\u{0E16}\u{0E36}\u{0E07}",
        Kind::Range,
        Place::Leads,
    ), // จนถึง
    signal(
        "\u{0E44}\u{0E1B}\u{0E08}\u{0E19}\u{0E16}\u{0E36}\u{0E07}",
        Kind::Range,
        Place::Leads,
    ), // ไปจนถึง
    signal(
        "\u{0E08}\u{0E19}\u{0E01}\u{0E23}\u{0E30}\u{0E17}\u{0E31}\u{0E48}\u{0E07}",
        Kind::Range,
        Place::Leads,
    ), // จนกระทั่ง
    signal(
        "\u{0E15}\u{0E31}\u{0E49}\u{0E07}\u{0E41}\u{0E15}\u{0E48}",
        Kind::Range,
        Place::Opens,
    ), // ตั้งแต่
    signal("\u{0E08}\u{0E32}\u{0E01}", Kind::Range, Place::Opens), // จาก
    signal(
        "\u{0E23}\u{0E30}\u{0E2B}\u{0E27}\u{0E48}\u{0E32}\u{0E07}",
        Kind::Range,
        Place::Opens,
    ), // ระหว่าง
    signal("between", Kind::Range, Place::Opens),
    signal("from", Kind::Range, Place::Opens),
    signal("to", Kind::Range, Place::Leads),
    signal("through", Kind::Range, Place::Leads),
    signal("thru", Kind::Range, Place::Leads),
    signal("till", Kind::Range, Place::Leads),
    signal("until", Kind::Range, Place::Leads),
    signal("up to", Kind::Range, Place::Leads),
    signal("up to and including", Kind::Range, Place::Leads),
    signal("-", Kind::Range, Place::Joins),
    signal("\u{2014}", Kind::Range, Place::Joins), // em dash
    signal("~", Kind::Range, Place::Joins),
    signal("\u{2192}", Kind::Range, Place::Joins), // →
    signal("->", Kind::Range, Place::Joins),
    // Lists.
    signal("\u{0E41}\u{0E25}\u{0E30}", Kind::List, Place::Joins), // และ
    signal("\u{0E2B}\u{0E23}\u{0E37}\u{0E2D}", Kind::List, Place::Joins), // หรือ
    signal("\u{0E01}\u{0E31}\u{0E1A}", Kind::List, Place::Joins), // กับ
    signal("and", Kind::List, Place::Joins),
    signal("or", Kind::List, Place::Joins),
    signal(",", Kind::List, Place::Joins),
    signal("&", Kind::List, Place::Joins),
    // Fractions and ratios.
    signal("/", Kind::Fraction, Place::Joins),
    // Periods shorter than the year or month a date names.
    signal("\u{0E07}\u{0E27}\u{0E14}", Kind::Period, Place::Around), // งวด
    signal(
        "\u{0E04}\u{0E23}\u{0E36}\u{0E48}\u{0E07}",
        Kind::Period,
        Place::Around,
    ), // ครึ่ง
    signal(
        "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}\u{0E41}\u{0E23}\u{0E01}",
        Kind::Period,
        Place::Around,
    ), // เดือนแรก
    signal(
        "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}\u{0E2B}\u{0E25}\u{0E31}\u{0E07}",
        Kind::Period,
        Place::Around,
    ), // เดือนหลัง
    signal(
        "\u{0E44}\u{0E15}\u{0E23}\u{0E21}\u{0E32}\u{0E2A}",
        Kind::Period,
        Place::Around,
    ), // ไตรมาส
    signal(
        "\u{0E1B}\u{0E35}\u{0E07}\u{0E1A}",
        Kind::Period,
        Place::Around,
    ), // ปีงบ
    signal(START, Kind::Period, Place::Around),
    signal(MIDDLE, Kind::Period, Place::Around),
    signal(LATE, Kind::Period, Place::Around),
    signal(END, Kind::Period, Place::Around),
    signal("q1", Kind::Period, Place::Around),
    signal("q2", Kind::Period, Place::Around),
    signal("q3", Kind::Period, Place::Around),
    signal("q4", Kind::Period, Place::Around),
    signal("1q", Kind::Period, Place::Around),
    signal("2q", Kind::Period, Place::Around),
    signal("3q", Kind::Period, Place::Around),
    signal("4q", Kind::Period, Place::Around),
    signal("h1", Kind::Period, Place::Around),
    signal("h2", Kind::Period, Place::Around),
    signal("1h", Kind::Period, Place::Around),
    signal("2h", Kind::Period, Place::Around),
    signal("quarter", Kind::Period, Place::Around),
    signal("half", Kind::Period, Place::Around),
    // Signs.
    signal(
        "\u{0E15}\u{0E34}\u{0E14}\u{0E25}\u{0E1A}",
        Kind::Sign,
        Place::Before,
    ), // ติดลบ
    signal("\u{0E25}\u{0E1A}", Kind::Sign, Place::Before), // ลบ
    signal("minus", Kind::Sign, Place::Before),
];

/// An upper bound on the length of a signal in code points: its length in bytes.
const LONGEST: usize = {
    let mut longest = 0;
    let mut i = 0;
    while i < SIGNALS.len() {
        if SIGNALS[i].text.len() > longest {
            longest = SIGNALS[i].text.len();
        }
        i += 1;
    }
    longest
};

/// The start, middle, late part and end of a year or month (ต้นปี, กลางเดือน, ปลายปี, สิ้นปี):
/// signals only before ปี or เดือน, so ต้นทุน (cost) and ส่วนกลาง are none.
const START: &str = "\u{0E15}\u{0E49}\u{0E19}"; // ต้น
const MIDDLE: &str = "\u{0E01}\u{0E25}\u{0E32}\u{0E07}"; // กลาง
const LATE: &str = "\u{0E1B}\u{0E25}\u{0E32}\u{0E22}"; // ปลาย
const END: &str = "\u{0E2A}\u{0E34}\u{0E49}\u{0E19}"; // สิ้น
const PI: &str = "\u{0E1B}\u{0E35}"; // ปี
const MONTH: &str = "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}"; // เดือน

/// Words that may stand between a joining or opening signal and the reading it reaches, longest
/// first: `ถึงวันที่ 31 ธ.ค.`, `to the 31st`.
const FILLERS: [&str; 4] = [
    "\u{0E27}\u{0E31}\u{0E19}\u{0E17}\u{0E35}\u{0E48}", // วันที่
    "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}",         // เดือน
    "\u{0E27}\u{0E31}\u{0E19}",                         // วัน
    "the",
];

/// What a reading is, which decides the signals that apply to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A quantity: ranges, lists, fractions and signs.
    Figure,
    /// A day: ranges, lists and fractions.
    Day,
    /// A month, year or period: ranges, lists, fractions and periods.
    Period,
}

impl Target {
    fn takes(self, kind: Kind) -> bool {
        match kind {
            Kind::Range | Kind::List | Kind::Fraction => true,
            Kind::Period => self == Target::Period,
            Kind::Sign => self == Target::Figure,
        }
    }
}

/// What the signals near one reading say.
#[derive(Debug, Default)]
pub(crate) struct Near {
    /// Some signal applies: the reading is capped at [`CONF_CAP`].
    pub(crate) capped: bool,
    /// A sign signal applies: the negative is another reading.
    pub(crate) sign: bool,
    /// Where the readings a joining signal joins this one to end, before it.
    pub(crate) partners_before: Vec<usize>,
    /// Where the readings a joining signal joins this one to start, after it.
    pub(crate) partners_after: Vec<usize>,
}

/// One signal found in the text.
struct Occurrence {
    start: usize,
    end: usize,
    signal: Signal,
}

/// The signals of a text and where its readings start and end.
pub(crate) struct Context<'t> {
    t: &'t [char],
    /// Every signal outside a quantity or a date, in text order.
    found: Vec<Occurrence>,
    /// Where a reading (a number of any role, a date or a month name) starts and ends: a joining
    /// signal must reach one.
    starts: Vec<bool>,
    ends: Vec<bool>,
}

impl<'t> Context<'t> {
    /// `readings` are the extents of every number, date and month name; `own` the spans whose
    /// signals belong to their own pattern (quantities and dates).
    pub(crate) fn new(t: &'t [char], readings: &[Range<usize>], own: &[Range<usize>]) -> Self {
        let mut hidden = vec![false; t.len()];
        for range in own {
            if let Some(slots) = hidden.get_mut(range.clone()) {
                slots.iter_mut().for_each(|slot| *slot = true);
            }
        }
        let mut starts = vec![false; t.len() + 1];
        let mut ends = vec![false; t.len() + 1];
        for range in readings {
            if let Some(slot) = starts.get_mut(range.start) {
                *slot = true;
            }
            if let Some(slot) = ends.get_mut(range.end) {
                *slot = true;
            }
        }
        let mut found = Vec::new();
        for (at, c) in t.iter().enumerate() {
            if hidden[at] {
                continue;
            }
            for signal in SIGNALS.iter().filter(|signal| signal.text.starts_with(*c)) {
                let end = at + signal.text.chars().count();
                if has_at(t, at, signal.text)
                    && !hidden[at..end.min(t.len())].iter().any(|slot| *slot)
                    && bounded(t, at, end, signal.text)
                {
                    found.push(Occurrence {
                        start: at,
                        end,
                        signal: *signal,
                    });
                }
            }
        }
        Context {
            t,
            found,
            starts,
            ends,
        }
    }

    /// The signals near the reading over `extent` (its span and its unit words).
    pub(crate) fn near(&self, extent: &Range<usize>, target: Target) -> Near {
        let mut near = Near::default();
        let applies = |occurrence: &Occurrence| target.takes(occurrence.signal.kind);
        // Loose signals: a sign before the reading, a period on either side.
        let lo = extent.start.saturating_sub(NEAR + LONGEST);
        let first = self.found.partition_point(|found| found.start < lo);
        for occurrence in self.found[first..]
            .iter()
            .take_while(|found| found.start <= extent.end + NEAR)
        {
            crate::found::work(1);
            let before = occurrence.end <= extent.start && extent.start - occurrence.end <= NEAR;
            let after = occurrence.start >= extent.end && occurrence.start - extent.end <= NEAR;
            let counts = match occurrence.signal.place {
                Place::Before => before,
                Place::Around => before || after,
                _ => false,
            };
            if counts && applies(occurrence) {
                near.capped = true;
                near.sign |= occurrence.signal.kind == Kind::Sign;
            }
        }
        // Signals right before the reading: an opening one, or a joining one with another
        // reading before it.
        for at in self.reach_back(extent.start) {
            for occurrence in self.ending_at(at).filter(|found| applies(found)) {
                match occurrence.signal.place {
                    Place::Opens | Place::Leads => near.capped = true,
                    Place::Joins => {
                        let far = self.back(occurrence.start);
                        if self.ends.get(far) == Some(&true) {
                            near.capped = true;
                            near.partners_before.push(far);
                        }
                    }
                    Place::Before | Place::Around => {}
                }
            }
        }
        // A joining signal right after the reading, with another reading after it.
        let at = self.forward(extent.end);
        for occurrence in self.starting_at(at).filter(|found| applies(found)) {
            if !matches!(occurrence.signal.place, Place::Joins | Place::Leads) {
                continue;
            }
            for far in self.reach_forward(occurrence.end) {
                if self.starts.get(far) == Some(&true) {
                    near.capped = true;
                    near.partners_after.push(far);
                }
            }
        }
        near
    }

    fn ending_at(&self, at: usize) -> impl Iterator<Item = &Occurrence> {
        let first = self
            .found
            .partition_point(|found| found.start + LONGEST < at);
        self.found[first..]
            .iter()
            .take_while(move |found| found.start < at)
            .filter(move |found| found.end == at)
    }

    fn starting_at(&self, at: usize) -> impl Iterator<Item = &Occurrence> {
        let first = self.found.partition_point(|found| found.start < at);
        self.found[first..]
            .iter()
            .take_while(move |found| found.start == at)
    }

    /// `at` with one space before it skipped.
    fn back(&self, at: usize) -> usize {
        if at > 0 && self.t.get(at - 1) == Some(&' ') {
            at - 1
        } else {
            at
        }
    }

    /// `at` with one space skipped.
    fn forward(&self, at: usize) -> usize {
        if self.t.get(at) == Some(&' ') {
            at + 1
        } else {
            at
        }
    }

    /// Where a signal right before a reading starting at `at` may end: past one space, and past
    /// a filler word and a space before it.
    fn reach_back(&self, at: usize) -> Vec<usize> {
        let at = self.back(at);
        let mut ends = vec![at];
        for filler in FILLERS {
            if crate::found::has_before(self.t, at, filler) {
                ends.push(self.back(at - filler.chars().count()));
                break;
            }
        }
        ends
    }

    /// Where a reading may start after a signal ending at `at`: past one space, and past a filler
    /// word and a space after it.
    fn reach_forward(&self, at: usize) -> Vec<usize> {
        let at = self.forward(at);
        let mut starts = vec![at];
        for filler in FILLERS {
            if has_at(self.t, at, filler) {
                starts.push(self.forward(at + filler.chars().count()));
                break;
            }
        }
        starts
    }
}

/// A signal at `start..end` stands as a whole word where it must: a Latin one with no Latin
/// letter or digit on either side, ลบ with no Thai letter before it, and a part of a year or
/// month (ต้น, กลาง, ปลาย, สิ้น) before ปี or เดือน.
fn bounded(t: &[char], start: usize, end: usize, text: &str) -> bool {
    if [START, MIDDLE, LATE, END].contains(&text) {
        return has_at(t, end, PI) || has_at(t, end, MONTH);
    }
    let before = start.checked_sub(1).and_then(|prev| t.get(prev)).copied();
    let after = t.get(end).copied();
    let word = |c: Option<char>| c.is_some_and(|c| is_latin_letter(c) || c.is_ascii_digit());
    if text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return !word(before) && !word(after);
    }
    if text == "\u{0E25}\u{0E1A}" {
        return !before.is_some_and(is_thai);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_are_written_as_normalize_th_writes_them() {
        for signal in SIGNALS {
            assert_eq!(
                crate::normalize_th(signal.text).text,
                signal.text,
                "{signal:?}"
            );
        }
        for filler in FILLERS {
            assert_eq!(crate::normalize_th(filler).text, filler, "{filler:?}");
        }
    }

    /// Every entry of the table, placed where its `Place` says, keeps the reading near it below
    /// 0.8, and the same text with a plain word in its place does not: a list change is tested
    /// here with no other edit.
    #[test]
    fn every_signal_in_the_table_caps_a_reading_near_it() {
        let ctx = crate::Ctx {
            anchor: chrono::NaiveDate::from_ymd_opt(2025, 11, 1),
        };
        let figure = "1,250 \u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E1A}\u{0E32}\u{0E17}"; // 1,250 ล้านบาท
        let other = "2,500 \u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E1A}\u{0E32}\u{0E17}"; // 2,500 ล้านบาท
        let year = "\u{0E1B}\u{0E35} 2568"; // ปี 2568
        let near = "\u{0E2D}\u{0E22}\u{0E39}\u{0E48}\u{0E17}\u{0E35}\u{0E48}"; // อยู่ที่
        let plain = "\u{0E04}\u{0E37}\u{0E2D}"; // คือ
        let figures_capped = |text: &str| {
            let found = crate::find_numbers(text, &ctx).unwrap();
            found
                .iter()
                .any(|found| found.value.role == crate::NumberRole::Quantity)
                && found
                    .iter()
                    .filter(|found| found.value.role == crate::NumberRole::Quantity)
                    .all(|found| found.conf <= CONF_CAP)
        };
        let years_capped = |text: &str| {
            let found = crate::find_dates(text, &ctx).unwrap();
            !found.is_empty() && found.iter().all(|found| found.conf <= CONF_CAP)
        };
        for signal in SIGNALS {
            let text = signal.text;
            let (capped, control) = match (signal.kind, signal.place) {
                (Kind::Period, _) => {
                    // A part of a year is glued to its ปี: ต้นปี 2568.
                    let probe = if [START, MIDDLE, LATE, END].contains(&text) {
                        format!("{text}{year}")
                    } else {
                        format!("{text} {year}")
                    };
                    (
                        years_capped(&probe),
                        years_capped(&format!("{plain} {year}")),
                    )
                }
                (Kind::Sign, _) => (
                    figures_capped(&format!("{text} {near} {figure}")),
                    figures_capped(&format!("{plain} {near} {figure}")),
                ),
                (_, Place::Joins) => (
                    figures_capped(&format!("{figure} {text} {other}")),
                    figures_capped(&format!("{figure} {plain} {other}")),
                ),
                (_, _) => (
                    figures_capped(&format!("{text} {figure}")),
                    figures_capped(&format!("{plain} {figure}")),
                ),
            };
            assert!(capped, "{signal:?} caps nothing");
            assert!(!control, "{signal:?}: the control is capped too");
        }
    }

    #[test]
    fn no_signal_is_listed_twice() {
        for (i, a) in SIGNALS.iter().enumerate() {
            for b in &SIGNALS[i + 1..] {
                assert_ne!(a.text, b.text);
            }
        }
    }
}
