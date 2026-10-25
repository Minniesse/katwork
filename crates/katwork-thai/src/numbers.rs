//! Figures, Thai number words and scale words.

use std::ops::Range;

use rust_decimal::Decimal;

use crate::context::{CONF_CAP, CONF_OTHER, Context, Target};
use crate::dates::{self, DateValue, Hit, Scan};
use crate::digits::{Script, script};
use crate::found::{
    View, char_len, check_len, code_letter, digits_end, foreign_separator, has_at, has_before,
    is_digit, is_latin_letter, is_numeral, is_thai, numerals_end, unknown_glyph,
};
use crate::{Ctx, Flags, Found, TooLong};

/// A power of ten that a figure is multiplied by, as `ล้าน` (6) or `billion` (9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scale(pub u8);

/// The largest power of ten a `Decimal` holds.
const MAX_SCALE: u32 = 28;

impl Scale {
    /// 10 to the power of the scale; `Decimal::MAX` for a scale above 28, which no scan
    /// returns (see [`Number::checked_effective`]).
    pub fn multiplier(self) -> Decimal {
        power_of_ten(u32::from(self.0)).unwrap_or(Decimal::MAX)
    }
}

fn power_of_ten(exponent: u32) -> Option<Decimal> {
    if exponent > MAX_SCALE {
        return None;
    }
    Decimal::try_from_i128_with_scale(10_i128.checked_pow(exponent)?, 0).ok()
}

/// What a figure counts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    Percent,
    /// An ISO 4217 code: `THB` or `USD`.
    Currency(&'static str),
    /// Reserved: SP2a never produces it, so an unknown word after a figure is not a unit.
    Other(String),
}

/// How a number was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Form {
    Digits,
    Words,
    /// A figure followed by number words, as `3 ล้านห้าแสน` (3,500,000).
    Mixed,
}

/// What a number is in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NumberRole {
    /// An amount, the only role the number checks compare.
    Quantity,
    /// A position, after a marker such as วาระที่ or `No.`.
    Ordinal,
    /// A clock time.
    Time,
    /// An identifier, or a figure no `Decimal` holds.
    Code,
    /// A day or year inside a date.
    DatePart,
}

/// A number as written: its value before the scale, the scale, the unit and the precision.
#[derive(Clone, Debug, PartialEq)]
pub struct Number {
    pub value: Decimal,
    pub scale: Scale,
    pub unit: Option<Unit>,
    pub form: Form,
    /// Digits written after the decimal point.
    pub decimals: u32,
    pub role: NumberRole,
}

impl Number {
    /// The value times the scale. A number built by hand whose product does not fit saturates
    /// at `Decimal::MAX` or `Decimal::MIN`; [`Number::checked_effective`] reports it instead.
    pub fn effective(&self) -> Decimal {
        self.checked_effective()
            .unwrap_or(if self.value.is_sign_negative() {
                Decimal::MIN
            } else {
                Decimal::MAX
            })
    }

    /// The value times the scale, or `None` when it does not fit in a `Decimal`. Every number
    /// `find_numbers` returns fits.
    pub fn checked_effective(&self) -> Option<Decimal> {
        if self.value.is_zero() {
            return Some(self.value);
        }
        self.value
            .checked_mul(power_of_ten(u32::from(self.scale.0))?)
    }

    /// Half a unit of the last displayed digit, times the scale: 0.005 × 10⁹ for `1.25 พันล้าน`.
    /// Zero when that cannot be held exactly, which only a number built by hand can need.
    pub fn half_ulp(&self) -> Decimal {
        let exponent = i64::from(self.scale.0) - i64::from(self.decimals) - 1;
        match u32::try_from(exponent) {
            Ok(up) => power_of_ten(up).and_then(|power| power.checked_mul(Decimal::from(5))),
            Err(_) => u32::try_from(-exponent)
                .ok()
                .and_then(|down| Decimal::try_from_i128_with_scale(5, down).ok()),
        }
        .unwrap_or(Decimal::ZERO)
    }
}

/// A Thai number word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Word {
    Digit(u8),
    /// สิบ, ร้อย, พัน, หมื่น or แสน, as its power of ten.
    Power(u8),
    Lan,
    Lop,
    Chut,
    Sun,
}

/// PyThaiNLP's numeral words, as normalize_th writes them. No word is a prefix of another, so
/// longest match is the only match.
const WORDS: [(&str, Word); 20] = [
    ("\u{0E2B}\u{0E19}\u{0E36}\u{0E48}\u{0E07}", Word::Digit(1)), // หนึ่ง
    ("\u{0E40}\u{0E2D}\u{0E47}\u{0E14}", Word::Digit(1)),         // เอ็ด
    ("\u{0E2A}\u{0E2D}\u{0E07}", Word::Digit(2)),                 // สอง
    ("\u{0E22}\u{0E35}\u{0E48}", Word::Digit(2)),                 // ยี่
    ("\u{0E2A}\u{0E32}\u{0E21}", Word::Digit(3)),                 // สาม
    ("\u{0E2A}\u{0E35}\u{0E48}", Word::Digit(4)),                 // สี่
    ("\u{0E2B}\u{0E49}\u{0E32}", Word::Digit(5)),                 // ห้า
    ("\u{0E2B}\u{0E01}", Word::Digit(6)),                         // หก
    ("\u{0E40}\u{0E08}\u{0E47}\u{0E14}", Word::Digit(7)),         // เจ็ด
    ("\u{0E41}\u{0E1B}\u{0E14}", Word::Digit(8)),                 // แปด
    ("\u{0E40}\u{0E01}\u{0E49}\u{0E32}", Word::Digit(9)),         // เก้า
    ("\u{0E2A}\u{0E34}\u{0E1A}", Word::Power(1)),                 // สิบ
    ("\u{0E23}\u{0E49}\u{0E2D}\u{0E22}", Word::Power(2)),         // ร้อย
    ("\u{0E1E}\u{0E31}\u{0E19}", Word::Power(3)),                 // พัน
    ("\u{0E2B}\u{0E21}\u{0E37}\u{0E48}\u{0E19}", Word::Power(4)), // หมื่น
    ("\u{0E41}\u{0E2A}\u{0E19}", Word::Power(5)),                 // แสน
    ("\u{0E25}\u{0E49}\u{0E32}\u{0E19}", Word::Lan),              // ล้าน
    ("\u{0E25}\u{0E1A}", Word::Lop),                              // ลบ
    ("\u{0E08}\u{0E38}\u{0E14}", Word::Chut),                     // จุด
    ("\u{0E28}\u{0E39}\u{0E19}\u{0E22}\u{0E4C}", Word::Sun),      // ศูนย์
];

/// Ordinary words that hold a power word or ล้าน: no number word inside one starts a number,
/// and after a figure none is a scale (`3 พันธุ์` is three varieties, `5 สิบเอก` five sergeants).
const ORDINARY_WORDS: [&str; 23] = [
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E18}", // พันธ (พันธุ์, พันธ์, พันธะ, พันธมิตร, กุมภาพันธ์)
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E15}\u{0E23}\u{0E35}", // พันตรี
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E42}\u{0E17}", // พันโท
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E40}\u{0E2D}\u{0E01}", // พันเอก
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E15}\u{0E33}\u{0E23}\u{0E27}\u{0E08}", // พันตำรวจ
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E08}\u{0E48}\u{0E32}", // พันจ่า
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E15}\u{0E23}\u{0E35}", // ร้อยตรี
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E42}\u{0E17}", // ร้อยโท
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E40}\u{0E2D}\u{0E01}", // ร้อยเอก
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E15}\u{0E33}\u{0E23}\u{0E27}\u{0E08}", // ร้อยตำรวจ
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E40}\u{0E2D}\u{0E47}\u{0E14}", // ร้อยเอ็ด
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E01}\u{0E23}\u{0E2D}\u{0E07}", // ร้อยกรอง
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E40}\u{0E23}\u{0E35}\u{0E22}\u{0E07}", // ร้อยเรียง
    "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E41}\u{0E01}\u{0E49}\u{0E27}", // ร้อยแก้ว
    "\u{0E40}\u{0E23}\u{0E35}\u{0E22}\u{0E1A}\u{0E23}\u{0E49}\u{0E2D}\u{0E22}", // เรียบร้อย
    "\u{0E2A}\u{0E23}\u{0E49}\u{0E2D}\u{0E22}", // สร้อย
    "\u{0E2A}\u{0E34}\u{0E1A}\u{0E15}\u{0E23}\u{0E35}", // สิบตรี
    "\u{0E2A}\u{0E34}\u{0E1A}\u{0E42}\u{0E17}", // สิบโท
    "\u{0E2A}\u{0E34}\u{0E1A}\u{0E40}\u{0E2D}\u{0E01}", // สิบเอก
    "\u{0E2A}\u{0E34}\u{0E1A}\u{0E15}\u{0E33}\u{0E23}\u{0E27}\u{0E08}", // สิบตำรวจ
    "\u{0E41}\u{0E2A}\u{0E19}\u{0E2A}\u{0E38}\u{0E02}", // แสนสุข
    "\u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E19}\u{0E32}", // ล้านนา
    "\u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E0A}\u{0E49}\u{0E32}\u{0E07}", // ล้านช้าง
];

const ROI_LA: &str = "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E25}\u{0E30}"; // ร้อยละ
const BAHT_THUAN: &str = "\u{0E1A}\u{0E32}\u{0E17}\u{0E16}\u{0E49}\u{0E27}\u{0E19}"; // บาทถ้วน
const NA_DOT: &str = "\u{0E19}."; // น. (the hour)
const NALIKA: &str = "\u{0E19}\u{0E32}\u{0E2C}\u{0E34}\u{0E01}\u{0E32}"; // นาฬิกา (o'clock)
const ET: &str = "\u{0E40}\u{0E2D}\u{0E47}\u{0E14}"; // เอ็ด (the one after a tens or hundreds)
const KHRUENG: &str = "\u{0E04}\u{0E23}\u{0E36}\u{0E48}\u{0E07}"; // ครึ่ง (half)
const TID: &str = "\u{0E15}\u{0E34}\u{0E14}"; // ติด, as in ติดลบ
const TID_LOP: &str = "\u{0E15}\u{0E34}\u{0E14}\u{0E25}\u{0E1A}"; // ติดลบ (negative)
const LOP: &str = "\u{0E25}\u{0E1A}"; // ลบ (minus, or "delete")
const THUENG: &str = "\u{0E16}\u{0E36}\u{0E07}"; // ถึง (to)

/// Unit words after a figure, longest first; Latin ones count only as whole words.
const UNITS_AFTER: [(&str, Unit); 10] = [
    (BAHT_THUAN, Unit::Currency("THB")),
    ("\u{0E1A}\u{0E32}\u{0E17}", Unit::Currency("THB")), // บาท
    (
        "\u{0E40}\u{0E1B}\u{0E2D}\u{0E23}\u{0E4C}\u{0E40}\u{0E0B}\u{0E47}\u{0E19}\u{0E15}\u{0E4C}",
        Unit::Percent,
    ), // เปอร์เซ็นต์
    (
        "\u{0E40}\u{0E1B}\u{0E2D}\u{0E23}\u{0E4C}\u{0E40}\u{0E0B}\u{0E19}\u{0E15}\u{0E4C}",
        Unit::Percent,
    ), // เปอร์เซนต์
    ("per cent", Unit::Percent),
    ("percent", Unit::Percent),
    ("baht", Unit::Currency("THB")),
    ("thb", Unit::Currency("THB")),
    ("usd", Unit::Currency("USD")),
    ("%", Unit::Percent),
];

/// Unit words before a figure.
const UNITS_BEFORE: [(&str, Unit); 3] = [
    (ROI_LA, Unit::Percent),
    ("thb", Unit::Currency("THB")),
    ("usd", Unit::Currency("USD")),
];

/// Symbol units, part of the span when written with no space.
fn symbol_unit(c: char) -> Option<Unit> {
    match c {
        '%' => Some(Unit::Percent),
        '\u{0E3F}' => Some(Unit::Currency("THB")),
        '$' => Some(Unit::Currency("USD")),
        _ => None,
    }
}

/// Latin abbreviations of a scale the reader does not read, one space after a figure (`5 mln`,
/// `1,250 MB`): the figure is one code with them. A single `M`, `B`, `K` or `T` counts too, in
/// capitals only.
const LOOSE_SCALES: [&str; 6] = ["mil", "mln", "bln", "bil", "mm", "mb"];

const ENGLISH_SCALES: [(&str, u32); 7] = [
    ("thousand", 3),
    ("million", 6),
    ("billion", 9),
    ("trillion", 12),
    ("mn", 6),
    ("bn", 9),
    ("tn", 12),
];

/// Ordinal markers, longest first. Thai ones may follow Thai text directly.
const ORDINAL_MARKERS: [&str; 12] = [
    "\u{0E27}\u{0E32}\u{0E23}\u{0E30}\u{0E17}\u{0E35}\u{0E48}", // วาระที่
    "\u{0E04}\u{0E23}\u{0E31}\u{0E49}\u{0E07}\u{0E17}\u{0E35}\u{0E48}", // ครั้งที่
    "\u{0E25}\u{0E33}\u{0E14}\u{0E31}\u{0E1A}\u{0E17}\u{0E35}\u{0E48}", // ลำดับที่
    "\u{0E17}\u{0E35}\u{0E48}",                                 // ที่
    "\u{0E02}\u{0E49}\u{0E2D}",                                 // ข้อ
    "\u{0E2B}\u{0E19}\u{0E49}\u{0E32}",                         // หน้า
    "no.",
    "#",
    "item",
    "agenda",
    "section",
    "page",
];

