//! Excel number formats as KatWork reads them (ECMA-376 Part 1 §18.8.30-31): the built-in format
//! ids, the section of a format code a value takes, whether that section shows the value
//! (`section_is_empty`), and `display`, KatWork's renderer for numeric formats: digit
//! placeholders, grouping, decimals, percent (×100) and trailing commas, each of which divides
//! by 1,000. Dates, times, fractions, scientific notation, conditional sections and codes written
//! in other digits are not rendered: `display` returns `None` and the cell shows its cached value.
//!
//! Numbers are rounded as Excel shows them: to the 15 significant digits Excel keeps, then to the
//! format's decimals, half away from zero, in decimal arithmetic (so 1.005 under `0.00` is 1.01).
use std::str::FromStr;

use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, Timelike};
use rust_decimal::{Decimal, RoundingStrategy};

use super::CellKind;

/// The significant digits Excel keeps of a number.
const PRECISION: u32 = 15;
/// Positive, negative, zero and text.
const MAX_SECTIONS: usize = 4;
/// The letters of the Thai built-in date and time codes (ids 71 to 80): day, month, year, hour,
/// minute, second.
const THAI_DATE_LETTERS: [char; 6] = ['ว', 'ด', 'ป', 'ช', 'น', 'ท'];
/// Excel's last date, 9999-12-31, as a 1900-system serial.
const LAST_SERIAL: f64 = 2_958_465.0;

/// The format code of a built-in `numFmtId`, in the forms Excel writes (ECMA-376 Part 1
/// §18.8.30, with Excel's own forms of ids 14, 22, 37 to 40 and 47), and the Thai ids 59 to 81.
/// Ids whose code depends on the locale (5 to 8, 27 to 36, 50 to 58) and the ids the standard
/// leaves unassigned have none: a file must define them in `numFmts`.
pub fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0_);(#,##0)",
        38 => "#,##0_);[Red](#,##0)",
        39 => "#,##0.00_);(#,##0.00)",
        40 => "#,##0.00_);[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        59 => "t0",
        60 => "t0.00",
        61 => "t#,##0",
        62 => "t#,##0.00",
        67 => "t0%",
        68 => "t0.00%",
        69 => "t# ?/?",
        70 => "t# ??/??",
        71 => "ว/ด/ปปปป",
        72 => "ว-ดดด-ปป",
        73 => "ว-ดดด",
        74 => "ดดด-ปป",
        75 => "ช:นน",
        76 => "ช:นน:ทท",
        77 => "ว/ด/ปปปป ช:นน",
        78 => "นน:ทท",
        79 => "[ช]:นน:ทท",
        80 => "นน:ทท.0",
        81 => "d/m/bb",
        _ => return None,
    })
}

/// `cached`, a number as the cell stores it, rendered under `number_format` as Excel shows it;
/// `None` when the format is not one KatWork renders, the section the value takes shows
/// nothing, or `cached` is not a number.
pub fn display(cached: &str, number_format: &str) -> Option<String> {
    let value = number(cached)?;
    let exact = decimal(cached)?;
    let sections = parse(number_format)?;
    if sections.iter().any(|section| section.condition.is_some()) {
        return None;
    }
    let section = &sections[number_section(&sections, value)?];
    // One section shows a negative number with a minus sign in front; with more, the negative
    // section shows its magnitude and writes its own signs.
    let sign = if value < 0.0 && sections.len() == 1 {
        "-"
    } else {
        ""
    };
    let shown = render(section, exact.abs())?;
    (!shown.is_empty()).then(|| format!("{sign}{shown}"))
}

/// `text`, a text cell's value, under `number_format`: its text section with `@` replaced by the
/// text, or the text as it is when the format has no text section; `None` when the text section
/// shows none of the text or holds codes KatWork does not render.
pub fn display_text(text: &str, number_format: &str) -> Option<String> {
    let sections = parse(number_format)?;
    let Some(section) = text_section(&sections) else {
        return Some(text.to_owned());
    };
    if !section.has(|token| *token == Token::Text) {
        return None;
    }
    let mut out = String::new();
    for token in &section.tokens {
        match token {
            Token::Text => out.push_str(text),
            Token::Digit(_) => return None,
            token => out.push_str(&literal(token)?),
        }
    }
    Some(out)
}

