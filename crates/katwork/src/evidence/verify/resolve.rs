//! Step 1 of the ladder: where a stored quote sits in the block's current text, and one
//! `Resolve` check per ref from its quality.
use katwork_thai::match_key;
use serde_json::{Map, Value, json};
use unicode_segmentation::UnicodeSegmentation;

use super::{Basis, CheckMethod, CheckOutcome, CheckVerdict, RefQuality, ResolvedRef, ordinal};
use crate::evidence::source_ref::{Selector, SourceRef};

/// The score an OCR line needs on `conf` or `align_score` to count as full quality.
const OCR_MIN_SCORE: f32 = 0.8;

/// True when the ref's `Resolve` check is `Supported`: the only refs the comparing checks read.
pub(super) fn is_resolved(r: &ResolvedRef) -> bool {
    matches!(resolve_code(r), Ok(()))
}

/// `Ok` for a full-quality ref, else the code its `Resolve` check carries.
fn resolve_code(r: &ResolvedRef) -> Result<(), &'static str> {
    let quality = match r {
        ResolvedRef::Stale { code } => return Err(code),
        ResolvedRef::Ok { quality, .. } => quality,
    };
    match quality {
        RefQuality::Full => Ok(()),
        RefQuality::Generated => Err("source_generated"),
        RefQuality::Hidden => Err("hidden_source"),
        RefQuality::InstructionLike => Err("instruction_like_source"),
        RefQuality::SuspectLayer(_) => Err("suspect_text_layer"),
        RefQuality::Deleted => Err("source_deleted"),
        RefQuality::Ocr {
            conf,
            align_score,
            boxed,
            ..
        } => {
            if !boxed {
                return Err("ocr_unaligned");
            }
            // A NaN score compares false, so it never passes.
            let good = |score: &Option<f32>| score.is_some_and(|s| s >= OCR_MIN_SCORE);
            if good(conf) || good(align_score) {
                Ok(())
            } else {
                Err("ocr_low_confidence")
            }
        }
    }
}

/// One `Resolve` check per ref, in order: `Supported` (code `checked`) for a full-quality ref,
/// otherwise `Inconclusive` with the quality's code. Its basis is always `None`: resolving is not
/// support.
pub fn check_resolve(refs: &[ResolvedRef]) -> Vec<CheckOutcome> {
    refs.iter()
        .enumerate()
        .map(|(i, r)| {
            let (verdict, code) = match resolve_code(r) {
                Ok(()) => (CheckVerdict::Supported, "checked"),
                Err(code) => (CheckVerdict::Inconclusive, code),
            };
            CheckOutcome {
                method: CheckMethod::Resolve,
                verdict,
                basis: Basis::None,
                code,
                ref_ordinal: Some(ordinal(i)),
                advisory: false,
                detail: resolve_detail(r),
            }
        })
        .collect()
}

fn resolve_detail(r: &ResolvedRef) -> Value {
    let mut detail = Map::new();
    match r {
        ResolvedRef::Ok {
            quality: RefQuality::SuspectLayer(reason),
            ..
        } => {
            detail.insert("reason".into(), json!(reason));
        }
        ResolvedRef::Ok {
            quality:
                RefQuality::Ocr {
                    conf,
                    align_score,
                    boxed,
                    ..
                },
            ..
        } => {
            // Scores as strings, so a stored row replays byte for byte.
            if let Some(conf) = conf {
                detail.insert("conf".into(), json!(conf.to_string()));
            }
            if let Some(score) = align_score {
                detail.insert("align_score".into(), json!(score.to_string()));
            }
            detail.insert("boxed".into(), json!(boxed.to_string()));
        }
        _ => {}
    }
    Value::Object(detail)
}