fn word_at(t: &[char], at: usize) -> Option<(Word, usize)> {
    crate::found::work(1);
    WORDS
        .iter()
        .find(|(text, _)| has_at(t, at, text))
        .map(|(text, word)| (*word, at + char_len(text)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordsError {
    /// Not a numeral by PyThaiNLP's grammar.
    Invalid,
    /// A numeral too large for a `Decimal`.
    Overflow,
}

/// A number word, or a figure standing where a digit word would: the figure of a Mixed number or
/// of a compound amount.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Term {
    Word(Word),
    /// A figure and its decimal places, used as written: never raised to one.
    Figure(Decimal, i64),
}

fn terms(words: &[Word]) -> Vec<Term> {
    words.iter().map(|word| Term::Word(*word)).collect()
}

/// `thaiword_to_num`'s grammar over `terms`: `(G ล้าน)*G` with
/// `G = (D แสน)?(D หมื่น)?(D พัน)?(D ร้อย)?(D สิบ)?D?`, where a figure may stand for a `D`. The
/// pending digit starts at 1, so a bare power word or ล้าน means one of it; a power word takes
/// `max(pending, 1)` (a figure as written) and leaves 0 pending; ล้าน multiplies everything so
/// far. Also returns the lowest place the terms write: 5 for `3 ล้านห้าแสน`, -1 for `3.5`.
fn numeral(terms: &[Term]) -> Result<(Decimal, i64), WordsError> {
    let million = Decimal::from(1_000_000);
    // None once the total no longer fits; the grammar is still checked to the end.
    let mut total = Some(Decimal::ZERO);
    let mut pending = Decimal::ONE;
    let mut exact = false;
    let mut pending_place = 0;
    let mut place = 0;
    let mut last_power: Option<u8> = None;
    let mut after_digit = false;
    for term in terms {
        match *term {
            Term::Figure(value, decimals) => {
                if after_digit {
                    return Err(WordsError::Invalid);
                }
                after_digit = true;
                pending = value;
                exact = true;
                pending_place = -decimals;
                place = pending_place;
            }
            Term::Word(Word::Digit(digit)) => {
                if after_digit {
                    return Err(WordsError::Invalid);
                }
                after_digit = true;
                pending = Decimal::from(digit);
                exact = false;
                pending_place = 0;
                place = 0;
            }
            Term::Word(Word::Power(power)) => {
                if last_power.is_some_and(|last| power >= last) {
                    return Err(WordsError::Invalid);
                }
                last_power = Some(power);
                after_digit = false;
                let times = if exact {
                    pending
                } else {
                    pending.max(Decimal::ONE)
                };
                let add = power_of_ten(u32::from(power)).and_then(|unit| times.checked_mul(unit));
                total = total
                    .zip(add)
                    .and_then(|(total, add)| total.checked_add(add));
                place = i64::from(power) + pending_place;
                pending = Decimal::ZERO;
                exact = false;
                pending_place = 0;
            }
            Term::Word(Word::Lan) => {
                last_power = None;
                after_digit = false;
                total = total
                    .and_then(|total| total.checked_add(pending))
                    .and_then(|sum| sum.checked_mul(million));
                place += 6;
                pending = Decimal::ZERO;
                exact = false;
                pending_place = 0;
            }
            Term::Word(Word::Lop | Word::Chut | Word::Sun) => return Err(WordsError::Invalid),
        }
    }
    let total = total
        .and_then(|total| total.checked_add(pending))
        .ok_or(WordsError::Overflow)?;
    Ok((total, place))
}

/// `thaiword_to_num`: `(ลบ)?` and [`numeral`]'s grammar, or `ศูนย์` alone. A leading ลบ
/// negates, and ลบ alone is −1, as in the oracle.
fn integer(words: &[Word]) -> Result<Decimal, WordsError> {
    if words.is_empty() {
        return Err(WordsError::Invalid);
    }
    if words == [Word::Sun] {
        return Ok(Decimal::ZERO);
    }
    let (negative, words) = match words.split_first() {
        Some((Word::Lop, rest)) => (true, rest),
        _ => (false, words),
    };
    let (mut total, _) = numeral(&terms(words))?;
    if negative && !total.is_zero() {
        total.set_sign_negative(true);
    }
    Ok(total)
}

/// Terms the grammar and a reader may read differently: a part that ends in ล้าน, then a later
/// part with its own ล้าน that fits below it. The grammar multiplies the earlier part again
/// (สามพันล้านห้าร้อยล้าน is 3 × 10¹⁵ + 5 × 10⁸), where a reader may mean their sum (3.5 × 10⁹).
fn regrouped(terms: &[Term]) -> bool {
    let lan = Term::Word(Word::Lan);
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, pair) in terms.windows(2).enumerate() {
        if pair[0] == lan && pair[1] != lan {
            parts.push(&terms[start..=index]);
            start = index + 1;
        }
    }
    parts.push(&terms[start..]);
    if !parts.iter().skip(1).any(|part| part.contains(&lan)) {
        return false;
    }
    let Some(read) = parts
        .iter()
        .map(|part| numeral(part).ok())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let fits = read.windows(2).all(|pair| {
        u32::try_from(pair[0].1)
            .ok()
            .and_then(power_of_ten)
            .is_some_and(|limit| pair[1].0 < limit)
    });
    let sum = read
        .iter()
        .try_fold(Decimal::ZERO, |sum, (value, _)| sum.checked_add(*value));
    fits && !matches!((numeral(terms), sum), (Ok((whole, _)), Some(sum)) if whole == sum)
}

/// `words_to_num`: without จุด the words are one numeral; with it, the words before the first
/// จุด are the integer and each word after it is read alone as the next decimal place, added
/// (subtracted when the integer is −1 or less). Exact, where PyThaiNLP adds floats.
fn words_value(words: &[Word]) -> Result<Decimal, WordsError> {
    let Some(point) = words.iter().position(|word| *word == Word::Chut) else {
        return integer(words);
    };
    let whole = integer(&words[..point])?;
    let mut fraction = Decimal::ZERO;
    let mut places = 0;
    for word in &words[point + 1..] {
        let digit = integer(std::slice::from_ref(word))?;
        places += 1;
        let part = Decimal::try_from_i128_with_scale(digit.mantissa(), digit.scale() + places)
            .map_err(|_| WordsError::Overflow)?;
        fraction = fraction.checked_add(part).ok_or(WordsError::Overflow)?;
    }
    let mut value = if whole <= Decimal::NEGATIVE_ONE {
        whole.checked_sub(fraction)
    } else {
        whole.checked_add(fraction)
    }
    .ok_or(WordsError::Overflow)?;
    // Adding a zero drops its scale; `7 จุด ศูนย์ ศูนย์ ศูนย์` keeps its three places.
    value.rescale(places);
    Ok(value)
}

/// The value of a number written in Thai words: a port of PyThaiNLP 5.4.0's `thaiword_to_num`
/// and `words_to_num`, tokenized by longest match over its fixed word list.
///
/// ศูนย์ is 0; the pending digit starts at 1, so a bare power word (ร้อย, พัน) or a bare ล้าน
/// means one of it; after another power word the pending digit is 0, so สิบล้าน is ten million;
/// ล้าน multiplies everything accumulated so far, so ล้านล้าน is 10¹²; a leading ลบ negates;
/// จุด starts decimals, each later word read alone as one decimal place. It follows the
/// oracle's quirks too: ลบ alone is −1, and สองจุดสิบ is 3.0. `None` for anything else, and for
/// a value a `Decimal` cannot hold.
pub fn parse_number_words(text: &str) -> Option<Decimal> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let (word, end) = word_at(&chars, at)?;
        words.push(word);
        at = end;
    }
    words_value(&words).ok()
}

/// Finds the numbers in `text`, whatever their role, in order.
///
/// A figure is digits with `,` thousands (a first group of 1–3 digits, later groups of exactly
/// 3) and `.` decimals, in any of the digit scripts `normalize_th` folds. A `-` (and each dash
/// `normalize_th` folds to it) touching the figure is a minus, unless a figure or a month name
/// stands right before the dash: `10-15` is a range, and so is `10 - 15`, but `10 -15` holds −15.
/// ติดลบ or `minus` right before a figure makes it negative (`minus` right after another number is
/// ambiguous, a subtraction or a sign); a bare ลบ, which also means "delete", leaves it positive at
/// confidence 0.5 with the negative reading in `alts`. `(1,250)` is an accounting negative, and so
/// are one or two digits in parentheses before a scale word or a unit, as `(5) ล้านบาท`; otherwise
/// `(1)` is a list marker. Scale words after a figure add their powers of ten (พันล้าน is 9). A
/// unit may stand before or after a figure; a currency word or symbol between two figures is the
/// first one's (`100 USD 3,500 บาท`), unless the second has no unit of its own and the first
/// already has one (`USD 100 THB 3,500`), or it is a symbol written against the second (`5 $10`).
/// A figure followed by number words that start with a power word or ล้าน is one `Mixed` number,
/// whatever follows it, and so is a figure with its scale followed by a figure or digit-led
/// number words with a smaller scale of their own (`3 ล้าน 9 แสน`), or by a figure with
/// thousands separators or a currency that writes only places below it (`1 ล้าน 250,000 บาท`); a
/// small bare figure never joins. Number words split by one space join the same way
/// (`สามล้าน ห้าแสน`, `หนึ่งล้าน 2 แสน`).
///
/// What the reader cannot read is one `Code` of value 0 over the whole run, never a reading of
/// part of it: a figure no `Decimal` holds; digits joined by anything but a boundary the reader
/// knows (`1·250`, `1'250`, U+FFFD, a thin space, a control or a Hangul filler in the input, a
/// combining mark: a rule, not a list), a glyph the text layer could not name touching a figure,
/// a digit `normalize_th` does not fold, or Arabic-Indic digits beside another script; digit
/// groups with letters between them (`12O5`, `A1B2`, `2Q68`); a fraction before a scale word or
/// a unit (`3/4 ล้าน`, `1/2%`); a comma group that is not three digits before a scale word or a
/// unit (`1,25 ล้าน`); a decimal part followed by a comma group (`1.234,5`); a figure with a Latin
/// abbreviation of a scale one space after it (`$5 M`, `5 mln`); a Mixed or compound number that
/// does not fit the grammar; ครึ่ง (half) beside a scale word; number words ending in a digit
/// word that a Thai letter continues (`5 พันเก้าอี้`); and number words with a power word or ล้าน
/// that do not form an amount. Number words are a
/// `Quantity` only when they start with a digit word, form a complete numeral with a power word
/// or ล้าน, stand right before a unit or percent word (one space allowed) or right after ร้อยละ,
/// or restate the figure before them in parentheses (`AMOUNT_IN_WORDS`; words that start with a
/// power word only when they equal it, as `(สิบล้านบาทถ้วน)`), and touch no date or month name;
/// right after ร้อยละ a numeral with no power word is a percent too (ร้อยละห้า). Number words
/// inside an ordinary word (สามารถ, เรียบร้อย, พันตรี) are no number.
///
/// The span covers the sign (a sign word too), accounting parentheses, the figure, its scale
/// words, a symbol unit written with no space and the letters glued to a code, never a unit word
/// except one between parts of the number (`1 THB million`, `ติดลบร้อยละ 6.1`). Spans are code
/// points of `text`; a text longer than [`crate::MAX_SCAN_CODE_POINTS`] is refused whole.
///
/// The role is the first that applies: `DatePart` for the digits of a date [`crate::find_dates`]
/// returns and for a day written before a month name with no year; `Code` for a figure right
/// after a stray dot or comma (`.5`, `x,250`), for one with Latin (or other non-Thai) letters
/// glued to it (`$5M`, `COVID19`) unless an English scale word is glued on (`1.25bn`) or the
/// letters follow a symbol unit (`12.5%YoY`), and for one with a Latin scale abbreviation after
/// it; `Quantity` when a scale word or a unit goes with the figure, whatever precedes it;
/// `Ordinal` after วาระที่, ข้อ, ครั้งที่, ลำดับที่, ที่, หน้า, `No.`, `#`, `item`, `agenda`,
/// `section` or `page`, with at most one space between; `Time` for `HH:MM` and `HH:MM:SS`, or
/// `HH.MM` before `น.` or นาฬิกา or before a range whose end has it; `Code` when a slash touches
/// it, it holds two dots, or it is one of three or more digit groups joined by `-`; otherwise
/// `Quantity`.
///
/// Confidence follows the clean-context rule ([`crate::find_dates`] keeps it too): a quantity
/// reads at 1.0 only when it matches a pattern the reader knows exactly and no signal of a
/// range, a list, a fraction or a sign stands near it (ถึง, จนถึง, ตั้งแต่, จาก, ระหว่าง, และ,
/// หรือ, ติดลบ, ลบ, `to`, `until`, `and`, `or`, a dash, slash, arrow or comma joining it to another
/// figure or date, and the rest of the crate's one list). Joining and opening signals count next
/// to the reading (one space, and a filler such as วันที่, between); sign signals within 15 code
/// points before it. Otherwise its confidence is at most 0.6 and the readings the signal
/// suggests go in `alts`: the negative after a sign, the scale and unit of the other end of a
/// range. Patterns the reader does not know exactly read at most 0.6 too: a bare ลบ (0.5), a
/// percent or currency symbol inside accounting parentheses unless the figure is the whole text
/// (`(5%)` in prose may restate a rise; the positive reading is in `alts`), two currencies on
/// one figure (`USD 100 บาท`), and a power word followed by one digit word, which everyday Thai
/// reads one place lower (`หนึ่งหมื่นห้า` is 10,005 by the grammar and 15,000 in speech, which is
/// in `alts`).
///
/// The text is read through [`crate::normalize_th`], so mark order, Thai and full-width digits,
/// invisible characters and dash variants do not change what is found. Latin era markers are
/// matched in the original capitals, so pass the original text, not a normalized one, when it
/// may hold them.
pub fn find_numbers(text: &str, ctx: &Ctx) -> Result<Vec<Found<Number>>, TooLong> {
    check_len(text)?;
    let view = View::new(text);
    let (items, _) = read(&view, dates::anchor(ctx));
    Ok(items
        .into_iter()
        .map(|item| {
            let source = if item.lead {
                view.source_with_gap(&item.range)
            } else {
                view.source(&item.range)
            };
            view.found_at(source, item.number, item.alts, item.conf, item.flags)
        })
        .collect())
}

