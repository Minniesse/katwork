//! The clean-paragraph rule (a founder decision, fix round 4): a figure or a date reads with
//! confidence 0.8 or more only when the whole text the scanner is given (a block: a paragraph or
//! a cell) holds no signal word from [`SIGNALS`] and at most one figure and one date (an amount
//! with its year, the founder's answer in round 3b). A code beside a date, a run the reader cannot
//! read, leaves the text unclean, and so does a control that step 1 deleted outside every
//! reading. Otherwise the confidence is capped at [`CONF_CAP`] and the other readings go in
//! `alts`, so a check against the reading is inconclusive, never a pass. A run the reader cannot
//! read at all stays one `Code`.
//!
//! Nothing splits the text: no line break, colon, full stop, ฯ or other punctuation ends the unit.
//! A caller that wants two readings judged apart gives the scanner two texts.
//!
//! [`SIGNALS`] is the one list of signals, matched anywhere in the text, Latin ones as whole
//! words. A signal inside a reading (the comma of `1,250`, the ไตรมาส of `ไตรมาส 2 ปีงบประมาณ
//! 2569`, the ติดลบ of `ติดลบ 5%`, the weekday written with its date) belongs to that reading's
//! pattern and caps nothing.

use std::ops::Range;

use crate::found::{dash_or_arrow, has_at, has_before, is_latin_letter, is_numeral, unknown_glyph};

/// The highest confidence of a reading in a text that is not clean: below 0.8, so it is never
/// passed.
pub(crate) const CONF_CAP: f32 = 0.6;

/// The confidence of each other reading beside a capped one.
pub(crate) const CONF_OTHER: f32 = 0.4;

/// What a signal says about the readings in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A reading may be one end of a range, open or closed (ถึง, `-`, `since`, เป็นต้นไป, `after`
    /// before a date, `+` after a figure).
    Range,
    /// A reading may be one item of a list (และ, `,`, `or`).
    List,
    /// A reading may be one term of a fraction or a ratio (`/`, ส่วน).
    Fraction,
    /// A reading may name part of a year, a month or a week, or a figure be for one or for
    /// another year (ไตรมาส, งวด, สะสม, รายเดือน, ปีก่อน, `9M`, `YTD`, `monthly`, a weekday).
    Period,
    /// A figure may be negative (ติดลบ, เป็นลบ, `negative`): its negative is another reading.
    Sign,
}

/// What a signal matches.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Pattern {
    /// These code points, as `normalize_th` writes them.
    Text(&'static str),
    /// One code point of a class, with its name.
    Class(&'static str, fn(char) -> bool),
}

/// One signal. Latin text counts only as a whole word (no Latin letter or digit on either side);
/// a few entries count only where [`counts`] says.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Signal {
    pub(crate) pattern: Pattern,
    pub(crate) kind: Kind,
}

const fn text(text: &'static str, kind: Kind) -> Signal {
    Signal {
        pattern: Pattern::Text(text),
        kind,
    }
}

const fn class(name: &'static str, class: fn(char) -> bool, kind: Kind) -> Signal {
    Signal {
        pattern: Pattern::Class(name, class),
        kind,
    }
}

/// The class of dashes and arrows, whose own rules (`dash_counts`) say where one counts.
const DASHES: &str = "dash_or_arrow";

