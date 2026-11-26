//! Qualifiers: negations and comparators must mean the same in the claim and around its quote,
//! so "อนุมัติงบประมาณ" quoted from "ไม่อนุมัติงบประมาณ" cannot pass (ADR-0007).
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use katwork_thai::{Ctx, NumberRole, TooLong, find_numbers, match_key, normalize_th};
use serde::Serialize;
use serde_json::json;

use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, ResolvedRef, resolved_refs,
};

/// Code points of block text read on each side of a quote.
const WINDOW: usize = 10;

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

/// The Thai lexicon, exactly as the plan gives it.
const THAI: &[(&str, Meaning)] = &[
    ("ไม่", Meaning::Neg),
    ("มิได้", Meaning::Neg),
    ("มิใช่", Meaning::Neg),
    ("ไม่เกิน", Meaning::Le),
    ("ไม่มากกว่า", Meaning::Le),
    ("อย่างมาก", Meaning::Le),
    ("ไม่ต่ำกว่า", Meaning::Ge),
    ("ไม่น้อยกว่า", Meaning::Ge),
    ("อย่างน้อย", Meaning::Ge),
    ("ต่ำกว่า", Meaning::Lt),
    ("น้อยกว่า", Meaning::Lt),
    ("ไม่ถึง", Meaning::Lt),
    ("มากกว่า", Meaning::Gt),
    ("สูงกว่า", Meaning::Gt),
    ("กว่า", Meaning::Gt),
    ("เกิน", Meaning::Gt),
    ("ประมาณ", Meaning::Approx),
    ("เกือบ", Meaning::Nearly),
];

/// `ประมาณ` right after this is `งบประมาณ`, "budget", not a qualifier.
const BUDGET: &str = "งบ";

/// The English lexicon, exactly as the plan gives it; `no` before `.` and words ending in `n't`
/// are handled in [`english_scan`].
const ENGLISH: &[(&str, Meaning)] = &[
    ("not", Meaning::Neg),
    ("no", Meaning::Neg),
    ("never", Meaning::Neg),
    ("none", Meaning::Neg),
    ("without", Meaning::Neg),
    ("no more than", Meaning::Le),
    ("not more than", Meaning::Le),
    ("at most", Meaning::Le),
    ("up to", Meaning::Le),
    ("at least", Meaning::Ge),
    ("no less than", Meaning::Ge),
    ("not less than", Meaning::Ge),
    ("less than", Meaning::Lt),
    ("fewer than", Meaning::Lt),
    ("under", Meaning::Lt),
    ("below", Meaning::Lt),
    ("more than", Meaning::Gt),
    ("greater than", Meaning::Gt),
    ("over", Meaning::Gt),
    ("above", Meaning::Gt),
    ("exceeding", Meaning::Gt),
    ("upwards of", Meaning::Gt),
    ("about", Meaning::Approx),
    ("approximately", Meaning::Approx),
    ("around", Meaning::Approx),
    ("roughly", Meaning::Approx),
    ("nearly", Meaning::Nearly),
    ("almost", Meaning::Nearly),
];

struct Lexicon {
    /// Thai entries as match keys, longest first.
    thai: Vec<(Vec<char>, Meaning)>,
    budget: Vec<char>,
    /// English entries as word sequences, longest first.
    english: Vec<(Vec<String>, Meaning)>,
}

/// Built once, through the same normalizers as the text, so an entry typed in another mark order
/// still matches.
static LEXICON: LazyLock<Lexicon> = LazyLock::new(|| {
    let key = |text: &str| -> Vec<char> { match_key(text).text.chars().collect() };
    let mut thai: Vec<_> = THAI.iter().map(|(t, m)| (key(t), *m)).collect();
    thai.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    let mut english: Vec<_> = ENGLISH
        .iter()
        .map(|(t, m)| {
            (
                words(&normalize_th(t).text)
                    .into_iter()
                    .map(|w| w.text)
                    .collect(),
                *m,
            )
        })
        .collect();
    english.sort_by_key(|(w, _): &(Vec<String>, Meaning)| std::cmp::Reverse(w.len()));
    Lexicon {
        thai,
        budget: key(BUDGET),
        english,
    }
});

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
    let mut found = thai_scan(text);
    found.extend(english_scan(text));
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

fn thai_scan(text: &str) -> Vec<Qualifier> {
    let lexicon = &*LEXICON;
    let key = match_key(text);
    let chars: Vec<char> = key.text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let hit = lexicon
            .thai
            .iter()
            .find(|(entry, _)| chars[i..].starts_with(entry));
        match hit {
            Some((entry, meaning)) => {
                let budget = *meaning == Meaning::Approx && chars[..i].ends_with(&lexicon.budget);
                if !budget {
                    let range = key.original_range(i, i + entry.len());
                    found.push(Qualifier {
                        meaning: *meaning,
                        start: range.start,
                        end: range.end,
                    });
                }
                i += entry.len();
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
fn words(text: &str) -> Vec<Word> {
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

fn english_scan(text: &str) -> Vec<Qualifier> {
    let lexicon = &*LEXICON;
    let normalized = normalize_th(text);
    let words = words(&normalized.text);
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

/// The qualifiers [`check_qualifiers`] reads around a ref: those of [`window`], plus any entry
/// that straddles a window edge, read whole. An edge inside an English word moves outward to the
/// word's end ("cannot" cut to "not" invents no negation, "didn't" cut to "n't" hides none), and
/// the text up to [`REACH`] code points past each edge is scanned too, so "ไม่" cut to "่" or
/// "ไม่เกิน" cut to "เกิน" is still read as written.
fn window_qualifiers(r: &ResolvedRef) -> Option<Vec<Qualifier>> {
    let (chars, mut lo, mut hi) = window_range(r)?;
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
        spans(&text)
            .into_iter()
            .filter(|q| q.end as usize > lo && (q.start as usize) < hi)
            .collect(),
    )
}

fn names(counts: &BTreeMap<Meaning, usize>) -> Vec<&'static str> {
    counts
        .iter()
        .flat_map(|(m, n)| std::iter::repeat_n(m.wire(), *n))
        .collect()
}

/// For a cited claim, one `Qualifier` check, `Inconclusive`, `qualifier_mismatch`, per
/// full-quality ref whose window holds a different count of any qualifier than the claim (so
/// "not not approved" does not match "not approved"); nothing when the counts are equal. Runs for
/// every cited claim, numbers and dates included.
pub fn check_qualifiers(claim: &ClaimInput) -> Vec<CheckOutcome> {
    if !claim.cited {
        return vec![];
    }
    let wanted = counts(&spans(&claim.text));
    resolved_refs(&claim.refs)
        .filter_map(|(ordinal, r)| {
            let around = counts(&window_qualifiers(r)?);
            (around != wanted).then(|| CheckOutcome {
                method: CheckMethod::Qualifier,
                verdict: CheckVerdict::Inconclusive,
                basis: Basis::None,
                code: "qualifier_mismatch",
                ref_ordinal: Some(ordinal),
                advisory: false,
                detail: json!({"claim": names(&wanted), "source": names(&around)}),
            })
        })
        .collect()
}
