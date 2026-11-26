//! Qualifiers: negations and comparators must mean the same in the claim and around its quote,
//! so "อนุมัติงบประมาณ" quoted from "ไม่อนุมัติงบประมาณ" cannot pass (ADR-0007).
use std::collections::BTreeSet;
use std::sync::LazyLock;

use katwork_thai::{match_key, normalize_th};
use serde::Serialize;
use serde_json::json;

use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, ResolvedRef, resolved_refs,
};

/// Code points of block text read on each side of a quote.
const WINDOW: usize = 10;

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

/// The qualifiers in a text: a left-to-right, longest-match-first, non-overlapping scan. Thai
/// entries match as substrings of `match_key(text)`; English entries as whole lowercased words
/// or word sequences of `normalize_th(text)`.
pub fn find(text: &str) -> BTreeSet<Meaning> {
    let mut found = BTreeSet::new();
    thai_scan(&match_key(text).text, &mut found);
    english_scan(&normalize_th(text).text, &mut found);
    found
}

fn thai_scan(key: &str, found: &mut BTreeSet<Meaning>) {
    let lexicon = &*LEXICON;
    let chars: Vec<char> = key.chars().collect();
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
                    found.insert(*meaning);
                }
                i += entry.len();
            }
            None => i += 1,
        }
    }
}

struct Word {
    text: String,
    /// The code point right after the word, if any.
    next: Option<char>,
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
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_word_char(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_word_char(chars[i]) {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        let trimmed = word.trim_matches('\'');
        if !trimmed.is_empty() {
            out.push(Word {
                text: trimmed.to_string(),
                next: chars.get(i).copied(),
            });
        }
    }
    out
}

fn english_scan(normalized: &str, found: &mut BTreeSet<Meaning>) {
    let lexicon = &*LEXICON;
    let words = words(normalized);
    let mut i = 0;
    while i < words.len() {
        let hit = lexicon.english.iter().find(|(entry, _)| {
            let Some(run) = words.get(i..i + entry.len()) else {
                return false;
            };
            let same = run.iter().zip(entry).all(|(w, e)| w.text == *e);
            // "No." (as in "No. 5") is not a negation.
            same && !(entry.len() == 1 && entry[0] == "no" && run[0].next == Some('.'))
        });
        if let Some((entry, meaning)) = hit {
            found.insert(*meaning);
            i += entry.len();
            continue;
        }
        if words[i].text.ends_with("n't") {
            found.insert(Meaning::Neg);
        }
        i += 1;
    }
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

/// [`window`], with an edge that falls inside an English word moved outward to the word's end,
/// so a cut word is read whole: "cannot" cut to "not" invents no negation, and "didn't" cut to
/// "n't" or "'t" hides none.
fn checked_window(r: &ResolvedRef) -> Option<String> {
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
    Some(chars[lo..hi].iter().collect())
}

fn names(set: &BTreeSet<Meaning>) -> Vec<&'static str> {
    set.iter().map(|m| m.wire()).collect()
}

/// For a cited claim, one `Qualifier` check, `Inconclusive`, `qualifier_mismatch`, per
/// full-quality ref whose window's qualifiers differ from the claim's; nothing when they are
/// equal. Runs for every cited claim, numbers and dates included.
pub fn check_qualifiers(claim: &ClaimInput) -> Vec<CheckOutcome> {
    if !claim.cited {
        return vec![];
    }
    let wanted = find(&claim.text);
    resolved_refs(&claim.refs)
        .filter_map(|(ordinal, r)| {
            let around = find(&checked_window(r)?);
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
