//! Figures, Thai number words and scale words.

use std::ops::Range;

use rust_decimal::Decimal;

use crate::dates::{self, Scan};
use crate::found::{
    View, char_len, check_len, digits_end, has_at, has_before, is_digit, is_latin_letter, is_thai,
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

/// Words that begin with a scale word but are not one: `3 พันธุ์` is three varieties.
const NOT_SCALES: [&str; 6] = [
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E18}", // พันธ (พันธุ์, พันธ์, พันธะ, พันธมิตร)
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E15}\u{0E23}\u{0E35}", // พันตรี
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E42}\u{0E17}", // พันโท
    "\u{0E1E}\u{0E31}\u{0E19}\u{0E40}\u{0E2D}\u{0E01}", // พันเอก
    "\u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E19}\u{0E32}", // ล้านนา
    "\u{0E25}\u{0E49}\u{0E32}\u{0E19}\u{0E0A}\u{0E49}\u{0E32}\u{0E07}", // ล้านช้าง
];

const ROI_LA: &str = "\u{0E23}\u{0E49}\u{0E2D}\u{0E22}\u{0E25}\u{0E30}"; // ร้อยละ
const BAHT_THUAN: &str = "\u{0E1A}\u{0E32}\u{0E17}\u{0E16}\u{0E49}\u{0E27}\u{0E19}"; // บาทถ้วน
const NA_DOT: &str = "\u{0E19}."; // น. (the hour)

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

/// `thaiword_to_num`: `(ลบ)?(G ล้าน)*G` with `G = (D แสน)?(D หมื่น)?(D พัน)?(D ร้อย)?(D สิบ)?D?`,
/// or `ศูนย์` alone. The pending digit starts at 1, so a bare power word or ล้าน means one of
/// it; a power word takes `max(pending, 1)` and leaves 0 pending; ล้าน multiplies everything so
/// far. `lead` is a figure written before the words (a `Mixed` number): it is the first pending
/// digit, used as it is.
fn integer(words: &[Word], lead: Option<Decimal>) -> Result<Decimal, WordsError> {
    if words.is_empty() && lead.is_none() {
        return Err(WordsError::Invalid);
    }
    if lead.is_none() && words == [Word::Sun] {
        return Ok(Decimal::ZERO);
    }
    let (negative, words) = match words.split_first() {
        Some((Word::Lop, rest)) if lead.is_none() => (true, rest),
        _ => (false, words),
    };
    let million = Decimal::from(1_000_000);
    // None once the total no longer fits; the grammar is still checked to the end.
    let mut total = Some(Decimal::ZERO);
    let mut pending = lead.unwrap_or(Decimal::ONE);
    let mut pending_is_lead = lead.is_some();
    let mut last_power: Option<u8> = None;
    let mut after_digit = lead.is_some();
    for word in words {
        match *word {
            Word::Digit(digit) => {
                if after_digit {
                    return Err(WordsError::Invalid);
                }
                after_digit = true;
                pending = Decimal::from(digit);
                pending_is_lead = false;
            }
            Word::Power(power) => {
                if last_power.is_some_and(|last| power >= last) {
                    return Err(WordsError::Invalid);
                }
                last_power = Some(power);
                after_digit = false;
                let times = if pending_is_lead {
                    pending
                } else {
                    pending.max(Decimal::ONE)
                };
                let add = power_of_ten(u32::from(power)).and_then(|unit| times.checked_mul(unit));
                total = total
                    .zip(add)
                    .and_then(|(total, add)| total.checked_add(add));
                pending = Decimal::ZERO;
                pending_is_lead = false;
            }
            Word::Lan => {
                last_power = None;
                after_digit = false;
                total = total
                    .and_then(|total| total.checked_add(pending))
                    .and_then(|sum| sum.checked_mul(million));
                pending = Decimal::ZERO;
                pending_is_lead = false;
            }
            Word::Lop | Word::Chut | Word::Sun => return Err(WordsError::Invalid),
        }
    }
    let mut total = total
        .and_then(|total| total.checked_add(pending))
        .ok_or(WordsError::Overflow)?;
    if negative && !total.is_zero() {
        total.set_sign_negative(true);
    }
    Ok(total)
}

