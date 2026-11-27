//! The date check: a claim's date against the dates of a ref, compared by meaning, a
//! date-formatted cell read from its serial.
use katwork_thai::{DateValue, Era, Flags, Found};
use serde_json::json;

use super::cell::{self, CellValue};
use super::number::{ClaimSide, Compared, field_qualifiers, settle, stopped};
use super::scan::Scan;
use super::subject::{self, Against};
use super::{Basis, CheckMethod, CheckOutcome, CheckVerdict};

/// The date check of a claim whose text holds dates and no quantity, against one read ref,
/// plus a `Qualifier` check when the ref's fields hold other negations or comparators than the
/// claim. A claim with more than one date is `date_ambiguous` (the crate reads none of them with
/// full confidence).
pub(super) fn check(claim: &ClaimSide, scan: &Scan) -> Vec<CheckOutcome> {
    let at = Some(scan.ordinal);
    let open = |code: &'static str, detail| stopped((CheckMethod::Date, code, detail), at);
    let mut dates = scan.source.dates.clone();
    // The quote of a date cell is its serial; its day comes from the serial.
    if let Some(CellValue::Date(Some(date))) = scan.cell.map(cell::value) {
        dates.push((cell_date(date), true));
    }
    let subject = claim.subject;
    let [date] = subject.periods.as_slice() else {
        return vec![open(
            "date_ambiguous",
            json!({"dates": subject.periods.len()}),
        )];
    };
    let detail = json!({"claim": date.raw});
    let met = || subject::check(&subject.words, &[], &scan.source, true);
    let qualified = !claim.counts.is_empty() || !scan.counts.is_empty();
    let first = match subject::against(date, &dates) {
        Against::Same => settle(
            Compared::Equal(Basis::Exact),
            met(),
            CheckMethod::Date,
            at,
            detail,
        ),
        Against::Different => settle(
            Compared::Differs("date_mismatch", qualified),
            met(),
            CheckMethod::Date,
            at,
            detail,
        ),
        Against::LowConf => open("date_ambiguous", detail),
        Against::KindDiffers => open("period_kind_differs", detail),
        Against::Ambiguous => open("period_ambiguous", detail),
        Against::Several | Against::Absent => CheckOutcome {
            verdict: CheckVerdict::Unsupported,
            ..open("date_not_in_source", detail)
        },
    };
    let mut out = vec![first];
    out.extend(field_qualifiers(&claim.counts, scan));
    out
}

/// A day read from a cell's serial: exact, era-free.
fn cell_date(date: chrono::NaiveDate) -> Found<DateValue> {
    Found {
        start: 0,
        end: 0,
        raw: date.to_string(),
        value: DateValue::Day { date, era: Era::Ce },
        alts: vec![],
        conf: 1.0,
        flags: Flags::default(),
    }
}
