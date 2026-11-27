//! The number check: a claim's single quantity against a ref's quantities, its scale and unit
//! read from the figure, the cell's format, its header unit or the block's unit context.
use std::collections::BTreeMap;

use katwork_thai::{Ctx, Flags, Found, Number, NumberRole, TooLong, Unit, find_numbers, match_key};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};

use super::cell::{self, CellValue};
use super::qualifier::{self, Meaning, Qualifier};
use super::scan::Scan;
use super::subject::{self, MIN_CONF, Source, Subject};
use super::{Basis, CheckMethod, CheckOutcome, CheckVerdict, ResolvedRef};
use crate::evidence::instruction::instruction_like;

/// A quantity as the comparisons read it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Qty {
    /// The figure as written or shown, before its scale.
    pub raw: Decimal,
    pub effective: Decimal,
    pub scale: u8,
    pub unit: Option<Unit>,
    /// The power of ten of the last digit the figure states; `None` for a cell's stored value.
    pub exp: Option<i64>,
    /// The power of ten of the last digit of `raw`.
    pub raw_exp: Option<i64>,
    /// Read below the clean-paragraph confidence with another reading beside it.
    pub ambiguous: bool,
    /// Its index in `find_numbers` of the quote, for pairing comparators with it.
    pub index: Option<usize>,
    /// Percentage points (`2.5% points`, `ร้อยละ 2.5 จุด`), not a percent.
    pub points: bool,
}

impl Qty {
    /// A figure `find_numbers` read; a figure stating neither scale nor unit takes the
    /// context's (`header_unit`, else `unit_context`). `None` when the scaled value does not fit
    /// in a `Decimal`: never compared at a wrong scale. `text` is the text it was read from.
    pub fn read(
        n: &Found<Number>,
        index: Option<usize>,
        context: &Context,
        text: &str,
    ) -> Option<Qty> {
        let v = &n.value;
        let (scale, unit) = match (v.scale.0, &v.unit, context) {
            (0, None, Some((scale, unit))) => (*scale, unit.clone()),
            _ => (v.scale.0, v.unit.clone()),
        };
        let decimals = i64::from(v.decimals);
        Some(Qty {
            raw: v.value,
            effective: scaled(v.value, scale)?,
            scale,
            unit,
            exp: Some(i64::from(scale) - decimals),
            raw_exp: Some(-decimals),
            ambiguous: n.conf < MIN_CONF && !n.alts.is_empty(),
            index,
            points: points_after(text, n.end as usize),
        })
    }
}

/// Whether `points`, `point`, `pp`, `ppt`, `percentage point(s)` or `จุด` follows the figure
/// ending at `end`.
fn points_after(text: &str, end: usize) -> bool {
    let rest: String = text
        .chars()
        .skip(end)
        .skip_while(|c| c.is_whitespace())
        .take(24)
        .collect::<String>()
        .to_lowercase();
    let word = |w: &str| {
        rest.strip_prefix(w)
            .is_some_and(|after| after.chars().next().is_none_or(|c| !c.is_alphanumeric()))
    };
    rest.starts_with("จุด")
        || rest.starts_with("point")
        || rest.starts_with("percentage point")
        || word("pp")
        || word("ppt")
}

/// `value` × 10^`scale`.
fn scaled(value: Decimal, scale: u8) -> Option<Decimal> {
    (0..scale).try_fold(value, |v, _| v.checked_mul(Decimal::TEN))
}

/// The scale and unit a cell's header unit (else the block's unit context) states, read as
/// the XLSX reader read it: the text after `1 ` gives `1` a scale or a unit. A text that
/// `instruction_like` flags states nothing.
pub(super) type Context = Option<(u8, Option<Unit>)>;