/// `words_to_num`: without จุด the words are one numeral; with it, the words before the first
/// จุด are the integer and each word after it is read alone as the next decimal place, added
/// (subtracted when the integer is −1 or less). Exact, where PyThaiNLP adds floats.
fn words_value(words: &[Word]) -> Result<Decimal, WordsError> {
    let Some(point) = words.iter().position(|word| *word == Word::Chut) else {
        return integer(words, None);
    };
    let whole = integer(&words[..point], None)?;
    let mut fraction = Decimal::ZERO;
    let mut places = 0;
    for word in &words[point + 1..] {
        let digit = integer(std::slice::from_ref(word), None)?;
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
/// 3) and `.` decimals, in any of the digit scripts `normalize_th` folds. A `-`, U+2212, U+2012
/// or U+2013 is a minus only when no figure stands right before it; `(1,250)` is an accounting
/// negative, while one or two digits in parentheses, as `(1)`, are a list marker. Scale words
/// after a figure add their powers of ten (พันล้าน is 9); a unit may stand before or after.
/// A figure followed by number words that start with a power word or ล้าน is one `Mixed`
/// number, whatever follows it. Number words alone are read only when they form a complete
/// numeral with a power word or ล้าน, stand right before a unit or percent word or right after
/// ร้อยละ, or restate the figure before them in parentheses (`AMOUNT_IN_WORDS`), and touch no
/// date or month name. A figure no `Decimal` can hold is reported as a `Code` of value 0 over
/// its whole span, never dropped.
///
/// The span covers the sign, accounting parentheses, the figure, its scale words and a symbol
/// unit written with no space, never a unit word (except one standing between a figure and its
/// scale words, as in `1 THB million`). Spans are code points of `text`; a text longer than
/// [`crate::MAX_SCAN_CODE_POINTS`] is refused whole.
///
/// The role is the first that applies: `DatePart` for the digits of a date [`crate::find_dates`]
/// returns and for a day written before a month name with no year; `Code` for a figure right
/// after a stray dot (`.5`); `Quantity` when a scale word or a unit goes with the figure,
/// whatever precedes it; `Ordinal` after วาระที่, ข้อ, ครั้งที่, ลำดับที่, ที่, หน้า, `No.`, `#`,
/// `item`, `agenda`, `section` or `page`, with at most one space between; `Time` for `HH:MM`,
/// or `HH.MM` before `น.`; `Code` when a Latin (or other non-Thai) letter or a slash touches it,
/// it holds two dots, or it is one of three or more digit groups joined by `-`; otherwise
/// `Quantity`. Every number has confidence 1.0 and no alternatives.
///
/// The text is read through [`crate::normalize_th`], so mark order, Thai and full-width digits,
/// zero-width characters and dash variants do not change what is found. Latin era markers are
/// matched in the original capitals, so pass the original text, not a normalized one, when it
/// may hold them.
pub fn find_numbers(text: &str, ctx: &Ctx) -> Result<Vec<Found<Number>>, TooLong> {
    check_len(text)?;
    let view = View::new(text);
    let dates = dates::scan(&view, dates::anchor(ctx));
    let mut scanner = Scanner::new(&view, &dates);
    scanner.figures();
    scanner.words();
    let mut items = scanner.items;
    items.sort_by_key(|item| item.range.start);
    Ok(items
        .into_iter()
        .map(|item| view.found(&item.range, item.number, Vec::new(), 1.0, item.flags))
        .collect())
}

struct Item {
    range: Range<usize>,
    number: Number,
    flags: Flags,
}

/// A figure as lexed, in normalized code points.
#[derive(Clone, Debug)]
struct Token {
    start: usize,
    end: usize,
    kind: TokenKind,
}

#[derive(Clone, Debug)]
enum TokenKind {
    /// Digits of the integer and the decimal part, separators removed.
    Figure {
        int: Vec<u8>,
        frac: Vec<u8>,
        grouped: bool,
    },
    /// Digit groups joined by more than one `.`, or three or more joined by `-`.
    Code,
    /// `HH:MM`.
    Clock { hours: u32, minutes: u32 },
}

/// What follows a figure in Thai words.
enum ScaleRead {
    /// Scale words only: `ล้าน`, `พันล้าน`.
    Scale { end: usize, exponent: u32 },
    /// Scale or power words with more number words after them: `3 ล้านห้าแสน`.
    Mixed {
        end: usize,
        exponent: u32,
        total: Decimal,
    },
    /// A Mixed number too large for a `Decimal`.
    Unholdable { end: usize },
}

struct Scanner<'a> {
    t: &'a [char],
    dates: &'a Scan,
    /// Code points no number word may come from: percent words, dates, month names and
    /// what figures already took.
    taken: Vec<bool>,
    items: Vec<Item>,
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
        let mut at = 0;
        while at < t.len() {
            if has_at(t, at, ROI_LA) {
                let end = at + char_len(ROI_LA);
                taken[at..end].iter_mut().for_each(|slot| *slot = true);
                at = end;
            } else {
                at += 1;
            }
        }
        Scanner {
            t,
            dates,
            taken,
            items: Vec::new(),
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
        for token in &tokens {
            self.figure(token);
        }
    }

    fn tokens(&self) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut at = 0;
        while at < self.t.len() {
            if !self.get(at).is_some_and(is_digit) {
                at += 1;
                continue;
            }
            let token = self.lex(at);
            at = token.end;
            tokens.push(token);
        }
        self.join_runs(tokens)
    }

    /// One figure from `at`: digit groups joined by `,` thousands (a first group of 1–3 digits,
    /// later groups of exactly 3) and one `.` decimal part. A second `.` makes the whole chain
    /// of groups one code (`1.2.3`, `192.168.0.1`). Each group is read once, so a run such as
    /// `1,2,3,…` costs no more than its length.
    fn lex(&self, at: usize) -> Token {
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
            };
        };
        if self.get(frac_group.end) == Some('.') && group_after(frac_group.end).is_some() {
            let mut end = frac_group.end;
            while matches!(self.get(end), Some('.' | ',')) {
                match group_after(end) {
                    Some(group) => end = group.end,
                    None => break,
                }
            }
            return Token {
                start: at,
                end,
                kind: TokenKind::Code,
            };
        }
        let frac = self.digits_with_commas(frac_group.clone());
        Token {
            start: at,
            end: frac_group.end,
            kind: TokenKind::Figure { int, frac, grouped },
        }
    }

    fn digits_with_commas(&self, range: Range<usize>) -> Vec<u8> {
        self.t[range]
            .iter()
            .filter_map(|c| c.to_digit(10))
            .filter_map(|d| u8::try_from(d).ok())
            .collect()
    }

    /// Three or more figures joined by `-` are one code (`000-000-0000`); `HH:MM` is a time.
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
                });
                i = last + 1;
                continue;
            }
            if let (Some(clock), Some(next)) =
                (self.clock(&tokens[i], tokens.get(i + 1)), tokens.get(i + 1))
            {
                out.push(Token {
                    start: tokens[i].start,
                    end: next.end,
                    kind: clock,
                });
                i += 2;
                continue;
            }
            out.push(tokens[i].clone());
            i += 1;
        }
        out
    }

    fn clock(&self, hours: &Token, minutes: Option<&Token>) -> Option<TokenKind> {
        let minutes = minutes?;
        let (
            TokenKind::Figure {
                int: h,
                frac: hf,
                grouped: false,
            },
            TokenKind::Figure {
                int: m,
                frac: mf,
                grouped: false,
            },
        ) = (&hours.kind, &minutes.kind)
        else {
            return None;
        };
        if self.get(hours.end) != Some(':')
            || minutes.start != hours.end + 1
            || !hf.is_empty()
            || !mf.is_empty()
        {
            return None;
        }
        if self.get(minutes.end) == Some(':') && self.get(minutes.end + 1).is_some_and(is_digit) {
            return None;
        }
        let hours_value = clock_value(h, 1..=2, 24)?;
        let minutes_value = clock_value(m, 2..=2, 59)?;
        Some(TokenKind::Clock {
            hours: hours_value,
            minutes: minutes_value,
        })
    }

    /// Reads one lexed figure with what surrounds it, and the amount in words after it.
    fn figure(&mut self, token: &Token) {
        let (int, frac) = match &token.kind {
            TokenKind::Figure { int, frac, .. } => (int.clone(), frac.clone()),
            TokenKind::Code => {
                let role = self.role_without_value(token);
                self.push_fixed(token.start..token.end, role, Flags::default());
                return;
            }
            TokenKind::Clock { hours, minutes } => {
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
                match Decimal::try_new(i64::from(hours * 100 + minutes), 2) {
                    Ok(value) => {
                        let number = Number {
                            value,
                            scale: Scale(0),
                            unit: None,
                            form: Form::Digits,
                            decimals: 2,
                            role,
                        };
                        self.push(range, number, flags);
                    }
                    Err(_) => self.push_fixed(range, NumberRole::Code, Flags::default()),
                }
                return;
            }
        };
        let mut flags = Flags::default();
        // Before the digits: a symbol unit and a minus, in either order ($-5, -$5).
        let mut start = token.start;
        let mut unit = self.prefix_symbol(start);
        if unit.is_some() {
            start -= 1;
        }
        let minus = self.free_before(start) == Some('-') && !self.figure_before(start - 1);
        if minus {
            start -= 1;
            if unit.is_none()
                && let Some(symbol) = self.prefix_symbol(start)
            {
                unit = Some(symbol);
                start -= 1;
            }
        }
        // After them: a symbol unit written with no space, as in 12.5%.
        let mut end = token.end;
        let suffix = self.get(end).and_then(symbol_unit);
        if let Some(symbol) = &suffix {
            unit = Some(symbol.clone());
            end += 1;
        }
        let (bare_start, bare_end) = (start, end);
        // Accounting parentheses, unless the figure is a list marker such as (1).
        let list_marker = !minus
            && frac.is_empty()
            && int.len() <= 2
            && token.end - token.start == int.len()
            && (start, end) == (token.start, token.end);
        let parens = !minus && self.free_before(start) == Some('(') && self.get(end) == Some(')');
        let negative = minus || (parens && !list_marker);
        if parens && !list_marker {
            flags |= Flags::PAREN_NEGATIVE;
            start -= 1;
            end += 1;
        }
        // Scale words, and the number words a Mixed number goes on with.
        let mut exponent: u32 = 0;
        let mut mixed_total = None;
        let reads_on = suffix.is_none() && !(parens && list_marker);
        if reads_on {
            let next = self.space(end);
            match self.thai_scale(next, &int, &frac) {
                Some(ScaleRead::Scale {
                    end: scale_end,
                    exponent: scale,
                }) => {
                    end = scale_end;
                    exponent = scale;
                }
                Some(ScaleRead::Mixed {
                    end: scale_end,
                    exponent: scale,
                    total,
                }) => {
                    end = scale_end;
                    exponent = scale;
                    mixed_total = Some(total);
                }
                Some(ScaleRead::Unholdable { end: words_end }) => {
                    self.push_fixed(start..words_end, NumberRole::Code, Flags::default());
                    return;
                }
                None => {
                    if let Some((scale_end, scale)) = self.english_scale(next) {
                        end = scale_end;
                        exponent = scale;
                    }
                }
            }
        }
        // A unit word after them. An English scale may still follow it (1 THB million); the
        // span then runs over the unit word to the end of the scale.
        let mut tail = end;
        if reads_on && let Some((after, word_end)) = self.unit_after(self.space(end)) {
            unit.get_or_insert(after);
            tail = word_end;
            if exponent == 0
                && let Some((scale_end, scale)) = self.english_scale(self.space(word_end))
            {
                exponent = scale;
                end = scale_end;
                tail = scale_end;
            }
        }
        if unit.is_none()
            && let Some((before, _)) = self.unit_before(start)
        {
            unit = Some(before);
        }
        let range = start..end;
        let number = match mixed_total {
            Some(total) => {
                mixed(total, negative, exponent, frac.len()).map(|number| (number, Form::Mixed))
            }
            None => digits(&int, &frac, negative, exponent).map(|number| (number, Form::Digits)),
        };
        let Some(((value, scale, decimals), form)) = number else {
            // Ruling 2: a figure no Decimal holds is a code over its whole span, never dropped.
            self.push_fixed(range, NumberRole::Code, Flags::default());
            self.amount_in_words(tail);
            return;
        };
        let role = if self.in_date(&(token.start..token.end)) {
            NumberRole::DatePart
        } else if self.stray_dot(start) {
            NumberRole::Code
        } else if unit.is_some() || exponent > 0 {
            NumberRole::Quantity
        } else if self.ordinal_marker(start) {
            NumberRole::Ordinal
        } else if self.time_after(&int, &frac, token.end) {
            NumberRole::Time
        } else if self.touches_code(bare_start, bare_end) {
            NumberRole::Code
        } else {
            NumberRole::Quantity
        };
        if form == Form::Mixed {
            flags |= Flags::WORDS;
        }
        if role == NumberRole::Time {
            flags |= Flags::TIME;
        }
        self.push(
            range,
            Number {
                value,
                scale: Scale(scale),
                unit,
                form,
                decimals,
                role,
            },
            flags,
        );
        self.amount_in_words(tail);
    }

    /// A symbol unit right before `start` that no earlier figure took as its own suffix.
    fn prefix_symbol(&self, start: usize) -> Option<Unit> {
        let at = start.checked_sub(1)?;
        if self.taken[at] {
            return None;
        }
        self.symbol_before_figure(at)
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

    fn push(&mut self, range: Range<usize>, number: Number, flags: Flags) {
        self.take(&range);
        self.items.push(Item {
            range,
            number,
            flags,
        });
    }

    /// A number whose value is not read: a code or a date part, value 0.
    fn push_fixed(&mut self, range: Range<usize>, role: NumberRole, flags: Flags) {
        let number = Number {
            value: Decimal::ZERO,
            scale: Scale(0),
            unit: None,
            form: Form::Digits,
            decimals: 0,
            role,
        };
        self.push(range, number, flags);
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

    /// A figure ends right before `at`, perhaps with one space between: then a dash at `at`
    /// joins a range and is no minus.
    fn figure_before(&self, at: usize) -> bool {
        let prev = self.before(at);
        if prev == Some(' ') {
            return at
                .checked_sub(1)
                .and_then(|p| self.before(p))
                .is_some_and(is_digit);
        }
        prev.is_some_and(is_digit)
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

    /// Thai scale words after a figure, and the number words a `Mixed` number goes on with.
    fn thai_scale(&self, at: usize, int: &[u8], frac: &[u8]) -> Option<ScaleRead> {
        let mut words = Vec::new();
        let mut ends = Vec::new();
        let mut next = at;
        while let Some((word, end)) = word_at(self.t, next) {
            if NOT_SCALES.iter().any(|not| has_at(self.t, next, not))
                || self.taken[next..end].iter().any(|slot| *slot)
            {
                break;
            }
            words.push(word);
            ends.push(end);
            next = end;
        }
        let scales = words
            .iter()
            .take_while(|word| matches!(word, Word::Power(3..=5) | Word::Lan))
            .count();
        let exponent: u32 = words[..scales]
            .iter()
            .map(|word| {
                if let Word::Power(power) = word {
                    u32::from(*power)
                } else {
                    6
                }
            })
            .sum();
        if words.len() > scales && matches!(words.first(), Some(Word::Power(_) | Word::Lan)) {
            let end = *ends.last()?;
            let Some((lead, _, _)) = digits(int, frac, false, 0) else {
                return Some(ScaleRead::Unholdable { end });
            };
            match integer(&words, Some(lead)) {
                Ok(total) => {
                    return Some(ScaleRead::Mixed {
                        end,
                        exponent,
                        total,
                    });
                }
                Err(WordsError::Overflow) => return Some(ScaleRead::Unholdable { end }),
                Err(WordsError::Invalid) => {}
            }
        }
        let end = *ends.get(scales.checked_sub(1)?)?;
        Some(ScaleRead::Scale { end, exponent })
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

    fn unit_after(&self, at: usize) -> Option<(Unit, usize)> {
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

    /// A unit word right before the figure, with one space between: `THB 1,250`, `ร้อยละ 12.5`.
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
                    && !(latin && self.before(word_start).is_some_and(is_latin_letter)))
                .then(|| (unit.clone(), word_start))
            })
            .or_else(|| {
                if end == start {
                    return None;
                }
                let symbol = self.symbol_before_figure(end.checked_sub(1)?)?;
                Some((symbol, end - 1))
            })
    }

    /// A dot right before the figure that belongs to nothing: `.5` is no figure we can read.
    fn stray_dot(&self, head: usize) -> bool {
        self.before(head) == Some('.')
            && !self.ordinal_marker(head)
            && !head
                .checked_sub(1)
                .and_then(|dot| self.before(dot))
                .is_some_and(is_digit)
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

    /// `HH.MM น.`: a figure of one or two digits, two decimals, and the Thai hour after it.
    fn time_after(&self, int: &[u8], frac: &[u8], end: usize) -> bool {
        frac.len() == 2
            && clock_value(int, 1..=2, 24).is_some()
            && clock_value(frac, 2..=2, 59).is_some()
            && has_at(self.t, self.space(end), NA_DOT)
    }

    /// A Latin (or other non-Thai) letter or a slash touches the figure.
    fn touches_code(&self, head: usize, end: usize) -> bool {
        let code_neighbour = |c: char| c == '/' || (c.is_alphabetic() && !is_thai(c));
        self.before(head).is_some_and(code_neighbour) || self.get(end).is_some_and(code_neighbour)
    }

    /// Number words in parentheses right after a figure, restating it.
    fn amount_in_words(&mut self, tail: usize) {
        let open = self.space(tail);
        if self.get(open) != Some('(') {
            return;
        }
        let start = open + 1;
        let (words, end) = self.word_run(start);
        if words.is_empty() {
            return;
        }
        let after = self.space(end);
        let (unit, close) = match self.unit_after(after) {
            Some((unit, word_end)) => (Some(unit), self.space(word_end)),
            None => (None, after),
        };
        if self.get(close) != Some(')') {
            return;
        }
        self.push_words(start..end, &words, false, unit, Flags::AMOUNT_IN_WORDS);
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

    /// A number in words over `range`, if it is a complete numeral with a power word or ล้าน.
    fn push_words(
        &mut self,
        range: Range<usize>,
        words: &[Word],
        negative: bool,
        unit: Option<Unit>,
        flags: Flags,
    ) {
        if !words
            .iter()
            .any(|word| matches!(word, Word::Power(_) | Word::Lan))
        {
            return;
        }
        let decimals = words
            .iter()
            .position(|word| *word == Word::Chut)
            .map_or(0, |point| words.len() - point - 1);
        match words_value(words) {
            Ok(mut value) if decimals <= 27 => {
                if negative && !value.is_zero() {
                    value.set_sign_negative(!value.is_sign_negative());
                }
                let decimals = u32::try_from(decimals).unwrap_or(0);
                let number = Number {
                    value,
                    scale: Scale(0),
                    unit,
                    form: Form::Words,
                    decimals,
                    role: NumberRole::Quantity,
                };
                self.push(range, number, flags | Flags::WORDS);
            }
            Ok(_) | Err(WordsError::Overflow) => {
                self.push_fixed(range, NumberRole::Code, Flags::WORDS)
            }
            Err(WordsError::Invalid) => {}
        }
    }

    /// Number words standing alone: read only before a unit or percent word, or after ร้อยละ.
    fn words(&mut self) {
        let mut at = 0;
        while at < self.t.len() {
            if self.taken[at] || word_at(self.t, at).is_none() {
                at += 1;
                continue;
            }
            let (words, end) = self.word_run(at);
            if end == at {
                at += 1;
                continue;
            }
            let unit = self.unit_after(self.space(end)).map(|(unit, _)| unit);
            let after_percent = {
                let before = if self.before(at) == Some(' ') {
                    at - 1
                } else {
                    at
                };
                has_before(self.t, before, ROI_LA)
            };
            let unit = if after_percent {
                Some(Unit::Percent)
            } else {
                unit
            };
            // A minus right before the words, as before a figure: a dash after a figure is a range.
            let minus = self.free_before(at) == Some('-') && !self.figure_before(at - 1);
            let start = if minus { at - 1 } else { at };
            if unit.is_some() {
                self.push_words(start..end, &words, minus, unit, Flags::default());
            }
            at = end;
        }
    }
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
/// 3.5 with scale 6, so its last written place, แสน, sets the precision.
fn mixed(
    total: Decimal,
    negative: bool,
    exponent: u32,
    figure_decimals: usize,
) -> Option<(Decimal, u8, u32)> {
    let unit = power_of_ten(exponent)?;
    let mut value = total.checked_div(unit)?.normalize();
    let decimals = value.scale().max(u32::try_from(figure_decimals).ok()?);
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
        words.extend(NOT_SCALES);
        words.extend(ORDINAL_MARKERS);
        words.extend(UNITS_AFTER.iter().map(|(word, _)| *word));
        words.extend([ROI_LA, BAHT_THUAN, NA_DOT]);
        for word in words {
            assert_eq!(crate::normalize_th(word).text, word, "{word:?}");
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