/// Every number and date of the text, each capped by the signals near it: what both scanners
/// return.
pub(crate) fn read(view: &View, anchor: chrono::NaiveDate) -> (Vec<Item>, Scan) {
    let mut dates = dates::scan(view, anchor);
    let mut items = {
        let mut scanner = Scanner::new(view, &dates);
        scanner.figures();
        scanner.words();
        scanner.items
    };
    items.retain(|item| !item.dead);
    items.sort_by_key(|item| item.range.start);
    clean_context(view, &mut items, &mut dates.hits, &dates.months);
    (items, dates)
}

/// Applies the clean-context rule: a quantity or a date with a signal near it reads at most
/// [`CONF_CAP`], with the readings the signal suggests in `alts`.
fn clean_context(view: &View, items: &mut [Item], hits: &mut [Hit], months: &[Range<usize>]) {
    let t = &view.chars;
    let readings: Vec<Range<usize>> = items
        .iter()
        .map(|item| item.extent.clone())
        .chain(hits.iter().map(|hit| hit.range.clone()))
        .chain(months.iter().cloned())
        .collect();
    let own: Vec<Range<usize>> = items
        .iter()
        .filter(|item| item.number.role == NumberRole::Quantity)
        .map(|item| item.range.clone())
        .chain(hits.iter().map(|hit| hit.range.clone()))
        .collect();
    let context = Context::new(t, &readings, &own);
    let mut by_start = vec![None; t.len() + 1];
    let mut by_end = vec![None; t.len() + 1];
    for (index, item) in items.iter().enumerate() {
        if let Some(slot) = by_start.get_mut(item.extent.start) {
            *slot = Some(index);
        }
        if let Some(slot) = by_end.get_mut(item.extent.end) {
            *slot = Some(index);
        }
    }
    let numbers: Vec<Number> = items.iter().map(|item| item.number.clone()).collect();
    for item in items
        .iter_mut()
        .filter(|item| item.number.role == NumberRole::Quantity)
    {
        let near = context.near(&item.extent, Target::Figure);
        if !near.capped {
            continue;
        }
        item.conf = item.conf.min(CONF_CAP);
        let mut alts = Vec::new();
        if near.sign {
            alts.push(opposite(&item.number));
        }
        let partners = near
            .partners_before
            .iter()
            .filter_map(|at| by_end.get(*at).copied().flatten())
            .chain(
                near.partners_after
                    .iter()
                    .filter_map(|at| by_start.get(*at).copied().flatten()),
            );
        for partner in partners {
            if let Some(carried) = carried(&item.number, &numbers[partner]) {
                alts.push(carried);
            }
        }
        for alt in alts {
            if alt != item.number && !item.alts.iter().any(|(other, _)| *other == alt) {
                item.alts.push((alt, CONF_OTHER));
            }
        }
    }
    for hit in hits.iter_mut() {
        let target = if matches!(hit.value, DateValue::Day { .. }) {
            Target::Day
        } else {
            Target::Period
        };
        if context.near(&hit.range, target).capped {
            hit.conf = hit.conf.min(CONF_CAP);
        }
    }
}

/// `number` with the scale and unit of the figure it is joined to, when it has none of its own:
/// the other reading of the bare end of `10-15 ล้านบาท`. A scale or a unit it has that differs
/// from the other's says it is no part of the same amount (`5% to 1,250 million`).
fn carried(number: &Number, partner: &Number) -> Option<Number> {
    let agrees = (number.scale.0 == 0 || number.scale == partner.scale)
        && (number.unit.is_none() || number.unit == partner.unit);
    if partner.role != NumberRole::Quantity || !agrees {
        return None;
    }
    let mut carried = number.clone();
    if carried.scale.0 == 0 && carried.form == Form::Digits {
        carried.scale = partner.scale;
    }
    if carried.unit.is_none() {
        carried.unit.clone_from(&partner.unit);
    }
    (carried != *number && carried.checked_effective().is_some()).then_some(carried)
}

pub(crate) struct Item {
    range: Range<usize>,
    /// The span and the unit words that go with it: where a signal must stand next to.
    extent: Range<usize>,
    number: Number,
    flags: Flags,
    conf: f32,
    alts: Vec<(Number, f32)>,
    /// The span starts at the code points step 1 deleted before it: a control the reader cannot
    /// place belongs to the code it touches.
    lead: bool,
    /// Replaced by a reading that took it in (number words joined to the figure after them).
    dead: bool,
}

/// A figure as lexed, in normalized code points.
#[derive(Clone, Debug)]
struct Token {
    start: usize,
    end: usize,
    kind: TokenKind,
    /// A code whose first digit follows a control or filler step 1 deleted.
    lead: bool,
}

#[derive(Clone, Debug)]
enum TokenKind {
    /// Digits of the integer and the decimal part, separators removed.
    Figure {
        int: Vec<u8>,
        frac: Vec<u8>,
        grouped: bool,
    },
    /// Digit groups joined by more than one `.`, by a decimal part and a comma group, or by a
    /// separator the grammar does not read; three or more joined by `-`; digits `normalize_th`
    /// does not fold; or Arabic-Indic digits beside another script.
    Code,
    /// `HH:MM`, or `HH:MM:SS` with an optional fraction of a second.
    Clock {
        hours: u32,
        minutes: u32,
        seconds: Option<u32>,
    },
}

/// The scale part of a figure, or a later part of a compound amount.
struct Group {
    end: usize,
    /// The part's whole value: the figure times its scale, plus the number words after it.
    value: Decimal,
    /// The lowest place the part writes: 6 for `3 ล้าน`, 5 for `3.5 ล้าน` and `3 ล้านห้าแสน`.
    place: i64,
    /// The sum of its scale words: พัน, หมื่น, แสน, ล้าน and the English ones.
    scale: u32,
    /// It holds number words beyond its scale words, so it makes a Mixed number.
    words: bool,
    /// Its terms for the grammar; empty for an English scale.
    terms: Vec<Term>,
    /// It ends with a digit word.
    digit_last: bool,
}

enum GroupRead {
    /// No scale or power word.
    None,
    Read(Group),
    /// Words the reader cannot place: one code up to here.
    Unreadable(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sign {
    Negative,
    /// ลบ alone: a minus, or the verb "delete".
    Ambiguous,
}

struct Scanner<'a> {
    view: &'a View,
    t: &'a [char],
    dates: &'a Scan,
    /// Code points no number word may come from: percent words, dates, month names and
    /// what figures already took.
    taken: Vec<bool>,
    /// Code points inside an ordinary word that holds a power word or ล้าน (เรียบร้อย, พันตรี).
    ordinary: Vec<bool>,
    /// Where a number read so far, or a month name, ends: a dash right after one is no minus.
    ends: Vec<bool>,
    /// Unit words and symbols a figure took as its own after it, which the next figure may not
    /// take as its own before it.
    units_taken: Vec<bool>,
    items: Vec<Item>,
    /// How many items the figures pass pushed, in text order.
    figures_len: usize,
}

impl<'a> Scanner<'a> {
    fn new(view: &'a View, dates: &'a Scan) -> Scanner<'a> {
        let t: &[char] = &view.chars;
        let mut taken = vec![false; t.len()];
        for range in dates.hits.iter().map(|hit| &hit.range).chain(&dates.months) {
            taken[range.clone()]
                .iter_mut()
                .for_each(|slot| *slot = true);
        }
        let mut ordinary = vec![false; t.len()];
        let mut at = 0;
        while at < t.len() {
            if has_at(t, at, ROI_LA) {
                let end = at + char_len(ROI_LA);
                taken[at..end].iter_mut().for_each(|slot| *slot = true);
                at = end;
                continue;
            }
            if let Some(word) = ORDINARY_WORDS.iter().find(|word| has_at(t, at, word)) {
                ordinary[at..at + char_len(word)]
                    .iter_mut()
                    .for_each(|slot| *slot = true);
            }
            at += 1;
        }
        let mut ends = vec![false; t.len() + 1];
        for month in &dates.months {
            if let Some(end) = ends.get_mut(month.end) {
                *end = true;
            }
        }
        Scanner {
            view,
            t,
            dates,
            taken,
            ordinary,
            ends,
            units_taken: vec![false; t.len() + 1],
            items: Vec::new(),
            figures_len: 0,
        }
    }

    fn get(&self, at: usize) -> Option<char> {
        self.t.get(at).copied()
    }

    fn before(&self, at: usize) -> Option<char> {
        at.checked_sub(1).and_then(|prev| self.get(prev))
    }

    fn space(&self, at: usize) -> usize {
        if self.get(at) == Some(' ') {
            at + 1
        } else {
            at
        }
    }

    /// Lexes every figure, then reads each with its sign, units and scale.
    fn figures(&mut self) {
        let tokens = self.tokens();
        let mut i = 0;
        while i < tokens.len() {
            i += self.figure(&tokens, i);
        }
        self.figures_len = self.items.len();
    }

