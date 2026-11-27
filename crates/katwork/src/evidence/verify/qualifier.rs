//! Qualifiers: negations and comparators must mean the same in the claim and around its quote,
//! so "อนุมัติงบประมาณ" quoted from "ไม่อนุมัติงบประมาณ" cannot pass (ADR-0007).
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use katwork_thai::{
    Ctx, MAX_SCAN_CODE_POINTS, NumberRole, TooLong, find_numbers, match_key, normalize_th,
};
use serde::Serialize;
use serde_json::json;

use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, ResolvedRef, resolved_refs,
};

/// Code points of block text read on each side of a quote.
const WINDOW: usize = 10;

/// Code points of block text the sentence reading always covers on each side of a quote in Thai
/// text, where sentences have no reliable end mark.
const THAI_MIN: usize = 30;

/// A run of at least this many code points before a space makes that space a Thai sentence end.
const THAI_SENTENCE_RUN: usize = 30;

/// Code points read past each window edge, so a lexicon entry that straddles an edge is read
/// whole: twice the longest entry, which leaves room for invisible characters inside it.
const REACH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Meaning {
    Neg,
    Le,
    Ge,
    Lt,
    Gt,
    Approx,
    Nearly,
}

impl Meaning {
    pub fn wire(self) -> &'static str {
        match self {
            Meaning::Neg => "neg",
            Meaning::Le => "le",
            Meaning::Ge => "ge",
            Meaning::Lt => "lt",
            Meaning::Gt => "gt",
            Meaning::Approx => "approx",
            Meaning::Nearly => "nearly",
        }
    }
}

/// An entry matched as a substring of `match_key(text)`: Thai words and comparison symbols.
struct Sub {
    text: &'static str,
    meaning: Meaning,
    /// In the plan's original list; the rest came with the founder decision of 2026-11-27.
    plan: bool,
    /// Not a qualifier right after one of these (`งบประมาณ` is "budget").
    not_after: &'static [&'static str],
    /// Not a qualifier right before one of these (`อย่าง` is "kind of", `งดงาม` "beautiful").
    not_before: &'static [&'static str],
    /// Only right after a digit (`5+`; spaces between them are ignored).
    after_digit: bool,
    /// Only right before a digit (`<-5`).
    before_digit: bool,
}

const fn sub(text: &'static str, meaning: Meaning, plan: bool) -> Sub {
    Sub {
        text,
        meaning,
        plan,
        not_after: &[],
        not_before: &[],
        after_digit: false,
        before_digit: false,
    }
}

const SUBSTRINGS: &[Sub] = &[
    sub("ไม่", Meaning::Neg, true),
    sub("มิได้", Meaning::Neg, true),
    sub("มิใช่", Meaning::Neg, true),
    sub("ห้าม", Meaning::Neg, false),
    sub("ปฏิเสธ", Meaning::Neg, false),
    Sub {
        not_before: &["งาม"],
        ..sub("งด", Meaning::Neg, false)
    },
    sub("ยกเลิก", Meaning::Neg, false),
    sub("ระงับ", Meaning::Neg, false),
    sub("ไร้", Meaning::Neg, false),
    sub("ปราศจาก", Meaning::Neg, false),
    Sub {
        not_before: &["ง"],
        ..sub("อย่า", Meaning::Neg, false)
    },
    sub("ไม่เกิน", Meaning::Le, true),
    sub("ไม่มากกว่า", Meaning::Le, true),
    sub("อย่างมาก", Meaning::Le, true),
    sub("ลงมา", Meaning::Le, false),
    sub("สูงสุด", Meaning::Le, false),
    sub("ไม่ต่ำกว่า", Meaning::Ge, true),
    sub("ไม่น้อยกว่า", Meaning::Ge, true),
    sub("อย่างน้อย", Meaning::Ge, true),
    sub("ขึ้นไป", Meaning::Ge, false),
    sub("ต่ำสุด", Meaning::Ge, false),
    sub("ต่ำกว่า", Meaning::Lt, true),
    sub("น้อยกว่า", Meaning::Lt, true),
    sub("ไม่ถึง", Meaning::Lt, true),
    sub("มากกว่า", Meaning::Gt, true),
    sub("สูงกว่า", Meaning::Gt, true),
    sub("กว่า", Meaning::Gt, true),
    sub("เกิน", Meaning::Gt, true),
    Sub {
        not_after: &["งบ"],
        ..sub("ประมาณ", Meaning::Approx, true)
    },
    sub("เกือบ", Meaning::Nearly, true),
    Sub {
        not_after: &["-", "="],
        ..sub(">", Meaning::Gt, false)
    },
    Sub {
        not_before: &["-"],
        ..sub("<", Meaning::Lt, false)
    },
    Sub {
        before_digit: true,
        ..sub("<-", Meaning::Lt, false)
    },
    sub(">=", Meaning::Ge, false),
    sub("<=", Meaning::Le, false),
    sub("≥", Meaning::Ge, false),
    sub("≤", Meaning::Le, false),
    Sub {
        after_digit: true,
        ..sub("+", Meaning::Ge, false)
    },
];

