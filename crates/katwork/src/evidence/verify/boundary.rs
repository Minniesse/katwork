//! A quote never cuts a figure or a date: `cite` widens new quotes to whole tokens (Task 7), and
//! this catches refs made any other way (an older citation, a relocated quote, a ref built by
//! hand).
use katwork_thai::{Ctx, MAX_SCAN_CODE_POINTS, find_dates, find_numbers};

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
    // A quote of the whole block cuts nothing, so its block is not scanned again.
    let len = block_text.chars().take(MAX_SCAN_CODE_POINTS + 1).count();
    if start == 0 && end as usize == len && len <= MAX_SCAN_CODE_POINTS {
        return false;
    }
    let cut = |s: u32, e: u32| s < end && e > start && !(s >= start && e <= end);
    let Ok(numbers) = find_numbers(block_text, ctx) else {
        return true;
    };
    let Ok(dates) = find_dates(block_text, ctx) else {
        return true;
    };
    numbers.iter().any(|n| cut(n.start, n.end)) || dates.iter().any(|d| cut(d.start, d.end))
}