    fn tokens(&self) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut at = 0;
        while at < self.t.len() {
            if !self.get(at).is_some_and(is_numeral) {
                at += 1;
                continue;
            }
            let mut chain = self.chain_end(at);
            if self.get(chain).is_some_and(unknown_glyph) {
                chain += 1;
            }
            if let Some((start, end)) = self.alphanumeric_run(at, chain) {
                tokens.push(Token {
                    start,
                    end,
                    kind: TokenKind::Code,
                    lead: false,
                });
                at = end;
                continue;
            }
            let lead = self.view.broken_before(at);
            if lead || self.foreign(at, chain) {
                tokens.push(Token {
                    start: at,
                    end: chain,
                    kind: TokenKind::Code,
                    lead,
                });
                at = chain;
                continue;
            }
            // A chain of ASCII digit groups joined by `,` and `.` holds one figure or more.
            let amount_after_chain = self.amount_after(chain);
            let mut next = at;
            while next < chain {
                if !self.get(next).is_some_and(is_digit) {
                    next += 1;
                    continue;
                }
                let token = self.lex(next, chain, amount_after_chain);
                next = token.end;
                let mixed = matches!(token.kind, TokenKind::Figure { .. })
                    && self.mixed_scripts(&(token.start..token.end));
                tokens.push(if mixed {
                    Token {
                        kind: TokenKind::Code,
                        ..token
                    }
                } else {
                    token
                });
            }
            at = chain;
        }
        self.join_runs(tokens)
    }

    /// Where the run of numerals from `at` ends, over the separators a numeral follows: one `,`,
    /// `.`, separator the grammar does not read, or thin or figure space; or two of them when one
    /// is not `,` or `.` (a comma and U+FFFD).
    fn chain_end(&self, at: usize) -> usize {
        let separator = |at: usize| {
            self.get(at)
                .is_some_and(|c| c == ',' || c == '.' || foreign_separator(c))
                || self.view.group_space(at)
        };
        let unread =
            |at: usize| self.get(at).is_some_and(foreign_separator) || self.view.group_space(at);
        let mut end = numerals_end(self.t, at);
        loop {
            let next = if separator(end) && self.get(end + 1).is_some_and(is_numeral) {
                end + 1
            } else if separator(end)
                && separator(end + 1)
                && (unread(end) || unread(end + 1))
                && self.get(end + 2).is_some_and(is_numeral)
            {
                end + 2
            } else {
                return end;
            };
            end = numerals_end(self.t, next);
        }
    }

    /// The chain holds a numeral `normalize_th` does not fold, a separator the grammar does not
    /// read, or a control or filler step 1 deleted between two of its code points.
    fn foreign(&self, start: usize, end: usize) -> bool {
        (start..end).any(|at| {
            self.get(at).is_some_and(|c| {
                (is_numeral(c) && !is_digit(c)) || foreign_separator(c) || unknown_glyph(c)
            }) || self.view.group_space(at)
                || (at > start && self.view.broken_before(at))
        })
    }

    /// Digit groups with non-Thai letters between them and nothing else (`A1B2`, `12O5`, `2Q68`,
    /// `1920x1080`): where the run, with the letters glued before and after it, starts and ends.
    /// `None` for a single digit group, whose glued letters `figure` reads.
    fn alphanumeric_run(&self, at: usize, chain: usize) -> Option<(usize, usize)> {
        if self.in_date(&(at..chain)) {
            return None;
        }
        let mut end = chain;
        let mut groups = 1;
        loop {
            let letters = self.letters_after(end);
            if letters > end && self.get(letters).is_some_and(is_numeral) {
                end = self.chain_end(letters);
                groups += 1;
            } else {
                break;
            }
        }
        (groups > 1).then(|| (self.letters_before(at), self.letters_after(end)))
    }

    /// The figure mixes digit scripts other than Thai with ASCII, as `1٠250` (an Arabic-Indic
    /// zero that looks like a dot).
    fn mixed_scripts(&self, range: &Range<usize>) -> bool {
        let written = self.view.written(range);
        let mut scripts = written.chars().filter_map(script);
        let Some(first) = scripts.next() else {
            return false;
        };
        scripts.any(|other| {
            other != first
                && !matches!(
                    (first, other),
                    (Script::Ascii, Script::Thai) | (Script::Thai, Script::Ascii)
                )
        })
    }

    /// One figure from `at`, inside the chain of digit groups that ends at `chain`: groups joined
    /// by `,` thousands (a first group of 1–3 digits, later groups of exactly 3) and one `.`
    /// decimal part. A second `.`, or a comma group after the decimal part (`1.234,5`), makes the
    /// whole chain one code. Each group is read once, so a run such as `1,2,3,…` costs no more
    /// than its length.
    fn lex(&self, at: usize, chain: usize, amount_after_chain: bool) -> Token {
        let first = digits_end(self.t, at);
        let mut int_end = first;
        // The group after a separator at `sep`, when one follows it.
        let group_after = |sep: usize| {
            let next = sep + 1;
            self.get(next)
                .is_some_and(is_digit)
                .then(|| next..digits_end(self.t, next))
        };
        let mut grouped = false;
        if first - at <= 3 {
            while self.get(int_end) == Some(',') {
                match group_after(int_end) {
                    Some(group) if group.len() == 3 => {
                        int_end = group.end;
                        grouped = true;
                    }
                    _ => break,
                }
            }
        }
        let code = Token {
            start: at,
            end: chain,
            kind: TokenKind::Code,
            lead: false,
        };
        // A comma group that is not three digits, before a scale word or a unit, is no list of
        // figures but one the reader cannot place (`1,25 ล้านบาท`).
        if amount_after_chain && self.get(int_end) == Some(',') && group_after(int_end).is_some() {
            return code;
        }
        let int = self.digits_with_commas(at..int_end);
        let Some(frac_group) = (self.get(int_end) == Some('.'))
            .then(|| group_after(int_end))
            .flatten()
        else {
            return Token {
                start: at,
                end: int_end,
                kind: TokenKind::Figure {
                    int,
                    frac: Vec::new(),
                    grouped,
                },
                lead: false,
            };
        };
        if matches!(self.get(frac_group.end), Some('.' | ','))
            && group_after(frac_group.end).is_some()
        {
            return code;
        }
        let frac = self.digits_with_commas(frac_group.clone());
        Token {
            start: at,
            end: frac_group.end,
            kind: TokenKind::Figure { int, frac, grouped },
            lead: false,
        }
    }

    fn digits_with_commas(&self, range: Range<usize>) -> Vec<u8> {
        self.t[range]
            .iter()
            .filter_map(|c| c.to_digit(10))
            .filter_map(|d| u8::try_from(d).ok())
            .collect()
    }

    /// Three or more figures joined by `-` are one code (`000-000-0000`); `HH:MM` and
    /// `HH:MM:SS` are a time; a fraction before a scale word or a unit is one code, with a whole
    /// figure one space before it (`5 1/2%`).
    fn join_runs(&self, tokens: Vec<Token>) -> Vec<Token> {
        let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
        let mut i = 0;
        while i < tokens.len() {
            let mut last = i;
            while tokens.get(last + 1).is_some_and(|next| {
                next.start == tokens[last].end + 1 && self.get(tokens[last].end) == Some('-')
            }) {
                last += 1;
            }
            if last - i >= 2 {
                out.push(Token {
                    start: tokens[i].start,
                    end: tokens[last].end,
                    kind: TokenKind::Code,
                    lead: tokens[i].lead,
                });
                i = last + 1;
                continue;
            }
            if let Some((clock, used)) = self.clock(&tokens[i..]) {
                out.push(Token {
                    start: tokens[i].start,
                    end: tokens[i + used - 1].end,
                    kind: clock,
                    lead: false,
                });
                i += used;
                continue;
            }
            if self.fraction(&tokens[i..]) {
                let mut start = tokens[i].start;
                if let Some(whole) = out.last()
                    && self.figure_token(whole)
                    && whole.end + 1 == start
                    && self.get(whole.end) == Some(' ')
                {
                    start = whole.start;
                    out.pop();
                }
                out.push(Token {
                    start,
                    end: tokens[i + 1].end,
                    kind: TokenKind::Code,
                    lead: false,
                });
                i += 2;
                continue;
            }
            out.push(tokens[i].clone());
            i += 1;
        }
        out
    }

    /// The first two tokens are figures joined by `/` with no space, before a scale word or a
    /// unit (`3/4 ล้าน`, `1/2%`): a fraction, which the grammar does not read.
    fn fraction(&self, tokens: &[Token]) -> bool {
        let [numerator, denominator, ..] = tokens else {
            return false;
        };
        self.figure_token(numerator)
            && self.figure_token(denominator)
            && denominator.start == numerator.end + 1
            && self.get(numerator.end) == Some('/')
            && self.amount_after(denominator.end)
    }

    /// A figure, not a code or a time, outside every date.
    fn figure_token(&self, token: &Token) -> bool {
        matches!(token.kind, TokenKind::Figure { .. }) && !self.in_date(&(token.start..token.end))
    }

    /// `HH:MM`, or `HH:MM:SS` with an optional fraction of a second, from the first token, and
    /// how many tokens it takes. A field that follows another clock field (`05` in `9:05:15`)
    /// starts no clock, and four fields are no time.
    fn clock(&self, tokens: &[Token]) -> Option<(TokenKind, usize)> {
        let [hours, minutes, rest @ ..] = tokens else {
            return None;
        };
        let joined = |field: &Token, next: &Token| {
            self.get(field.end) == Some(':') && next.start == field.end + 1
        };
        let more = |field: &Token| {
            self.get(field.end) == Some(':') && self.get(field.end + 1).is_some_and(is_digit)
        };
        if self.before(hours.start) == Some(':')
            && hours
                .start
                .checked_sub(1)
                .and_then(|colon| self.before(colon))
                .is_some_and(is_digit)
        {
            return None;
        }
        if !joined(hours, minutes) {
            return None;
        }
        let hours_value = clock_value(whole(hours)?, 1..=2, 24)?;
        let minutes_value = clock_value(whole(minutes)?, 2..=2, 59)?;
        if !more(minutes) {
            return Some((
                TokenKind::Clock {
                    hours: hours_value,
                    minutes: minutes_value,
                    seconds: None,
                },
                2,
            ));
        }
        let seconds = rest.first().filter(|seconds| joined(minutes, seconds))?;
        let TokenKind::Figure {
            int,
            grouped: false,
            ..
        } = &seconds.kind
        else {
            return None;
        };
        let seconds_value = clock_value(int, 2..=2, 59)?;
        if more(seconds) {
            return None;
        }
        Some((
            TokenKind::Clock {
                hours: hours_value,
                minutes: minutes_value,
                seconds: Some(seconds_value),
            },
            3,
        ))
    }

    /// Reads `tokens[i]` with what surrounds it, and the amount in words after it. Returns how
    /// many tokens it read: more than one when the later parts of a compound amount follow.
    fn figure(&mut self, tokens: &[Token], i: usize) -> usize {
        let Some(token) = tokens.get(i) else {
            return 1;
        };
        let (int, frac) = match &token.kind {
            TokenKind::Figure { int, frac, .. } => (int.clone(), frac.clone()),
            TokenKind::Code => {
                self.code(token);
                return 1;
            }
            TokenKind::Clock {
                hours,
                minutes,
                seconds,
            } => {
                self.clock_found(token, *hours, *minutes, *seconds);
                return 1;
            }
        };
        let mut flags = Flags::default();
        // Before the digits: a symbol unit and a minus, in either order ($-5, -$5).
        let mut start = token.start;
        let mut unit = None;
        // A symbol unit written against the figure, inside any parentheses around it.
        let mut symbol = false;
        if let Some((prefix, width)) = self.prefix_symbol(start) {
            unit = Some(prefix);
            start -= width;
            symbol = true;
        }
        let minus = self.minus_before(start);
        if minus {
            start -= 1;
            if unit.is_none()
                && let Some((prefix, width)) = self.prefix_symbol(start)
            {
                unit = Some(prefix);
                start -= width;
                symbol = true;
            }
        }
        // After them: a symbol unit written with no space, as in 12.5%; a unit before wins.
        let mut end = token.end;
        let suffix = self.get(end).and_then(symbol_unit);
        if let Some(written) = &suffix {
            unit.get_or_insert(written.clone());
            end += 1;
            symbol = true;
        }
        let (bare_start, bare_end) = (start, end);
        // Accounting parentheses. One or two bare digits in them are a list marker, as (1),
        // unless a scale word or a unit follows: (5) ล้านบาท is -5,000,000.
        let parens = !minus && self.free_before(start) == Some('(') && self.get(end) == Some(')');
        let list_marker = parens
            && frac.is_empty()
            && int.len() <= 2
            && token.end - token.start == int.len()
            && (start, end) == (token.start, token.end)
            && !self.amount_after(end + 1);
        let accounting = parens && !list_marker;
        if accounting {
            flags |= Flags::PAREN_NEGATIVE;
            start -= 1;
            end += 1;
            // A symbol right before the parentheses is the figure's unit: $(1,250).
            if unit.is_none()
                && let Some((prefix, width)) = self.prefix_symbol(start)
            {
                unit = Some(prefix);
                start -= width;
            }
        }
        // A unit word before the figure or its parentheses (THB 1,250, ร้อยละ 12.5,
        // THB (1,250)), and a sign word before that or before the figure.
        let word_before = if unit.is_none() && !list_marker {
            self.unit_before(start)
        } else {
            None
        };
        let sign = if minus || parens {
            None
        } else {
            self.sign_word(
                word_before
                    .as_ref()
                    .map_or(start, |(_, word_start)| *word_start),
            )
        };
        let unit_word_before = word_before.is_some();
        let extent_start = word_before
            .as_ref()
            .map_or(start, |(_, word_start)| *word_start);
        if let Some((before, _)) = word_before {
            unit = Some(before);
        }
        let negative = minus || accounting || matches!(sign, Some((Sign::Negative, _)));
        let ambiguous = matches!(sign, Some((Sign::Ambiguous, _)));
        if let Some((_, sign_start)) = sign {
            start = sign_start;
        }
        let reads_on = suffix.is_none() && !list_marker;
        let decimals = i64::try_from(frac.len()).unwrap_or(i64::MAX);
        let Some((figure, _, _)) = digits(&int, &frac, false, 0) else {
            // Coordinator ruling 2: a figure no Decimal holds is a code over its whole span.
            let end = if reads_on {
                self.scale_end(end).unwrap_or(end)
            } else {
                end
            };
            self.push_fixed(start..end, NumberRole::Code, Flags::default());
            return 1;
        };
        // Scale words, the number words a Mixed number goes on with, and the later parts of a
        // compound amount.
        let mut groups = Vec::new();
        let mut consumed = 1;
        if reads_on {
            match self.group(end, figure, decimals) {
                GroupRead::None => {}
                GroupRead::Unreadable(code_end) => {
                    self.push_fixed(start..code_end, NumberRole::Code, Flags::default());
                    return 1;
                }
                GroupRead::Read(group) => {
                    groups.push(group);
                    match self.compound(tokens, i, &mut groups) {
                        Ok(more) => consumed += more,
                        Err((code_end, more)) => {
                            self.push_fixed(start..code_end, NumberRole::Code, Flags::default());
                            return 1 + more;
                        }
                    }
                }
            }
        }
        if let Some(last) = groups.last() {
            end = last.end;
        }
        // A unit word after them. An English scale may still follow it (1 THB million); the
        // span then runs over the unit word to the end of the scale.
        let mut tail = end;
        // A second, different currency after one before the figure (`USD 100 บาท`).
        let mut other_unit = None;
        if reads_on
            && let Some((after, word_end)) = self.unit_after(self.space(end), unit.is_some())
        {
            self.take_unit(self.space(end)..word_end);
            match &unit {
                Some(before) if *before != after => other_unit = Some(after),
                Some(_) => {}
                None => unit = Some(after),
            }
            tail = word_end;
            if groups.is_empty()
                && let Some((scale_end, scale)) = self.english_scale(self.space(word_end))
            {
                let Some(value) = power_of_ten(scale).and_then(|power| figure.checked_mul(power))
                else {
                    self.push_fixed(start..scale_end, NumberRole::Code, Flags::default());
                    return consumed;
                };
                groups.push(Group {
                    end: scale_end,
                    value,
                    place: i64::from(scale) - decimals,
                    scale,
                    words: false,
                    terms: Vec::new(),
                    digit_last: false,
                });
                end = scale_end;
                tail = scale_end;
            }
        }
        // Letters glued to the figure, unless a scale or unit word starts right there or they
        // follow a symbol unit (12.5%YoY).
        let left = if unit_word_before || sign.is_some() || accounting {
            bare_start
        } else {
            self.letters_before(bare_start)
        };
        let right = if suffix.is_none() && end == bare_end && tail == bare_end {
            self.letters_after(bare_end)
        } else {
            bare_end
        };
        // A Latin abbreviation of a scale one space after the figure: `$5 M`, `5 mln`.
        let loose = (suffix.is_none() && groups.is_empty() && tail == bare_end)
            .then(|| self.loose_scale(self.space(bare_end)))
            .flatten();
        let lowest = groups.last().map_or(-decimals, |last| last.place);
        let built = if let [group] = groups.as_slice()
            && !group.words
        {
            digits(&int, &frac, negative, group.scale).map(|number| (number, Form::Digits))
        } else if let Some(first) = groups.first() {
            groups
                .iter()
                .try_fold(Decimal::ZERO, |sum, group| sum.checked_add(group.value))
                .and_then(|total| mixed(total, negative, first.scale, lowest))
                .map(|number| (number, Form::Mixed))
        } else {
            digits(&int, &frac, negative, 0).map(|number| (number, Form::Digits))
        };
        let Some(((value, scale, decimals), form)) = built else {
            self.push_fixed(start..end, NumberRole::Code, Flags::default());
            self.amount_in_words(tail, None);
            return consumed;
        };
        let role = if self.in_date(&(token.start..token.end)) {
            NumberRole::DatePart
        } else if let Some(stray) = self.stray(bare_start) {
            start = start.min(stray);
            NumberRole::Code
        } else if left < bare_start || right > bare_end {
            end = end.max(right);
            // An ordinal marker glued on (`item5`) is no code: the letters are the marker.
            if self.ordinal_marker(start) {
                NumberRole::Ordinal
            } else {
                start = start.min(left);
                NumberRole::Code
            }
        } else if let Some(loose_end) = loose {
            end = loose_end;
            NumberRole::Code
        } else if unit.is_some() || !groups.is_empty() {
            NumberRole::Quantity
        } else if self.ordinal_marker(start) {
            NumberRole::Ordinal
        } else if self.time_after(&int, &frac, token.end) {
            NumberRole::Time
        } else if self.slash_touches(bare_start, bare_end) {
            NumberRole::Code
        } else {
            NumberRole::Quantity
        };
        if role == NumberRole::Code {
            self.push_fixed(start..end, NumberRole::Code, Flags::default());
            self.amount_in_words(tail.max(end), None);
            return consumed;
        }
        if form == Form::Mixed {
            flags |= Flags::WORDS;
        }
        if role == NumberRole::Time {
            flags |= Flags::TIME;
        }
        let number = Number {
            value,
            scale: Scale(scale),
            unit,
            form,
            decimals,
            role,
        };
        // What the reader does not know exactly reads at 0.6 at most, the other reading beside it.
        let mut conf: f32 = 1.0;
        let mut alts = Vec::new();
        if role == NumberRole::Quantity {
            let other = CONF_OTHER;
            if ambiguous {
                conf = 0.5;
                alts.push((opposite(&number), 0.5));
            }
            // A percent or a currency in parentheses may restate a rise in prose; only a figure
            // that is the whole text, as in a table cell, is an accounting negative for sure.
            if accounting && symbol && (start, end) != (0, self.t.len()) {
                conf = conf.min(CONF_CAP);
                alts.push((opposite(&number), other));
            }
            if let Some(other_unit) = other_unit {
                conf = conf.min(CONF_CAP);
                let mut written_after = number.clone();
                written_after.unit = Some(other_unit);
                alts.push((written_after, other));
            }
            if let Some(last) = groups.last()
                && !has_before(self.t, last.end, ET)
                && let Some(total) = number.checked_effective()
                && let Some((spoken, place)) = spoken(&last.terms, total)
                && let Some((value, scale, decimals)) =
                    mixed(spoken, false, u32::from(number.scale.0), place)
            {
                conf = conf.min(CONF_CAP);
                alts.push((
                    Number {
                        value,
                        scale: Scale(scale),
                        decimals,
                        ..number.clone()
                    },
                    other,
                ));
            }
        }
        let restated = number.checked_effective();
        self.push_item(
            start..end,
            extent_start.min(start)..tail.max(end),
            number,
            flags,
            conf,
            alts,
        );
        self.amount_in_words(tail.max(end), restated);
        consumed
    }

    /// A code the lexer made, with what a figure takes around it: a symbol unit, a minus,
    /// accounting parentheses, a sign word and its scale words are part of it (`−1'250 ล้าน`,
    /// `(1 250) ล้าน`), so a quote that drops them cuts it.
    fn code(&mut self, token: &Token) {
        let role = self.role_without_value(token);
        let (mut start, mut end) = (token.start, token.end);
        // Letters the lexer put at its head that a figure before took (as its scale) stay that
        // figure's.
        while start < end && self.taken[start] {
            start += 1;
        }
        if role == NumberRole::Code {
            if let Some((_, width)) = self.prefix_symbol(start) {
                start -= width;
            }
            let minus = self.minus_before(start);
            if minus {
                start -= 1;
                if let Some((_, width)) = self.prefix_symbol(start) {
                    start -= width;
                }
                // Letters before the dash are the code's too: `SKU-12AB34`, as `TH-01`.
                start = self.letters_before(start);
            }
            if self.get(end).and_then(symbol_unit).is_some() {
                end += 1;
            }
            if self.free_before(start) == Some('(') && self.get(end) == Some(')') {
                start -= 1;
                end += 1;
            } else if !minus && let Some((_, sign_start)) = self.sign_word(start) {
                start = sign_start;
            }
            end = self.scale_end(end).unwrap_or(end);
        }
        let lead = token.lead && start == token.start;
        if start < end {
            self.push_code(start..end, role, Flags::default(), lead);
        }
    }

    /// The later parts of a compound amount after `groups` (`3 ล้าน 9 แสน`, `1ล้าน2แสน`,
    /// `3 ล้าน เก้าแสน`): each a figure with a scale word of its own (the token after
    /// `tokens[i]` and the parts read so far), or Thai number words that start with a digit word
    /// and hold a power word, with one space at most before it. Returns how many more tokens it
    /// read. `Err` is one code over the whole run, to its end and with the tokens it took: a part
    /// that does not fit below the one before or that the grammar does not read as their sum,
    /// ครึ่ง beside a scale word, or a digit word that a Thai letter continues.
    fn compound(
        &self,
        tokens: &[Token],
        i: usize,
        groups: &mut Vec<Group>,
    ) -> Result<usize, (usize, usize)> {
        let mut more = 0;
        while let Some(last) = groups.last() {
            let after = last.end;
            let thai = !last.terms.is_empty();
            if thai {
                let half = [after, self.space(after)]
                    .into_iter()
                    .find(|at| has_at(self.t, *at, KHRUENG));
                if let Some(half) = half {
                    return Err((half + char_len(KHRUENG), more));
                }
            }
            if last.digit_last && self.glued_word(after) {
                return Err((after, more));
            }
            let at = self.space(after);
            let (next, used) = match tokens.get(i + 1 + more).filter(|token| token.start == at) {
                Some(token) => {
                    let TokenKind::Figure { int, frac, grouped } = &token.kind else {
                        return Ok(more);
                    };
                    if self.in_date(&(token.start..token.end))
                        || self.get(token.end).and_then(symbol_unit).is_some()
                    {
                        return Ok(more);
                    }
                    let Some((figure, _, _)) = digits(int, frac, false, 0) else {
                        return Ok(more);
                    };
                    let decimals = i64::try_from(frac.len()).unwrap_or(i64::MAX);
                    match self.group(token.end, figure, decimals) {
                        // A figure with no scale word of its own joins only when it is written
                        // as an amount, with thousands separators or a currency after it
                        // (`1 ล้าน 250,000 บาท`); a small bare one counts something else.
                        GroupRead::None if *grouped || self.currency_after(token.end) => (
                            Group {
                                end: token.end,
                                value: figure,
                                place: -decimals,
                                scale: 0,
                                words: true,
                                terms: if thai {
                                    vec![Term::Figure(figure, decimals)]
                                } else {
                                    Vec::new()
                                },
                                digit_last: false,
                            },
                            1,
                        ),
                        GroupRead::None => return Ok(more),
                        GroupRead::Unreadable(end) => return Err((end, more + 1)),
                        GroupRead::Read(group) => (group, 1),
                    }
                }
                None if thai => match self.word_group(at) {
                    GroupRead::None => return Ok(more),
                    GroupRead::Unreadable(end) => return Err((end, more)),
                    GroupRead::Read(group) => (group, 0),
                },
                None => return Ok(more),
            };
            // A Thai part and an English one do not join.
            if next.terms.is_empty() == thai {
                return Ok(more);
            }
            if !fits(groups, &next) {
                return Err((next.end, more + used));
            }
            groups.push(next);
            more += used;
        }
        Ok(more)
    }

    /// The scale part after a figure that ends at `end`: Thai scale and number words, or an
    /// English scale.
    fn group(&self, end: usize, figure: Decimal, decimals: i64) -> GroupRead {
        let at = self.space(end);
        match self.thai_group(at, figure, decimals) {
            GroupRead::None => {}
            read => return read,
        }
        let Some((scale_end, exponent)) = self.english_scale(at) else {
            return GroupRead::None;
        };
        match power_of_ten(exponent).and_then(|power| figure.checked_mul(power)) {
            Some(value) => GroupRead::Read(Group {
                end: scale_end,
                value,
                place: i64::from(exponent) - decimals,
                scale: exponent,
                words: false,
                terms: Vec::new(),
                digit_last: false,
            }),
            None => GroupRead::Unreadable(scale_end),
        }
    }

    /// Thai words after a figure: its own scale (one power word, then ล้าน words; an ordinary word
    /// such as พันตรี or ล้านนา ends it), and the number words a Mixed number goes on with, read by
    /// the grammar with the figure as the first digit. Words after the scale that start with a
    /// power word, follow a figure with decimals, or read otherwise than the grammar says make the
    /// run unreadable.
    fn thai_group(&self, at: usize, figure: Decimal, decimals: i64) -> GroupRead {
        let mut words = Vec::new();
        let mut own = 0;
        let mut next = at;
        while let Some((word, end)) = word_at(self.t, next) {
            if self.taken[next..end].iter().any(|slot| *slot) {
                break;
            }
            let scale_word = match word {
                Word::Power(_) => words.is_empty(),
                Word::Lan => true,
                _ => false,
            };
            if own == words.len() && scale_word {
                if self.ordinary[next] {
                    break;
                }
                own += 1;
            }
            words.push(word);
            next = end;
        }
        if own == 0 {
            return GroupRead::None;
        }
        let rest = &words[own..];
        let rest_unreadable = matches!(rest.first(), Some(Word::Power(_) | Word::Lan))
            || (decimals > 0 && !rest.is_empty());
        let mut terms = vec![Term::Figure(figure, decimals)];
        terms.extend(words.iter().map(|word| Term::Word(*word)));
        if rest_unreadable || regrouped(&terms) {
            return GroupRead::Unreadable(next);
        }
        let Ok((value, place)) = numeral(&terms) else {
            return GroupRead::Unreadable(next);
        };
        let scale = words[..own]
            .iter()
            .map(|word| match word {
                Word::Power(power @ 3..=5) => u32::from(*power),
                Word::Lan => 6,
                _ => 0,
            })
            .sum();
        GroupRead::Read(Group {
            end: next,
            value,
            place,
            scale,
            words: !rest.is_empty() || matches!(words.first(), Some(Word::Power(1 | 2))),
            terms,
            digit_last: matches!(words.last(), Some(Word::Digit(_))),
        })
    }

    /// Thai number words at `at` that start with a digit word and hold a power word or ล้าน: a
    /// later part of a compound amount (`3 ล้าน เก้าแสน`).
    fn word_group(&self, at: usize) -> GroupRead {
        let (words, end) = self.word_run(at);
        if self.ordinary.get(at) != Some(&false)
            || !matches!(words.first(), Some(Word::Digit(_)))
            || !words
                .iter()
                .any(|word| matches!(word, Word::Power(_) | Word::Lan))
        {
            return GroupRead::None;
        }
        let terms = terms(&words);
        match numeral(&terms) {
            Ok((value, place)) if !regrouped(&terms) => GroupRead::Read(Group {
                end,
                value,
                place,
                scale: 0,
                words: true,
                terms,
                digit_last: matches!(words.last(), Some(Word::Digit(_))),
            }),
            _ => GroupRead::Unreadable(end),
        }
    }

    /// Where the scale or number words after a figure that ends at `end` end, read or not.
    fn scale_end(&self, end: usize) -> Option<usize> {
        let at = self.space(end);
        let scale_first =
            word_at(self.t, at).is_some_and(|(word, _)| matches!(word, Word::Power(_) | Word::Lan));
        if scale_first && self.ordinary.get(at) == Some(&false) {
            let (_, words_end) = self.word_run(at);
            return (words_end > at).then_some(words_end);
        }
        self.english_scale(at).map(|(scale_end, _)| scale_end)
    }

    /// A symbol unit right before `start` that no earlier figure took as its own suffix, and
    /// how many code points it takes: 3 for US$.
    fn prefix_symbol(&self, start: usize) -> Option<(Unit, usize)> {
        let at = start.checked_sub(1)?;
        if self.taken[at] {
            return None;
        }
        let unit = self.symbol_before_figure(at)?;
        let us = self.get(at) == Some('$')
            && has_before(self.t, at, "us")
            && at
                .checked_sub(2)
                .is_some_and(|us| !self.taken[us] && !self.taken[us + 1]);
        Some((unit, if us { 3 } else { 1 }))
    }

    /// The unit of a symbol at `at` written before a figure: ฿, or $ unless letters name
    /// another dollar (S$, HK$; US$ is USD).
    fn symbol_before_figure(&self, at: usize) -> Option<Unit> {
        let unit = self.get(at).and_then(symbol_unit)?;
        if unit == Unit::Percent {
            return None;
        }
        let other_dollar = self.get(at) == Some('$')
            && self.before(at).is_some_and(is_latin_letter)
            && !has_before(self.t, at, "us");
        (!other_dollar).then_some(unit)
    }

    /// A character right before `start` that no earlier figure took.
    fn free_before(&self, start: usize) -> Option<char> {
        let at = start.checked_sub(1)?;
        (!self.taken[at]).then(|| self.get(at)).flatten()
    }

    /// A free `-` right before `start` is a minus, unless a figure stands right before the
    /// dash: its digit, or the end of a number read just before (`5%-3%`, `1 ล้าน-2 ล้าน`).
    fn minus_before(&self, start: usize) -> bool {
        let Some(dash) = start.checked_sub(1) else {
            return false;
        };
        self.free_before(start) == Some('-')
            && !self.before(dash).is_some_and(is_digit)
            && self.ends.get(dash) != Some(&true)
    }

    /// A sign word right before `head`, with one space at most between: ติดลบ or `minus` makes
    /// the number negative; ลบ alone also means "delete", so it makes the number ambiguous, and so
    /// does `minus` right after another number (a subtraction). A ลบ that ends a Thai word
    /// (สลบ, หลบ) is no sign. Returns where the word starts.
    fn sign_word(&self, head: usize) -> Option<(Sign, usize)> {
        let end = if self.before(head) == Some(' ') {
            head - 1
        } else {
            head
        };
        let word = |text: &str| {
            let start = end.checked_sub(char_len(text))?;
            (has_before(self.t, end, text) && !self.taken[start..end].iter().any(|slot| *slot))
                .then_some(start)
        };
        if let Some(start) = word(TID_LOP) {
            return Some((Sign::Negative, start));
        }
        if let Some(start) =
            word("minus").filter(|start| !self.before(*start).is_some_and(is_latin_letter))
        {
            let before = if self.before(start) == Some(' ') {
                start - 1
            } else {
                start
            };
            let sign = if self.ends.get(before) == Some(&true) {
                Sign::Ambiguous
            } else {
                Sign::Negative
            };
            return Some((sign, start));
        }
        word(LOP)
            .filter(|start| !self.before(*start).is_some_and(is_thai))
            .map(|start| (Sign::Ambiguous, start))
    }

    /// A scale word or a unit at `at` (one space allowed) that is not the next figure's own.
    fn amount_after(&self, at: usize) -> bool {
        let at = self.space(at);
        let thai_scale = word_at(self.t, at).is_some_and(|(word, end)| {
            matches!(word, Word::Power(_) | Word::Lan)
                && self.ordinary.get(at) == Some(&false)
                && !self.taken[at..end].iter().any(|slot| *slot)
        });
        thai_scale || self.english_scale(at).is_some() || self.unit_after(at, true).is_some()
    }

    fn push(&mut self, range: Range<usize>, number: Number, flags: Flags) {
        self.push_item(range.clone(), range, number, flags, 1.0, Vec::new());
    }

    fn push_item(
        &mut self,
        range: Range<usize>,
        extent: Range<usize>,
        number: Number,
        flags: Flags,
        conf: f32,
        alts: Vec<(Number, f32)>,
    ) {
        self.take(&range);
        if let Some(end) = self.ends.get_mut(range.end) {
            *end = true;
        }
        self.items.push(Item {
            range,
            extent,
            number,
            flags,
            conf,
            alts,
            lead: false,
            dead: false,
        });
    }

    /// A number whose value is not read: a code or a date part, value 0.
    fn push_fixed(&mut self, range: Range<usize>, role: NumberRole, flags: Flags) {
        self.push_code(range, role, flags, false);
    }

    /// A code or a date part, value 0; `lead` when its span starts at the code points step 1
    /// deleted before it.
    fn push_code(&mut self, range: Range<usize>, role: NumberRole, flags: Flags, lead: bool) {
        let number = Number {
            value: Decimal::ZERO,
            scale: Scale(0),
            unit: None,
            form: Form::Digits,
            decimals: 0,
            role,
        };
        self.push(range, number, flags);
        if let Some(item) = self.items.last_mut() {
            item.lead = lead;
        }
    }

    /// Marks a unit word or symbol after a figure as that figure's.
    fn take_unit(&mut self, range: Range<usize>) {
        if let Some(slots) = self.units_taken.get_mut(range) {
            slots.iter_mut().for_each(|slot| *slot = true);
        }
    }

    fn take(&mut self, range: &Range<usize>) {
        if let Some(slots) = self.taken.get_mut(range.clone()) {
            slots.iter_mut().for_each(|slot| *slot = true);
        }
    }

    fn role_without_value(&self, token: &Token) -> NumberRole {
        let range = token.start..token.end;
        if self.in_date(&range) {
            NumberRole::DatePart
        } else if self.ordinal_marker(token.start) {
            NumberRole::Ordinal
        } else {
            NumberRole::Code
        }
    }

    /// A clock time: `Time`, or `DatePart` inside a date. Its value is `HH.MM`, or `HH.MMSS`
    /// with seconds.
    fn clock_found(&mut self, token: &Token, hours: u32, minutes: u32, seconds: Option<u32>) {
        let range = token.start..token.end;
        let role = if self.in_date(&range) {
            NumberRole::DatePart
        } else {
            NumberRole::Time
        };
        let flags = if role == NumberRole::Time {
            Flags::TIME
        } else {
            Flags::default()
        };
        let (mantissa, decimals) = match seconds {
            Some(seconds) => (hours * 10_000 + minutes * 100 + seconds, 4),
            None => (hours * 100 + minutes, 2),
        };
        match Decimal::try_new(i64::from(mantissa), decimals) {
            Ok(value) => {
                let number = Number {
                    value,
                    scale: Scale(0),
                    unit: None,
                    form: Form::Digits,
                    decimals,
                    role,
                };
                self.push(range, number, flags);
            }
            Err(_) => self.push_fixed(range, NumberRole::Code, Flags::default()),
        }
    }

    /// The digits lie inside a date `find_dates` returns, or overlap a day number written
    /// before a month name with no year. Both lists are in text order and never overlap, so a
    /// binary search finds the one candidate.
    fn in_date(&self, range: &Range<usize>) -> bool {
        let hits = &self.dates.hits;
        let last_starting = hits.partition_point(|hit| hit.range.start <= range.start);
        crate::found::work(1);
        let inside = last_starting
            .checked_sub(1)
            .and_then(|i| hits.get(i))
            .is_some_and(|hit| range.end <= hit.range.end);
        let parts = &self.dates.day_parts;
        let first_ending = parts.partition_point(|part| part.end <= range.start);
        inside
            || parts
                .get(first_ending)
                .is_some_and(|part| part.start < range.end)
    }

    fn english_scale(&self, at: usize) -> Option<(usize, u32)> {
        let mut exponent = 0;
        let mut end = None;
        let mut next = at;
        while let Some((word, power)) = ENGLISH_SCALES.iter().find(|(word, _)| {
            has_at(self.t, next, word)
                && !self.before(next).is_some_and(is_latin_letter)
                && !self.get(next + char_len(word)).is_some_and(is_latin_letter)
        }) {
            exponent += power;
            let word_end = next + char_len(word);
            end = Some(word_end);
            next = self.space(word_end);
        }
        end.map(|end| (end, exponent))
    }

    /// A unit word or symbol at `at`, as written, and where it ends.
    fn unit_word(&self, at: usize) -> Option<(Unit, usize)> {
        UNITS_AFTER
            .iter()
            .find_map(|(word, unit)| {
                let end = at + char_len(word);
                let latin = word.chars().next().is_some_and(is_latin_letter);
                (has_at(self.t, at, word) && !(latin && self.get(end).is_some_and(is_latin_letter)))
                    .then(|| (unit.clone(), end))
            })
            .or_else(|| {
                let symbol = symbol_unit(self.get(at)?)?;
                Some((symbol, at + 1))
            })
    }

    /// A unit word or symbol at `at`, after a figure that already `has_unit` or not. A currency
    /// word or symbol between two figures is the first figure's (`100 USD 3,500 บาท`), unless the
    /// second has no unit of its own and the first already has one (`USD 100 THB 3,500`), or it
    /// is a symbol written against the second (`5 $10`): then it is the second figure's.
    fn unit_after(&self, at: usize, has_unit: bool) -> Option<(Unit, usize)> {
        let (unit, end) = self.unit_word(at)?;
        let symbol = matches!(self.get(at), Some('$' | '\u{0E3F}'));
        let currency = symbol || has_at(self.t, at, "thb") || has_at(self.t, at, "usd");
        let next = self.space(end);
        if !currency || !self.get(next).is_some_and(is_digit) {
            return Some((unit, end));
        }
        if symbol && next == end && self.before(at) == Some(' ') {
            return None;
        }
        (!has_unit || self.own_unit_after(next)).then_some((unit, end))
    }

    /// The figure at `at` has a unit of its own after it and its scale words: `3,500 บาท`,
    /// `12.5%`, `35 ล้าน USD`.
    fn own_unit_after(&self, at: usize) -> bool {
        let end = self.chain_end(at);
        if self.get(end).and_then(symbol_unit).is_some() {
            return true;
        }
        let end = self.scale_end(end).unwrap_or(end);
        self.unit_word(self.space(end)).is_some()
    }

    /// A currency word or symbol after the figure that ends at `end`, one space allowed.
    fn currency_after(&self, end: usize) -> bool {
        matches!(
            self.unit_word(self.space(end)),
            Some((Unit::Currency(_), _))
        )
    }

    /// A Latin abbreviation of a scale at `at`, one space after a figure, with no letter after
    /// it: `mil`, `mln`, `bln`, `bil`, `mm` or `MB`, or `M`, `B`, `K` or `T` in capitals. Where
    /// it ends.
    fn loose_scale(&self, at: usize) -> Option<usize> {
        if at == 0 || self.get(at - 1) != Some(' ') {
            return None;
        }
        let word_end = |end: usize| !self.get(end).is_some_and(char::is_alphanumeric);
        if let Some(word) = LOOSE_SCALES
            .iter()
            .find(|word| has_at(self.t, at, word) && word_end(at + char_len(word)))
        {
            return Some(at + char_len(word));
        }
        let letter = self.get(at)?;
        (matches!(letter, 'm' | 'b' | 'k' | 't')
            && word_end(at + 1)
            && self.view.written(&(at..at + 1)) == letter.to_ascii_uppercase().to_string())
        .then_some(at + 1)
    }

    /// A unit word right before the figure, with one space between: `THB 1,250`, `ร้อยละ 12.5`;
    /// or a symbol, with one space, that no earlier figure took as its own. A word the figure
    /// before took as its unit is not this one's.
    fn unit_before(&self, start: usize) -> Option<(Unit, usize)> {
        let end = if self.before(start) == Some(' ') {
            start - 1
        } else {
            start
        };
        UNITS_BEFORE
            .iter()
            .find_map(|(word, unit)| {
                let word_start = end.checked_sub(char_len(word))?;
                let latin = word.chars().next().is_some_and(is_latin_letter);
                (has_before(self.t, end, word)
                    && !self.units_taken[word_start]
                    && !(latin && self.before(word_start).is_some_and(is_latin_letter)))
                .then(|| (unit.clone(), word_start))
            })
            .or_else(|| {
                if end == start {
                    return None;
                }
                let at = end.checked_sub(1)?;
                if self.taken[at] || self.units_taken[at] {
                    return None;
                }
                let symbol = self.symbol_before_figure(at)?;
                Some((symbol, at))
            })
    }

    /// A mark right before the figure that belongs to nothing: a dot (`.5`), a comma at the start
    /// of the text, after a glyph the text layer could not name or after one lone letter (`x,250`,
    /// `l,250`, never after a word or a Thai letter: `revenue,1250` and `รายได้,1250` are CSV
    /// fields), or such a glyph itself (U+FFFD before `250`). What was written is not this
    /// figure, so it is a code from that mark and the letter glued before it. Returns where the
    /// code starts.
    fn stray(&self, head: usize) -> Option<usize> {
        let mark = head.checked_sub(1)?;
        if self.taken[mark] {
            return None;
        }
        let before = self.before(mark);
        let lone_letter = before.is_some_and(code_letter)
            && !mark
                .checked_sub(1)
                .and_then(|letter| self.before(letter))
                .is_some_and(char::is_alphabetic);
        let stray = match self.get(mark) {
            Some('.') => !self.ordinal_marker(head) && !before.is_some_and(is_digit),
            Some(',') => before.is_none_or(unknown_glyph) || lone_letter,
            Some(c) => unknown_glyph(c),
            None => false,
        };
        let from = if before.is_some_and(unknown_glyph) && !self.taken[mark - 1] {
            mark - 1
        } else {
            mark
        };
        stray.then(|| self.letters_before(from))
    }

    /// Where the run of non-Thai letters that no figure took starts, going back from `at`.
    fn letters_before(&self, at: usize) -> usize {
        let mut start = at;
        while let Some(prev) = start.checked_sub(1)
            && !self.taken[prev]
            && self.get(prev).is_some_and(code_letter)
        {
            start = prev;
        }
        crate::found::work(at - start + 1);
        start
    }

    /// Where the run of non-Thai letters that no figure took ends, going on from `at`.
    fn letters_after(&self, at: usize) -> usize {
        let mut end = at;
        while self.taken.get(end) == Some(&false) && self.get(end).is_some_and(code_letter) {
            end += 1;
        }
        crate::found::work(end - at + 1);
        end
    }

    fn ordinal_marker(&self, head: usize) -> bool {
        let end = if self.before(head) == Some(' ') {
            head - 1
        } else {
            head
        };
        ORDINAL_MARKERS.iter().any(|marker| {
            let latin = marker.chars().next().is_some_and(is_latin_letter);
            has_before(self.t, end, marker)
                && !(latin
                    && end
                        .checked_sub(char_len(marker))
                        .and_then(|start| self.before(start))
                        .is_some_and(is_latin_letter))
        })
    }

    /// `HH.MM น.`: a figure of one or two digits and two decimals read as a time, with the Thai
    /// hour (น. or นาฬิกา) after it or after the end of a range (`09.00 - 12.00 น.`,
    /// `09.00 ถึง 12.00 นาฬิกา`).
    fn time_after(&self, int: &[u8], frac: &[u8], end: usize) -> bool {
        if frac.len() != 2
            || clock_value(int, 1..=2, 24).is_none()
            || clock_value(frac, 2..=2, 59).is_none()
        {
            return false;
        }
        let next = self.space(end);
        if self.hour_word(next) {
            return true;
        }
        let range_end = if self.get(next) == Some('-') {
            Some(next + 1)
        } else if has_at(self.t, next, THUENG) {
            Some(next + char_len(THUENG))
        } else {
            None
        };
        range_end
            .and_then(|at| self.hhmm_end(self.space(at)))
            .is_some_and(|after| self.hour_word(self.space(after)))
    }

    /// น. or นาฬิกา, the Thai hour, at `at`.
    fn hour_word(&self, at: usize) -> bool {
        has_at(self.t, at, NA_DOT) || has_at(self.t, at, NALIKA)
    }

    /// An `HH.MM` time at `at`: where it ends.
    fn hhmm_end(&self, at: usize) -> Option<usize> {
        let hours_end = digits_end(self.t, at);
        if self.get(hours_end) != Some('.') {
            return None;
        }
        let minutes_end = digits_end(self.t, hours_end + 1);
        clock_value(&self.digits_with_commas(at..hours_end), 1..=2, 24)?;
        clock_value(
            &self.digits_with_commas(hours_end + 1..minutes_end),
            2..=2,
            59,
        )?;
        Some(minutes_end)
    }

    /// A slash touches the figure, as in `0401/123`.
    fn slash_touches(&self, head: usize, end: usize) -> bool {
        self.before(head) == Some('/') || self.get(end) == Some('/')
    }

    /// A Thai letter at `at` that continues the word before it: no unit word starts there.
    fn glued_word(&self, at: usize) -> bool {
        self.get(at).is_some_and(is_thai)
            && !UNITS_AFTER.iter().any(|(word, _)| {
                word.chars().next().is_some_and(is_thai) && has_at(self.t, at, word)
            })
    }

    /// Number words in parentheses right after a figure, restating it (`restates` is the figure's
    /// value): one space may stand inside each parenthesis and between groups of words.
    fn amount_in_words(&mut self, tail: usize, restates: Option<Decimal>) {
        let open = self.space(tail);
        if self.get(open) != Some('(') || self.taken.get(open) != Some(&false) {
            return;
        }
        let start = self.space(open + 1);
        let (words, end) = self.word_run(start);
        if words.is_empty() {
            return;
        }
        let (words, end, fits) = self.spaced_parts(words, end);
        let after = self.space(end);
        let (unit, close) = match self.unit_after(after, false) {
            Some((unit, word_end)) => (Some(unit), self.space(word_end)),
            None => (None, after),
        };
        if self.get(close) != Some(')') {
            return;
        }
        if !fits {
            self.push_fixed(start..end, NumberRole::Code, Flags::WORDS);
            return;
        }
        let restated = Restated {
            unit,
            value: restates,
        };
        self.word_number(start, &words, end, Some(restated));
    }

    /// The longest run of number words from `at` that no figure, date or percent word took.
    fn word_run(&self, at: usize) -> (Vec<Word>, usize) {
        let mut words = Vec::new();
        let mut next = at;
        while self.taken.get(next) == Some(&false) {
            let Some((word, end)) = word_at(self.t, next) else {
                break;
            };
            if self.taken[next..end].iter().any(|slot| *slot) {
                break;
            }
            words.push(word);
            next = end;
        }
        (words, next)
    }

    /// Number words `words` ending at `end`, continued across one space (a line break included)
    /// by runs that start with a digit word and hold a power word or ล้าน (`สามล้าน ห้าแสน`): all
    /// the words, where the last run ends, and whether each run writes only places below the one
    /// before it and the grammar reads them as their sum. When one does not, the whole span is
    /// one code.
    fn spaced_parts(&self, mut words: Vec<Word>, mut end: usize) -> (Vec<Word>, usize, bool) {
        if !words
            .iter()
            .any(|word| matches!(word, Word::Power(_) | Word::Lan))
        {
            return (words, end, true);
        }
        let Ok((value, place)) = numeral(&terms(&words)) else {
            return (words, end, true);
        };
        let mut groups = vec![Group {
            end,
            value,
            place,
            scale: 0,
            words: true,
            terms: terms(&words),
            digit_last: false,
        }];
        while self.get(end) == Some(' ') {
            match self.word_group(end + 1) {
                GroupRead::Read(next) => {
                    end = next.end;
                    words.extend(next.terms.iter().filter_map(|term| match term {
                        Term::Word(word) => Some(*word),
                        Term::Figure(..) => None,
                    }));
                    if !fits(&groups, &next) {
                        return (words, end, false);
                    }
                    groups.push(next);
                }
                GroupRead::Unreadable(next_end) => return (words, next_end, false),
                GroupRead::None => break,
            }
        }
        (words, end, true)
    }

    /// Number words with a power word or ล้าน over `at..end`, then one space and a figure with a
    /// Thai scale word of its own (`หนึ่งล้าน 2 แสนบาท`): one number when the figure writes only
    /// places below the words and the grammar reads them as their sum, else one code over both.
    /// The figure, read already, gives way. False when no such figure follows.
    fn join_figure(&mut self, at: usize, words: &[Word], end: usize) -> bool {
        if self.get(end) != Some(' ')
            || !words
                .iter()
                .any(|word| matches!(word, Word::Power(_) | Word::Lan))
        {
            return false;
        }
        let index = self.items[..self.figures_len].partition_point(|item| item.range.start <= end);
        let Some(figure) = self.items[..self.figures_len]
            .get(index)
            .filter(|item| item.range.start == end + 1 && item.number.role == NumberRole::Quantity)
        else {
            return false;
        };
        let thai_scale = figure
            .range
            .clone()
            .any(|at| matches!(word_at(self.t, at), Some((Word::Power(_) | Word::Lan, _))));
        let (Some(figure_value), Ok((value, place))) =
            (figure.number.checked_effective(), numeral(&terms(words)))
        else {
            return false;
        };
        if !thai_scale || figure_value.is_sign_negative() {
            return false;
        }
        let figure_place = i64::from(figure.number.scale.0) - i64::from(figure.number.decimals);
        let first = Group {
            end,
            value,
            place,
            scale: 0,
            words: true,
            terms: terms(words),
            digit_last: false,
        };
        let next = Group {
            end: figure.range.end,
            value: figure_value,
            place: figure_place,
            scale: 0,
            words: true,
            terms: vec![Term::Figure(figure_value, 0)],
            digit_last: false,
        };
        let (range_end, extent_end) = (figure.range.end, figure.extent.end);
        let (unit, flags, conf) = (figure.number.unit.clone(), figure.flags, figure.conf);
        self.items[index].dead = true;
        let (start, sign) = self.word_sign(at, false);
        let total = value.checked_add(figure_value);
        match total.filter(|_| fits(std::slice::from_ref(&first), &next)) {
            Some(mut total) => {
                if sign == Some(Sign::Negative) && !total.is_zero() {
                    total.set_sign_negative(true);
                }
                let number = Number {
                    value: total,
                    scale: Scale(0),
                    unit,
                    form: Form::Mixed,
                    decimals: u32::try_from(-figure_place).unwrap_or(0),
                    role: NumberRole::Quantity,
                };
                let (conf, alts) = if sign == Some(Sign::Ambiguous) {
                    (conf.min(0.5), vec![(opposite(&number), 0.5)])
                } else {
                    (conf, Vec::new())
                };
                self.push_item(
                    start..range_end,
                    start..extent_end,
                    number,
                    flags | Flags::WORDS,
                    conf,
                    alts,
                );
            }
            None => self.push_fixed(start..range_end, NumberRole::Code, Flags::WORDS),
        }
        true
    }

    /// The sign of number words at `at`: a leading ลบ (negative after ติด, else ambiguous), a
    /// minus, or a sign word before them. Returns where the number starts with it.
    fn word_sign(&self, at: usize, lop: bool) -> (usize, Option<Sign>) {
        if lop {
            let tid = at.checked_sub(char_len(TID)).filter(|tid| {
                has_before(self.t, at, TID) && !self.taken[*tid..at].iter().any(|slot| *slot)
            });
            return match tid {
                Some(tid) => (tid, Some(Sign::Negative)),
                None => (at, Some(Sign::Ambiguous)),
            };
        }
        if self.minus_before(at) {
            return (at - 1, Some(Sign::Negative));
        }
        match self.sign_word(at) {
            Some((sign, sign_start)) => (sign_start, Some(sign)),
            None => (at, None),
        }
    }

    /// Number words `words` over `at..end`: alone, or restating the figure before them in
    /// parentheses (`restated` then holds the unit written inside them and the figure's value).
    /// A `Quantity` when they start with a digit word, form a complete numeral with a power word
    /// or ล้าน and stand before a unit, after ร้อยละ or in those parentheses; a percent after
    /// ร้อยละ with no power word too; in those parentheses, words that start with a power word
    /// when they equal the figure (`10,000,000 บาท (สิบล้านบาทถ้วน)`). Any other run with a power
    /// word or ล้าน is one code; a run without one is no number.
    fn word_number(&mut self, at: usize, words: &[Word], end: usize, restated: Option<Restated>) {
        let (lop, body) = match words.split_first() {
            Some((Word::Lop, rest)) => (true, rest),
            _ => (false, words),
        };
        let Some(first) = body.first() else {
            return;
        };
        let in_parens = restated.is_some();
        let (mut start, sign) = if in_parens && !lop {
            (at, None)
        } else {
            self.word_sign(at, lop)
        };
        let mut end = end;
        let power = |word: &Word| matches!(word, Word::Power(_) | Word::Lan);
        let has_power = body.iter().any(power);
        let power_first = power(first);
        let after_percent = !in_parens && {
            let before = if self.before(start) == Some(' ') {
                start - 1
            } else {
                start
            };
            has_before(self.t, before, ROI_LA)
        };
        let glued = matches!(body.last(), Some(Word::Digit(_))) && self.glued_word(end);
        // ครึ่ง beside power words makes the run one code with it.
        let half_before = power_first
            && start.checked_sub(char_len(KHRUENG)).is_some_and(|half| {
                has_before(self.t, start, KHRUENG)
                    && !self.taken[half..start].iter().any(|slot| *slot)
            });
        let half_after = body.last().is_some_and(power)
            && has_at(self.t, end, KHRUENG)
            && !self.taken[end..end + char_len(KHRUENG)]
                .iter()
                .any(|slot| *slot);
        if half_before {
            start -= char_len(KHRUENG);
        }
        if half_after {
            end += char_len(KHRUENG);
        }
        let point = body.iter().position(|word| *word == Word::Chut);
        let places = point.map_or(0, |point| body.len() - point - 1);
        let digits_after_point = point.is_none_or(|point| {
            body[point + 1..]
                .iter()
                .all(|word| matches!(word, Word::Digit(_) | Word::Sun))
        });
        let value = words_value(body)
            .ok()
            .filter(|_| digits_after_point && places <= 27 && !regrouped(&terms(body)));
        let mut unit_end = end;
        let (unit, restates) = match restated {
            Some(Restated { unit, value }) => (unit, value),
            None if after_percent => (Some(Unit::Percent), None),
            None => {
                let after = self.unit_after(self.space(end), false);
                if let Some((_, word_end)) = &after {
                    unit_end = *word_end;
                }
                (after.map(|(unit, _)| unit), None)
            }
        };
        let readable = !half_before && !half_after && !glued;
        let restates_it = in_parens && value.is_some() && value == restates;
        let quantity = readable
            && match (has_power, power_first) {
                (false, _) => after_percent,
                (true, true) => after_percent || restates_it,
                (true, false) => in_parens || after_percent || unit.is_some(),
            };
        match value {
            Some(mut value) if quantity => {
                if sign == Some(Sign::Negative) && !value.is_zero() {
                    value.set_sign_negative(true);
                }
                let number = Number {
                    value,
                    scale: Scale(0),
                    unit,
                    form: Form::Words,
                    decimals: u32::try_from(places).unwrap_or(0),
                    role: NumberRole::Quantity,
                };
                let mut flags = Flags::WORDS;
                if in_parens {
                    flags |= Flags::AMOUNT_IN_WORDS;
                }
                let mut conf = 1.0;
                let mut alts = Vec::new();
                if sign == Some(Sign::Ambiguous) {
                    conf = 0.5;
                    alts.push((opposite(&number), 0.5));
                }
                if point.is_none()
                    && !has_before(self.t, end, ET)
                    && let Some((spoken, _)) = spoken(&terms(body), number.value)
                {
                    conf = f32::min(conf, CONF_CAP);
                    alts.push((
                        Number {
                            value: spoken,
                            ..number.clone()
                        },
                        CONF_OTHER,
                    ));
                }
                self.push_item(start..end, start..unit_end, number, flags, conf, alts);
            }
            _ if has_power => self.push_fixed(start..end, NumberRole::Code, Flags::WORDS),
            _ => {}
        }
    }

    /// Number words that no figure, date or percent word took, and that start no ordinary word;
    /// groups of them split by one space join as one number, or one code.
    fn words(&mut self) {
        let mut at = 0;
        while at < self.t.len() {
            if self.taken[at] || self.ordinary[at] || word_at(self.t, at).is_none() {
                at += 1;
                continue;
            }
            let (words, end) = self.word_run(at);
            if end == at {
                at += 1;
                continue;
            }
            let (words, end, fits) = self.spaced_parts(words, end);
            if !fits {
                self.push_fixed(at..end, NumberRole::Code, Flags::WORDS);
            } else if !self.join_figure(at, &words, end) {
                self.word_number(at, &words, end, None);
            }
            at = end;
        }
    }
}