/// Entries matched as whole lowercased words or word sequences of `normalize_th(text)`, with
/// whether the plan's original list held them. `no` before a numbered `.` and words ending in
/// `n't` are handled in [`english_scan`].
const ENGLISH: &[(&str, Meaning, bool)] = &[
    ("not", Meaning::Neg, true),
    ("no", Meaning::Neg, true),
    ("never", Meaning::Neg, true),
    ("none", Meaning::Neg, true),
    ("without", Meaning::Neg, true),
    ("cannot", Meaning::Neg, false),
    ("nothing", Meaning::Neg, false),
    ("nobody", Meaning::Neg, false),
    ("neither", Meaning::Neg, false),
    ("nor", Meaning::Neg, false),
    ("hardly", Meaning::Neg, false),
    ("unable to", Meaning::Neg, false),
    ("failed to", Meaning::Neg, false),
    ("reject", Meaning::Neg, false),
    ("rejected", Meaning::Neg, false),
    ("rejects", Meaning::Neg, false),
    ("dont", Meaning::Neg, false),
    ("doesnt", Meaning::Neg, false),
    ("didnt", Meaning::Neg, false),
    ("cant", Meaning::Neg, false),
    ("couldnt", Meaning::Neg, false),
    ("wouldnt", Meaning::Neg, false),
    ("shouldnt", Meaning::Neg, false),
    ("isnt", Meaning::Neg, false),
    ("arent", Meaning::Neg, false),
    ("wasnt", Meaning::Neg, false),
    ("werent", Meaning::Neg, false),
    ("hasnt", Meaning::Neg, false),
    ("havent", Meaning::Neg, false),
    ("hadnt", Meaning::Neg, false),
    ("wont", Meaning::Neg, false),
    ("deny", Meaning::Neg, false),
    ("denied", Meaning::Neg, false),
    ("denies", Meaning::Neg, false),
    ("refuse", Meaning::Neg, false),
    ("refused", Meaning::Neg, false),
    ("refuses", Meaning::Neg, false),
    ("decline", Meaning::Neg, false),
    ("declined", Meaning::Neg, false),
    ("declines", Meaning::Neg, false),
    ("veto", Meaning::Neg, false),
    ("vetoed", Meaning::Neg, false),
    ("vetoes", Meaning::Neg, false),
    ("no more than", Meaning::Le, true),
    ("not more than", Meaning::Le, true),
    ("at most", Meaning::Le, true),
    ("up to", Meaning::Le, true),
    ("or less", Meaning::Le, false),
    ("maximum", Meaning::Le, false),
    ("at least", Meaning::Ge, true),
    ("no less than", Meaning::Ge, true),
    ("not less than", Meaning::Ge, true),
    ("or more", Meaning::Ge, false),
    ("minimum", Meaning::Ge, false),
    ("less than", Meaning::Lt, true),
    ("fewer than", Meaning::Lt, true),
    ("under", Meaning::Lt, true),
    ("below", Meaning::Lt, true),
    ("more than", Meaning::Gt, true),
    ("greater than", Meaning::Gt, true),
    ("over", Meaning::Gt, true),
    ("above", Meaning::Gt, true),
    ("exceeding", Meaning::Gt, true),
    ("upwards of", Meaning::Gt, true),
    ("in excess of", Meaning::Gt, false),
    ("about", Meaning::Approx, true),
    ("approximately", Meaning::Approx, true),
    ("around", Meaning::Approx, true),
    ("roughly", Meaning::Approx, true),
    ("nearly", Meaning::Nearly, true),
    ("almost", Meaning::Nearly, true),
];