/// Every signal the reader knows, and `signal_table_is_frozen` names it. ระหว่าง and `between`
/// have no row: on their own they mean "during" or "among" (the founder's answer in round 2b),
/// and with a connector in their text the connector caps. dates.rs keeps its own connector
/// lists for joining two dates into one range reading (follow-up 18); capping uses this table
/// alone.
pub(crate) const SIGNALS: [Signal; 169] = [
    // Ranges, closed or open.
    text("\u{0E16}\u{0E36}\u{0E07}", Kind::Range), // ถึง
    text("\u{0E08}\u{0E19}\u{0E16}\u{0E36}\u{0E07}", Kind::Range), // จนถึง
    text(
        "\u{0E44}\u{0E1B}\u{0E08}\u{0E19}\u{0E16}\u{0E36}\u{0E07}",
        Kind::Range,
    ), // ไปจนถึง
    text(
        "\u{0E08}\u{0E19}\u{0E01}\u{0E23}\u{0E30}\u{0E17}\u{0E31}\u{0E48}\u{0E07}",
        Kind::Range,
    ), // จนกระทั่ง
    text("\u{0E08}\u{0E19}", Kind::Range),         // จน
    text(
        "\u{0E15}\u{0E31}\u{0E49}\u{0E07}\u{0E41}\u{0E15}\u{0E48}",
        Kind::Range,
    ), // ตั้งแต่
    text(
        "\u{0E19}\u{0E31}\u{0E1A}\u{0E41}\u{0E15}\u{0E48}",
        Kind::Range,
    ), // นับแต่
    text("\u{0E08}\u{0E32}\u{0E01}", Kind::Range), // จาก
    text(
        "\u{0E16}\u{0E36}\u{0E07}\u{0E1B}\u{0E31}\u{0E08}\u{0E08}\u{0E38}\u{0E1A}\u{0E31}\u{0E19}",
        Kind::Range,
    ), // ถึงปัจจุบัน
    text(
        "\u{0E40}\u{0E1B}\u{0E47}\u{0E19}\u{0E15}\u{0E49}\u{0E19}\u{0E44}\u{0E1B}",
        Kind::Range,
    ), // เป็นต้นไป
    text(
        "\u{0E40}\u{0E1B}\u{0E47}\u{0E19}\u{0E15}\u{0E49}\u{0E19}\u{0E21}\u{0E32}",
        Kind::Range,
    ), // เป็นต้นมา
    text(
        "\u{0E02}\u{0E36}\u{0E49}\u{0E19}\u{0E44}\u{0E1B}",
        Kind::Range,
    ), // ขึ้นไป
    text(
        "\u{0E40}\u{0E23}\u{0E34}\u{0E48}\u{0E21}\u{0E15}\u{0E49}\u{0E19}",
        Kind::Range,
    ), // เริ่มต้น
    text(
        "\u{0E40}\u{0E23}\u{0E34}\u{0E48}\u{0E21}\u{0E17}\u{0E35}\u{0E48}",
        Kind::Range,
    ), // เริ่มที่
    text(BEFORE, Kind::Range),
    text(AFTER, Kind::Range),
    text("from", Kind::Range),
    text("since", Kind::Range),
    text("starting", Kind::Range),
    text("to", Kind::Range),
    text("to date", Kind::Range),
    text("through", Kind::Range),
    text("thru", Kind::Range),
    text("till", Kind::Range),
    text("until", Kind::Range),
    text("until now", Kind::Range),
    text("up to", Kind::Range),
    text("up to and including", Kind::Range),
    text("onward", Kind::Range),
    text("onwards", Kind::Range),
    text("after", Kind::Range),
    text("before", Kind::Range),
    text("beyond", Kind::Range),
    text("following", Kind::Range),
    text("later than", Kind::Range),
    text("earlier than", Kind::Range),
    class(DASHES, dash_or_arrow, Kind::Range),
    class("unknown_glyph", unknown_glyph, Kind::Range), // a glyph the text layer could not name
    text("->", Kind::Range),
    text("=>", Kind::Range),
    text(PLUS, Kind::Range),
    // Lists.
    text("\u{0E41}\u{0E25}\u{0E30}", Kind::List), // และ
    text("\u{0E2B}\u{0E23}\u{0E37}\u{0E2D}", Kind::List), // หรือ
    text("\u{0E01}\u{0E31}\u{0E1A}", Kind::List), // กับ
    text("and", Kind::List),
    text("or", Kind::List),
    text(",", Kind::List),
    text("&", Kind::List),
    // Fractions and ratios.
    text("/", Kind::Fraction),
    text("\u{2044}", Kind::Fraction), // fraction slash
    text("\u{2215}", Kind::Fraction), // division slash
    text("\u{00F7}", Kind::Fraction), // ÷
    text("\u{0E2A}\u{0E48}\u{0E27}\u{0E19}", Kind::Fraction), // ส่วน
    text("thirds", Kind::Fraction),
    text("fourths", Kind::Fraction),
    text("quarters", Kind::Fraction),
    // Parts of a year, a month or a week, figures for one, and figures for another year.
    text("\u{0E07}\u{0E27}\u{0E14}", Kind::Period), // งวด
    text("\u{0E04}\u{0E23}\u{0E36}\u{0E48}\u{0E07}", Kind::Period), // ครึ่ง
    text(
        "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}\u{0E41}\u{0E23}\u{0E01}",
        Kind::Period,
    ), // เดือนแรก
    text(
        "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}\u{0E2B}\u{0E25}\u{0E31}\u{0E07}",
        Kind::Period,
    ), // เดือนหลัง
    text(
        "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}\u{0E2A}\u{0E38}\u{0E14}\u{0E17}\u{0E49}\u{0E32}\u{0E22}",
        Kind::Period,
    ), // เดือนสุดท้าย
    text(MONTH, Kind::Period),
    text(
        "\u{0E15}\u{0E48}\u{0E2D}\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}",
        Kind::Period,
    ), // ต่อเดือน
    text(
        "\u{0E23}\u{0E32}\u{0E22}\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}",
        Kind::Period,
    ), // รายเดือน
    text(
        "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}\u{0E25}\u{0E30}",
        Kind::Period,
    ), // เดือนละ
    text(
        "\u{0E17}\u{0E38}\u{0E01}\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}",
        Kind::Period,
    ), // ทุกเดือน
    text(
        "\u{0E15}\u{0E48}\u{0E2D}\u{0E27}\u{0E31}\u{0E19}",
        Kind::Period,
    ), // ต่อวัน
    text(
        "\u{0E23}\u{0E32}\u{0E22}\u{0E27}\u{0E31}\u{0E19}",
        Kind::Period,
    ), // รายวัน
    text("\u{0E27}\u{0E31}\u{0E19}\u{0E25}\u{0E30}", Kind::Period), // วันละ
    text(
        "\u{0E15}\u{0E48}\u{0E2D}\u{0E0A}\u{0E31}\u{0E48}\u{0E27}\u{0E42}\u{0E21}\u{0E07}",
        Kind::Period,
    ), // ต่อชั่วโมง
    text(
        "\u{0E44}\u{0E15}\u{0E23}\u{0E21}\u{0E32}\u{0E2A}",
        Kind::Period,
    ), // ไตรมาส
    text("\u{0E1B}\u{0E35}\u{0E07}\u{0E1A}", Kind::Period),         // ปีงบ
    text(
        "\u{0E1B}\u{0E35}\u{0E01}\u{0E48}\u{0E2D}\u{0E19}",
        Kind::Period,
    ), // ปีก่อน
    text(
        "\u{0E1B}\u{0E35}\u{0E17}\u{0E35}\u{0E48}\u{0E41}\u{0E25}\u{0E49}\u{0E27}",
        Kind::Period,
    ), // ปีที่แล้ว
    text(
        "\u{0E1B}\u{0E35}\u{0E17}\u{0E35}\u{0E48}\u{0E1C}\u{0E48}\u{0E32}\u{0E19}\u{0E21}\u{0E32}",
        Kind::Period,
    ), // ปีที่ผ่านมา
    text(
        "\u{0E2A}\u{0E31}\u{0E1B}\u{0E14}\u{0E32}\u{0E2B}\u{0E4C}",
        Kind::Period,
    ), // สัปดาห์
    text("\u{0E1B}\u{0E31}\u{0E01}\u{0E29}\u{0E4C}", Kind::Period), // ปักษ์
    text("\u{0E2A}\u{0E30}\u{0E2A}\u{0E21}", Kind::Period),         // สะสม
    text("\u{0E41}\u{0E23}\u{0E01}", Kind::Period),                 // แรก
    text(
        "\u{0E2A}\u{0E38}\u{0E14}\u{0E17}\u{0E49}\u{0E32}\u{0E22}",
        Kind::Period,
    ), // สุดท้าย
    text(START, Kind::Period),
    text(MIDDLE, Kind::Period),
    text(LATE, Kind::Period),
    text(END, Kind::Period),
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E08}\u{0E31}\u{0E19}\u{0E17}\u{0E23}\u{0E4C}",
        Kind::Period,
    ), // วันจันทร์
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E2D}\u{0E31}\u{0E07}\u{0E04}\u{0E32}\u{0E23}",
        Kind::Period,
    ), // วันอังคาร
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E1E}\u{0E38}\u{0E18}",
        Kind::Period,
    ), // วันพุธ
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E1E}\u{0E24}\u{0E2B}\u{0E31}\u{0E2A}",
        Kind::Period,
    ), // วันพฤหัส
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E28}\u{0E38}\u{0E01}\u{0E23}\u{0E4C}",
        Kind::Period,
    ), // วันศุกร์
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E40}\u{0E2A}\u{0E32}\u{0E23}\u{0E4C}",
        Kind::Period,
    ), // วันเสาร์
    text(
        "\u{0E27}\u{0E31}\u{0E19}\u{0E2D}\u{0E32}\u{0E17}\u{0E34}\u{0E15}\u{0E22}\u{0E4C}",
        Kind::Period,
    ), // วันอาทิตย์
    text("q1", Kind::Period),
    text("q2", Kind::Period),
    text("q3", Kind::Period),
    text("q4", Kind::Period),
    text("1q", Kind::Period),
    text("2q", Kind::Period),
    text("3q", Kind::Period),
    text("4q", Kind::Period),
    text("h1", Kind::Period),
    text("h2", Kind::Period),
    text("1h", Kind::Period),
    text("2h", Kind::Period),
    text("3m", Kind::Period),
    text("6m", Kind::Period),
    text("9m", Kind::Period),
    text("12m", Kind::Period),
    text("quarter", Kind::Period),
    text("quarterly", Kind::Period),
    text("half", Kind::Period),
    text("semi-annual", Kind::Period),
    text("semiannual", Kind::Period),
    text("month", Kind::Period),
    text("months", Kind::Period),
    text("monthly", Kind::Period),
    text("week", Kind::Period),
    text("weeks", Kind::Period),
    text("weekly", Kind::Period),
    text("fortnight", Kind::Period),
    text("fortnightly", Kind::Period),
    text("bimonthly", Kind::Period),
    text("biannual", Kind::Period),
    text("daily", Kind::Period),
    text("per day", Kind::Period),
    text("hourly", Kind::Period),
    text("annualized", Kind::Period),
    text("annualised", Kind::Period),
    text("run-rate", Kind::Period),
    text("run rate", Kind::Period),
    text("ytd", Kind::Period),
    text("mtd", Kind::Period),
    text("qtd", Kind::Period),
    text("ttm", Kind::Period),
    text("ltm", Kind::Period),
    text("cumulative", Kind::Period),
    text("interim", Kind::Period),
    text("early", Kind::Period),
    text("mid", Kind::Period),
    text("late", Kind::Period),
    text("first", Kind::Period),
    text("last", Kind::Period),
    text("previous", Kind::Period),
    text("prior", Kind::Period),
    text("preceding", Kind::Period),
    text("year-ago", Kind::Period),
    text("end of", Kind::Period),
    text("beginning of", Kind::Period),
    text("start of", Kind::Period),
    text("monday", Kind::Period),
    text("tuesday", Kind::Period),
    text("wednesday", Kind::Period),
    text("thursday", Kind::Period),
    text("friday", Kind::Period),
    text("saturday", Kind::Period),
    text("sunday", Kind::Period),
    text("mon", Kind::Period),
    text("tue", Kind::Period),
    text("wed", Kind::Period),
    text("thu", Kind::Period),
    text("fri", Kind::Period),
    text("sat", Kind::Period),
    // Signs.
    text("\u{0E15}\u{0E34}\u{0E14}\u{0E25}\u{0E1A}", Kind::Sign), // ติดลบ
    text(LOP, Kind::Sign),
    text(
        "\u{0E40}\u{0E1B}\u{0E47}\u{0E19}\u{0E25}\u{0E1A}",
        Kind::Sign,
    ), // เป็นลบ
    text("\u{0E1A}\u{0E27}\u{0E01}\u{0E25}\u{0E1A}", Kind::Sign), // บวกลบ
    text("minus", Kind::Sign),
    text("negative", Kind::Sign),
    text("\u{00B1}", Kind::Sign), // ±
    text("+/-", Kind::Sign),
];

