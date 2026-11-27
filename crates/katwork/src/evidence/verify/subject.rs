//! What a figure or a date is about: the claim's subject words and periods, the source text
//! they are looked up in, and the subject rule a number or date check must meet before it can
//! be `Supported` (ADR-0007: a right number with a wrong entity or period is not support).
use std::collections::HashSet;

use katwork_thai::{
    Ctx, DateValue, Found, NumberRole, PeriodCmp, TooLong, compare_periods, find_dates,
    find_numbers, match_key, normalize_th,
};
use serde_json::{Value, json};

use super::{CheckVerdict, ResolvedRef};
use crate::evidence::instruction::instruction_like;

/// Words dropped from a claim's subject, compared whole after `normalize_th`.
pub const STOP_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "of",
    "in",
    "on",
    "at",
    "for",
    "to",
    "and",
    "or",
    "is",
    "are",
    "was",
    "were",
    "be",
    "by",
    "with",
    "as",
    "its",
    "their",
    "ของ",
    "ที่",
    "และ",
    "หรือ",
    "ใน",
    "เป็น",
    "คือ",
    "ได้",
    "มี",
    "จาก",
    "โดย",
    "กับ",
    "แก่",
    "แห่ง",
    "ซึ่ง",
    "นั้น",
    "นี้",
    "ว่า",
    "จำนวน",
];

/// An operation's own words, dropped from a derived claim's subject: the operands' sources do
/// not state the total or the change the claim computes.
pub const OP_WORDS: &[&str] = &[
    "รวม",
    "ทั้งหมด",
    "total",
    "sum",
    "ผลต่าง",
    "difference",
    "สัดส่วน",
    "ร้อยละ",
    "share",
    "percent",
    "เพิ่มขึ้น",
    "ลดลง",
    "increase",
    "decrease",
    "change",
];

/// The unit words `find_numbers` reads as a figure's unit but leaves outside its span, longest
/// first, with the sides of the figure they are read on: `(word, after, before)`. A claim's
/// `1,250 ล้านบาท` is cut out whole, so `บาท` is not a subject word.
pub const UNIT_WORDS: &[(&str, bool, bool)] = &[
    ("เปอร์เซ็นต์", true, false),
    ("เปอร์เซนต์", true, false),
    ("บาทถ้วน", true, false),
    ("per cent", true, false),
    ("percent", true, false),
    ("ร้อยละ", false, true),
    ("baht", true, false),
    ("บาท", true, false),
    ("THB", true, true),
    ("USD", true, true),
];

/// The confidence a figure or a date needs on each side (the crate's clean-paragraph rule).
pub(super) const MIN_CONF: f32 = 0.8;

/// One subject word, as `normalize_th` writes it. `alone` is true when whitespace, punctuation,
/// a cut figure or the text's end lies on both sides of it in the claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    pub alone: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    pub words: Vec<Term>,
    pub periods: Vec<Found<DateValue>>,
}

/// The subject of a claim text: every date `find_dates` finds is cut out and goes to `periods`;
/// every `Quantity` is cut out with the unit words beside it; the rest is split into runs of
/// Thai and of other letters and digits, and the stop words are dropped. Nothing else is dropped:
/// "B" of "Entity B" and "ข" of "ฝ่าย ข" stay.
pub fn terms(claim_text: &str, ctx: &Ctx) -> Result<Subject, TooLong> {
    let periods = find_dates(claim_text, ctx)?;
    let numbers = find_numbers(claim_text, ctx)?;
    let chars: Vec<char> = claim_text.chars().collect();
    let mut cut = vec![false; chars.len()];
    let mut mark = |start: usize, end: usize| {
        for c in cut.iter_mut().take(end.min(chars.len())).skip(start) {
            *c = true;
        }
    };
    for d in &periods {
        mark(d.start as usize, d.end as usize);
    }
    for n in numbers
        .iter()
        .filter(|n| n.value.role == NumberRole::Quantity)
    {
        let (start, end) = if n.value.unit.is_some() {
            with_unit_words(&chars, n.start as usize, n.end as usize)
        } else {
            (n.start as usize, n.end as usize)
        };
        mark(start, end);
    }
    let rest: String = chars
        .iter()
        .zip(&cut)
        .map(|(c, cut)| if *cut { ' ' } else { *c })
        .collect();
    let words = runs(&rest)
        .into_iter()
        .filter(|t| !STOP_WORDS.contains(&t.text.as_str()))
        .collect();
    Ok(Subject { words, periods })
}