pub(super) fn context(r: &ResolvedRef, ctx: &Ctx) -> Context {
    let ResolvedRef::Ok {
        unit_context, cell, ..
    } = r
    else {
        return None;
    };
    let read = |text: &str| -> Context {
        if instruction_like(text).is_some() {
            return None;
        }
        let found = find_numbers(&format!("1 {}", text.trim()), ctx).ok()?;
        found
            .iter()
            .find(|n| n.start == 0 && (n.value.scale.0 > 0 || n.value.unit.is_some()))
            .map(|n| (n.value.scale.0, n.value.unit.clone()))
    };
    let header = cell
        .as_ref()
        .and_then(|c| c.header_unit.as_deref())
        .and_then(read);
    header.or_else(|| {
        unit_context.as_deref().and_then(|text| {
            let after = text.split_once(':').map_or(text, |(_, after)| after);
            read(after)
        })
    })
}

/// Why a ref yields no quantities to compare, as a check code.
pub(super) type Stop = (CheckMethod, &'static str, Value);

/// A ref's quantities: a `Number` cell's value, else the `Quantity` figures of the quote (a
/// `Bool`, `Error` or date cell has none). An amount in words is never one of them; one that
/// disagrees with the figure it restates makes the quote ambiguous (`amount_words_mismatch`).
pub(super) fn quantities(scan: &Scan, method: CheckMethod) -> Result<Vec<Qty>, Stop> {
    let context = &scan.context;
    match scan.cell.map(cell::value) {
        Some(CellValue::Number {
            value,
            percent,
            thousands,
        }) => {
            let unit = if percent {
                Some(Unit::Percent)
            } else {
                context.as_ref().and_then(|(_, unit)| unit.clone())
            };
            let cell_qty = |raw, effective, scale| Qty {
                raw,
                effective,
                scale,
                unit: unit.clone(),
                exp: None,
                raw_exp: None,
                ambiguous: false,
                index: None,
                points: false,
            };
            let qty = if thousands > 0 {
                // The format shows the value in thousands, millions...: that is its stated scale.
                let scale = u8::try_from(thousands * 3).unwrap_or(u8::MAX);
                let shown =
                    (0..thousands).try_fold(value, |v, _| v.checked_div(Decimal::ONE_THOUSAND));
                let Some(shown) = shown else {
                    return Ok(vec![]);
                };
                cell_qty(shown, value, scale)
            } else {
                let scale = context.as_ref().map_or(0, |(scale, _)| *scale);
                // A value too large for its scale is not a quantity to compare.
                let Some(effective) = scaled(value, scale) else {
                    return Ok(vec![]);
                };
                cell_qty(value, effective, scale)
            };
            return Ok(vec![qty]);
        }
        Some(CellValue::Date(_) | CellValue::Nothing) => return Ok(vec![]),
        Some(CellValue::Quote) | None => {}
    }
    if let Some(words) = words_disagree(&scan.numbers) {
        return Err((method, "amount_words_mismatch", words));
    }
    Ok(counted(&scan.numbers)
        .filter_map(|(i, n)| Qty::read(n, Some(i), context, scan.exact))
        .collect())
}

pub(super) fn too_long(method: CheckMethod) -> impl Fn(TooLong) -> Stop {
    move |e| {
        (
            method,
            "text_too_long",
            json!({"code_points": e.code_points}),
        )
    }
}

/// The figures of a text that count among its quantities, with their indexes: every `Quantity`
/// that is not an amount in words.
pub(super) fn counted(found: &[Found<Number>]) -> impl Iterator<Item = (usize, &Found<Number>)> {
    found.iter().enumerate().filter(|(_, n)| {
        n.value.role == NumberRole::Quantity && !n.flags.contains(Flags::AMOUNT_IN_WORDS)
    })
}

/// Each amount in words of a text with the figure right before it that it restates.
pub(super) fn amount_words(
    found: &[Found<Number>],
) -> impl Iterator<Item = (&Found<Number>, &Found<Number>)> {
    found
        .iter()
        .enumerate()
        .filter(|(_, n)| n.flags.contains(Flags::AMOUNT_IN_WORDS))
        .filter_map(|(i, words)| {
            let figure = found[..i].iter().rev().find(|n| {
                n.value.role == NumberRole::Quantity && !n.flags.contains(Flags::AMOUNT_IN_WORDS)
            })?;
            Some((figure, words))
        })
}

fn words_disagree(found: &[Found<Number>]) -> Option<Value> {
    amount_words(found)
        .find(|(figure, words)| figure.value.effective() != words.value.effective())
        .map(|(figure, words)| json!({"figure": figure.raw, "words": words.raw}))
}

/// `value` rounded half away from zero to a multiple of 10^`exp` (Excel's rule); unchanged for
/// `None` (an exact value).
pub(super) fn round_to(value: Decimal, exp: Option<i64>) -> Option<Decimal> {
    let Some(exp) = exp else {
        return Some(value);
    };
    let strategy = RoundingStrategy::MidpointAwayFromZero;
    if exp <= 0 {
        let dp = u32::try_from(-exp).ok()?;
        return Some(if dp > 28 {
            value
        } else {
            value.round_dp_with_strategy(dp, strategy)
        });
    }
    let power = scaled(Decimal::ONE, u8::try_from(exp).ok()?)?;
    value
        .checked_div(power)?
        .round_dp_with_strategy(0, strategy)
        .checked_mul(power)
}

/// The significant digits of a value, trailing zeros dropped: 1,250 and 1.25 share them.
fn digits(value: Decimal) -> u128 {
    let mut m = value.normalize().mantissa().unsigned_abs();
    while m != 0 && m.is_multiple_of(10) {
        m /= 10;
    }
    m
}

/// How a source quantity stands against the claim's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rel {
    Equal,
    /// The raw values agree and only one side states the scale or the unit.
    ScaleUnstated,
    /// The claim states a finer value than the source, and they agree at the source's precision.
    PrecisionDiffers,
    UnitMismatch,
    /// The values agree but for their sign.
    SignDiffers,
    Mismatch,
}