/// The start, middle, late part and end of a year or month (ต้นปี, กลางเดือน, ปลายปี, สิ้นปี):
/// signals only before ปี or เดือน, so ต้นทุน (cost) and ส่วนกลาง are none.
const START: &str = "\u{0E15}\u{0E49}\u{0E19}"; // ต้น
const MIDDLE: &str = "\u{0E01}\u{0E25}\u{0E32}\u{0E07}"; // กลาง
const LATE: &str = "\u{0E1B}\u{0E25}\u{0E32}\u{0E22}"; // ปลาย
const END: &str = "\u{0E2A}\u{0E34}\u{0E49}\u{0E19}"; // สิ้น
const PI: &str = "\u{0E1B}\u{0E35}"; // ปี
const MONTH: &str = "\u{0E40}\u{0E14}\u{0E37}\u{0E2D}\u{0E19}"; // เดือน
const PART: &str = "\u{0E2A}\u{0E48}\u{0E27}\u{0E19}"; // ส่วน
const LOP: &str = "\u{0E25}\u{0E1A}"; // ลบ
/// ก่อน (before) and หลัง (after, so ภายหลัง too): signals only before a date or a figure, so
/// กำไรก่อนหักภาษี (profit before tax) and ก่อนหน้านี้ are none.
const BEFORE: &str = "\u{0E01}\u{0E48}\u{0E2D}\u{0E19}"; // ก่อน
const AFTER: &str = "\u{0E2B}\u{0E25}\u{0E31}\u{0E07}"; // หลัง
/// `+` right after a reading or its unit words: an open end (`500+`, `100 million+`).
const PLUS: &str = "+";

