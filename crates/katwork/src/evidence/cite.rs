//! The `cite` tool (`WorkspaceRead`, `Untrusted`): a Kat names a span the harness minted and,
//! optionally, a quote inside it; the harness finds the quote once, widens it to whole figures
//! and dates, narrows the span's ref to it and stores a citation for the job (ADR-0007).
use std::{borrow::Cow, ops::Range};

use async_trait::async_trait;
use katwork_thai::{Ctx, TooLong, find_dates, find_numbers, match_key};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sqlx::Connection;
use uuid::Uuid;

use super::{
    scan_ctx,
    source_ref::{Selector, SourceRef},
    span::{self, SpanRow},
};
use crate::{
    agent::{Tool, ToolCtx, ToolError, ToolOutput},
    artifacts::{
        store,
        tools::{
            from_kat,
            page::{PAGE_SCALARS, line, scalars, shown},
        },
    },
    db,
    error::KatError,
    fence,
    ids::{CitationId, SpanId},
    models::ToolSpec,
    types::SideEffect,
};

/// The longest quote, in code points.
pub const MAX_QUOTE_CHARS: usize = 2_000;

/// Extends `start..end` (code points of `text`) until no number and no date that `find_numbers`
/// or `find_dates` reports is cut: every token that overlaps the range without lying inside it
/// is taken whole, repeated until nothing changes. A token that only touches the range is not
/// taken.
pub fn widen_to_tokens(text: &str, start: u32, end: u32, ctx: &Ctx) -> Result<(u32, u32), TooLong> {
    let numbers = find_numbers(text, ctx)?;
    let dates = find_dates(text, ctx)?;
    let tokens: Vec<(u32, u32)> = numbers
        .iter()
        .map(|f| (f.start, f.end))
        .chain(dates.iter().map(|f| (f.start, f.end)))
        .collect();
    Ok(widen_over(&tokens, start, end))
}

/// The fixed point of `widen_to_tokens` over token ranges in any order.
fn widen_over(tokens: &[(u32, u32)], start: u32, end: u32) -> (u32, u32) {
    let (mut start, mut end) = (start, end);
    loop {
        let mut changed = false;
        for &(t_start, t_end) in tokens {
            let overlaps = t_start < end && t_end > start;
            if overlaps && (t_start < start || t_end > end) {
                start = start.min(t_start);
                end = end.max(t_end);
                changed = true;
            }
        }
        if !changed {
            return (start, end);
        }
    }
}

/// Where `needle` occurs in `hay`, counting overlapping occurrences (`"aa"` in `"aaa"` is two).
#[derive(Debug, PartialEq, Eq)]
enum Hit {
    None,
    /// The occurrence's first code point.
    Once(usize),
    Many,
}

fn find_once(hay: &str, needle: &str) -> Hit {
    debug_assert!(!needle.is_empty());
    let mut found = None;
    let mut from = 0;
    while let Some(i) = hay[from..].find(needle) {
        let at = from + i;
        if found.is_some() {
            return Hit::Many;
        }
        found = Some(at);
        from = at + hay[at..].chars().next().map_or(1, char::len_utf8);
    }
    match found {
        Some(byte) => Hit::Once(hay[..byte].chars().count()),
        None => Hit::None,
    }
}

/// The code-point range of `span_text` the quote selects, before widening.
fn quote_range(span_text: &str, quote: &str) -> Result<Range<u32>, ToolError> {
    let quote_key = match_key(quote);
    // A quote of spaces and invisible characters only matches nothing a person could read.
    if quote_key.text.is_empty() {
        return Err(ToolError::Coded("quote_not_in_span"));
    }
    // The key is recomputed from the span's text, never read from the stored `match_key`, so
    // a normaliser version change moves which quotes match, never an offset.
    let span_key = match_key(span_text);
    match find_once(&span_key.text, &quote_key.text) {
        Hit::None => Err(ToolError::Coded("quote_not_in_span")),
        Hit::Many => Err(ToolError::Coded("quote_ambiguous")),
        Hit::Once(at) => Ok(span_key.source_range(at, at + quote_key.text.chars().count())),
    }
}

/// The narrowed ref's quote and position.
fn narrowed_quote(r: &SourceRef) -> Option<(String, u32, u32)> {
    let mut node = &r.selector;
    loop {
        let Selector::FragmentSelector { refined_by, .. } = node else {
            return None;
        };
        match refined_by.as_slice() {
            [next @ Selector::FragmentSelector { .. }] => node = next,
            [
                Selector::TextQuoteSelector { exact, .. },
                Selector::TextPositionSelector { start, end, .. },
            ] => return Some((exact.clone(), *start, *end)),
            _ => return None,
        }
    }
}