fn relate(c: &Qty, s: &Qty) -> Rel {
    let raw_eq = round_to(s.raw, c.raw_exp) == Some(c.raw);
    if c.points != s.points {
        // Percentage points and a percent are other units; the same figure is held open.
        return if raw_eq {
            Rel::ScaleUnstated
        } else {
            Rel::Mismatch
        };
    }
    let units_eq = c.unit == s.unit;
    if units_eq && round_to(s.effective, c.exp) == Some(c.effective) {
        // A claim of one significant digit ("2 พันล้าน") rounds too much of a finer source away.
        let finer = s.exp.is_none_or(|se| c.exp.is_some_and(|ce| se < ce));
        if digits(c.raw) < 10 && finer && s.effective != c.effective {
            return Rel::PrecisionDiffers;
        }
        return Rel::Equal;
    }
    let one_unit = c.unit.is_some() != s.unit.is_some();
    // Exactly one side states a scale.
    let scales_differ = (c.scale == 0) != (s.scale == 0);
    if (scales_differ || one_unit) && (raw_eq || (scales_differ && digits(c.raw) == digits(s.raw)))
    {
        return Rel::ScaleUnstated;
    }
    if units_eq
        && let (Some(ce), Some(se)) = (c.exp, s.exp)
        && se > ce
        && round_to(c.effective, Some(se)) == Some(s.effective)
    {
        return Rel::PrecisionDiffers;
    }
    if c.unit.is_some() && s.unit.is_some() && !units_eq {
        return Rel::UnitMismatch;
    }
    if units_eq && round_to(-s.effective, c.exp) == Some(c.effective) {
        return Rel::SignDiffers;
    }
    Rel::Mismatch
}

/// Words of a gain and of a loss: a source line holding both ("กำไร (ขาดทุน)", "profit
/// (loss)") shows a loss as a negative figure.
const GAIN_WORDS: &[&str] = &["กำไร", "profit", "profits", "income", "gain", "gains"];
const LOSS_WORDS: &[&str] = &["ขาดทุน", "loss", "losses", "deficit"];