/// Whether the section of `number_format` that `cached` takes shows nothing of it, so a person
/// sees none of the value: the section is blank (colours, conditions, locales and padding
/// only), renders to blank (zero under `#,###`), or shows only literals in place of a number
/// that is not zero (a literal standing for zero, as accounting formats' `-`, shows the zero).
/// A number takes its section by condition or sign as `display` does, text takes the fourth
/// section, a boolean is judged as its number (1 or 0) and as text, an error as text. A code that
/// does not parse, has more than four sections, or has no section for the value hides it.
pub fn section_is_empty(cached: &str, kind: CellKind, number_format: &str) -> bool {
    let Some(sections) = parse(number_format) else {
        return true;
    };
    match kind {
        CellKind::Number => {
            number(cached).is_some_and(|value| number_hidden(&sections, value, decimal(cached)))
        }
        CellKind::Text => text_hidden(&sections),
        CellKind::Bool => {
            let readings: &[i64] = match cached {
                "TRUE" | "1" => &[1],
                "FALSE" | "0" => &[0],
                _ => &[0, 1],
            };
            text_hidden(&sections)
                || readings.iter().any(|&reading| {
                    number_hidden(&sections, reading as f64, Some(Decimal::from(reading)))
                })
        }
        CellKind::Error => text_hidden(&sections),
    }
}

/// Whether `number_format` shows numbers as dates or times: its first section holds a date or
/// time code (`d`, `m`, `y`, `h`, `s`, `AM/PM`, `[h]`, a Buddhist-era `b`, an era `e` or `g`, or
/// the Thai built-in codes' letters).
pub fn is_date_format(number_format: &str) -> bool {
    parse(number_format).is_some_and(|sections| sections[0].has(|token| *token == Token::Date))
}

/// The day an Excel serial number names (its whole part; the fraction is the time of day). In the
/// 1900 system serial 1 is 1900-01-01 and serial 60 is the 29 February 1900 that Excel counts and
/// that never was (`None`); in the 1904 system serial 0 is 1904-01-01. `None` past 9999-12-31,
/// for a negative serial, the 1900 system's serial 0, and anything not finite.
pub fn serial_to_date(serial: f64, date1904: bool) -> Option<NaiveDate> {
    if !serial.is_finite() || serial < 0.0 || serial.floor() > LAST_SERIAL {
        return None;
    }
    let days = serial.floor() as u64;
    let date = if date1904 {
        NaiveDate::from_ymd_opt(1904, 1, 1)?.checked_add_days(Days::new(days))?
    } else {
        match days {
            0 | 60 => return None,
            1..=59 => NaiveDate::from_ymd_opt(1899, 12, 31)?.checked_add_days(Days::new(days))?,
            _ => NaiveDate::from_ymd_opt(1899, 12, 30)?.checked_add_days(Days::new(days))?,
        }
    };
    (date.year() <= 9999).then_some(date)
}

/// The Excel serial number of `date`, as `serial_to_date` counts days (Excel's 29 February 1900
/// included in the 1900 system), its time of day the fraction. `None` before the system's first
/// day (1900-01-01, or 1904-01-01 in the 1904 system) and past 9999-12-31.
pub fn date_to_serial(date: NaiveDateTime, date1904: bool) -> Option<f64> {
    let day = date.date();
    if day.year() > 9999 {
        return None;
    }
    let (epoch, first) = if date1904 {
        (NaiveDate::from_ymd_opt(1904, 1, 1)?, 0)
    } else if day <= NaiveDate::from_ymd_opt(1900, 2, 28)? {
        (NaiveDate::from_ymd_opt(1899, 12, 31)?, 1)
    } else {
        (NaiveDate::from_ymd_opt(1899, 12, 30)?, 1)
    };
    let days = day.signed_duration_since(epoch).num_days();
    if days < first {
        return None;
    }
    let time = date.time();
    let seconds = f64::from(time.num_seconds_from_midnight()) + f64::from(time.nanosecond()) / 1e9;
    Some(days as f64 + seconds / 86_400.0)
}