/// Latin words that make a date or a figure the edge of an open range only right before one
/// (`after 1 October 2025`, `beyond FY2025`), so `profit before tax` and `the following table`
/// stay plain.
const DATED: [&str; 6] = [
    "after",
    "before",
    "beyond",
    "following",
    "later than",
    "earlier than",
];

/// A filler word that may stand between ก่อน, หลัง or `after` and the date: วันที่, วัน, เดือน.
const FILLERS: [&str; 3] = [
    "\u{0E27}\u{0E31}\u{0E19}\u{0E17}\u{0E35}\u{0E48}", // วันที่
    "\u{0E27}\u{0E31}\u{0E19}",                         // วัน
    MONTH,
];

/// Ordinary words that hold ลบ with no sign in it: one letter, then ลบ.
const ORDINARY_LOP: [&str; 7] = [
    "\u{0E2A}\u{0E25}\u{0E1A}",                 // สลบ
    "\u{0E2B}\u{0E25}\u{0E1A}",                 // หลบ
    "\u{0E01}\u{0E25}\u{0E1A}",                 // กลบ
    "\u{0E15}\u{0E25}\u{0E1A}",                 // ตลบ
    "\u{0E1E}\u{0E25}\u{0E1A}",                 // พลบ
    "\u{0E02}\u{0E25}\u{0E1A}",                 // ขลบ
    "\u{0E1C}\u{0E25}\u{0E1A}\u{0E27}\u{0E01}", // ผลบวก
];

