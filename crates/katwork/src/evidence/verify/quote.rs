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
            let super::ResolvedRef::Ok { exact, .. } = r else {
                return None;
            };
            let contained = contains_whole(&keyed(exact), &key);
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

/// Two code points of one run that no space separates: an English word, a number, or Thai
/// text, which has no word boundaries this crate can see.
fn same_run(a: char, b: char) -> bool {
    (is_word(a) && is_word(b)) || (is_thai(a) && is_thai(b))
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
/// English word or a number ("approved" in "disapproved", "1" in "12") is not one, nor is a hit
/// whose end falls inside a run of Thai text, which has no word boundaries to check (it is then
/// a paraphrase). An empty needle is never contained.
fn contains_whole(hay: &[(char, bool)], needle: &[(char, bool)]) -> bool {
    let (Some(first), Some(last)) = (needle.first(), needle.last()) else {
        return false;
    };
    let hay_chars: Vec<char> = hay.iter().map(|(c, _)| *c).collect();
    let needle_chars: Vec<char> = needle.iter().map(|(c, _)| *c).collect();
    let mut found = false;
    each_hit(&hay_chars, &needle_chars, |at| {
        let end = at + needle.len();
        let left = at == 0 || hay[at].1 || !same_run(hay[at - 1].0, first.0);
        let right = end == hay.len() || hay[end].1 || !same_run(last.0, hay[end].0);
        found = left && right;
        !found
    });
    found
}