/// What the parentheses of the amount-in-words pattern hold besides the words: the unit written
/// in them and the value of the figure before them.
struct Restated {
    unit: Option<Unit>,
    value: Option<Decimal>,
}

/// A figure written as one field: no thousands separator, no decimal part.
fn whole(token: &Token) -> Option<&[u8]> {
    match &token.kind {
        TokenKind::Figure {
            int,
            frac,
            grouped: false,
        } if frac.is_empty() => Some(int),
        _ => None,
    }
}

/// `next` writes only places below the lowest one of the part before it, and for Thai parts the
/// grammar reads all of them as their sum (so `3 ล้าน 5 ล้าน` and `3 พันล้าน 5 ร้อยล้าน` do not
/// join).
fn fits(groups: &[Group], next: &Group) -> bool {
    let Some(last) = groups.last() else {
        return false;
    };
    let below = u32::try_from(last.place)
        .ok()
        .and_then(power_of_ten)
        .is_some_and(|limit| next.value < limit);
    if !below || next.terms.is_empty() {
        return below;
    }
    let parts = || groups.iter().chain([next]);
    let terms: Vec<Term> = parts()
        .flat_map(|group| group.terms.iter().copied())
        .collect();
    let sum = parts().try_fold(Decimal::ZERO, |sum, group| sum.checked_add(group.value));
    matches!((numeral(&terms), sum), (Ok((whole, _)), Some(sum)) if whole == sum)
}