/// Thai weekday names and the English ones, full and short: written with a date, part of it.
const WEEKDAYS_TH: [&str; 7] = [
    "\u{0E08}\u{0E31}\u{0E19}\u{0E17}\u{0E23}\u{0E4C}", // จันทร์
    "\u{0E2D}\u{0E31}\u{0E07}\u{0E04}\u{0E32}\u{0E23}", // อังคาร
    "\u{0E1E}\u{0E38}\u{0E18}",                         // พุธ
    "\u{0E1E}\u{0E24}\u{0E2B}\u{0E31}\u{0E2A}\u{0E1A}\u{0E14}\u{0E35}", // พฤหัสบดี
    "\u{0E28}\u{0E38}\u{0E01}\u{0E23}\u{0E4C}",         // ศุกร์
    "\u{0E40}\u{0E2A}\u{0E32}\u{0E23}\u{0E4C}",         // เสาร์
    "\u{0E2D}\u{0E32}\u{0E17}\u{0E34}\u{0E15}\u{0E22}\u{0E4C}", // อาทิตย์
];
const THURSDAY_SHORT_TH: &str = "\u{0E1E}\u{0E24}\u{0E2B}\u{0E31}\u{0E2A}"; // พฤหัส
const WAN: &str = "\u{0E27}\u{0E31}\u{0E19}"; // วัน
const THI: &str = "\u{0E17}\u{0E35}\u{0E48}"; // ที่
const WEEKDAYS_EN: [&str; 14] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "wed",
    "thu",
    "fri",
    "sat",
    "sun",
];

/// What the scanners found at each code point of the text: where a signal stands decides
/// whether it counts.
pub(crate) struct Marks {
    /// Inside a reading, or the weekday written with a date: a signal there is part of the
    /// reading's pattern (the comma of `1,250`, the ถึง of a range read whole, วันพุธ in
    /// `วันพุธที่ 1 ตุลาคม 2568`).
    pub(crate) covered: Vec<bool>,
    /// A date or a month name starts here (one longer than the text).
    pub(crate) date_start: Vec<bool>,
    /// A reading, with the unit words that go with it, ends right before here (one longer than
    /// the text).
    pub(crate) reading_end: Vec<bool>,
    /// Where the line of the input that holds each code point starts: a dash that opens a line
    /// is a bullet or a minus, not a range.
    pub(crate) line_start: Vec<usize>,
    /// A signal no row of the table writes: a control or a Hangul filler step 1 deleted outside
    /// every reading (U+0096 is an en dash read with the wrong code page), and Latin letters
    /// glued to the end of a date (`FY2025YTD`, `FY2025E`), a period code the reader cannot read.
    pub(crate) marked: Vec<Option<Kind>>,
}

/// A number written right before `start`, one space allowed: a figure or a Thai number word.
fn number_before(t: &[char], start: usize) -> bool {
    let end = if start > 0 && t[start - 1] == ' ' {
        start - 1
    } else {
        start
    };
    end > 0 && (is_numeral(t[end - 1]) || crate::numbers::number_word_ends_at(t, end))
}

