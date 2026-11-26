//! Quote containment for `quote` and `fact` claims that hold no figure.
use katwork_thai::{Ctx, NumberRole, find_dates, find_numbers, match_key};
use serde_json::json;

use super::{Basis, CheckMethod, CheckOutcome, CheckVerdict, ClaimInput, resolved_refs};
use crate::evidence::claim::ClaimKind;

/// For a `quote` or `fact` claim whose text holds no quantity and no date (those go to Task 11's
/// checks whatever their kind), one `Quote` check per full-quality ref: `match_key(claim)` inside
/// `match_key(exact)` is `Supported`, `Exact`; anything else, an empty key included, is
/// `Inconclusive`, `paraphrase` (quote) or `needs_judge` (fact).
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
    let key = match_key(&claim.text).text;
    resolved_refs(&claim.refs)
        .filter_map(|(ordinal, r)| {
            let super::ResolvedRef::Ok { exact, .. } = r else {
                return None;
            };
            let contained = !key.is_empty() && match_key(exact).text.contains(&key);
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