/// What `cite` stores for a span and an optional quote, computed without the database.
struct Planned {
    source_ref: SourceRef,
    exact: String,
    start: u32,
    end: u32,
}

fn plan(span: &SpanRow, quote: Option<&str>) -> Result<Planned, ToolError> {
    if span.text.is_empty() {
        return Err(ToolError::Coded("quote_not_in_span"));
    }
    let range = match quote {
        Some(quote) => quote_range(&span.text, quote)?,
        None => 0..span.text.chars().count() as u32,
    };
    let (start, end) = widen_to_tokens(&span.text, range.start, range.end, &scan_ctx())
        .map_err(|_| ToolError::Coded("text_too_long"))?;
    let source_ref = span.source_ref.narrow_text(&span.text, start, end);
    let checked = narrowed_quote(&source_ref)
        .filter(|(exact, _, _)| !exact.is_empty())
        .filter(|_| source_ref.validate_against(&span.text).is_ok());
    let Some((exact, start, end)) = checked else {
        tracing::error!(span_id = %span.id, "cite_ref_invalid");
        return Err(ToolError::Failed("internal_error".into()));
    };
    Ok(Planned {
        source_ref,
        exact,
        start,
        end,
    })
}

/// `span_unknown` for every way a span can be out of reach; the rest as the read tools map it.
fn from_kat_cite(error: KatError) -> ToolError {
    match error {
        KatError::NotFound => ToolError::Coded("span_unknown"),
        other => from_kat(other),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CiteArgs {
    span_id: Uuid,
    #[serde(default, deserialize_with = "present")]
    quote: Option<String>,
}

/// A `quote` that is present must be a string: `null` is refused, not read as "no quote".
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

fn cite_args(args: Value) -> Result<CiteArgs, ToolError> {
    let args: CiteArgs =
        serde_json::from_value(args).map_err(|_| ToolError::InvalidArgs("cite".into()))?;
    if let Some(quote) = &args.quote {
        let chars = quote.chars().count();
        if chars == 0 || chars > MAX_QUOTE_CHARS {
            return Err(ToolError::InvalidArgs("quote".into()));
        }
    }
    Ok(args)
}

#[derive(Serialize)]
struct CiteLine<'a> {
    citation_id: CitationId,
    /// `null` for a hidden span: its text never reaches the model.
    exact: Option<Cow<'a, str>>,
    #[serde(rename = "where")]
    at: Cow<'a, str>,
    generated: bool,
    hidden: bool,
    instruction_like: Option<&'a str>,
    suspect: Option<&'a str>,
    /// `exact` was cut to keep the line under the page cap.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    cut: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<&'static str>,
}

pub struct Cite;

#[async_trait]
impl Tool for Cite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "cite".into(),
            description: "Cite a span that `read_file` showed you, by its `span` id, and \
                optionally a quote copied from that span's text. The quote must occur exactly \
                once in the span; if it occurs more than once, lengthen it. A quote that cuts a \
                number or a date is widened to the whole figure. Without a quote the whole span \
                is cited. Returns a `citation_id`: put citation ids in a claim's `cite` list, \
                never a page, cell or quote you typed yourself."
                .into(),
            parameters: json!({"type":"object","properties":{
                "span_id":{"type":"string","format":"uuid"},
                "quote":{"type":"string","minLength":1,"maxLength":MAX_QUOTE_CHARS}},
                "required":["span_id"],"additionalProperties":false}),
        }
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::WorkspaceRead
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let args = cite_args(args)?;
        let span_id = SpanId(args.span_id);
        // A read outside any transaction names the span's artifact, so the transaction below
        // can lock it before it reads the span again.
        let span = db::guarded(&ctx.pool, async |conn| {
            span::get_in_space_on(conn, &ctx.tenant, ctx.space_id, span_id).await
        })
        .await
        .map_err(from_kat_cite)?;
        let planned = plan(&span, args.quote.as_deref())?;
        let display = planned.source_ref.display(&span.file_name);
        // `to_string` keeps each `f32` in its shortest text, which jsonb stores exactly.
        let ref_json = serde_json::to_string(&planned.source_ref)
            .map_err(|_| ToolError::Failed("internal_error".into()))?;
        let id = db::guarded(&ctx.pool, async |conn| {
            let mut tx = conn.begin().await?;
            db::set_lock_timeout(&mut tx, ctx.lock_timeout).await?;
            // Lock order: the task row (the fence), then the artifact row.
            fence::check(&mut tx, &ctx.fence()).await?;
            store::lock_source_on(&mut tx, &ctx.tenant, &[span.artifact_id]).await?;
            // Under the lock: the file may have been deleted or moved to another Space since
            // the first read.
            let current =
                span::get_in_space_on(&mut tx, &ctx.tenant, ctx.space_id, span_id).await?;
            if current.artifact_id != span.artifact_id || current.text != span.text {
                return Err(KatError::NotFound);
            }
            let inserted: Option<CitationId> = sqlx::query_scalar(
                "INSERT INTO citations (id, workspace_id, job_id, run_id, span_id, source_ref,
                     exact, start_cp, end_cp, display)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10)
                 ON CONFLICT ON CONSTRAINT citations_range_key DO NOTHING
                 RETURNING id",
            )
            .bind(CitationId::new())
            .bind(ctx.tenant.workspace_id())
            .bind(ctx.job_id)
            .bind(ctx.run_id)
            .bind(span_id)
            .bind(&ref_json)
            .bind(&planned.exact)
            .bind(i32::try_from(planned.start).map_err(|_| KatError::RequestInvalid)?)
            .bind(i32::try_from(planned.end).map_err(|_| KatError::RequestInvalid)?)
            .bind(&display)
            .fetch_optional(&mut *tx)
            .await?;
            let id = match inserted {
                Some(id) => id,
                // A new statement sees a concurrent winner the INSERT's snapshot could not.
                None => {
                    sqlx::query_scalar(
                        "SELECT id FROM citations
                         WHERE workspace_id = $1 AND job_id = $2 AND span_id = $3
                           AND start_cp = $4 AND end_cp = $5",
                    )
                    .bind(ctx.tenant.workspace_id())
                    .bind(ctx.job_id)
                    .bind(span_id)
                    .bind(planned.start as i32)
                    .bind(planned.end as i32)
                    .fetch_one(&mut *tx)
                    .await?
                }
            };
            tx.commit().await?;
            Ok(id)
        })
        .await
        .map_err(from_kat_cite)?;
        Ok(ToolOutput::Text(result_line(id, &span, &planned)))
    }
}