/// A date, a month name or a figure starts right after `end`: one space allowed, and a filler
/// word (วันที่, วัน, เดือน) with one space after it.
fn dated(t: &[char], end: usize, date_start: &[bool]) -> bool {
    let space = |at: usize| if t.get(at) == Some(&' ') { at + 1 } else { at };
    let at = space(end);
    let after_filler = FILLERS
        .iter()
        .find(|filler| has_at(t, at, filler))
        .map_or(at, |filler| space(at + filler.chars().count()));
    [at, after_filler].into_iter().any(|at| {
        date_start.get(at).copied().unwrap_or(false) || t.get(at).is_some_and(|c| is_numeral(*c))
    })
}

/// A text signal at `start..end` counts where it stands: a Latin one as a whole word; ลบ unless a
/// `.` (ลบ.ม.) or a Thai combining mark (ชลบุรี) follows it or it lies inside an ordinary word
/// (สลบ, ผลบวก); ต้น, กลาง, ปลาย and สิ้น before ปี or เดือน; เดือน and ส่วน after a number
/// (`9 เดือน`, `3 ส่วน 4`); ก่อน, หลัง, `after`, `before` and the other [`DATED`] words right
/// before a date or a figure; `+` right after a reading.
fn counts(t: &[char], start: usize, end: usize, text: &str, marks: &Marks) -> bool {
    if text.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        let word = |c: Option<&char>| c.is_some_and(|c| is_latin_letter(*c) || c.is_ascii_digit());
        if word(start.checked_sub(1).and_then(|prev| t.get(prev))) || word(t.get(end)) {
            return false;
        }
    }
    match text {
        LOP => {
            let after = t.get(end).copied();
            after != Some('.')
                && !after.is_some_and(crate::normalize::is_thai_mark)
                && !(start > 0 && ORDINARY_LOP.iter().any(|word| has_at(t, start - 1, word)))
        }
        START | MIDDLE | LATE | END => has_at(t, end, PI) || has_at(t, end, MONTH),
        MONTH | PART => number_before(t, start),
        BEFORE | AFTER => dated(t, end, &marks.date_start),
        _ if DATED.contains(&text) => dated(t, end, &marks.date_start),
        PLUS => marks.reading_end[start],
        _ => true,
    }
}

/// A dash or an arrow at `at` counts: not a hyphen between two letters (`year-on-year`), and not
/// a bullet that opens its line or a label's value before a word or a date (`- รายได้รวม`,
/// `- 15 ต.ค. 2568`). Opening them before a figure, it may be a minus (`net profit: - 5.2
/// million`), so it counts as a sign.
fn dash_counts(t: &[char], at: usize, marks: &Marks) -> Option<Kind> {
    let letter = |c: Option<&char>| c.is_some_and(|c| c.is_alphabetic());
    if t[at] == '-'
        && letter(at.checked_sub(1).and_then(|prev| t.get(prev)))
        && letter(t.get(at + 1))
    {
        return None;
    }
    let opens = t[marks.line_start[at]..at]
        .iter()
        .all(|c| c.is_whitespace())
        || t[..at].iter().rev().find(|c| !c.is_whitespace()) == Some(&':');
    if opens {
        let next = if t.get(at + 1) == Some(&' ') {
            at + 2
        } else {
            at + 1
        };
        let figure = t.get(next).is_some_and(|c| is_numeral(*c)) && !marks.date_start[next];
        return figure.then_some(Kind::Sign);
    }
    Some(Kind::Range)
}

/// `range` of a date with the weekday written with it (`วันพุธที่ 1 ตุลาคม 2568`, `Monday 6
/// October 2025`, `Wed. 1 October 2025`, `1 ตุลาคม 2568 (วันพุธ)`): there the weekday is part
/// of the date.
pub(crate) fn with_weekday(t: &[char], range: &Range<usize>) -> Range<usize> {
    let back = |at: usize| {
        if at > 0 && t[at - 1] == ' ' {
            at - 1
        } else {
            at
        }
    };
    let mut start = range.start;
    let mut at = back(range.start);
    if has_before(t, at, THI) {
        at -= THI.chars().count();
    }
    let thai = WEEKDAYS_TH
        .iter()
        .copied()
        .chain([THURSDAY_SHORT_TH])
        .find(|day| has_before(t, at, day));
    if let Some(day) = thai {
        let day_start = at - day.chars().count();
        start = if has_before(t, day_start, WAN) {
            day_start - WAN.chars().count()
        } else {
            day_start
        };
    } else {
        let at = if at > 0 && matches!(t[at - 1], ',' | '.') {
            at - 1
        } else {
            at
        };
        if let Some(day) = WEEKDAYS_EN.iter().find(|day| has_before(t, at, day)) {
            let day_start = at - day.chars().count();
            if day_start == 0 || !is_latin_letter(t[day_start - 1]) {
                start = day_start;
            }
        }
    }
    let mut end = range.end;
    let open = if t.get(range.end) == Some(&' ') {
        range.end + 1
    } else {
        range.end
    };
    if t.get(open) == Some(&'(') {
        let mut inner = open + 1;
        if has_at(t, inner, WAN) {
            inner += WAN.chars().count();
        }
        let day = WEEKDAYS_TH
            .iter()
            .chain(WEEKDAYS_EN.iter())
            .chain([&THURSDAY_SHORT_TH])
            .find(|day| has_at(t, inner, day));
        if let Some(day) = day {
            let close = inner + day.chars().count();
            if t.get(close) == Some(&')') {
                end = close + 1;
            }
        }
    }
    start..end
}