/// Which word list a reading uses: the plan's original list, or the current, extended one.
/// `check_qualifiers` reads with both, so adding a word can only lower a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Words {
    Plan,
    Extended,
}

struct SubKey {
    key: Vec<char>,
    meaning: Meaning,
    not_after: Vec<Vec<char>>,
    not_before: Vec<Vec<char>>,
    after_digit: bool,
    before_digit: bool,
}

struct Lexicon {
    /// Substring entries as match keys, longest first.
    subs: Vec<SubKey>,
    /// English entries as word sequences, longest first.
    english: Vec<(Vec<String>, Meaning)>,
}

fn key(text: &str) -> Vec<char> {
    match_key(text).text.chars().collect()
}

/// Built once, through the same normalizers as the text, so an entry typed in another mark order
/// still matches.
fn build(words: Words) -> Lexicon {
    let keep = |plan: bool| plan || words == Words::Extended;
    let mut subs: Vec<SubKey> = SUBSTRINGS
        .iter()
        .filter(|e| keep(e.plan))
        .map(|e| SubKey {
            key: key(e.text),
            meaning: e.meaning,
            not_after: e.not_after.iter().map(|t| key(t)).collect(),
            not_before: e.not_before.iter().map(|t| key(t)).collect(),
            after_digit: e.after_digit,
            before_digit: e.before_digit,
        })
        .collect();
    subs.sort_by_key(|e| std::cmp::Reverse(e.key.len()));
    let mut english: Vec<(Vec<String>, Meaning)> = ENGLISH
        .iter()
        .filter(|(_, _, plan)| keep(*plan))
        .map(|(t, m, _)| {
            (
                words_of(&normalize_th(t).text)
                    .into_iter()
                    .map(|w| w.text)
                    .collect(),
                *m,
            )
        })
        .collect();
    english.sort_by_key(|(w, _)| std::cmp::Reverse(w.len()));
    Lexicon { subs, english }
}

static PLAN: LazyLock<Lexicon> = LazyLock::new(|| build(Words::Plan));
static EXTENDED: LazyLock<Lexicon> = LazyLock::new(|| build(Words::Extended));

fn lexicon(words: Words) -> &'static Lexicon {
    match words {
        Words::Plan => &PLAN,
        Words::Extended => &EXTENDED,
    }
}

/// One qualifier found in a text, at code points `start..end` of that text (widened to whole
/// grapheme clusters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qualifier {
    pub meaning: Meaning,
    pub start: u32,
    pub end: u32,
}

/// The qualifiers in a text: a left-to-right, longest-match-first, non-overlapping scan. Thai
/// entries match as substrings of `match_key(text)`; English entries as whole lowercased words
/// or word sequences of `normalize_th(text)`.
pub fn find(text: &str) -> BTreeSet<Meaning> {
    spans(text).iter().map(|q| q.meaning).collect()
}

/// Every qualifier of [`find`] with its place in `text`, in text order.
pub fn spans(text: &str) -> Vec<Qualifier> {
    spans_with(text, Words::Extended)
}

/// [`spans`] with the given word list.
pub fn spans_with(text: &str, words: Words) -> Vec<Qualifier> {
    let lexicon = lexicon(words);
    let mut found = substring_scan(text, lexicon);
    found.extend(english_scan(text, lexicon));
    found.sort_by_key(|q| (q.start, q.end));
    found
}

/// How often each meaning occurs, so a doubled negation does not cancel out against one.
fn counts(found: &[Qualifier]) -> BTreeMap<Meaning, usize> {
    let mut counts = BTreeMap::new();
    for q in found {
        *counts.entry(q.meaning).or_insert(0) += 1;
    }
    counts
}