/// The result line, under the read tools' page cap: an `exact` whose escaped form would push
/// the line over it is cut at a code point (never inside an escape) and marked `cut`; the
/// citation holds all of it.
fn result_line(id: CitationId, span: &SpanRow, planned: &Planned) -> String {
    let hidden = span.hidden.is_some();
    let locator = planned.source_ref.locator();
    let (at, at_escaped) = shown(&locator);
    let total = planned.exact.chars().count();
    let render = |keep: usize| -> String {
        let prefix: String = planned.exact.chars().take(keep).collect();
        let (exact, exact_escaped) = shown(&prefix);
        line(&CiteLine {
            citation_id: id,
            exact: (!hidden).then_some(exact),
            at: at.clone(),
            generated: span.generated,
            hidden,
            instruction_like: span.instruction_like.as_deref(),
            suspect: span.suspect_text_layer.as_deref(),
            cut: keep < total,
            warning: (at_escaped || (exact_escaped && !hidden))
                .then_some("invisible_characters_escaped"),
        })
    };
    let fits = |text: &String| scalars(text) < PAGE_SCALARS;
    let whole = render(total);
    if fits(&whole) {
        return whole;
    }
    // The longest prefix whose line fits: `lo` always fits (an empty `exact` does, since the
    // locator is short), `hi` never does.
    let (mut lo, mut hi) = (0, total);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(&render(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    render(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cps(text: &str, needle: &str) -> (u32, u32) {
        let byte = text.find(needle).unwrap();
        let start = text[..byte].chars().count() as u32;
        (start, start + needle.chars().count() as u32)
    }

    fn widened(text: &str, quote: &str) -> String {
        let (start, end) = cps(text, quote);
        let (start, end) = widen_to_tokens(text, start, end, &scan_ctx()).unwrap();
        text.chars()
            .skip(start as usize)
            .take((end - start) as usize)
            .collect()
    }

    #[test]
    fn widen_takes_whole_numbers_and_dates() {
        for (text, quote, expected) in [
            ("งบประมาณ 12,500 บาท", "งบประมาณ 12", "งบประมาณ 12,500"),
            (
                "11,250 baht total revenue",
                "1,250 baht total revenue",
                "11,250 baht total revenue",
            ),
            (
                "11 ต.ค. 2568 ประชุม",
                "1 ต.ค. 2568 ประชุม",
                "11 ต.ค. 2568 ประชุม",
            ),
            ("รายได้ 1.25 พันล้านบาท", "รายได้ 1.2", "รายได้ 1.25 พันล้าน"),
            ("รายได้รวม 1,250 ล้านบาท", "รายได้รวม", "รายได้รวม"),
        ] {
            assert_eq!(widened(text, quote), expected, "{text} / {quote}");
        }
    }

    #[test]
    fn widen_never_takes_a_token_that_only_touches_the_range() {
        // The quote ends right where the figure starts (no space between them).
        let text = "ยอดรวม1,250";
        assert_eq!(widened(text, "ยอดรวม"), "ยอดรวม");
    }

    #[test]
    fn widen_a_cut_year_takes_the_whole_date() {
        assert_eq!(
            widened("ประชุม 11 ต.ค. 2568 แล้ว", "68 แล้ว"),
            "11 ต.ค. 2568 แล้ว"
        );
    }

    #[test]
    fn widen_signs_parentheses_words_scales_and_thai_digits() {
        for (text, quote, expected) in [
            ("ขาดทุน (1,250) บาท", "1,2", "(1,250)"),
            ("ขาดทุน -1,250 บาท", "250 บาท", "-1,250 บาท"),
            ("ยอด ๑,๒๕๐ บาท", "๒๕๐", "๑,๒๕๐"),
            ("รวม 1.25 พันล้าน บาท", "1.25 พัน", "1.25 พันล้าน"),
            ("รวมสามสิบห้าคน", "สามสิบ", "สามสิบห้า"),
        ] {
            assert_eq!(widened(text, quote), expected, "{text} / {quote}");
        }
    }

    #[test]
    fn widen_repeats_until_no_token_is_cut() {
        // The second token overlaps the range only once the first one is taken.
        assert_eq!(widen_over(&[(4, 9), (8, 12)], 10, 14), (4, 14));
        assert_eq!(widen_over(&[(0, 3), (2, 6), (5, 9)], 8, 10), (0, 10));
        // Touching tokens stay out; tokens inside stay inside.
        assert_eq!(widen_over(&[(0, 3), (5, 6), (9, 12)], 3, 9), (3, 9));
    }

    #[test]
    fn widen_refuses_a_text_over_the_scan_limit() {
        let text = "ก".repeat(katwork_thai::MAX_SCAN_CODE_POINTS + 1);
        assert!(widen_to_tokens(&text, 0, 1, &scan_ctx()).is_err());
    }

    #[test]
    fn find_once_counts_overlaps_and_returns_code_points() {
        assert_eq!(find_once("aaa", "aa"), Hit::Many);
        assert_eq!(find_once("abab", "ab"), Hit::Many);
        assert_eq!(find_once("กขคab", "ab"), Hit::Once(3));
        assert_eq!(find_once("abc", "x"), Hit::None);
    }

    #[test]
    fn quote_range_excludes_removed_spaces_around_the_hit() {
        let text = "ปี 2567 รายได้รวม ๑,๒๕๐ ล้านบาท  ";
        let range = quote_range(text, "รายได้รวม 1,250 ล้านบาท").unwrap();
        let got: String = text
            .chars()
            .skip(range.start as usize)
            .take((range.end - range.start) as usize)
            .collect();
        assert_eq!(got, "รายได้รวม ๑,๒๕๐ ล้านบาท");
    }

    #[test]
    fn quote_args_are_checked() {
        let id = Uuid::now_v7();
        for args in [
            json!({"span_id": id, "quote": ""}),
            json!({"span_id": id, "quote": "ก".repeat(MAX_QUOTE_CHARS + 1)}),
            json!({"span_id": id, "quote": null}),
            json!({"span_id": id, "quote": 5}),
            json!({"span_id": id, "extra": 1}),
            json!({"span_id": "not-a-uuid"}),
            json!({"quote": "x"}),
        ] {
            assert_eq!(
                cite_args(args.clone()).err().map(|e| e.code()),
                Some("invalid_args"),
                "{args}"
            );
        }
        assert!(cite_args(json!({"span_id": id, "quote": "ก".repeat(MAX_QUOTE_CHARS)})).is_ok());
        assert!(cite_args(json!({"span_id": id})).unwrap().quote.is_none());
    }
}