/// One signal found in the text.
#[derive(Clone, Copy, Debug)]
struct Occurrence {
    start: usize,
    end: usize,
}

/// What the signals of a reading's text say, outside its own span.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Signals {
    /// Some signal stands in the text: the reading is capped.
    pub(crate) any: bool,
    /// A sign signal stands in the text: a figure's negative is another reading.
    pub(crate) sign: bool,
}

/// The signals of a text.
pub(crate) struct Context {
    /// Every signal that counts, in text order, the sign signals among them, and the range and
    /// list signals.
    found: Vec<Occurrence>,
    signs: Vec<Occurrence>,
    joins: Vec<Occurrence>,
}

impl Context {
    pub(crate) fn new(t: &[char], marks: &Marks) -> Self {
        let mut found = Vec::new();
        let mut signs = Vec::new();
        let mut joins = Vec::new();
        let mut push = |occurrence: Occurrence, kind: Kind| {
            found.push(occurrence);
            match kind {
                Kind::Sign => signs.push(occurrence),
                Kind::Range | Kind::List => joins.push(occurrence),
                Kind::Fraction | Kind::Period => {}
            }
        };
        for (at, c) in t.iter().enumerate() {
            crate::found::work(1);
            // Inside a reading a signal is part of its pattern: the comma of `1,250`, the dash of
            // `2567-2569`, the ถึง of a range read whole.
            if marks.covered[at] {
                continue;
            }
            if let Some(kind) = marks.marked[at] {
                push(
                    Occurrence {
                        start: at,
                        end: at + 1,
                    },
                    kind,
                );
            }
            for signal in &SIGNALS {
                let (end, kind) = match signal.pattern {
                    Pattern::Text(text) => {
                        if !text.starts_with(*c) || !has_at(t, at, text) {
                            continue;
                        }
                        let end = at + text.chars().count();
                        if !counts(t, at, end, text, marks) {
                            continue;
                        }
                        (end, signal.kind)
                    }
                    Pattern::Class(name, class) => {
                        if !class(*c) {
                            continue;
                        }
                        let kind = if name == DASHES {
                            let Some(kind) = dash_counts(t, at, marks) else {
                                continue;
                            };
                            kind
                        } else {
                            signal.kind
                        };
                        (at + 1, kind)
                    }
                };
                push(Occurrence { start: at, end }, kind);
            }
        }
        Context {
            found,
            signs,
            joins,
        }
    }

    /// A range or list signal stands between `from` and `to`: the two readings there may be the
    /// ends of one range or items of one list.
    pub(crate) fn joined(&self, from: usize, to: usize) -> bool {
        let first = self.joins.partition_point(|found| found.start < from);
        self.joins[first..]
            .iter()
            .take_while(|found| found.start < to)
            .any(|found| found.end <= to)
    }