fn substring_scan(text: &str, lexicon: &Lexicon) -> Vec<Qualifier> {
    let key = match_key(text);
    let chars: Vec<char> = key.text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let hit = lexicon.subs.iter().find(|e| {
            chars[i..].starts_with(&e.key)
                && !(e.after_digit && !(i > 0 && chars[i - 1].is_ascii_digit()))
                && !(e.before_digit
                    && !chars.get(i + e.key.len()).is_some_and(char::is_ascii_digit))
                && !e
                    .not_before
                    .iter()
                    .any(|b| chars[i + e.key.len()..].starts_with(b))
        });
        match hit {
            Some(e) => {
                if !e.not_after.iter().any(|a| chars[..i].ends_with(a)) {
                    let range = key.original_range(i, i + e.key.len());
                    found.push(Qualifier {
                        meaning: e.meaning,
                        start: range.start,
                        end: range.end,
                    });
                }
                i += e.key.len();
            }
            None => i += 1,
        }
    }
    found
}

struct Word {
    text: String,
    /// Code points of the normalized text the word (apostrophes trimmed) spans.
    start: usize,
    end: usize,
    /// A `.` right after the word, then an optional space and a digit: "No. 5".
    before_numbered_dot: bool,
}

/// A letter of an English word: ASCII letters and digits, Latin letters up to U+024F and the
/// apostrophe. Thai letters end a word.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '\''
        || (('\u{00C0}'..='\u{024F}').contains(&c) && c.is_alphabetic())
}

/// The words of a normalized text, with apostrophes trimmed from their ends.
fn words_of(text: &str) -> Vec<Word> {
    let chars: Vec<char> = text.chars().collect();
    let digit_at = |at: usize| chars.get(at).is_some_and(char::is_ascii_digit);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_word_char(chars[i]) {
            i += 1;
            continue;
        }
        let mut start = i;
        while i < chars.len() && is_word_char(chars[i]) {
            i += 1;
        }
        let mut end = i;
        while start < end && chars[start] == '\'' {
            start += 1;
        }
        while end > start && chars[end - 1] == '\'' {
            end -= 1;
        }
        if start < end {
            out.push(Word {
                text: chars[start..end].iter().collect(),
                start,
                end,
                before_numbered_dot: chars.get(i) == Some(&'.')
                    && (digit_at(i + 1) || (chars.get(i + 1) == Some(&' ') && digit_at(i + 2))),
            });
        }
    }
    out
}

fn english_scan(text: &str, lexicon: &Lexicon) -> Vec<Qualifier> {
    let normalized = normalize_th(text);
    let words = words_of(&normalized.text);
    let mut found = Vec::new();
    let mut push = |meaning, first: &Word, last: &Word| {
        let range = normalized.original_range(first.start, last.end);
        found.push(Qualifier {
            meaning,
            start: range.start,
            end: range.end,
        });
    };
    let mut i = 0;
    while i < words.len() {
        let hit = lexicon.english.iter().find(|(entry, _)| {
            let Some(run) = words.get(i..i + entry.len()) else {
                return false;
            };
            let same = run.iter().zip(entry).all(|(w, e)| w.text == *e);
            // "No." before a number (as in "No. 5") abbreviates "number"; it is not a negation.
            same && !(entry.len() == 1 && entry[0] == "no" && run[0].before_numbered_dot)
        });
        if let Some((entry, meaning)) = hit {
            push(*meaning, &words[i], &words[i + entry.len() - 1]);
            i += entry.len();
            continue;
        }
        if words[i].text.ends_with("n't") {
            push(Meaning::Neg, &words[i], &words[i]);
        }
        i += 1;
    }
    found
}

/// Each qualifier of `text` in order, with the index in `find_numbers(text, ctx)` of the first
/// `Quantity` that starts at or after its end: the figure it most likely qualifies (for Task 11's
/// number check).
pub fn pair_with_numbers(
    text: &str,
    ctx: &Ctx,
) -> Result<Vec<(Qualifier, Option<usize>)>, TooLong> {
    let numbers = find_numbers(text, ctx)?;
    Ok(spans(text)
        .into_iter()
        .map(|q| {
            let next = numbers
                .iter()
                .position(|n| n.value.role == NumberRole::Quantity && n.start >= q.end);
            (q, next)
        })
        .collect())
}