/// The words that make a sign-only difference a matter of wording ("a loss of 1,250" against
/// "(1,250)"), not a contradiction.
const SIGN_WORDS: &[&str] = &[
    "ขาดทุน",
    "ติดลบ",
    "ลบ",
    "ลดลง",
    "loss",
    "losses",
    "deficit",
    "negative",
    "decrease",
    "decline",
];

/// Whether `text` holds a word of `list`: a Latin word, or a Thai word of at most 3 code
/// points, as a whole word; a longer Thai word inside the text's match key.
pub(super) fn has_word(text: &str, list: &[&str]) -> bool {
    let words = subject::runs(text);
    let key = match_key(text).text;
    list.iter().any(|w| {
        if w.is_ascii() || w.chars().count() <= 3 {
            words.iter().any(|t| t.text == *w)
        } else {
            key.contains(w)
        }
    })
}

/// The comparison's result before the subject rule.
enum Decision<'a> {
    Equal(&'a Qty),
    /// A contradiction, if the subject agrees and no comparator qualifies the figures.
    Contradicts(&'static str),
    Open(&'static str),
    Absent,
}

fn decide<'a>(c: &Qty, sources: &'a [Qty], sign_words: bool) -> Decision<'a> {
    if c.ambiguous {
        return Decision::Open("number_ambiguous");
    }
    if let Some(s) = sources.iter().find(|s| relate(c, s) == Rel::Equal) {
        return if s.ambiguous {
            Decision::Open("number_ambiguous")
        } else {
            Decision::Equal(s)
        };
    }
    let [s] = sources else {
        return Decision::Absent;
    };
    if s.ambiguous {
        return Decision::Open("number_ambiguous");
    }
    match relate(c, s) {
        Rel::Equal => Decision::Equal(s),
        Rel::ScaleUnstated => Decision::Open("scale_unstated"),
        Rel::PrecisionDiffers => Decision::Open("precision_differs"),
        Rel::UnitMismatch => Decision::Contradicts("unit_mismatch"),
        Rel::SignDiffers if sign_words => Decision::Open("sign_differs"),
        Rel::SignDiffers | Rel::Mismatch => Decision::Contradicts("number_mismatch"),
    }
}

fn outcome(
    method: CheckMethod,
    verdict: CheckVerdict,
    basis: Basis,
    code: &'static str,
    ordinal: Option<u32>,
    detail: Value,
) -> CheckOutcome {
    CheckOutcome {
        method,
        verdict,
        basis,
        code,
        ref_ordinal: ordinal,
        advisory: false,
        detail,
    }
}

pub(super) fn stopped(stop: Stop, ordinal: Option<u32>) -> CheckOutcome {
    let (method, code, detail) = stop;
    outcome(
        method,
        CheckVerdict::Inconclusive,
        Basis::None,
        code,
        ordinal,
        detail,
    )
}