/// `start..end` widened over one unit word right after it (or, failing that, right before it),
/// across whitespace.
fn with_unit_words(chars: &[char], start: usize, end: usize) -> (usize, usize) {
    let after = end
        + chars[end..]
            .iter()
            .take_while(|c| c.is_whitespace())
            .count();
    for (word, is_after, _) in UNIT_WORDS {
        let word: Vec<char> = word.chars().collect();
        if *is_after && word_at(chars, after, &word) {
            return (start, after + word.len());
        }
    }
    let before = start
        - chars[..start]
            .iter()
            .rev()
            .take_while(|c| c.is_whitespace())
            .count();
    for (word, _, is_before) in UNIT_WORDS {
        let word: Vec<char> = word.chars().collect();
        if *is_before && before >= word.len() && word_at(chars, before - word.len(), &word) {
            return (before - word.len(), end);
        }
    }
    (start, end)
}

/// `word` sits at `at` in `chars` (ASCII case ignored); a Latin word must not run on into a
/// letter or a digit on either side.
fn word_at(chars: &[char], at: usize, word: &[char]) -> bool {
    let Some(here) = chars.get(at..at + word.len()) else {
        return false;
    };
    if !here
        .iter()
        .zip(word)
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return false;
    }
    let latin = word.first().is_some_and(char::is_ascii_alphabetic);
    let runs_on = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric() && !is_thai(*c));
    !latin || !(runs_on(chars.get(at + word.len())) || at > 0 && runs_on(chars.get(at - 1)))
}

/// A Thai letter, vowel or mark (Thai digits are ASCII after `normalize_th`; `฿` is a sign).
fn is_thai(c: char) -> bool {
    ('\u{0E01}'..='\u{0E4E}').contains(&c) && c != '\u{0E3F}'
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Thai,
    Word,
    Gap,
}

fn class(c: char) -> Class {
    if is_thai(c) {
        Class::Thai
    } else if c.is_alphanumeric() {
        Class::Word
    } else {
        Class::Gap
    }
}

/// `normalize_th(text)` split into runs of Thai and runs of other letters and digits; a Thai run
/// is `alone` when a gap or an end lies on both sides of it.
fn runs(text: &str) -> Vec<Term> {
    let chars: Vec<char> = normalize_th(text).text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let kind = class(chars[i]);
        let start = i;
        while i < chars.len() && class(chars[i]) == kind {
            i += 1;
        }
        if kind == Class::Gap {
            continue;
        }
        let gap = |at: Option<&char>| at.is_none_or(|c| class(*c) == Class::Gap);
        let alone = gap(start.checked_sub(1).and_then(|p| chars.get(p))) && gap(chars.get(i));
        out.push(Term {
            text: chars[start..i].iter().collect(),
            alone,
        });
    }
    out
}

/// The texts of a ref a subject is looked up in, each with whether a date in it may contradict
/// a claim: the quote and the cell's row and column headers may; the header unit, the sheet name
/// and the unit context may only agree. A cell fact that `instruction_like` flags is left out.
pub(super) fn fields<'a>(r: &'a ResolvedRef) -> Vec<(&'a str, bool)> {
    let ResolvedRef::Ok {
        exact,
        unit_context,
        cell,
        sheet,
        ..
    } = r
    else {
        return vec![];
    };
    let mut out = vec![(exact.as_str(), true)];
    let mut push = |text: &'a Option<String>, strong: bool| {
        if let Some(text) = text.as_deref().filter(|t| instruction_like(t).is_none()) {
            out.push((text, strong));
        }
    };
    if let Some(cell) = cell {
        push(&cell.row_header, true);
        push(&cell.col_header, true);
        push(&cell.header_unit, false);
    }
    push(sheet, false);
    push(unit_context, false);
    out
}