/// For an `Ok` ref, the 10 code points of `block_text` before `quote_start`, the quote, and the
/// 10 after `quote_end`, clamped to the block; `None` for a stale ref.
pub fn window(r: &ResolvedRef) -> Option<String> {
    let (chars, lo, hi) = window_range(r)?;
    Some(chars[lo..hi].iter().collect())
}

fn window_range(r: &ResolvedRef) -> Option<(Vec<char>, usize, usize)> {
    let ResolvedRef::Ok {
        block_text,
        quote_start,
        quote_end,
        ..
    } = r
    else {
        return None;
    };
    let chars: Vec<char> = block_text.chars().collect();
    let end = (*quote_end as usize).min(chars.len());
    let start = (*quote_start as usize).min(end);
    let lo = start.saturating_sub(WINDOW);
    let hi = end.saturating_add(WINDOW).min(chars.len());
    Some((chars, lo, hi))
}

/// A code point that may sit inside an English word of the raw block text (before
/// normalization): a word letter, a curly apostrophe or an invisible character.
fn in_raw_word(c: char) -> bool {
    is_word_char(c)
        || c == '\u{2019}'
        || matches!(
            c,
            '\u{00AD}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
        || (c.is_alphanumeric() && !('\u{0E00}'..='\u{0E7F}').contains(&c))
}

/// Which stretch of block text a reading compares with the claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// [`window`]: 10 code points each side of the quote.
    Window,
    /// The quote's whole sentence (ends: `.` before a space or the end, `!`, `?`, `ฯ`, a line
    /// break, and a space after a Thai run of 30 or more code points), never less than
    /// [`window`], and never less than 30 code points each side when Thai text is near.
    Sentence,
}

fn is_thai(c: char) -> bool {
    ('\u{0E00}'..='\u{0E7F}').contains(&c)
}

/// A `.` before a space that does not end a sentence: the next word starts with a digit or a
/// lowercase letter ("No. 5", "e.g. at"), or the word before it is a single letter or a dotted
/// run ("J.", "e.g.", "4.b."). An abbreviation before a capital ("Mr. Smith") still ends one.
fn abbreviation_dot(chars: &[char], i: usize) -> bool {
    let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
    if next.is_some_and(|c| c.is_numeric() || c.is_lowercase()) {
        return true;
    }
    let word_start = chars[..i]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |p| p + 1);
    let word = &chars[word_start..i];
    (word.len() == 1 && word[0].is_alphabetic()) || (word.len() > 1 && word.contains(&'.'))
}

/// The code-point range of the quote's sentence reading in `chars`.
fn sentence_range(
    chars: &[char],
    start: usize,
    end: usize,
    lo: usize,
    hi: usize,
) -> (usize, usize) {
    let len = chars.len();
    // Non-space code points right before each position.
    let mut run = vec![0usize; len + 1];
    for i in 0..len {
        run[i + 1] = if chars[i].is_whitespace() {
            0
        } else {
            run[i] + 1
        };
    }
    let is_end = |i: usize| -> bool {
        let c = chars[i];
        match c {
            '!' | '?' | 'ฯ' | '\n' | '\r' | '\u{2028}' | '\u{2029}' => true,
            '.' => {
                chars.get(i + 1).is_none_or(|n| n.is_whitespace()) && !abbreviation_dot(chars, i)
            }
            _ if c.is_whitespace() => run[i] >= THAI_SENTENCE_RUN && i > 0 && is_thai(chars[i - 1]),
            _ => false,
        }
    };
    let mut s_lo = (0..start).rev().find(|&i| is_end(i)).map_or(0, |i| i + 1);
    let mut s_hi = (end..len).find(|&i| is_end(i)).map_or(len, |i| i + 1);
    let near_from = start.saturating_sub(THAI_MIN);
    let near_to = end.saturating_add(THAI_MIN).min(len);
    if chars[near_from..near_to].iter().copied().any(is_thai) {
        s_lo = s_lo.min(near_from);
        s_hi = s_hi.max(near_to);
    }
    (s_lo.min(lo), s_hi.max(hi))
}