/// A comparison's result: equal (with the basis its support has), or different with the code
/// a contradiction carries and whether a comparator qualifies either side.
pub(super) enum Compared {
    Equal(Basis),
    Differs(&'static str, bool),
}

/// The verdict a comparison gives once the subject rule is applied: an equal figure is
/// `Supported` only when the subject is met (otherwise the subject's own verdict, which may be
/// `period_mismatch`); a differing figure contradicts only when the subject is met and no
/// comparator qualifies either side, and is otherwise held open.
pub(super) fn settle(
    compared: Compared,
    subject: Result<(), subject::Miss>,
    method: CheckMethod,
    ordinal: Option<u32>,
    detail: Value,
) -> CheckOutcome {
    use CheckVerdict::{Contradicted, Inconclusive, Supported};
    let with = |extra: Value| {
        let mut d = detail.clone();
        if let (Some(d), Value::Object(extra)) = (d.as_object_mut(), extra) {
            d.extend(extra);
        }
        d
    };
    match (compared, subject) {
        (Compared::Equal(basis), Ok(())) => {
            outcome(method, Supported, basis, "checked", ordinal, detail)
        }
        (Compared::Equal(_), Err(miss)) => outcome(
            method,
            miss.verdict,
            Basis::None,
            miss.code,
            ordinal,
            with(miss.detail),
        ),
        (_, Err(miss)) => outcome(
            method,
            Inconclusive,
            Basis::None,
            miss.code,
            ordinal,
            with(miss.detail),
        ),
        (Compared::Differs(code, true), Ok(())) => outcome(
            method,
            Inconclusive,
            Basis::None,
            code,
            ordinal,
            with(json!({"reason": "qualified"})),
        ),
        (Compared::Differs(code, false), Ok(())) => {
            outcome(method, Contradicted, Basis::None, code, ordinal, detail)
        }
    }
}

/// Each qualifier of a text with the index (in `numbers`, the text's `find_numbers`) of the
/// first `Quantity` that starts at or after its end, as `qualifier::pair_with_numbers` pairs
/// them.
pub(super) fn pair(
    spans: &[Qualifier],
    numbers: &[Found<Number>],
) -> Vec<(Meaning, Option<usize>)> {
    spans
        .iter()
        .map(|q| {
            let next = numbers
                .iter()
                .position(|n| n.value.role == NumberRole::Quantity && n.start >= q.end);
            (q.meaning, next)
        })
        .collect()
}

/// The comparator meanings that belong to the figure at `index`: those paired with it and those
/// paired with no figure (a postfix comparator such as "or more" pairs with the next figure, so
/// an unpaired one is kept, to fail closed).
fn belonging(paired: &[(Meaning, Option<usize>)], index: usize) -> Vec<Meaning> {
    let mut found: Vec<Meaning> = paired
        .iter()
        .filter(|(_, at)| at.is_none_or(|p| p == index))
        .map(|(m, _)| *m)
        .collect();
    found.sort();
    found
}

fn names(m: &[Meaning]) -> Vec<&'static str> {
    m.iter().map(|m| m.wire()).collect()
}

fn counts_names(counts: &BTreeMap<Meaning, usize>) -> Vec<&'static str> {
    counts
        .iter()
        .flat_map(|(m, n)| std::iter::repeat_n(m.wire(), *n))
        .collect()
}

/// A `Qualifier` check, `qualifier_mismatch`, when the claim and the ref's fields (quote and
/// cell facts) hold a negation or comparator a different number of times.
pub(super) fn field_qualifiers(
    claim: &BTreeMap<Meaning, usize>,
    scan: &Scan,
) -> Option<CheckOutcome> {
    (*claim != scan.counts).then(|| {
        outcome(
            CheckMethod::Qualifier,
            CheckVerdict::Inconclusive,
            Basis::None,
            "qualifier_mismatch",
            Some(scan.ordinal),
            json!({"claim": counts_names(claim), "source": counts_names(&scan.counts)}),
        )
    })
}

/// What every ref of one claim is compared with, read once per claim.
pub(super) struct ClaimSide<'a> {
    pub subject: &'a Subject,
    /// The claim's one quantity and its index in `find_numbers` of the text.
    pub figure: Option<(usize, Qty)>,
    /// How often each negation and comparator occurs in the claim.
    pub counts: BTreeMap<Meaning, usize>,
    /// The comparators that belong to the claim's figure.
    pub comparators: Vec<Meaning>,
    pub sign_words: bool,
    /// A loss word and no gain word.
    pub loss: bool,
}

impl<'a> ClaimSide<'a> {
    pub fn new(
        text: &'a str,
        subject: &'a Subject,
        numbers: &[Found<Number>],
        figure: Option<(usize, Qty)>,
    ) -> ClaimSide<'a> {
        let spans = qualifier::spans(text);
        let mut counts = BTreeMap::new();
        for q in &spans {
            *counts.entry(q.meaning).or_insert(0) += 1;
        }
        let comparators = figure
            .as_ref()
            .map(|(i, _)| belonging(&pair(&spans, numbers), *i))
            .unwrap_or_default();
        ClaimSide {
            subject,
            figure,
            counts,
            comparators,
            sign_words: has_word(text, SIGN_WORDS),
            loss: has_word(text, LOSS_WORDS) && !has_word(text, GAIN_WORDS),
        }
    }
}