/// A number as SpreadsheetML stores it: XML Schema's `double` written without `INF` or `NaN`,
/// white space around it read past, and finite. `None` for anything else.
pub(crate) fn number(text: &str) -> Option<f64> {
    let text = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    let bytes = text.as_bytes();
    let digits = |at: &mut usize| {
        let start = *at;
        while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
            *at += 1;
        }
        *at - start
    };
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut mantissa = digits(&mut at);
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        mantissa += digits(&mut at);
    }
    if mantissa == 0 {
        return None;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        if digits(&mut at) == 0 {
            return None;
        }
    }
    if at != bytes.len() {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// The exact decimal value of a stored number, when a `Decimal` holds it.
fn decimal(text: &str) -> Option<Decimal> {
    let text = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    let text = text.strip_prefix('+').unwrap_or(text);
    if text.contains(['e', 'E']) {
        Decimal::from_scientific(text).ok()
    } else {
        Decimal::from_str(text).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placeholder {
    Zero,
    Hash,
    Question,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// Text shown as it is: quoted, escaped, or a character Excel shows unquoted.
    Literal(String),
    Digit(Placeholder),
    Point,
    Comma,
    Percent,
    /// `@`: the cell's text.
    Text,
    General,
    /// A date or time code.
    Date,
    /// `E+` or `E-`: scientific notation.
    Exponent,
    /// `/`, which with digit placeholders around it makes a fraction.
    Slash,
    /// `_x`: a space as wide as `x`.
    Space,
    /// `*x`: `x` repeated to fill the cell.
    Fill(char),
    /// `[Red]`, `[Color7]`.
    Colour,
    /// `[$symbol-lcid]`: the currency symbol is shown; a number system other than Latin digits
    /// (the locale id's top byte) is not rendered here.
    Locale {
        symbol: String,
        native_digits: bool,
    },
    /// The `t` of the Thai built-in codes, which asks for Thai digits.
    ThaiDigits,
    /// A bracket code KatWork does not render (`[DBNum1]` ...).
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Section {
    condition: Option<(Op, f64)>,
    tokens: Vec<Token>,
}

impl Section {
    fn has(&self, test: impl Fn(&Token) -> bool) -> bool {
        self.tokens.iter().any(test)
    }

    /// Whether the section shows a number's value: a digit placeholder, `General`, a date or time
    /// code, scientific notation, or `@`.
    fn shows_number(&self) -> bool {
        self.has(|token| {
            matches!(
                token,
                Token::Digit(_) | Token::General | Token::Date | Token::Exponent | Token::Text
            )
        })
    }

    /// Whether the section shows nothing at all: colours, locales without a symbol, padding,
    /// white space and codes that only change how the rest is drawn.
    fn is_blank(&self) -> bool {
        self.tokens.iter().all(|token| match token {
            Token::Literal(text) => text.chars().all(char::is_whitespace),
            Token::Fill(c) => c.is_whitespace(),
            Token::Locale { symbol, .. } => symbol.chars().all(char::is_whitespace),
            Token::Space | Token::Colour | Token::ThaiDigits | Token::Unknown => true,
            _ => false,
        })
    }
}

/// The sections of a format code, or `None` when it does not parse: an unclosed quote or
/// bracket, an escape, padding or fill with no character after it, a condition that is not one,
/// two conditions in one section, or more than four sections.
fn parse(code: &str) -> Option<Vec<Section>> {
    let mut sections = Vec::new();
    for raw in split_sections(code)? {
        sections.push(tokenize(raw)?);
        if sections.len() > MAX_SECTIONS {
            return None;
        }
    }
    Some(sections)
}

/// `code` split at its semicolons, outside quotes, brackets and escapes.
fn split_sections(code: &str) -> Option<Vec<&str>> {
    let mut sections = Vec::new();
    let (mut start, mut chars) = (0, code.char_indices());
    while let Some((at, c)) = chars.next() {
        match c {
            ';' => {
                sections.push(&code[start..at]);
                start = at + 1;
            }
            '"' => {
                chars.find(|(_, c)| *c == '"')?;
            }
            '[' => {
                chars.find(|(_, c)| *c == ']')?;
            }
            '\\' | '_' | '*' => {
                chars.next()?;
            }
            _ => {}
        }
    }
    sections.push(&code[start..]);
    Some(sections)
}

fn tokenize(raw: &str) -> Option<Section> {
    // The Thai built-in date codes write their letters unquoted; elsewhere a Thai letter is text.
    let thai_dates = raw.chars().any(|c| THAI_DATE_LETTERS.contains(&c))
        && raw
            .chars()
            .all(|c| THAI_DATE_LETTERS.contains(&c) || "/-:. []0".contains(c));
    let mut section = Section::default();
    let chars: Vec<char> = raw.chars().collect();
    let mut at = 0;
    let push = |section: &mut Section, token: Token| section.tokens.push(token);
    while at < chars.len() {
        let c = chars[at];
        at += 1;
        match c {
            '"' => {
                let close = chars[at..].iter().position(|&c| c == '"')?;
                push(
                    &mut section,
                    Token::Literal(chars[at..at + close].iter().collect()),
                );
                at += close + 1;
            }
            '\\' => {
                push(&mut section, Token::Literal(chars.get(at)?.to_string()));
                at += 1;
            }
            '_' => {
                chars.get(at)?;
                push(&mut section, Token::Space);
                at += 1;
            }
            '*' => {
                push(&mut section, Token::Fill(*chars.get(at)?));
                at += 1;
            }
            '[' => {
                let close = chars[at..].iter().position(|&c| c == ']')?;
                let inner: String = chars[at..at + close].iter().collect();
                at += close + 1;
                bracket(&inner, &mut section)?;
            }
            '0' => push(&mut section, Token::Digit(Placeholder::Zero)),
            '#' => push(&mut section, Token::Digit(Placeholder::Hash)),
            '?' => push(&mut section, Token::Digit(Placeholder::Question)),
            '.' => push(&mut section, Token::Point),
            ',' => push(&mut section, Token::Comma),
            '%' => push(&mut section, Token::Percent),
            '@' => push(&mut section, Token::Text),
            '/' => push(&mut section, Token::Slash),
            'E' | 'e' if matches!(chars.get(at), Some('+' | '-')) => {
                at += 1;
                push(&mut section, Token::Exponent);
            }
            'G' | 'g' if starts_with_ignore_case(&chars[at - 1..], "general") => {
                at += "general".len() - 1;
                push(&mut section, Token::General);
            }
            'A' | 'a' if starts_with_ignore_case(&chars[at - 1..], "am/pm") => {
                at += "am/pm".len() - 1;
                push(&mut section, Token::Date);
            }
            'A' | 'a' if starts_with_ignore_case(&chars[at - 1..], "a/p") => {
                at += "a/p".len() - 1;
                push(&mut section, Token::Date);
            }
            'y' | 'Y' | 'm' | 'M' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'e' | 'E' | 'g' | 'G'
            | 'b' | 'B' => push(&mut section, Token::Date),
            't' | 'T' if section.tokens.is_empty() => push(&mut section, Token::ThaiDigits),
            c if thai_dates && THAI_DATE_LETTERS.contains(&c) => push(&mut section, Token::Date),
            c => push(&mut section, Token::Literal(c.to_string())),
        }
    }
    Some(section)
}

fn starts_with_ignore_case(chars: &[char], word: &str) -> bool {
    let mut rest = chars.iter();
    word.chars().all(|expected| {
        rest.next()
            .is_some_and(|c| c.to_ascii_lowercase() == expected)
    })
}

/// A bracketed code: a colour, a condition, an elapsed time, a locale, or one KatWork does not
/// render. `None` for a second condition in one section or a condition that does not parse.
fn bracket(inner: &str, section: &mut Section) -> Option<()> {
    const COLOURS: [&str; 8] = [
        "black", "blue", "cyan", "green", "magenta", "red", "white", "yellow",
    ];
    let lower = inner.to_ascii_lowercase();
    let token = if COLOURS.contains(&lower.as_str())
        || lower
            .strip_prefix("color")
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=56).contains(&n))
    {
        Token::Colour
    } else if inner.starts_with(['<', '>', '=']) {
        if section.condition.is_some() {
            return None;
        }
        section.condition = Some(condition(inner)?);
        return Some(());
    } else if !lower.is_empty()
        && (lower.chars().all(|c| c == 'h')
            || lower.chars().all(|c| c == 'm')
            || lower.chars().all(|c| c == 's')
            || inner.chars().all(|c| THAI_DATE_LETTERS.contains(&c)))
    {
        Token::Date
    } else if let Some(locale) = inner.strip_prefix('$') {
        let (symbol, id) = locale.split_once('-').unwrap_or((locale, ""));
        let native_digits = match id {
            "" => false,
            id if id.len() <= 8 && id.bytes().all(|b| b.is_ascii_hexdigit()) => {
                u32::from_str_radix(id, 16).ok()? >> 24 != 0
            }
            // `[$-x-sysdate]` and other named locales.
            _ => {
                section.tokens.push(Token::Unknown);
                return Some(());
            }
        };
        Token::Locale {
            symbol: symbol.to_owned(),
            native_digits,
        }
    } else {
        Token::Unknown
    };
    section.tokens.push(token);
    Some(())
}

fn condition(inner: &str) -> Option<(Op, f64)> {
    let (op, rest) = [
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("<>", Op::Ne),
        ("<", Op::Lt),
        (">", Op::Gt),
        ("=", Op::Eq),
    ]
    .into_iter()
    .find_map(|(text, op)| inner.strip_prefix(text).map(|rest| (op, rest)))?;
    Some((op, number(rest)?))
}

impl Op {
    fn holds(self, value: f64, bound: f64) -> bool {
        match self {
            Op::Lt => value < bound,
            Op::Le => value <= bound,
            Op::Gt => value > bound,
            Op::Ge => value >= bound,
            Op::Eq => value == bound,
            Op::Ne => value != bound,
        }
    }
}

/// The index of the section a number takes, or `None` when no section fits it (Excel then fills
/// the cell with `#`). With a condition on the first section, the first two sections are tried
/// in order, each taking the number when its condition holds or it has none, then the third;
/// otherwise the sign decides: one section for every number; two for positive and zero, then
/// negative; three or four for positive, negative, then zero.
fn number_section(sections: &[Section], value: f64) -> Option<usize> {
    if sections[0].condition.is_some() {
        for (at, section) in sections.iter().enumerate().take(2) {
            match section.condition {
                Some((op, bound)) if !op.holds(value, bound) => continue,
                _ => return Some(at),
            }
        }
        return (sections.len() > 2).then_some(2);
    }
    Some(match sections.len() {
        1 => 0,
        _ if value > 0.0 => 0,
        _ if value < 0.0 => 1,
        2 => 0,
        _ => 2,
    })
}

/// The text section: the fourth, or a last section that holds `@`.
fn text_section(sections: &[Section]) -> Option<&Section> {
    match sections.len() {
        MAX_SECTIONS => sections.last(),
        _ => sections
            .last()
            .filter(|section| section.has(|token| *token == Token::Text)),
    }
}

fn text_hidden(sections: &[Section]) -> bool {
    text_section(sections).is_some_and(|section| !section.has(|token| *token == Token::Text))
}

/// Whether a number shows nothing of itself under `sections`; `exact` is its decimal value when
/// a `Decimal` holds it.
fn number_hidden(sections: &[Section], value: f64, exact: Option<Decimal>) -> bool {
    let Some(at) = number_section(sections, value) else {
        return true;
    };
    let section = &sections[at];
    if section.shows_number() {
        // Digits that render to nothing (zero under `#,###`) show nothing either.
        return exact
            .and_then(|exact| render(section, exact.abs()))
            .is_some_and(|shown| shown.trim().is_empty());
    }
    section.is_blank() || value != 0.0
}

/// A literal token as it shows; `None` for a token that is not one.
fn literal(token: &Token) -> Option<String> {
    Some(match token {
        Token::Literal(text) => text.clone(),
        Token::Percent => "%".into(),
        Token::Comma => ",".into(),
        Token::Point => ".".into(),
        Token::Locale { symbol, .. } => symbol.clone(),
        Token::Space | Token::Fill(_) | Token::Colour => String::new(),
        _ => return None,
    })
}

/// `magnitude` (not negative) under `section`, padding left out and the result trimmed of
/// spaces; `None` when the section holds a code KatWork does not render or no digit
/// placeholder, or its digits are not one run of placeholders, grouping commas and a point.
fn render(section: &Section, magnitude: Decimal) -> Option<String> {
    let tokens = &section.tokens;
    let unrendered = |token: &Token| {
        matches!(
            token,
            Token::General
                | Token::Date
                | Token::Exponent
                | Token::Slash
                | Token::Text
                | Token::ThaiDigits
                | Token::Unknown
                | Token::Locale {
                    native_digits: true,
                    ..
                }
        )
    };
    if tokens.iter().any(unrendered) {
        return None;
    }
    let is_digit = |token: &Token| matches!(token, Token::Digit(_));
    let mut first = tokens.iter().position(is_digit)?;
    let last = tokens.iter().rposition(is_digit)?;
    // The number's own tokens: its placeholders, a point right before them (`.00`), and the
    // commas and point right after them.
    if first > 0 && tokens[first - 1] == Token::Point {
        first -= 1;
    }
    let mut end = last + 1;
    while matches!(tokens.get(end), Some(Token::Comma | Token::Point)) {
        end += 1;
    }
    let span = &tokens[first..end];
    if span
        .iter()
        .any(|token| !matches!(token, Token::Digit(_) | Token::Comma | Token::Point))
        || span.iter().filter(|token| **token == Token::Point).count() > 1
    {
        return None;
    }
    let point = span.iter().position(|token| *token == Token::Point);
    let (integer, fraction) = match point {
        Some(point) => (&span[..point], &span[point + 1..]),
        None => (span, &span[span.len()..]),
    };
    // A comma between two integer placeholders groups thousands; any other comma after a
    // placeholder divides by 1,000.
    let (mut grouping, mut thousands) = (false, 0);
    for (at, token) in integer.iter().enumerate() {
        if *token == Token::Comma {
            if integer[at..].iter().any(is_digit) {
                grouping = true;
            } else {
                thousands += 1;
            }
        }
    }
    for (at, token) in fraction.iter().enumerate() {
        if *token == Token::Comma {
            if fraction[at..].iter().any(is_digit) {
                return None;
            }
            thousands += 1;
        }
    }
    let placeholders = |zone: &[Token]| -> Vec<Placeholder> {
        zone.iter()
            .filter_map(|token| match token {
                Token::Digit(placeholder) => Some(*placeholder),
                _ => None,
            })
            .collect()
    };
    let (integer, fraction) = (placeholders(integer), placeholders(fraction));
    let decimals = u32::try_from(fraction.len()).ok().filter(|&d| d <= 28)?;
    let mut value =
        magnitude.round_sf_with_strategy(PRECISION, RoundingStrategy::MidpointAwayFromZero)?;
    for _ in tokens.iter().filter(|token| **token == Token::Percent) {
        value = value.checked_mul(Decimal::ONE_HUNDRED)?;
    }
    for _ in 0..thousands {
        value = value.checked_div(Decimal::ONE_THOUSAND)?;
    }
    let mut rounded =
        value.round_dp_with_strategy(decimals, RoundingStrategy::MidpointAwayFromZero);
    // A value too large for a decimal at this many decimals keeps fewer: the ones it cannot
    // hold are zeros, since it was rounded to 15 significant digits.
    rounded.rescale(decimals);
    let text = rounded.to_string();
    let (whole, part) = text.split_once('.').unwrap_or((&text, ""));
    let part = format!("{part:0<width$}", width = fraction.len());
    let whole = if whole == "0" { "" } else { whole };
    let mut out = String::new();
    for token in &tokens[..first] {
        out.push_str(&literal(token)?);
    }
    out.push_str(&fill_integer(&integer, whole, grouping));
    if point.is_some() {
        out.push('.');
        out.push_str(&fill_fraction(&fraction, &part));
    }
    for token in &tokens[end..] {
        out.push_str(&literal(token)?);
    }
    Some(out.trim_matches(' ').to_owned())
}

/// The integer digits laid into their placeholders from the right, the leftmost taking any left
/// over: a missing digit is `0` under `0`, a space under `?` and nothing under `#`. With no
/// integer placeholder (`.00`) every integer digit still shows, as Excel shows them.
fn fill_integer(placeholders: &[Placeholder], digits: &str, grouping: bool) -> String {
    if placeholders.is_empty() {
        return digits.to_owned();
    }
    let digits: Vec<char> = digits.chars().collect();
    let mut left = digits.len();
    let mut out = Vec::new();
    for (at, placeholder) in placeholders.iter().enumerate().rev() {
        if left > 0 {
            left -= 1;
            out.push(digits[left]);
        } else {
            match placeholder {
                Placeholder::Zero => out.push('0'),
                Placeholder::Question => out.push(' '),
                Placeholder::Hash => {}
            }
        }
        if at == 0 {
            while left > 0 {
                left -= 1;
                out.push(digits[left]);
            }
        }
    }
    if grouping {
        let mut grouped = Vec::new();
        let mut seen = 0;
        for c in out {
            if c.is_ascii_digit() {
                if seen > 0 && seen % 3 == 0 {
                    grouped.push(',');
                }
                seen += 1;
            }
            grouped.push(c);
        }
        out = grouped;
    }
    out.iter().rev().collect()
}

/// The decimal digits under their placeholders: trailing zeros are dropped under `#` and spaced
/// under `?`, back to the first other digit or `0` placeholder.
fn fill_fraction(placeholders: &[Placeholder], digits: &str) -> String {
    let mut out: Vec<Option<char>> = digits.chars().map(Some).collect();
    for (at, placeholder) in placeholders.iter().enumerate().rev() {
        let Some(digit) = out.get_mut(at) else {
            continue;
        };
        if *digit != Some('0') || *placeholder == Placeholder::Zero {
            break;
        }
        *digit = (*placeholder == Placeholder::Question).then_some(' ');
    }
    out.into_iter().flatten().collect()
}