    /// The signals in the text of a reading whose own span is `own`, outside it.
    pub(crate) fn signals(&self, own: &Range<usize>) -> Signals {
        let outside = |list: &[Occurrence]| {
            let first = list.partition_point(|found| found.start < own.start);
            let last = list
                .partition_point(|found| found.start < own.end)
                .max(first);
            let inside = list[first..last]
                .iter()
                .filter(|found| found.end <= own.end)
                .count();
            list.len() > inside
        };
        Signals {
            any: outside(&self.found),
            sign: outside(&self.signs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(signal: &Signal) -> String {
        match signal.pattern {
            Pattern::Text(text) => text.to_owned(),
            Pattern::Class(DASHES, _) => "\u{21D2}".to_owned(),
            Pattern::Class(..) => "\u{FFFD}".to_owned(),
        }
    }

    #[test]
    fn signals_are_written_as_normalize_th_writes_them() {
        for signal in SIGNALS {
            if let Pattern::Text(text) = signal.pattern {
                assert_eq!(crate::normalize_th(text).text, text, "{signal:?}");
            }
        }
        for word in ORDINARY_LOP
            .iter()
            .chain(WEEKDAYS_TH.iter())
            .chain(WEEKDAYS_EN.iter())
            .chain(DATED.iter())
            .chain(FILLERS.iter())
        {
            assert_eq!(crate::normalize_th(word).text, *word, "{word:?}");
        }
    }

    /// Every entry of the table, in a text with one figure (or one year, for a period word or a
    /// word that counts only before a date), caps that reading, and the same text with a plain
    /// word in its place does not.
    #[test]
    fn every_signal_in_the_table_caps_a_reading_in_its_text() {
        let ctx = crate::Ctx {
            anchor: chrono::NaiveDate::from_ymd_opt(2025, 11, 1),
        };
        let figure = "1,250 \u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E1A}\u{0E32}\u{0E17}"; // 1,250 ล้านบาท
        let year = "\u{0E1B}\u{0E35} 2568"; // ปี 2568
        let plain = "\u{0E04}\u{0E37}\u{0E2D}"; // คือ
        let three = "\u{0E2A}\u{0E32}\u{0E21}"; // สาม, a number word that is no reading alone
        let capped = |text: &str| {
            let numbers = crate::find_numbers(text, &ctx).unwrap();
            let quantities: Vec<_> = numbers
                .iter()
                .filter(|found| found.value.role == crate::NumberRole::Quantity)
                .collect();
            let dates = crate::find_dates(text, &ctx).unwrap();
            assert!(
                !(quantities.is_empty() && dates.is_empty()),
                "{text:?} reads nothing"
            );
            quantities.iter().all(|found| found.conf <= CONF_CAP)
                && dates.iter().all(|found| found.conf <= CONF_CAP)
        };
        for signal in SIGNALS {
            let word = example(&signal);
            let dated = [BEFORE, AFTER].contains(&word.as_str()) || DATED.contains(&word.as_str());
            // Each entry where its own rule lets it count, kept apart from the reading.
            let probe = if [START, MIDDLE, LATE, END].contains(&word.as_str()) {
                format!("{word}{year}")
            } else if [MONTH, PART].contains(&word.as_str()) {
                format!("{three}{word} {plain} {year}")
            } else if dated {
                format!("{plain} {word} {year}")
            } else if word == PLUS {
                format!("{plain} {figure}{word}")
            } else {
                format!("{plain} {word} {plain} {figure}")
            };
            let control = if [MONTH, PART].contains(&word.as_str()) || dated {
                format!("{plain} {plain} {year}")
            } else {
                format!("{plain} {plain} {plain} {figure}")
            };
            assert!(capped(&probe), "{signal:?} caps nothing in {probe:?}");
            assert!(!capped(&control), "{signal:?}: {control:?} is capped too");
        }
    }

    #[test]
    fn no_signal_is_listed_twice() {
        for (i, a) in SIGNALS.iter().enumerate() {
            for b in &SIGNALS[i + 1..] {
                assert_ne!(example(a), example(b));
            }
        }
    }

    /// The table, with each entry's kind and the dash and arrow class over every scalar value,
    /// and the weekday names a date takes in, fingerprinted: deleting or changing a row changes
    /// it, and the test names the table.
    #[test]
    fn signal_table_is_frozen() {
        const TABLE: u64 = 0xd608_e3b6_52d0_d3e4;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |byte: u8| {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        };
        for signal in SIGNALS {
            let name = match signal.pattern {
                Pattern::Text(text) => text,
                Pattern::Class(name, _) => name,
            };
            name.bytes().for_each(&mut eat);
            eat(signal.kind as u8);
            eat(0xff);
        }
        for word in WEEKDAYS_TH
            .iter()
            .chain([&THURSDAY_SHORT_TH])
            .chain(WEEKDAYS_EN.iter())
            .chain(DATED.iter())
            .chain(FILLERS.iter())
        {
            word.bytes().for_each(&mut eat);
            eat(0xfd);
        }
        for c in (0..=0x10_FFFF).filter_map(char::from_u32) {
            if dash_or_arrow(c) {
                (c as u32).to_le_bytes().into_iter().for_each(&mut eat);
            }
            if unknown_glyph(c) {
                eat(0xfe);
                (c as u32).to_le_bytes().into_iter().for_each(&mut eat);
            }
        }
        assert_eq!(
            hash, TABLE,
            "the signal table changed (fingerprint {hash:#018x}): update TABLE and say so in the PR"
        );
    }
}