/// The number check of the claim's one quantity against one read ref, plus `Qualifier` checks
/// when the ref's fields hold other negations or comparators than the claim, when the figure
/// the claim matched carries other comparators than the claim's, or when a range or a product
/// joins it to another figure.
pub(super) fn check(claim: &ClaimSide, scan: &Scan, ctx: &Ctx) -> Vec<CheckOutcome> {
    let at = Some(scan.ordinal);
    let Some((_, mine)) = &claim.figure else {
        return vec![];
    };
    let sources = match quantities(scan, CheckMethod::Number) {
        Ok(sources) => sources,
        Err(stop) => return vec![stopped(stop, at)],
    };
    // On a profit-or-loss line a loss is the negative figure.
    let profit_or_loss = has_word(&scan.text, GAIN_WORDS) && has_word(&scan.text, LOSS_WORDS);
    let mut value = mine.clone();
    if profit_or_loss && claim.loss && value.effective.is_sign_positive() {
        value.raw = -value.raw;
        value.effective = -value.effective;
    }
    let detail = json!({
        "claim": value.effective.to_string(),
        "source": sources.iter().map(|s| s.effective.to_string()).collect::<Vec<_>>(),
    });
    let sign_words = claim.sign_words || has_word(scan.exact, SIGN_WORDS);
    let qualified = !claim.counts.is_empty() || !scan.counts.is_empty();
    let mut out = Vec::new();
    match decide(&value, &sources, sign_words) {
        Decision::Equal(s) => {
            let segment = s.index.and_then(|i| scan.segment(i));
            let own = match &segment {
                Some((text, _)) => {
                    let mut fields = scan.fields.clone();
                    fields[0] = (text.as_str(), true);
                    match Source::read(&fields, ctx) {
                        Ok(own) => Some(own),
                        Err(e) => return vec![stopped(too_long(CheckMethod::Number)(e), at)],
                    }
                }
                None => None,
            };
            let source = own.as_ref().unwrap_or(&scan.source);
            let met = subject::check(&claim.subject.words, &claim.subject.periods, source, true);
            out.push(settle(
                Compared::Equal(Basis::Exact),
                met,
                CheckMethod::Number,
                at,
                detail,
            ));
            let mismatch = |detail| {
                outcome(
                    CheckMethod::Qualifier,
                    CheckVerdict::Inconclusive,
                    Basis::None,
                    "qualifier_mismatch",
                    at,
                    detail,
                )
            };
            if segment.as_ref().is_some_and(|(_, joined)| *joined) {
                out.push(mismatch(json!({"reason": "range"})));
            }
            if let Some(i) = s.index {
                let theirs = belonging(&scan.paired, i);
                if claim.comparators != theirs {
                    out.push(mismatch(json!({
                        "claim": names(&claim.comparators),
                        "source": names(&theirs),
                    })));
                }
            }
        }
        Decision::Contradicts(code) => {
            let met = subject::check(
                &claim.subject.words,
                &claim.subject.periods,
                &scan.source,
                true,
            );
            out.push(settle(
                Compared::Differs(code, qualified),
                met,
                CheckMethod::Number,
                at,
                detail,
            ));
        }
        Decision::Open(code) => out.push(outcome(
            CheckMethod::Number,
            CheckVerdict::Inconclusive,
            Basis::None,
            code,
            at,
            detail,
        )),
        Decision::Absent => out.push(outcome(
            CheckMethod::Number,
            CheckVerdict::Unsupported,
            Basis::None,
            "number_not_in_source",
            at,
            detail,
        )),
    }
    out.extend(field_qualifiers(&claim.counts, scan));
    out
}
