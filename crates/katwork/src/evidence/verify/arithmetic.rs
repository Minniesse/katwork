//! A derived figure: one aggregate comparison of the claim's value with the operation over its
//! cited operands, in `Decimal`, never operand-by-operand equality.
use std::collections::{BTreeMap, BTreeSet};

use katwork_thai::{Ctx, Unit};
use rust_decimal::Decimal;
use serde_json::json;

use super::number::{self, Compared, Qty, round_to, settle, stopped};
use super::subject::{self, OP_WORDS, Source, Subject};
use super::{Basis, CheckMethod, CheckOutcome, ClaimInput, resolve::is_resolved};
use crate::evidence::claim::{Derive, DeriveOp};

/// Words saying the change went up or down: a decrease word makes the claim's value negative.
const INCREASE: &[&str] = &["เพิ่มขึ้น", "increase", "increased", "rose", "grew"];
const DECREASE: &[&str] = &["ลดลง", "decrease", "decreased", "fell", "declined"];

/// `sum` of all; `difference` = a − b; `ratio` = a ÷ b; `share` = a ÷ b × 100;
/// `percent_change` = (b − a) ÷ |a| × 100. `Err` for a division by zero (or an overflow).
fn compute(op: DeriveOp, values: &[Decimal]) -> Result<Decimal, &'static str> {
    let overflow = "arithmetic_mismatch";
    let pair = || match values {
        [a, b] => Ok((*a, *b)),
        _ => Err("operand_missing"),
    };
    let divide = |n: Decimal, d: Decimal| {
        if d.is_zero() {
            Err("division_by_zero")
        } else {
            n.checked_div(d).ok_or(overflow)
        }
    };
    let hundred = |v: Decimal| v.checked_mul(Decimal::ONE_HUNDRED).ok_or(overflow);
    match op {
        DeriveOp::Sum => values
            .iter()
            .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v))
            .ok_or(overflow),
        DeriveOp::Difference => {
            let (a, b) = pair()?;
            a.checked_sub(b).ok_or(overflow)
        }
        DeriveOp::Ratio => {
            let (a, b) = pair()?;
            divide(a, b)
        }
        DeriveOp::Share => {
            let (a, b) = pair()?;
            hundred(divide(a, b)?)
        }
        DeriveOp::PercentChange => {
            let (a, b) = pair()?;
            hundred(divide(b.checked_sub(a).ok_or(overflow)?, a.abs())?)
        }
    }
}

/// The one `Arithmetic` check of a derived claim: each operand ref must resolve with full
/// quality and yield exactly one quantity (`operand_missing`), their explicit units must agree
/// (`operand_units_differ`); the result, compared at the claim's precision, is `Supported`,
/// `Derived`, or `Contradicted`, `arithmetic_mismatch`. A result that differs only in sign from
/// a claim with no direction word is held open. The subject rule reads the union of the
/// operands' sources, the operation's own words left out of the claim's.
pub(super) fn check(
    claim: &ClaimInput,
    value: &Qty,
    subject: &Subject,
    derive: &Derive,
    ctx: &Ctx,
) -> CheckOutcome {
    let open = |code: &'static str, detail| stopped((CheckMethod::Arithmetic, code, detail), None);
    // Each distinct ref is read once, however often `args` names it.
    let mut read: BTreeMap<u8, Qty> = BTreeMap::new();
    let mut source: Option<Source> = None;
    for &arg in derive.args.iter().collect::<BTreeSet<_>>() {
        let Some(r) = claim.refs.get(usize::from(arg)).filter(|r| is_resolved(r)) else {
            return open("operand_missing", json!({"arg": arg}));
        };
        let found = match number::quantities(r, ctx, CheckMethod::Arithmetic) {
            Ok(found) => found,
            Err((_, code, detail)) => return open(code, detail),
        };
        let [qty] = found.as_slice() else {
            return open(
                "operand_missing",
                json!({"arg": arg, "quantities": found.len()}),
            );
        };
        read.insert(arg, qty.clone());
        let fields = match Source::read(&subject::fields(r), ctx) {
            Ok(fields) => fields,
            Err(e) => return stopped(number::too_long(CheckMethod::Arithmetic)(e), None),
        };
        match &mut source {
            Some(all) => all.extend(fields),
            None => source = Some(fields),
        }
    }
    let Some(source) = source else {
        return open("operand_missing", json!({}));
    };
    let operands: Vec<&Qty> = derive.args.iter().filter_map(|arg| read.get(arg)).collect();
    let mut units: Vec<_> = read.values().filter_map(|q| q.unit.as_ref()).collect();
    units.dedup();
    // The claim's unit: a sum or a difference keeps the operands' unit, a share or a percent
    // change is a percent, a ratio has none.
    let expected = match derive.op {
        DeriveOp::Sum | DeriveOp::Difference => units.first().copied(),
        DeriveOp::Share | DeriveOp::PercentChange => Some(&Unit::Percent),
        DeriveOp::Ratio => None,
    };
    let claim_unit_differs = value.unit.as_ref().is_some_and(|u| Some(u) != expected);
    if units.len() > 1 || claim_unit_differs {
        return open(
            "operand_units_differ",
            json!({
                "units": units.iter().map(|u| format!("{u:?}")).collect::<Vec<_>>(),
                "claim": value.unit.as_ref().map(|u| format!("{u:?}")),
            }),
        );
    }
    let values: Vec<Decimal> = operands.iter().map(|q| q.effective).collect();
    let result = match compute(derive.op, &values) {
        Ok(result) => result,
        Err(code) => return open(code, json!({})),
    };
    let decrease = number::has_word(&claim.text, DECREASE);
    let increase = number::has_word(&claim.text, INCREASE);
    let claimed = if decrease && value.effective.is_sign_positive() {
        -value.effective
    } else {
        value.effective
    };
    let rounded = round_to(result, value.exp);
    let detail = json!({
        "claim": claimed.to_string(),
        "result": result.to_string(),
        "operands": values.iter().map(ToString::to_string).collect::<Vec<_>>(),
    });
    let words: Vec<_> = subject
        .words
        .iter()
        .filter(|w| !OP_WORDS.contains(&w.text.as_str()))
        .cloned()
        .collect();
    let met = subject::check(&words, &subject.periods, &source, false);
    if rounded == Some(claimed) {
        return settle(
            Compared::Equal(Basis::Derived),
            met,
            CheckMethod::Arithmetic,
            None,
            detail,
        );
    }
    if !increase && !decrease && rounded.map(|r| -r) == Some(claimed) {
        return open("arithmetic_mismatch", detail);
    }
    let qualified = !super::qualifier::spans(&claim.text).is_empty();
    settle(
        Compared::Differs("arithmetic_mismatch", qualified),
        met,
        CheckMethod::Arithmetic,
        None,
        detail,
    )
}
