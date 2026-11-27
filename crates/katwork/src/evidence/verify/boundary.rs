//! A quote never cuts a figure or a date: `cite` widens new quotes to whole tokens (Task 7), and
//! this catches refs made any other way (an older citation, a relocated quote, a ref built by
//! hand).
use katwork_thai::{Ctx, find_dates, find_numbers};

use super::ResolvedRef;

/// True when a number `find_numbers` finds in `block_text`, whatever its role (an ordinal such
/// as `วาระที่ 31` too), or a date `find_dates` finds there, overlaps `quote_start..quote_end`
/// without lying wholly inside it. A block text too long to scan counts as cut; a stale ref
/// cuts nothing.
pub fn cuts(r: &ResolvedRef, ctx: &Ctx) -> bool {
    let ResolvedRef::Ok {
        block_text,
        quote_start,
        quote_end,
        ..
    } = r
    else {
        return false;
    };
    let (start, end) = (*quote_start, *quote_end);
    let cut = |s: u32, e: u32| s < end && e > start && !(s >= start && e <= end);
    let Ok(numbers) = find_numbers(block_text, ctx) else {
        return true;
    };
    let Ok(dates) = find_dates(block_text, ctx) else {
        return true;
    };
    numbers.iter().any(|n| cut(n.start, n.end)) || dates.iter().any(|d| cut(d.start, d.end))
}
