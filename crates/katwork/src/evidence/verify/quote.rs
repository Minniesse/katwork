//! Quote containment for `quote` and `fact` claims that hold no figure.
use katwork_thai::{Ctx, NumberRole, find_dates, find_numbers, normalize_th};
use serde_json::json;

use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, resolve::each_hit, resolved_refs,
};
use crate::evidence::claim::ClaimKind;

/// For a `quote` or `fact` claim whose text holds no quantity and no date (those go to Task 11's
/// checks whatever their kind), one `Quote` check per full-quality ref: the claim's key inside
/// the quote's key, on whole words ([`contains_whole`]), is `Supported`, `Exact`; anything else,
/// an empty key included, is `Inconclusive`, `paraphrase` (quote) or `needs_judge` (fact).
pub fn check_quote(claim: &ClaimInput, ctx: &Ctx) -> Vec<CheckOutcome> {
    let open = match claim.kind {
        ClaimKind::Quote => "paraphrase",
        ClaimKind::Fact => "needs_judge",
        ClaimKind::Number | ClaimKind::Date => return vec![],
    };
    // First, so a text too long to scan is never normalized here.
    if holds_figure(&claim.text, ctx) {
        return vec![];
    }
    let key = keyed(&claim.text);
    resolved_refs(&claim.refs)
        .filter_map(|(ordinal, r)| {
            let super::ResolvedRef::Ok {
                exact,
                block_text,
                quote_start,
                quote_end,
                ..
            } = r
            else {
                return None;
            };
            let (cut_start, cut_end) = quote_cuts(block_text, *quote_start, *quote_end);
            let contained = contains_whole(&keyed(exact), &key, cut_start, cut_end);
            let (verdict, basis, code) = if contained {
                (CheckVerdict::Supported, Basis::Exact, "checked")
            } else {
                (CheckVerdict::Inconclusive, Basis::None, open)
            };
            Some(CheckOutcome {
                method: CheckMethod::Quote,
                verdict,
                basis,
                code,
                ref_ordinal: Some(ordinal),
                advisory: false,
                detail: json!({"claim": claim.text, "quote": exact}),
            })
        })
        .collect()
}

/// A `Quantity` number or any date in the text; a text the scanners refuse as too long counts as
/// holding one, so Task 11 answers it with `text_too_long`.
fn holds_figure(text: &str, ctx: &Ctx) -> bool {
    let quantity = match find_numbers(text, ctx) {
        Ok(numbers) => numbers.iter().any(|n| n.value.role == NumberRole::Quantity),
        Err(_) => true,
    };
    quantity || find_dates(text, ctx).map_or(true, |dates| !dates.is_empty())
}

/// A letter or digit of a script with word spaces (anything alphanumeric but Thai), or an
/// apostrophe. Digits of every script count, so "1" is not found in "12".
fn is_word(c: char) -> bool {
    c == '\'' || (c.is_alphanumeric() && !is_thai(c))
}

/// A Thai letter, vowel or mark (Thai digits are ASCII after `normalize_th`).
fn is_thai(c: char) -> bool {
    ('\u{0E01}'..='\u{0E4E}').contains(&c) && c != '\u{0E3F}'
}

fn is_hyphen(c: char) -> bool {
    matches!(c, '-' | '\u{2010}' | '\u{2011}')
}

/// `x`, `j`, `y` read as one run: `. , / -` between two digits ("1.5", "1,250", "1/2567",
/// "1-3"), or a hyphen between two word code points ("non-binding").
fn joins(x: char, j: char, y: char) -> bool {
    let digit = |c: char| c.is_numeric();
    (digit(x) && digit(y) && (matches!(j, '.' | ',' | '/') || is_hyphen(j)))
        || (is_word(x) && is_word(y) && is_hyphen(j))
}

/// Whether the boundary before `text[i]` lies inside a run, for `0 < i < text.len()`. Each code
/// point records whether a space came right before it; a space is always a boundary. With
/// `thai` false, two Thai code points do not make a run (the lexicon covers Thai).
fn joined(text: &[(char, bool)], i: usize, thai: bool) -> bool {
    let (a, b) = (text[i - 1], text[i]);
    if b.1 {
        return false;
    }
    (is_word(a.0) && is_word(b.0))
        || (thai && is_thai(a.0) && is_thai(b.0))
        || text
            .get(i + 1)
            .is_some_and(|n| !n.1 && joins(a.0, b.0, n.0))
        || (i >= 2 && !a.1 && joins(text[i - 2].0, a.0, b.0))
}

/// Whether the quote's start and end fall inside an English word or a number of `block_text`
/// ("approved" cut from "disapproved", "Section 1" from "Section 12"); a cut between two Thai
/// code points is left to the qualifier lexicon. Positions are clamped to the block.
fn quote_cuts(block_text: &str, start: u32, end: u32) -> (bool, bool) {
    let raw: Vec<(char, bool)> = block_text.chars().map(|c| (c, false)).collect();
    let len = raw.len();
    let end = (end as usize).min(len);
    let start = (start as usize).min(end);
    let cut = |i: usize| i > 0 && i < len && joined(&raw, i, false);
    (cut(start), cut(end))
}

/// `normalize_th(text)` as `match_key` sees it, except that a space between two word code
/// points stays ("now here" is not "nowhere"); each code point records whether the text had a
/// space right before it.
fn keyed(text: &str) -> Vec<(char, bool)> {
    let mut out: Vec<(char, bool)> = Vec::new();
    let mut space = false;
    for c in normalize_th(text).text.chars() {
        if c == ' ' {
            space = true;
            continue;
        }
        if space && out.last().is_some_and(|(p, _)| is_word(*p)) && is_word(c) {
            out.push((' ', true));
        }
        out.push((c, space));
        space = false;
    }
    out
}

/// `needle` occurs in `hay` at a place whose ends do not cut a run: a hit in the middle of an
/// English word or a number ("approved" in "disapproved", "1" in "12", "1" in "1.5", "binding"
/// in "non-binding") is not one, nor is a hit whose end falls inside a run of Thai text, which
/// has no word boundaries to check (it is then a paraphrase). A hit that reaches an end of `hay`
/// the quote itself cut from a run (`cut_start`, `cut_end`) is not whole either. An empty needle
/// is never contained.
fn contains_whole(
    hay: &[(char, bool)],
    needle: &[(char, bool)],
    cut_start: bool,
    cut_end: bool,
) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay_chars: Vec<char> = hay.iter().map(|(c, _)| *c).collect();
    let needle_chars: Vec<char> = needle.iter().map(|(c, _)| *c).collect();
    let mut found = false;
    each_hit(&hay_chars, &needle_chars, |at| {
        let end = at + needle.len();
        let left = if at == 0 {
            !cut_start
        } else {
            !joined(hay, at, true)
        };
        let right = if end == hay.len() {
            !cut_end
        } else {
            !joined(hay, end, true)
        };
        found = left && right;
        !found
    });
    found
}