/// The number with the other sign: the other reading after a bare ลบ, beside a sign word or in
/// accounting parentheses that may restate a rise.
pub(crate) fn opposite(number: &Number) -> Number {
    let mut opposite = number.clone();
    if !opposite.value.is_zero() {
        opposite
            .value
            .set_sign_negative(!number.value.is_sign_negative());
    }
    opposite
}

/// A power word or ล้าน followed by one digit word at the end of `terms`: the grammar reads the
/// digit as units, everyday Thai one place below the power (หนึ่งหมื่นห้า is 10,005 by the grammar
/// and 15,000 in speech; ล้านสอง 1,000,002 and 1,200,000). The spoken total, from the grammar's
/// `total`, and its lowest place. The caller checks that the digit word is not เอ็ด, which only
/// ever means units.
fn spoken(terms: &[Term], total: Decimal) -> Option<(Decimal, i64)> {
    let [.., Term::Word(power), Term::Word(Word::Digit(digit))] = terms else {
        return None;
    };
    let place = match power {
        Word::Power(power) if *power >= 2 => i64::from(*power),
        Word::Lan => 6,
        _ => return None,
    };
    let digit = Decimal::from(*digit);
    let shifted = digit.checked_mul(power_of_ten(u32::try_from(place - 1).ok()?)?)?;
    let mut spoken = total.abs().checked_sub(digit)?.checked_add(shifted)?;
    spoken.set_sign_negative(total.is_sign_negative());
    Some((spoken, place - 1))
}

