//! One reading of a ref per claim: its block is scanned once (for `cuts`), its quote once, and
//! the figures, dates, words and qualifiers found are shared by every check of that ref.
use std::collections::BTreeMap;

use katwork_thai::{Ctx, Found, Number, find_dates, find_numbers};
use serde_json::json;

use super::number::{self, Context, Stop};
use super::qualifier::{self, Meaning};
use super::subject::{self, Source};
use super::{CheckMethod, ResolvedRef, boundary, cell};
use crate::docs::CellFacts;

pub(super) struct Scan<'a> {
    pub ordinal: u32,
    pub exact: &'a str,
    pub cell: Option<&'a CellFacts>,
    pub fields: Vec<(&'a str, bool)>,
    /// `find_numbers` of the quote, every role.
    pub numbers: Vec<Found<Number>>,
    pub source: Source,
    pub context: Context,
    /// How often each negation and comparator occurs over every field (quote and cell facts).
    pub counts: BTreeMap<Meaning, usize>,
    /// The quote's qualifiers paired with its figures, as `qualifier::pair_with_numbers` pairs
    /// them.
    pub paired: Vec<(Meaning, Option<usize>)>,
    /// The fields joined, for looking up sign and profit-or-loss words.
    pub text: String,
}

/// A ref read for comparison, or why its figures cannot be compared, first match: a cell fact
/// that bars the value (`Cell`), a quote that cuts a figure or a date, a text too long to scan.
pub(super) fn read<'a>(
    ordinal: u32,
    r: &'a ResolvedRef,
    method: CheckMethod,
    ctx: &Ctx,
) -> Result<Scan<'a>, Stop> {
    let ResolvedRef::Ok { exact, cell, .. } = r else {
        return Err((method, "stale", json!({})));
    };
    if let Some(code) = cell.as_ref().and_then(cell::gate) {
        return Err((CheckMethod::Cell, code, json!({})));
    }
    if boundary::cuts(r, ctx) {
        return Err((method, "quote_cuts_number", json!({"quote": exact})));
    }
    let too_long = number::too_long(method);
    let numbers = find_numbers(exact, ctx).map_err(&too_long)?;
    let dates = find_dates(exact, ctx).map_err(&too_long)?;
    let fields = subject::fields(r);
    let source = Source::read_with(&fields, Some((&numbers, &dates)), ctx).map_err(&too_long)?;
    let mut counts = BTreeMap::new();
    let mut paired = Vec::new();
    for (at, (text, _)) in fields.iter().enumerate() {
        let spans = qualifier::spans(text);
        for q in &spans {
            *counts.entry(q.meaning).or_insert(0) += 1;
        }
        if at == 0 {
            paired = number::pair(&spans, &numbers);
        }
    }
    let text = fields
        .iter()
        .map(|(text, _)| *text)
        .collect::<Vec<_>>()
        .join(" | ");
    Ok(Scan {
        ordinal,
        exact,
        cell: cell.as_ref(),
        context: number::context(r, ctx),
        fields,
        numbers,
        source,
        counts,
        paired,
        text,
    })
}

/// Clause separators: punctuation, and the words that join two clauses.
const SEPARATOR_WORDS: &[&str] = &["และ", "แต่", "while", "whereas"];

/// Text between two figures that joins them into one range or product (`1,250 to 1,300`,
/// `1,250–1,300`, `ถึง`, `between ... and`, `1,250 x 2`).
const JOINERS: &[&str] = &["-", "–", "—", "to", "ถึง", "x", "×", "*"];

impl Scan<'_> {
    /// The part of the quote that belongs to the figure at `index` of [`Scan::numbers`] when the
    /// quote holds several quantities, and whether a range or a product joins it to another
    /// figure (the joined figures share one part). In a quote that opens with words, a figure's
    /// words are those before it back to the previous figure or clause separator, and those
    /// after it up to the next separator unless another figure comes first; in one that opens
    /// with a figure, they are those after it up to the next figure or separator. `None` when
    /// the quote holds one quantity (the whole quote is its part).
    pub fn segment(&self, index: usize) -> Option<(String, bool)> {
        let figures: Vec<(usize, usize, usize)> = number::counted(&self.numbers)
            .map(|(i, n)| (i, n.start as usize, n.end as usize))
            .collect();
        if figures.len() < 2 {
            return None;
        }
        let chars: Vec<char> = self.exact.chars().collect();
        let text = |from: usize, to: usize| -> String {
            chars[from.min(to)..to.min(chars.len())]
                .iter()
                .collect::<String>()
                .trim()
                .to_lowercase()
        };
        let joined = |k: usize| {
            let (_, _, end) = figures[k];
            let (_, next, _) = figures[k + 1];
            let between = text(end, next);
            let opens_range = || {
                let before = text(0, figures[k].1);
                before.ends_with("between") || before.ends_with("ระหว่าง")
            };
            JOINERS.contains(&between.as_str()) || (between == "and" && opens_range())
        };
        let at = figures.iter().position(|(i, _, _)| *i == index)?;
        let (mut lo, mut hi) = (at, at);
        while lo > 0 && joined(lo - 1) {
            lo -= 1;
        }
        while hi + 1 < figures.len() && joined(hi) {
            hi += 1;
        }
        let separators = self.separators(&chars);
        let start = figures[lo].1;
        let end = figures[hi].2;
        let next_separator = separators
            .iter()
            .filter(|(s, _)| *s >= end)
            .map(|(s, _)| *s)
            .min()
            .unwrap_or(chars.len());
        let next_figure = figures.get(hi + 1).map_or(chars.len(), |f| f.1);
        // A quote that opens with words is laid out "label figure": a figure's words come
        // before it. One that opens with a figure is laid out "figure label".
        let label_first = text(0, figures[0].1).chars().any(char::is_alphanumeric);
        let (left, right) = if label_first {
            let previous = lo.checked_sub(1).map_or(0, |p| figures[p].2);
            let left = separators
                .iter()
                .filter(|(_, e)| *e <= start)
                .map(|(_, e)| *e)
                .max()
                .unwrap_or(0)
                .max(previous);
            let right = if next_figure < next_separator {
                end
            } else {
                next_separator
            };
            (left, right)
        } else {
            (start, next_figure.min(next_separator))
        };
        Some((chars[left..right].iter().collect(), lo != hi))
    }

    /// Where clause separators sit in the quote, outside every figure the scan read.
    fn separators(&self, chars: &[char]) -> Vec<(usize, usize)> {
        let inside = |i: usize| {
            self.numbers
                .iter()
                .any(|n| (n.start as usize..n.end as usize).contains(&i))
        };
        let mut out: Vec<(usize, usize)> = chars
            .iter()
            .enumerate()
            .filter(|(i, c)| matches!(c, ',' | ';' | '.' | '\n') && !inside(*i))
            .map(|(i, _)| (i, i + 1))
            .collect();
        let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
        if lower.len() == chars.len() {
            for word in SEPARATOR_WORDS {
                let w: Vec<char> = word.chars().collect();
                let latin = w[0].is_ascii_alphabetic();
                for i in 0..lower.len().saturating_sub(w.len() - 1) {
                    let edge = |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric());
                    if lower[i..].starts_with(&w)
                        && (!latin
                            || (edge(i.checked_sub(1).and_then(|p| lower.get(p)))
                                && edge(lower.get(i + w.len()))))
                    {
                        out.push((i, i + w.len()));
                    }
                }
            }
        }
        out
    }
}