/// The qualifiers one reading finds around a ref. An edge inside an English word moves outward
/// to the word's end ("cannot" cut to "not" invents no negation, "didn't" cut to "n't" hides
/// none), and the text up to [`REACH`] code points past each edge is scanned too, keeping every
/// entry that overlaps the stretch, so "ไม่" cut to "่" or "ไม่เกิน" cut to "เกิน" is read as
/// written.
fn read_around(r: &ResolvedRef, reach: Reach, words: Words) -> Option<Vec<Qualifier>> {
    let (chars, mut lo, mut hi) = window_range(r)?;
    if reach == Reach::Sentence {
        let ResolvedRef::Ok {
            quote_start,
            quote_end,
            ..
        } = r
        else {
            return None;
        };
        let end = (*quote_end as usize).min(chars.len());
        let start = (*quote_start as usize).min(end);
        (lo, hi) = sentence_range(&chars, start, end, lo, hi);
    }
    if lo > 0 && in_raw_word(chars[lo]) {
        while lo > 0 && in_raw_word(chars[lo - 1]) {
            lo -= 1;
        }
    }
    if hi < chars.len() && hi > lo && in_raw_word(chars[hi - 1]) {
        while hi < chars.len() && in_raw_word(chars[hi]) {
            hi += 1;
        }
    }
    let from = lo.saturating_sub(REACH);
    let to = hi.saturating_add(REACH).min(chars.len());
    let text: String = chars[from..to].iter().collect();
    let (lo, hi) = (lo - from, hi - from);
    Some(
        spans_with(&text, words)
            .into_iter()
            .filter(|q| q.end as usize > lo && (q.start as usize) < hi)
            .collect(),
    )
}

/// One reading of a claim against a ref: its word list and stretch, and the qualifier counts on
/// each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub words: Words,
    pub reach: Reach,
    pub claim: BTreeMap<Meaning, usize>,
    pub source: BTreeMap<Meaning, usize>,
}

/// Every reading `check_qualifiers` compares, widest first: the extended word list before the
/// plan's, the sentence before the 10-code-point window. `None` for a stale ref.
pub fn readings(claim_text: &str, r: &ResolvedRef) -> Option<Vec<Reading>> {
    let mut out = Vec::new();
    for words in [Words::Extended, Words::Plan] {
        let claim = counts(&spans_with(claim_text, words));
        for reach in [Reach::Sentence, Reach::Window] {
            out.push(Reading {
                words,
                reach,
                claim: claim.clone(),
                source: counts(&read_around(r, reach, words)?),
            });
        }
    }
    Some(out)
}

fn names(counts: &BTreeMap<Meaning, usize>) -> Vec<&'static str> {
    counts
        .iter()
        .flat_map(|(m, n)| std::iter::repeat_n(m.wire(), *n))
        .collect()
}

/// For a cited claim, one `Qualifier` check, `Inconclusive`, `qualifier_mismatch`, per
/// full-quality ref for which any of its [`readings`] holds a different count of any qualifier on
/// the two sides (so "not not approved" does not match "not approved"); nothing when every
/// reading agrees. A ref is flagged when any reading differs, so widening the stretch or adding
/// words, which only adds readings, can only lower a verdict. Runs for every cited claim, numbers
/// and dates included. `detail` holds the first differing reading's counts.
pub fn check_qualifiers(claim: &ClaimInput) -> Vec<CheckOutcome> {
    if !claim.cited {
        return vec![];
    }
    let too_long = |text: &str| text.chars().nth(MAX_SCAN_CODE_POINTS).is_some();
    let claim_too_long = too_long(&claim.text);
    resolved_refs(&claim.refs)
        .filter_map(|(ordinal, r)| {
            let mismatch = |detail| CheckOutcome {
                method: CheckMethod::Qualifier,
                verdict: CheckVerdict::Inconclusive,
                basis: Basis::None,
                code: "qualifier_mismatch",
                ref_ordinal: Some(ordinal),
                advisory: false,
                detail,
            };
            // A text too long to scan is never scanned in part: it is flagged.
            let ResolvedRef::Ok { block_text, .. } = r else {
                return None;
            };
            if claim_too_long || too_long(block_text) {
                return Some(mismatch(json!({"reason": "text_too_long"})));
            }
            let readings = readings(&claim.text, r)?;
            let differing = readings.iter().find(|x| x.claim != x.source)?;
            Some(mismatch(json!({
                "claim": names(&differing.claim),
                "source": names(&differing.source),
            })))
        })
        .collect()
}
