//! The date check: a claim's date against the dates of a ref, compared by meaning, a
//! date-formatted cell read from its serial.
use katwork_thai::{Ctx, DateValue, Era, Flags, Found};
use serde_json::json;

use super::cell::{self, CellValue};
use super::number::{Compared, settle, stopped, too_long};
use super::subject::{self, Against, Source, Subject};
use super::{Basis, CheckMethod, CheckOutcome, CheckVerdict, ResolvedRef, boundary};

/// The date check of a claim whose text holds dates and no quantity, against one full-quality
/// ref. A claim with more than one date is `date_ambiguous` (the crate reads none of them with
/// full confidence).
pub(super) fn check(subject: &Subject, ordinal: u32, r: &ResolvedRef, ctx: &Ctx) -> CheckOutcome {
    let at = Some(ordinal);
    let open = |code: &'static str, detail| stopped((CheckMethod::Date, code, detail), at);
    let ResolvedRef::Ok { exact, cell, .. } = r else {
        return open("date_not_in_source", json!({}));
    };
    if let Some(code) = cell.as_ref().and_then(cell::gate) {
        return stopped((CheckMethod::Cell, code, json!({})), at);
    }
    if boundary::cuts(r, ctx) {
        return open("quote_cuts_number", json!({"quote": exact}));
    }
    let mut source = match Source::read(&subject::fields(r), ctx) {
        Ok(source) => source,
        Err(e) => return stopped(too_long(CheckMethod::Date)(e), at),
    };
    // The quote of a date cell is its serial; its day comes from the serial.
    if let Some(CellValue::Date(Some(date))) = cell.as_ref().map(cell::value) {
        source.dates.push((cell_date(date), true));
    }
    let [claim] = subject.periods.as_slice() else {
        return open("date_ambiguous", json!({"dates": subject.periods.len()}));
    };
    let detail = json!({"claim": claim.raw});
    let met = || subject::check(&subject.words, &[], &source, true);
    match subject::against(claim, &source.dates) {
        Against::Same => settle(
            Compared::Equal(Basis::Exact),
            met(),
            CheckMethod::Date,
            at,
            detail,
        ),
        Against::Different => settle(
            Compared::Differs("date_mismatch", false),
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
    }
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