/// A Thai digit word or power word ends at `end`: a day written in words before a month name
/// (`สิบห้า ต.ค.`).
pub(crate) fn number_word_ends_at(t: &[char], end: usize) -> bool {
    WORDS.iter().any(|(word, kind)| {
        matches!(kind, Word::Digit(_) | Word::Power(_)) && has_before(t, end, word)
    })
}

/// An amount starts at `at`: Thai number words that start with a power word or ล้าน (not
/// ร้อยละ, the percent word, or an ordinary word such as ร้อยเอก) or that start with a digit word
/// and hold one; an English scale word; or a unit word or symbol. What may follow an amount, never
/// a two-digit year: `ยอดขายปี 68 ร้อยละ 5` and `ปี 68 ห้างสรรพสินค้า` keep their year.
pub(crate) fn amount_word_at(t: &[char], at: usize) -> bool {
    let power = |word: &Word| matches!(word, Word::Power(_) | Word::Lan);
    let mut words = Vec::new();
    let mut next = at;
    // An amount shows in its first few words; the cap keeps every call short.
    while words.len() < 12
        && let Some((word, end)) = word_at(t, next)
    {
        words.push(word);
        next = end;
    }
    let thai = !has_at(t, at, ROI_LA)
        && !ORDINARY_WORDS.iter().any(|word| has_at(t, at, word))
        && match words.first() {
            Some(word) if power(word) => true,
            Some(Word::Digit(_)) => words.iter().any(power),
            _ => false,
        };
    thai || ENGLISH_SCALES.iter().any(|(word, _)| {
        has_at(t, at, word)
            && !t
                .get(at + char_len(word))
                .is_some_and(|c| is_latin_letter(*c))
    }) || UNITS_AFTER.iter().any(|(word, _)| has_at(t, at, word))
        || t.get(at).copied().and_then(symbol_unit).is_some()
}