/// The quote's code-point range in the block's current text.
///
/// Keeps the stored position when the text there, and the stored `prefix` and `suffix` around
/// it, still have their `match_key`s; otherwise finds `prefix + exact + suffix`, then
/// `prefix + exact`, then `exact + suffix` (`exact` alone only for a quote stored without
/// context), each only when it occurs exactly once
/// (overlapping occurrences count). Searching compares code points as stored, with no fuzzy step:
/// an edited quote is `stale`, never guessed. A range never splits a grapheme cluster. A ref that
/// was never narrowed cites the whole block. Anything else, a malformed chain included, is
/// `Err("stale")`.
pub fn relocate(current_block_text: &str, r: &SourceRef) -> Result<(u32, u32), &'static str> {
    const STALE: &str = "stale";
    let chars: Vec<char> = current_block_text.chars().collect();
    let len = chars.len();
    let Ok(narrowing) = quote_refinement(&r.selector) else {
        return Err(STALE);
    };
    let Some(Narrowing {
        exact,
        prefix,
        suffix,
        start,
        end,
    }) = narrowing
    else {
        return if len == 0 {
            Err(STALE)
        } else {
            Ok((0, ordinal(len)))
        };
    };
    let key = match_key(exact).text;
    if key.is_empty() {
        return Err(STALE);
    }
    let bounds = grapheme_bounds(current_block_text, len);
    let at_bounds = |s: usize, e: usize| s < e && e <= len && bounds[s] && bounds[e];

    let (start, end) = (start as usize, end as usize);
    // The stored position is kept only with its context, so a shift that brings another copy of
    // the quote under it does not bind the ref to that copy.
    let text_key = |range: std::ops::Range<usize>| -> String {
        match_key(&chars[range].iter().collect::<String>()).text
    };
    if at_bounds(start, end)
        && text_key(start..end) == key
        && text_key(start.saturating_sub(prefix.chars().count())..start)
            .ends_with(&match_key(prefix).text)
        && text_key(end..(end + suffix.chars().count()).min(len))
            .starts_with(&match_key(suffix).text)
    {
        return Ok((ordinal(start), ordinal(end)));
    }

    let exact: Vec<char> = exact.chars().collect();
    let prefix: Vec<char> = prefix.chars().collect();
    let suffix: Vec<char> = suffix.chars().collect();
    let join = |before: &[char], after: &[char]| -> Vec<char> {
        before.iter().chain(&exact).chain(after).copied().collect()
    };
    // The quote with its whole context, then with the context of one side; the quote alone only
    // when it was stored without context. A ref whose context is gone is stale, never bound to
    // another copy of its quote.
    let mut tries: Vec<(Vec<char>, usize)> = vec![(join(&prefix, &suffix), prefix.len())];
    if !prefix.is_empty() {
        tries.push((join(&prefix, &[]), prefix.len()));
    }
    if !suffix.is_empty() {
        tries.push((join(&[], &suffix), 0));
    }
    let found = tries.iter().find_map(|(needle, offset)| {
        unique_hit(&chars, needle, |hit| {
            at_bounds(hit + offset, hit + offset + exact.len())
        })
        .map(|hit| hit + offset)
    });
    match found {
        Some(s) => Ok((ordinal(s), ordinal(s + exact.len()))),
        None => Err(STALE),
    }
}

struct Narrowing<'a> {
    exact: &'a str,
    prefix: &'a str,
    suffix: &'a str,
    start: u32,
    end: u32,
}

/// The `[TextQuoteSelector, TextPositionSelector]` refinement of the block link, through any
/// piece link; `None` for a ref that was never narrowed.
fn quote_refinement(selector: &Selector) -> Result<Option<Narrowing<'_>>, ()> {
    let mut node = selector;
    loop {
        let Selector::FragmentSelector { refined_by, .. } = node else {
            return Err(());
        };
        match refined_by.as_slice() {
            [next @ Selector::FragmentSelector { .. }] => node = next,
            [] => return Ok(None),
            [
                Selector::TextQuoteSelector {
                    exact,
                    prefix,
                    suffix,
                    ..
                },
                Selector::TextPositionSelector { start, end, .. },
            ] => {
                return Ok(Some(Narrowing {
                    exact,
                    prefix,
                    suffix,
                    start: *start,
                    end: *end,
                }));
            }
            _ => return Err(()),
        }
    }
}

/// For each code point offset `0..=len`, whether a grapheme cluster starts (or the text ends)
/// there.
fn grapheme_bounds(text: &str, len: usize) -> Vec<bool> {
    let mut bounds = vec![false; len + 1];
    let mut at = 0;
    bounds[0] = true;
    for grapheme in text.graphemes(true) {
        at += grapheme.chars().count();
        bounds[at] = true;
    }
    bounds
}

/// The one start of `needle` in `hay` that `accept` takes, or `None` when there is none or more
/// than one. Overlapping hits count.
fn unique_hit(hay: &[char], needle: &[char], accept: impl Fn(usize) -> bool) -> Option<usize> {
    let mut found = None;
    let mut unique = true;
    each_hit(hay, needle, |hit| {
        if accept(hit) {
            unique = found.is_none();
            found = Some(hit);
        }
        unique
    });
    if unique { found } else { None }
}

/// Calls `on_hit` with each start of `needle` in `hay`, overlapping hits included, until it
/// returns false. Knuth-Morris-Pratt, so hostile text costs linear time.
pub(super) fn each_hit(hay: &[char], needle: &[char], mut on_hit: impl FnMut(usize) -> bool) {
    if needle.is_empty() || needle.len() > hay.len() {
        return;
    }
    let mut fail = vec![0usize; needle.len()];
    let mut k = 0;
    for i in 1..needle.len() {
        while k > 0 && needle[i] != needle[k] {
            k = fail[k - 1];
        }
        if needle[i] == needle[k] {
            k += 1;
        }
        fail[i] = k;
    }
    let mut k = 0;
    for (i, c) in hay.iter().enumerate() {
        while k > 0 && *c != needle[k] {
            k = fail[k - 1];
        }
        if *c == needle[k] {
            k += 1;
        }
        if k == needle.len() {
            if !on_hit(i + 1 - needle.len()) {
                return;
            }
            k = fail[k - 1];
        }
    }
}