/// The cited quote, plus for a cell its row and column headers, its header unit, its sheet
/// name and the unit context, each left out when `instruction_like` flags it; joined by ` | `,
/// which no word or figure reads across.
pub fn source_text(r: &ResolvedRef) -> String {
    fields(r)
        .iter()
        .map(|(text, _)| *text)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// A source as the subject rule reads it: its words with its quantities cut out, and its dates.
pub(super) struct Source {
    latin: HashSet<String>,
    thai: HashSet<String>,
    key: String,
    /// Each date with whether it may contradict (see [`fields`]).
    pub dates: Vec<(Found<DateValue>, bool)>,
}

impl Source {
    /// Each field is scanned on its own, so no figure or date is read across two fields.
    pub fn read(fields: &[(&str, bool)], ctx: &Ctx) -> Result<Source, TooLong> {
        let mut cut_fields = Vec::with_capacity(fields.len());
        let mut dates = Vec::new();
        for (text, strong) in fields {
            dates.extend(find_dates(text, ctx)?.into_iter().map(|d| (d, *strong)));
            let numbers = find_numbers(text, ctx)?;
            let mut chars: Vec<char> = text.chars().collect();
            for n in numbers
                .iter()
                .filter(|n| n.value.role == NumberRole::Quantity)
            {
                let end = (n.end as usize).min(chars.len());
                for c in &mut chars[(n.start as usize).min(end)..end] {
                    *c = ' ';
                }
            }
            cut_fields.push(chars.into_iter().collect::<String>());
        }
        let joined = cut_fields.join(" | ");
        let mut latin = HashSet::new();
        let mut thai = HashSet::new();
        for t in runs(&joined) {
            if t.text.starts_with(is_thai) {
                thai.insert(t.text);
            } else {
                latin.insert(t.text);
            }
        }
        Ok(Source {
            latin,
            thai,
            key: match_key(&joined).text,
            dates,
        })
    }

    /// The source joined with another (a derived claim reads the union of its operands).
    pub fn extend(&mut self, other: Source) {
        self.latin.extend(other.latin);
        self.thai.extend(other.thai);
        self.key.push('|');
        self.key.push_str(&other.key);
        self.dates.extend(other.dates);
    }

    /// A Latin or digit word equals a whole such word of the source; a Thai word of at most 3
    /// code points that stood alone equals a whole Thai run; a longer Thai word occurs in the
    /// source's match key.
    fn has(&self, term: &Term) -> bool {
        if !term.text.starts_with(is_thai) {
            return self.latin.contains(&term.text);
        }
        if term.alone && term.text.chars().count() <= 3 {
            return self.thai.contains(&term.text);
        }
        self.key.contains(&match_key(&term.text).text)
    }
}

/// How a date or period of a claim stands against a source's dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Against {
    Same,
    /// The claim's or a comparable source date was read below [`MIN_CONF`].
    LowConf,
    /// The source's one comparable date, from a field that may contradict, is another one.
    Different,
    /// Several comparable dates and none the same, or a different one from a field that may
    /// only agree.
    Several,
    KindDiffers,
    Ambiguous,
    /// No comparable source date.
    Absent,
}

/// Months a period spans, for the dates whose granularity `compare_periods` does not tell apart:
/// a quarter and a fiscal year are both `Period`s.
fn months(v: &DateValue) -> Option<i32> {
    use chrono::Datelike;
    match v {
        DateValue::Year { .. } => Some(12),
        DateValue::Period { start, end, .. } => {
            Some((end.year() - start.year()) * 12 + end.month() as i32 - start.month() as i32 + 1)
        }
        _ => None,
    }
}

/// `compare_periods`, with periods of different lengths (a quarter and a year) incomparable.
pub(super) fn compare(a: &DateValue, b: &DateValue) -> PeriodCmp {
    match (months(a), months(b)) {
        (Some(x), Some(y)) if x != y => PeriodCmp::Incomparable,
        _ => compare_periods(a, b),
    }
}

/// The first rule that applies: a source date read with [`MIN_CONF`] that is the same; a claim
/// date or a comparable source date read below it; one comparable date, from a field that may
/// contradict, that is different; a kind that differs; an ambiguous kind; several (or only
/// weak) different dates; none comparable.
pub(super) fn against(claim: &Found<DateValue>, source: &[(Found<DateValue>, bool)]) -> Against {
    if claim.conf < MIN_CONF {
        return Against::LowConf;
    }
    let comparable: Vec<(PeriodCmp, &Found<DateValue>, bool)> = source
        .iter()
        .map(|(d, strong)| (compare(&claim.value, &d.value), d, *strong))
        .filter(|(cmp, _, _)| *cmp != PeriodCmp::Incomparable)
        .collect();
    if comparable
        .iter()
        .any(|(cmp, d, _)| *cmp == PeriodCmp::Same && d.conf >= MIN_CONF)
    {
        return Against::Same;
    }
    if comparable.iter().any(|(_, d, _)| d.conf < MIN_CONF) {
        return Against::LowConf;
    }
    let any = |want: PeriodCmp| comparable.iter().any(|(cmp, _, _)| *cmp == want);
    match comparable.as_slice() {
        [] => Against::Absent,
        [(PeriodCmp::Different, _, true)] => Against::Different,
        _ if any(PeriodCmp::KindDiffers) => Against::KindDiffers,
        _ if any(PeriodCmp::Ambiguous) => Against::Ambiguous,
        _ => Against::Several,
    }
}