/// A clock field: `len` digits and no more than `max`.
fn clock_value(digits: &[u8], len: std::ops::RangeInclusive<usize>, max: u32) -> Option<u32> {
    if !len.contains(&digits.len()) {
        return None;
    }
    let value = digits
        .iter()
        .fold(0_u32, |value, digit| value * 10 + u32::from(*digit));
    (value <= max).then_some(value)
}

/// A figure's value, scale and decimals, or `None` when a `Decimal` cannot hold it: more than
/// 27 decimals, too many digits, or a value times scale out of range.
fn digits(int: &[u8], frac: &[u8], negative: bool, exponent: u32) -> Option<(Decimal, u8, u32)> {
    if frac.len() > 27 || exponent > MAX_SCALE {
        return None;
    }
    let mut mantissa: i128 = 0;
    for digit in int.iter().chain(frac) {
        mantissa = mantissa.checked_mul(10)?.checked_add(i128::from(*digit))?;
    }
    let decimals = u32::try_from(frac.len()).ok()?;
    let mut value = Decimal::try_from_i128_with_scale(mantissa, decimals).ok()?;
    if negative && !value.is_zero() {
        value.set_sign_negative(true);
    }
    // The effective value must fit too.
    value.checked_mul(power_of_ten(exponent)?)?;
    Some((value, u8::try_from(exponent).ok()?, decimals))
}

/// A Mixed number as the figure times its leading scale words plus the rest: `3 ล้านห้าแสน` is
/// 3.5 with scale 6, so its last written place, แสน (`lowest`), sets the precision; so does the
/// 0 of `1 ล้าน 250,000`, which is 1.250000 with scale 6.
fn mixed(total: Decimal, negative: bool, exponent: u32, lowest: i64) -> Option<(Decimal, u8, u32)> {
    let unit = power_of_ten(exponent)?;
    let mut value = total.checked_div(unit)?.normalize();
    let written = u32::try_from(i64::from(exponent) - lowest).unwrap_or(0);
    let decimals = value.scale().max(written);
    if decimals > 27 {
        return None;
    }
    value.rescale(decimals);
    if negative && !value.is_zero() {
        value.set_sign_negative(true);
    }
    Some((value, u8::try_from(exponent).ok()?, decimals))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_written_as_normalize_th_writes_them() {
        let mut words: Vec<&str> = WORDS.iter().map(|(word, _)| *word).collect();
        words.extend(ORDINARY_WORDS);
        words.extend(ORDINAL_MARKERS);
        words.extend(UNITS_AFTER.iter().map(|(word, _)| *word));
        words.extend([
            ROI_LA, BAHT_THUAN, NA_DOT, KHRUENG, TID, TID_LOP, LOP, THUENG,
        ]);
        for word in words {
            assert_eq!(crate::normalize_th(word).text, word, "{word:?}");
        }
    }

    /// Every ordinary word is no number alone, and no scale after a figure.
    #[test]
    fn ordinary_words_hold_no_number() {
        let ctx = Ctx {
            anchor: chrono::NaiveDate::from_ymd_opt(2025, 11, 1),
        };
        for word in ORDINARY_WORDS {
            assert!(find_numbers(word, &ctx).unwrap().is_empty(), "{word:?}");
            let after = find_numbers(&format!("1 {word}"), &ctx).unwrap();
            assert_eq!(after.len(), 1, "{word:?}: {after:?}");
            assert_eq!(after[0].value.scale, Scale(0), "{word:?}");
            assert_eq!(after[0].value.form, Form::Digits, "{word:?}");
        }
    }

    #[test]
    fn no_number_word_is_a_prefix_of_another() {
        for (a, _) in WORDS {
            for (b, _) in WORDS {
                assert!(a == b || !b.starts_with(a), "{a:?} {b:?}");
            }
        }
    }
}

#[cfg(test)]
mod work_tests {
    use crate::found::WORK;
    use crate::{Ctx, find_dates, find_numbers};

    fn work_of(text: &str) -> u64 {
        let ctx = Ctx {
            anchor: chrono::NaiveDate::from_ymd_opt(2025, 11, 1),
        };
        WORK.with(|work| work.set(0));
        let numbers = find_numbers(text, &ctx).map(|found| found.len());
        let dates = find_dates(text, &ctx).map(|found| found.len());
        assert!(numbers.is_ok() && dates.is_ok());
        WORK.with(|work| work.get())
    }

    /// Doubling a hostile text at most doubles the scanners' work (plus a little), so no input
    /// up to the scan limit makes them quadratic.
    #[test]
    fn scanners_do_linear_work_on_hostile_text() {
        let fragments = [
            "1 ",
            "1,",
            "1.",
            "1-",
            "(",
            "(1)",
            "\u{0E25}\u{0E49}\u{0E32}\u{0E19}",
            "1 \u{0E25}\u{0E49}\u{0E32}\u{0E19}",
            "\u{0E48}",
            "\u{0E21}.\u{0E04}. ",
            "1 \u{0E15}.\u{0E04}. 2568 ",
            "\u{0E1B}\u{0E35} 2567 1,250 ",
            "\u{0E2B}\u{0E49}\u{0E32}\u{0E2A}\u{0E34}\u{0E1A}\u{0E1A}\u{0E32}\u{0E17} ",
            "FY2569 Q2 ",
            "1/1/1",
            "9",
            "1 \u{0E25}\u{0E49}\u{0E32}\u{0E19} 2 \u{0E41}\u{0E2A}\u{0E19} ",
            "1\u{0E25}\u{0E49}\u{0E32}\u{0E19}",
            "a1",
            "1a",
            "x,1",
            "1'",
            "1\u{2009}",
            "\u{0E25}\u{0E1A} 1 ",
            "\u{0E15}\u{0E34}\u{0E14}\u{0E25}\u{0E1A}",
            "\u{0E04}\u{0E23}\u{0E36}\u{0E48}\u{0E07}\u{0E25}\u{0E49}\u{0E32}\u{0E19}",
            "1:",
            "1-",
            "\u{0E1B}\u{0E35} 2567-2569 ",
            "1-31 \u{0E15}.\u{0E04}. 2568 ",
            "\u{0E15}.\u{0E04}.-",
            "q1-",
            "\u{0E44}\u{0E15}\u{0E23}\u{0E21}\u{0E32}\u{0E2A} 2 \u{0E1B}\u{0E35} 2568 ",
            "\u{0E40}\u{0E23}\u{0E35}\u{0E22}\u{0E1A}\u{0E23}\u{0E49}\u{0E2D}\u{0E22}",
            // Fix round 2: letter-digit runs, fractions, odd separators and controls, parts of
            // amounts, currencies between figures, signals, lists and numeric ranges.
            "a1b",
            "1/2 ",
            "1/2 \u{0E25}\u{0E49}\u{0E32}\u{0E19}",
            "1,25 ",
            "\u{FFFD}",
            "1\u{FFFD}",
            "1\u{0096}",
            "\u{3164}1",
            "\u{0E2A}\u{0E32}\u{0E21}\u{0E25}\u{0E49}\u{0E32}\u{0E19} ",
            "1 \u{0E25}\u{0E49}\u{0E32}\u{0E19} 250,000 ",
            "100 usd ",
            "\u{0E15}\u{0E34}\u{0E14}\u{0E25}\u{0E1A} 1 ",
            "\u{0E16}\u{0E36}\u{0E07} ",
            "q2 ",
            "\u{0E1B}\u{0E35} 2566, ",
            "1/10/2568-",
            "1 \u{0E41}\u{0E25}\u{0E30} ",
            "(5%) ",
            "1st ",
            "jan-",
        ];
        for fragment in fragments {
            let len = fragment.chars().count();
            let small: String = fragment.repeat(10_000 / len);
            let large: String = fragment.repeat(20_000 / len);
            let (small_work, large_work) = (work_of(&small), work_of(&large));
            assert!(
                large_work <= small_work * 22 / 10 + 1_000,
                "{fragment:?}: {small_work} then {large_work}"
            );
        }
    }
}
