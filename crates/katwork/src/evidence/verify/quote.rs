//! Quote containment for `quote` and `fact` claims that hold no figure.
use katwork_thai::{Ctx, MAX_SCAN_CODE_POINTS, NumberRole, find_dates, find_numbers, normalize_th};
use serde_json::json;

use super::{
    Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, qualifier::is_invisible,
    resolve::each_hit, resolved_refs,
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
            let (hay, quote) = with_context(block_text, exact, *quote_start, *quote_end);
            let contained = contains_whole(&hay, quote, &key);
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
    matches!(c, '-' | '\u{2010}' | '\u{2011}' | '\u{2013}' | '\u{2212}')
}

/// A letter or digit of a spaced script: the code points a cut is tested on. An apostrophe
/// joins two of them ("board's") but does not make a run alone ("'the board approved'").
fn is_alnum(c: char) -> bool {
    c.is_alphanumeric() && !is_thai(c)
}

/// `x`, `j`, `y` read as one run: `. , / -` between two digits ("1.5", "1,250", "1/2567",
/// "1-3"), or a hyphen or an apostrophe between two letters or digits ("non-binding",
/// "board's").
fn joins(x: char, j: char, y: char) -> bool {
    let digit = |c: char| c.is_numeric();
    (digit(x) && digit(y) && (matches!(j, '.' | ',' | '/') || is_hyphen(j)))
        || (is_alnum(x) && is_alnum(y) && (is_hyphen(j) || j == '\''))
}

/// Whether the boundary before `text[i]` lies inside a run, for `0 < i < text.len()`. Each code
/// point records whether a space came right before it; a space is always a boundary. With
/// `thai` false, two Thai code points do not make a run (the lexicon covers Thai).
fn joined(text: &[(char, bool)], i: usize, thai: bool) -> bool {
    let (a, b) = (text[i - 1], text[i]);
    if b.1 {
        return false;
    }
    (is_alnum(a.0) && is_alnum(b.0))
        || (thai && is_thai(a.0) && is_thai(b.0))
        || text
            .get(i + 1)
            .is_some_and(|n| !n.1 && joins(a.0, b.0, n.0))
        || (i >= 2 && !a.1 && joins(text[i - 2].0, a.0, b.0))
}

/// Raw code points of block text first read on each side of the quote for its edge test; the
/// read doubles until it yields [`EDGE_KEPT`] normalized code points or reaches the block's edge.
const EDGE_READ: usize = 16;

/// Normalized code points of block text kept on each side of the quote.
const EDGE_KEPT: usize = 2;

/// Up to [`EDGE_KEPT`] keyed code points of `chars[..start]`, nearest last, and whether a space
/// came right before `start`. Reads back as far as it takes to collect them, so no run of code
/// points that normalization drops hides a cut; bounded by `MAX_SCAN_CODE_POINTS`.
fn context_before(chars: &[char], start: usize) -> (Vec<(char, bool)>, bool) {
    let mut read = EDGE_READ;
    loop {
        let from = start.saturating_sub(read);
        // A sentinel letter after the text records whether a space precedes it.
        let text: String = chars[from..start].iter().chain(['x'].iter()).collect();
        let mut ctx = keyed(&text);
        let space = ctx.pop().is_some_and(|(_, space)| space);
        if ctx.last().is_some_and(|(c, _)| *c == ' ') {
            ctx.pop();
        }
        if ctx.len() >= EDGE_KEPT || from == 0 || read >= MAX_SCAN_CODE_POINTS {
            let keep = ctx.len().saturating_sub(EDGE_KEPT);
            return (ctx.split_off(keep), space);
        }
        read *= 2;
    }
}

/// Up to [`EDGE_KEPT`] keyed code points of `chars[end..]`, nearest first; the first records
/// whether a space came right after `end`. Reads forward as [`context_before`] reads back.
fn context_after(chars: &[char], end: usize) -> Vec<(char, bool)> {
    let mut read = EDGE_READ;
    loop {
        let to = end.saturating_add(read).min(chars.len());
        // A sentinel letter before the text, so a space at its start is recorded.
        let text: String = ['x'].iter().chain(&chars[end..to]).collect();
        let mut ctx = keyed(&text);
        ctx.remove(0);
        if ctx.first().is_some_and(|(c, _)| *c == ' ') {
            ctx.remove(0);
        }
        if ctx.len() >= EDGE_KEPT || to == chars.len() || read >= MAX_SCAN_CODE_POINTS {
            ctx.truncate(EDGE_KEPT);
            return ctx;
        }
        read *= 2;
    }
}

/// The quote, keyed, with up to two normalized code points of block text on each side (so an
/// invisible code point or a dash at the cut reads as the matcher reads it), and the range the
/// quote takes in it. The first code point of the quote and of the text after it record
/// whether the block had a space there.
fn with_context(
    block_text: &str,
    exact: &str,
    start: u32,
    end: u32,
) -> (Vec<(char, bool)>, std::ops::Range<usize>) {
    let chars: Vec<char> = block_text.chars().collect();
    let len = chars.len();
    let end = (end as usize).min(len);
    let start = (start as usize).min(end);
    let visible = |c: &&char| !is_invisible(**c);
    let exact_chars: Vec<char> = exact.chars().collect();
    let (mut hay, space_before) = context_before(&chars, start);
    let space_before = space_before
        || exact_chars
            .iter()
            .find(visible)
            .is_some_and(|c| c.is_whitespace());
    let mut after = context_after(&chars, end);
    let space_after = exact_chars
        .iter()
        .rev()
        .find(visible)
        .is_some_and(|c| c.is_whitespace());
    let mut quote = keyed(exact);
    if let Some(first) = quote.first_mut() {
        first.1 |= space_before;
    }
    if let Some(first) = after.first_mut() {
        first.1 |= space_after;
    }
    let range = hay.len()..hay.len() + quote.len();
    hay.append(&mut quote);
    hay.append(&mut after);
    (hay, range)
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

/// `needle` occurs inside the `quote` range of `hay` at a place whose ends do not cut a run: a
/// hit in the middle of an English word or a number ("approved" in "disapproved", "1" in "12",
/// "1" in "1.5", "binding" in "non-binding") is not one, nor is a hit whose end falls inside a
/// run of Thai text, which has no word boundaries to check (it is then a paraphrase). The
/// block text around the quote counts too: a hit at an edge the quote itself cut from a word or
/// a number is not whole, and neither is a hit anywhere in the quote right after a hyphen that
/// follows a word, with a space or a line break between ("dis- approved", "non-\nbinding"; a
/// suspended hyphen such as "pre- and post-war" becomes a paraphrase too, which only lowers). At
/// the quote's own two edges Thai is left to the qualifier lexicon. An empty needle is never
/// contained.
fn contains_whole(
    hay: &[(char, bool)],
    quote: std::ops::Range<usize>,
    needle: &[(char, bool)],
) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay_chars: Vec<char> = hay[quote.clone()].iter().map(|(c, _)| *c).collect();
    let needle_chars: Vec<char> = needle.iter().map(|(c, _)| *c).collect();
    let cut = |i: usize| {
        let edge = i == quote.start || i == quote.end;
        let hyphen_before =
            i >= 2 && is_hyphen(hay[i - 1].0) && !hay[i - 1].1 && is_alnum(hay[i - 2].0);
        i > 0 && i < hay.len() && (joined(hay, i, !edge) || hyphen_before)
    };
    let mut found = false;
    each_hit(&hay_chars, &needle_chars, |at| {
        let (start, end) = (quote.start + at, quote.start + at + needle.len());
        found = !cut(start) && !cut(end);
        !found
    });
    found
}