/// Why the subject rule failed: the verdict and code a check gives in place of `Supported`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Miss {
    pub verdict: CheckVerdict,
    pub code: &'static str,
    pub detail: Value,
}

fn miss(verdict: CheckVerdict, code: &'static str, detail: Value) -> Miss {
    Miss {
        verdict,
        code,
        detail,
    }
}

/// The subject rule: every word is in the source, then every period agrees with it. A subject
/// with no word and no period is never met (`bare` is false only for a derived claim, whose
/// operation is its subject): a bare figure says nothing to check the source's subject against.
pub(super) fn check(
    words: &[Term],
    periods: &[Found<DateValue>],
    source: &Source,
    bare_fails: bool,
) -> Result<(), Miss> {
    use CheckVerdict::{Contradicted, Inconclusive};
    if bare_fails && words.is_empty() && periods.is_empty() {
        return Err(miss(
            Inconclusive,
            "subject_not_in_source",
            json!({"reason": "claim_has_no_subject"}),
        ));
    }
    if let Some(word) = words.iter().find(|w| !source.has(w)) {
        return Err(miss(
            Inconclusive,
            "subject_not_in_source",
            json!({"missing": word.text}),
        ));
    }
    for period in periods {
        let detail = json!({"period": period.raw});
        let fail = match against(period, &source.dates) {
            Against::Same => continue,
            Against::LowConf => miss(Inconclusive, "date_ambiguous", detail),
            Against::Different => miss(Contradicted, "period_mismatch", detail),
            Against::KindDiffers => miss(Inconclusive, "period_kind_differs", detail),
            Against::Ambiguous | Against::Several => miss(Inconclusive, "period_ambiguous", detail),
            Against::Absent => miss(Inconclusive, "subject_not_in_source", detail),
        };
        return Err(fail);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        let ctx = crate::evidence::scan_ctx();
        terms(text, &ctx)
            .unwrap()
            .words
            .into_iter()
            .map(|t| t.text)
            .collect()
    }

    #[test]
    fn figures_dates_and_unit_words_are_cut_and_short_words_stay() {
        assert_eq!(words("รายได้รวม 1,250 ล้านบาท"), ["รายได้รวม"]);
        assert_eq!(
            words("Entity B revenue 1,250 baht"),
            ["entity", "b", "revenue"]
        );
        assert_eq!(
            words("ฝ่าย ข งบประมาณ 1,250 ล้านบาท"),
            ["ฝ่าย", "ข", "งบประมาณ"]
        );
        assert_eq!(words("รายได้ปี 2567 1,250 ล้านบาท"), ["รายได้"]);
        assert_eq!(words("growth of USD 12 in the year"), ["growth", "year"]);
        assert_eq!(words("ร้อยละ 5 ของรายได้"), ["ของรายได้"]);
        // An ordinal is a word, not a cut figure.
        assert_eq!(words("วาระที่ 3 งบ 1,250 บาท"), ["วาระที่", "3", "งบ"]);
        // A unit word glued to another word is not cut from it.
        assert_eq!(words("12 bahtwise"), ["bahtwise"]);
    }

    /// Every unit word reads as the unit of a figure on each side it is listed for, so the cut
    /// never removes a word `find_numbers` did not count as the figure's unit.
    #[test]
    fn unit_words_round_trip_through_find_numbers() {
        let ctx = crate::evidence::scan_ctx();
        for (word, after, before) in UNIT_WORDS {
            let mut texts = vec![];
            if *after {
                texts.push(format!("1 {word}"));
            }
            if *before {
                texts.push(format!("{word} 1"));
            }
            for text in texts {
                let found = find_numbers(&text, &ctx).unwrap();
                assert!(
                    found.iter().any(|n| n.value.unit.is_some()
                        && n.raw == "1"
                        && n.value.role == NumberRole::Quantity),
                    "{text:?}: {found:?}"
                );
            }
        }
    }
}
