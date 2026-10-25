//! Thai normalization for matching, with a map from every output code point back to the input.
//!
//! [`normalize_th`] is the only normalization KatWork matches text with. It never applies plain
//! NFKC, which would split SARA AM (U+0E33) into U+0E4D U+0E32, and never removes tone marks.

use std::ops::Range;

use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};
use unicode_segmentation::UnicodeSegmentation;

use crate::digits::fold_digit;

/// The version of these rules. Spans store it beside their `match_key`, so any change to what
/// [`normalize_th`] or [`match_key`] returns needs a new version.
pub const NORMALIZE_VERSION: u32 = 1;

/// Passes after which [`normalize_th`] stops looking for a fixed point.
const MAX_PASSES: u8 = 8;

const NIKHAHIT: char = '\u{0E4D}';
const SARA_AA: char = '\u{0E32}';
const SARA_AM: char = '\u{0E33}';
const SARA_E: char = '\u{0E40}';
const SARA_AE: char = '\u{0E41}';
const LAKKHANGYAO: char = '\u{0E45}';
const RU: char = '\u{0E24}';
const LU: char = '\u{0E26}';

/// Vowels and signs of which a run of repeats keeps one, in PyThaiNLP's order: follow, lead,
/// above and below vowels, then phinthu, thanthakhat, nikhahit and yamakkan.
const NO_REPEAT: [char; 22] = [
    '\u{0E30}', '\u{0E32}', '\u{0E33}', '\u{0E45}', '\u{0E40}', '\u{0E41}', '\u{0E42}', '\u{0E43}',
    '\u{0E44}', '\u{0E31}', '\u{0E34}', '\u{0E35}', '\u{0E36}', '\u{0E37}', '\u{0E4D}', '\u{0E47}',
    '\u{0E38}', '\u{0E39}', '\u{0E3A}', '\u{0E4C}', '\u{0E4D}', '\u{0E4E}',
];

/// A normalized text and the map from its code points back to the input's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalized {
    pub text: String,
    map: OffsetMap,
    /// Passes that changed the text; [`MAX_PASSES`] means the last one still did.
    passes: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OffsetMap {
    /// For each code point of `text`, the input code points it was made from. Neither end ever
    /// decreases along the text; a code point a step deleted lies in no range.
    from: Vec<(u32, u32)>,
    /// The input's grapheme-cluster boundaries in code points, from 0 to the input's length.
    bounds: Vec<u32>,
}

impl Normalized {
    /// The code-point range of the input that the code points `start..end` of `text` came from,
    /// widened to grapheme-cluster boundaries of the input.
    ///
    /// A code point a step deleted belongs to the next kept code point, or to the last kept one
    /// when none follows, so `original_range(0, text_len)` is always the whole input (also when
    /// `text` is empty). An empty or inverted range maps to an empty range at the mapped position,
    /// and an end past the text is clamped. It never panics.
    pub fn original_range(&self, start: usize, end: usize) -> Range<u32> {
        let map = &self.map;
        let len = map.from.len();
        if len == 0 {
            return 0..map.input_len();
        }
        let (start, end) = (start.min(len), end.min(len));
        let lo = map.floor(map.owned_start(start));
        if start >= end {
            return lo..lo;
        }
        let hi = if end == len {
            map.input_len()
        } else {
            map.ceil(map.from[end - 1].1)
        };
        lo..hi.max(lo)
    }

    /// False when the eighth pass still changed the text. The text is then returned as eight
    /// passes left it, never refused: evidence text cannot be rejected.
    pub fn stable(&self) -> bool {
        self.passes < MAX_PASSES
    }

    /// The input range the code points `start..end` were made from, without the deleted code
    /// points around them, widened to grapheme clusters. For the scanners' spans; `start < end`.
    pub(crate) fn source_range(&self, start: usize, end: usize) -> Range<u32> {
        let map = &self.map;
        match (
            map.from.get(start),
            end.checked_sub(1).and_then(|last| map.from.get(last)),
        ) {
            (Some(first), Some(last)) if start < end => map.floor(first.0)..map.ceil(last.1),
            _ => self.original_range(start, end),
        }
    }
}

impl OffsetMap {
    fn input_len(&self) -> u32 {
        self.bounds.last().copied().unwrap_or(0)
    }

    /// Where the input owned by code point `at` of the text starts: right after the previous kept
    /// code point, so the deleted code points between the two belong to `at`.
    fn owned_start(&self, at: usize) -> u32 {
        match (
            at.checked_sub(1).and_then(|prev| self.from.get(prev)),
            self.from.get(at),
        ) {
            (None, _) => 0,
            (Some(prev), Some(this)) => prev.1.min(this.0),
            (Some(_), None) => self.input_len(),
        }
    }

    fn floor(&self, at: u32) -> u32 {
        match self.bounds.binary_search(&at) {
            Ok(_) => at,
            Err(index) => index
                .checked_sub(1)
                .and_then(|i| self.bounds.get(i))
                .copied()
                .unwrap_or(0),
        }
    }

    fn ceil(&self, at: u32) -> u32 {
        match self.bounds.binary_search(&at) {
            Ok(_) => at,
            Err(index) => self
                .bounds
                .get(index)
                .copied()
                .unwrap_or_else(|| self.input_len()),
        }
    }
}

/// A code point on its way through the steps, with the input range it came from.
#[derive(Clone, Copy, Debug)]
struct Ch {
    c: char,
    lo: u32,
    hi: u32,
}

impl Ch {
    fn with(self, c: char) -> Ch {
        Ch { c, ..self }
    }
}

/// The input range a run of code points came from: the first one's start to the last one's end,
/// since neither end decreases along the text.
fn span_of(chars: &[Ch]) -> (u32, u32) {
    match (chars.first(), chars.last()) {
        (Some(first), Some(last)) => (first.lo, last.hi),
        _ => (0, 0),
    }
}

/// Every code point of `chars` with the range of the whole run, so the map never decreases
/// when a step reorders or merges code points.
fn joined<'a>(
    chars: impl Iterator<Item = &'a Ch>,
    (lo, hi): (u32, u32),
) -> impl Iterator<Item = Ch> {
    chars.map(move |ch| Ch { c: ch.c, lo, hi })
}

fn run_end(chars: &[Ch], from: usize, member: impl Fn(char) -> bool) -> usize {
    chars.get(from..).map_or(from, |rest| {
        from + rest.iter().take_while(|ch| member(ch.c)).count()
    })
}

fn offset(index: usize) -> u32 {
    // An input of more than u32::MAX code points saturates its offsets rather than panicking.
    u32::try_from(index).unwrap_or(u32::MAX)
}

/// Normalizes Thai text for matching.
///
/// One pass runs the steps below in order; passes repeat until one changes nothing, at most
/// eight times, and the offset map follows every step of every pass.
///
/// 1. U+FEFF, U+200B, U+200C, U+200D, U+2060 and U+00AD are removed, and so is every other
///    `Default_Ignorable_Code_Point`, U+FFF9–U+FFFB and every control that is not `White_Space`:
///    none of them is seen, so none may split a figure, a date or a word.
/// 2. NFC, chunk by chunk at code points of canonical combining class 0 with `NFC_QC=Yes`.
/// 3. Compatibility forms other than SARA AM: `Zs` to U+0020, full-width ASCII to ASCII, the
///    ligatures U+FB00–U+FB04 and U+FB06 to their letters, U+2019 to `'`, U+2212, U+2012 and
///    U+2013 to `-`, and so the hyphens and minus signs that look the same (U+2010, U+2011,
///    U+FE63, U+02D7, U+2796); nikhahit, tone marks and aa become the tone marks and SARA AM.
/// 4. PyThaiNLP's reordering: `เเ` to `แ`, a tone mark or thanthakhat before an above or below
///    vowel moves after it, nikhahit with tone marks and aa becomes the tone marks and SARA AM, a
///    tone mark after a follow vowel moves before it, `ๅ` after anything but ฤ and ฦ becomes `า`,
///    repeats of a vowel or sign keep one (with the spaces between them), a run of tone marks
///    keeps its last, and marks at the start of the text are dropped.
/// 5. Thai, Arabic-Indic and Extended Arabic-Indic digits become ASCII.
/// 6. A space right before a Thai combining mark is removed, each run of `White_Space` becomes
///    one U+0020, the ends are trimmed, and Latin letters of U+0041–U+024F whose lowercase is one
///    code point are lowercased.
pub fn normalize_th(input: &str) -> Normalized {
    let mut chars: Vec<Ch> = input
        .chars()
        .enumerate()
        .map(|(index, c)| Ch {
            c,
            lo: offset(index),
            hi: offset(index.saturating_add(1)),
        })
        .collect();
    let mut passes = 0;
    while passes < MAX_PASSES {
        let next = pass(&chars);
        if next.iter().map(|ch| ch.c).eq(chars.iter().map(|ch| ch.c)) {
            break;
        }
        chars = next;
        passes += 1;
    }
    Normalized {
        text: chars.iter().map(|ch| ch.c).collect(),
        map: OffsetMap {
            from: chars.iter().map(|ch| (ch.lo, ch.hi)).collect(),
            bounds: grapheme_bounds(input),
        },
        passes,
    }
}

/// One pass of [`normalize_th`], for tests that look for inputs needing many passes.
#[doc(hidden)]
pub fn normalize_pass(input: &str) -> String {
    let chars: Vec<Ch> = input.chars().map(|c| Ch { c, lo: 0, hi: 0 }).collect();
    pass(&chars).iter().map(|ch| ch.c).collect()
}

/// [`normalize_th`] with every U+0020 removed, for containment checks only.
///
/// The offset map still points into the input; each removed space belongs to the code point
/// after it. Never scan a match key for numbers or dates: without its spaces `ปี 2567 1,250`
/// is one run of digits. `find_numbers` and `find_dates` take the original text.
pub fn match_key(input: &str) -> Normalized {
    let mut normalized = normalize_th(input);
    let (text, from) = normalized
        .text
        .chars()
        .zip(&normalized.map.from)
        .filter(|(c, _)| *c != ' ')
        .map(|(c, range)| (c, *range))
        .unzip();
    normalized.text = text;
    normalized.map.from = from;
    normalized
}

fn grapheme_bounds(input: &str) -> Vec<u32> {
    let mut bounds = Vec::new();
    let mut at: usize = 0;
    for cluster in input.graphemes(true) {
        bounds.push(offset(at));
        at = at.saturating_add(cluster.chars().count());
    }
    bounds.push(offset(at));
    bounds
}

fn pass(chars: &[Ch]) -> Vec<Ch> {
    let chars: Vec<Ch> = chars
        .iter()
        .copied()
        .filter(|ch| !is_invisible(ch.c))
        .collect();
    let chars = nfc(&chars);
    let chars = recompose_sara_am(&compatibility(&chars));
    let chars = reorder(&chars);
    let chars: Vec<Ch> = chars
        .iter()
        .map(
            |ch| match fold_digit(ch.c).and_then(|digit| char::from_digit(digit, 10)) {
                Some(ascii) => ch.with(ascii),
                None => *ch,
            },
        )
        .collect();
    tidy(&chars)
}

/// What step 1 removes: the six code points the plan names, every other Default_Ignorable code
/// point, the interlinear annotation controls, and every control that is not White_Space. None
/// of them is seen, so none may split a figure, a date or a word.
pub(crate) fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{FEFF}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{00AD}'
    ) || is_default_ignorable(c)
        || matches!(c, '\u{FFF9}'..='\u{FFFB}')
        || (c.is_control() && !c.is_whitespace())
}

/// Unicode's Default_Ignorable_Code_Point (DerivedCoreProperties.txt).
fn is_default_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// A code point no NFC step crosses: class 0, `NFC_QC=Yes`, so nothing composes with what
/// came before it (UAX #15, section 9.1).
fn starts_nfc_chunk(c: char) -> bool {
    c < '\u{0300}'
        || (canonical_combining_class(c) == 0
            && is_nfc_quick(std::iter::once(c)) == IsNormalized::Yes)
}

fn nfc(chars: &[Ch]) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    let mut start = 0;
    while start < chars.len() {
        let end = run_end(chars, start + 1, |c| !starts_nfc_chunk(c));
        let chunk = &chars[start..end];
        if let [single] = chunk
            && starts_nfc_chunk(single.c)
        {
            // A lone stable code point is its own NFC.
            out.push(*single);
            start = end;
            continue;
        }
        let composed: Vec<char> = chunk.iter().map(|ch| ch.c).nfc().collect();
        if composed.iter().copied().eq(chunk.iter().map(|ch| ch.c)) {
            out.extend_from_slice(chunk);
        } else {
            let (lo, hi) = span_of(chunk);
            out.extend(composed.into_iter().map(|c| Ch { c, lo, hi }));
        }
        start = end;
    }
    out
}

fn is_space_separator(c: char) -> bool {
    matches!(
        c,
        ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

fn compatibility(chars: &[Ch]) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    for ch in chars {
        match ligature(ch.c) {
            Some(letters) => out.extend(letters.chars().map(|letter| ch.with(letter))),
            None => out.push(ch.with(compatible(ch.c))),
        }
    }
    out
}

/// The Latin ligatures U+FB00–U+FB04 and U+FB06, as the letters they join.
fn ligature(c: char) -> Option<&'static str> {
    match c {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// The compatibility mappings step 3 applies to one code point: space separators, full-width
/// ASCII, the right single quotation mark and the dashes that stand for a minus (the plan's
/// U+2212, U+2012 and U+2013, and the hyphens and minus signs that look the same: U+2010,
/// U+2011, U+FE63, U+02D7 and U+2796).
fn compatible(c: char) -> char {
    match c {
        c if is_space_separator(c) => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
        '\u{2019}' => '\'',
        '\u{2212}' | '\u{2012}' | '\u{2013}' | '\u{2010}' | '\u{2011}' | '\u{FE63}'
        | '\u{02D7}' | '\u{2796}' => '-',
        c => c,
    }
}

fn is_tone(c: char) -> bool {
    matches!(c, '\u{0E48}'..='\u{0E4B}')
}

fn is_above_or_below(c: char) -> bool {
    matches!(
        c,
        '\u{0E31}' | '\u{0E34}'..='\u{0E39}' | '\u{0E47}' | NIKHAHIT
    )
}

fn is_follow(c: char) -> bool {
    matches!(c, '\u{0E30}' | SARA_AA | SARA_AM | LAKKHANGYAO)
}

/// The Thai block's combining marks (`Mn`): above and below vowels, phinthu, tone marks,
/// maitaikhu, thanthakhat, nikhahit and yamakkan.
pub(crate) fn is_thai_mark(c: char) -> bool {
    matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}')
}

/// Nikhahit, any tone marks, then aa: the tone marks, then SARA AM.
fn recompose_sara_am(chars: &[Ch]) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(ch) = chars.get(i) {
        if ch.c == NIKHAHIT {
            let aa = run_end(chars, i + 1, is_tone);
            if chars.get(aa).is_some_and(|next| next.c == SARA_AA) {
                let range = span_of(&chars[i..=aa]);
                out.extend(joined(chars[i + 1..aa].iter(), range));
                out.push(Ch {
                    c: SARA_AM,
                    lo: range.0,
                    hi: range.1,
                });
                i = aa + 1;
                continue;
            }
        }
        out.push(*ch);
        i += 1;
    }
    out
}

/// Each run of `first` right before a run of `second` is moved after it.
fn swap_runs(chars: &[Ch], first: fn(char) -> bool, second: fn(char) -> bool) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(ch) = chars.get(i) {
        if !first(ch.c) {
            out.push(*ch);
            i += 1;
            continue;
        }
        let middle = run_end(chars, i, first);
        let end = run_end(chars, middle, second);
        if end > middle {
            let range = span_of(&chars[i..end]);
            out.extend(joined(
                chars[middle..end].iter().chain(&chars[i..middle]),
                range,
            ));
        } else {
            out.extend_from_slice(&chars[i..middle]);
        }
        i = end;
    }
    out
}

/// Step 4: PyThaiNLP's `reorder_vowels`, then its repeat and tone rules, then marks at the start.
fn reorder(chars: &[Ch]) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(ch) = chars.get(i) {
        if ch.c == SARA_E && chars.get(i + 1).is_some_and(|next| next.c == SARA_E) {
            let (lo, hi) = span_of(&chars[i..i + 2]);
            out.push(Ch { c: SARA_AE, lo, hi });
            i += 2;
        } else {
            out.push(*ch);
            i += 1;
        }
    }
    let out = swap_runs(&out, |c| is_tone(c) || c == '\u{0E4C}', is_above_or_below);
    let out = recompose_sara_am(&out);
    let out = swap_runs(&out, is_follow, is_tone);
    let mut chars = lakkhangyao(&out);
    for mark in NO_REPEAT {
        chars = collapse_repeats(&chars, mark);
    }
    let mut chars = last_tone(&chars);
    let leading = chars.iter().take_while(|ch| is_thai_mark(ch.c)).count();
    chars.drain(..leading);
    chars
}

/// `ๅ` after any code point but ฤ and ฦ becomes `า`; the code point before it is consumed, so
/// of `กๅๅ` only the first `ๅ` changes in one pass.
fn lakkhangyao(chars: &[Ch]) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(ch) = chars.get(i) {
        match chars.get(i + 1) {
            Some(next) if next.c == LAKKHANGYAO && ch.c != RU && ch.c != LU => {
                out.push(*ch);
                out.push(next.with(SARA_AA));
                i += 2;
            }
            _ => {
                out.push(*ch);
                i += 1;
            }
        }
    }
    out
}

/// Two or more of `mark`, with only U+0020 between them, become one `mark`.
fn collapse_repeats(chars: &[Ch], mark: char) -> Vec<Ch> {
    if !chars.iter().any(|ch| ch.c == mark) {
        return chars.to_vec();
    }
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(ch) = chars.get(i) {
        if ch.c != mark {
            out.push(*ch);
            i += 1;
            continue;
        }
        let mut last = i;
        loop {
            let next = run_end(chars, last + 1, |c| c == ' ');
            match chars.get(next) {
                Some(repeat) if repeat.c == mark => last = next,
                _ => break,
            }
        }
        let (lo, hi) = span_of(&chars[i..=last]);
        out.push(Ch { c: mark, lo, hi });
        i = last + 1;
    }
    out
}

fn last_tone(chars: &[Ch]) -> Vec<Ch> {
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(ch) = chars.get(i) {
        if is_tone(ch.c) {
            let end = run_end(chars, i, is_tone);
            let (lo, hi) = span_of(&chars[i..end]);
            out.push(Ch {
                c: chars[end - 1].c,
                lo,
                hi,
            });
            i = end;
        } else {
            out.push(*ch);
            i += 1;
        }
    }
    out
}

/// Step 6: spaces before marks, white space, trimming and Latin lowercase.
fn tidy(chars: &[Ch]) -> Vec<Ch> {
    let kept: Vec<Ch> = chars
        .iter()
        .enumerate()
        .filter(|(i, ch)| {
            !(ch.c == ' ' && chars.get(i + 1).is_some_and(|next| is_thai_mark(next.c)))
        })
        .map(|(_, ch)| *ch)
        .collect();
    let mut out: Vec<Ch> = Vec::with_capacity(kept.len());
    let mut i = 0;
    while let Some(ch) = kept.get(i) {
        if ch.c.is_whitespace() {
            let end = run_end(&kept, i, char::is_whitespace);
            let (lo, hi) = span_of(&kept[i..end]);
            out.push(Ch { c: ' ', lo, hi });
            i = end;
        } else {
            out.push(ch.with(lower_latin(ch.c)));
            i += 1;
        }
    }
    if out.last().is_some_and(|ch| ch.c == ' ') {
        out.pop();
    }
    if out.first().is_some_and(|ch| ch.c == ' ') {
        out.remove(0);
    }
    out
}

fn lower_latin(c: char) -> char {
    if !('\u{0041}'..='\u{024F}').contains(&c) {
        return c;
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run of tone marks costs pythainlp_compat linear work: doubling the run at most doubles
    /// the scans (plus a little).
    #[test]
    fn pythainlp_compat_is_linear_on_a_tone_mark_run() {
        let work_of = |marks: usize| {
            let text = format!("\u{0E01}{}", "\u{0E48}".repeat(marks));
            crate::found::WORK.with(|work| work.set(0));
            assert!(!pythainlp_compat(&text).is_empty());
            crate::found::WORK.with(|work| work.get())
        };
        let (small, large) = (work_of(10_000), work_of(20_000));
        assert!(large <= small * 22 / 10, "{small} then {large}");
    }

    #[test]
    fn code_points_below_u0300_start_nfc_chunks() {
        for c in (0..0x300).filter_map(char::from_u32) {
            assert_eq!(canonical_combining_class(c), 0, "{c:?}");
            assert_eq!(is_nfc_quick(std::iter::once(c)), IsNormalized::Yes, "{c:?}");
        }
    }
}

/// A line-by-line port of PyThaiNLP 5.4.0's `pythainlp.util.normalize`, for the oracle
/// fixture only.
///
/// It keeps PyThaiNLP's order and rules and shares no helper with [`normalize_th`]: it removes
/// only U+200B and U+200C, strips the ends as Python's `str.strip()` does, removes a space
/// before a mark only after a consonant that no vowel precedes, and drops marks that follow a
/// space. Ported from `pythainlp/util/normalize.py` (Apache-2.0).
#[doc(hidden)]
pub fn pythainlp_compat(input: &str) -> String {
    let text: Vec<char> = input.chars().collect();
    let text = pythainlp::remove_zw(text);
    let text = pythainlp::remove_dup_spaces(&text);
    let text = pythainlp::remove_spaces_before_marks(&text);
    let text = pythainlp::remove_repeat_vowels(text);
    pythainlp::remove_dangling(&text).into_iter().collect()
}

mod pythainlp {
    //! PyThaiNLP's character classes (`pythainlp/__init__.py`) and the five functions of
    //! `normalize`, each regular expression written out as one left-to-right scan that never
    //! overlaps a match, as Python's `re.sub` does.

    fn is_consonant(c: char) -> bool {
        matches!(c, '\u{0E01}'..='\u{0E2E}') && c != '\u{0E24}' && c != '\u{0E26}'
    }

    /// `thai_vowels`: ฤ, ฦ, U+0E30–U+0E39, U+0E40–U+0E45, nikhahit and maitaikhu.
    fn is_vowel(c: char) -> bool {
        matches!(c, '\u{0E24}' | '\u{0E26}' | '\u{0E30}'..='\u{0E39}' | '\u{0E40}'..='\u{0E45}' | '\u{0E4D}' | '\u{0E47}')
    }

    fn is_tonemark(c: char) -> bool {
        matches!(c, '\u{0E48}'..='\u{0E4B}')
    }

    /// `thai_above_vowels` and `thai_below_vowels`.
    fn is_above_below(c: char) -> bool {
        matches!(
            c,
            '\u{0E31}' | '\u{0E34}'
                ..='\u{0E37}' | '\u{0E4D}' | '\u{0E47}' | '\u{0E38}' | '\u{0E39}'
        )
    }

    fn is_follow(c: char) -> bool {
        matches!(c, '\u{0E30}' | '\u{0E32}' | '\u{0E33}' | '\u{0E45}')
    }

    /// `_DANGLING_CHARS`: above and below vowels, tone marks, phinthu, thanthakhat, nikhahit,
    /// yamakkan.
    fn is_dangling(c: char) -> bool {
        is_above_below(c)
            || is_tonemark(c)
            || matches!(c, '\u{0E3A}' | '\u{0E4C}' | '\u{0E4D}' | '\u{0E4E}')
    }

    /// `_NOREPEAT_CHARS`, in its order.
    const NOREPEAT: [char; 22] = [
        '\u{0E30}', '\u{0E32}', '\u{0E33}', '\u{0E45}', '\u{0E40}', '\u{0E41}', '\u{0E42}',
        '\u{0E43}', '\u{0E44}', '\u{0E31}', '\u{0E34}', '\u{0E35}', '\u{0E36}', '\u{0E37}',
        '\u{0E4D}', '\u{0E47}', '\u{0E38}', '\u{0E39}', '\u{0E3A}', '\u{0E4C}', '\u{0E4D}',
        '\u{0E4E}',
    ];

    /// Python's `str.isspace()`, which `str.strip()` uses: `Zs` and the bidirectional classes
    /// WS, B and S, so U+001C–U+001F count and U+200B does not.
    fn is_python_space(c: char) -> bool {
        matches!(
            c,
            '\u{0009}'..='\u{000D}'
                | '\u{001C}'..='\u{0020}'
                | '\u{0085}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
        )
    }

    fn end_of(text: &[char], from: usize, member: impl Fn(char) -> bool) -> usize {
        text.get(from..).map_or(from, |rest| {
            let run = rest.iter().take_while(|c| member(**c)).count();
            crate::found::work(run + 1);
            from + run
        })
    }

    /// `remove_zw`: every U+200B and U+200C.
    pub(super) fn remove_zw(mut text: Vec<char>) -> Vec<char> {
        text.retain(|c| !matches!(c, '\u{200B}' | '\u{200C}'));
        text
    }

    /// `remove_dup_spaces`: `"  "` to `" "` until none is left, `[ \n]*\n[ \n]*` to `"\n"`,
    /// then `str.strip()`.
    pub(super) fn remove_dup_spaces(text: &[char]) -> Vec<char> {
        let mut spaced: Vec<char> = Vec::with_capacity(text.len());
        for &c in text {
            if !(c == ' ' && spaced.last() == Some(&' ')) {
                spaced.push(c);
            }
        }
        let mut lines = Vec::with_capacity(spaced.len());
        let mut i = 0;
        while let Some(&c) = spaced.get(i) {
            if c == ' ' || c == '\n' {
                let end = end_of(&spaced, i, |c| c == ' ' || c == '\n');
                if spaced[i..end].contains(&'\n') {
                    lines.push('\n');
                } else {
                    lines.extend_from_slice(&spaced[i..end]);
                }
                i = end;
            } else {
                lines.push(c);
                i += 1;
            }
        }
        let start = lines.iter().take_while(|c| is_python_space(**c)).count();
        let end = lines.len()
            - lines[start..]
                .iter()
                .rev()
                .take_while(|c| is_python_space(**c))
                .count();
        lines[start..end].to_vec()
    }

    /// `([consonant])(?<![vowel][consonant]) ([dangling])` to `\1\2`: the look-behind reads the
    /// text as given, so a consonant after a vowel keeps its space.
    pub(super) fn remove_spaces_before_marks(text: &[char]) -> Vec<char> {
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            let after_vowel = i
                .checked_sub(1)
                .and_then(|prev| text.get(prev))
                .is_some_and(|p| is_vowel(*p));
            match (text.get(i + 1), text.get(i + 2)) {
                (Some(' '), Some(&mark))
                    if is_consonant(c) && !after_vowel && is_dangling(mark) =>
                {
                    out.push(c);
                    out.push(mark);
                    i += 3;
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }

    /// `(run of first)(run of second)` to `\2\1`.
    fn swap(text: &[char], first: fn(char) -> bool, second: fn(char) -> bool) -> Vec<char> {
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            if !first(c) {
                out.push(c);
                i += 1;
                continue;
            }
            let middle = end_of(text, i, first);
            let end = end_of(text, middle, second);
            out.extend_from_slice(&text[middle..end]);
            out.extend_from_slice(&text[i..middle]);
            i = end;
        }
        out
    }

    /// `reorder_vowels`: its five substitutions, each over the whole text, in order.
    fn reorder_vowels(text: &[char]) -> Vec<char> {
        let mut pairs = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            if c == '\u{0E40}' && text.get(i + 1) == Some(&'\u{0E40}') {
                pairs.push('\u{0E41}');
                i += 2;
            } else {
                pairs.push(c);
                i += 1;
            }
        }
        let text = swap(
            &pairs,
            |c| is_tonemark(c) || c == '\u{0E4C}',
            is_above_below,
        );
        let mut am = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            // The nikhahit is tested first, so a run of tone marks is scanned once, not once per mark.
            let aa = if c == '\u{0E4D}' {
                end_of(&text, i + 1, is_tonemark)
            } else {
                i
            };
            if c == '\u{0E4D}' && text.get(aa) == Some(&'\u{0E32}') {
                am.extend_from_slice(&text[i + 1..aa]);
                am.push('\u{0E33}');
                i = aa + 1;
            } else {
                am.push(c);
                i += 1;
            }
        }
        let text = swap(&am, is_follow, is_tonemark);
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            if text.get(i + 1) == Some(&'\u{0E45}') && c != '\u{0E24}' && c != '\u{0E26}' {
                out.push(c);
                out.push('\u{0E32}');
                i += 2;
            } else {
                out.push(c);
                i += 1;
            }
        }
        out
    }

    /// `remove_repeat_vowels`: `reorder_vowels`, then `(ch[ ]*)+ch` to `ch` for each repeat
    /// character, then each run of tone marks to its last.
    pub(super) fn remove_repeat_vowels(text: Vec<char>) -> Vec<char> {
        let mut text = reorder_vowels(&text);
        for ch in NOREPEAT {
            let mut out = Vec::with_capacity(text.len());
            let mut i = 0;
            while let Some(&c) = text.get(i) {
                if c != ch {
                    out.push(c);
                    i += 1;
                    continue;
                }
                let mut last = i;
                loop {
                    let next = end_of(&text, last + 1, |c| c == ' ');
                    if text.get(next) != Some(&ch) {
                        break;
                    }
                    last = next;
                }
                out.push(ch);
                i = last + 1;
            }
            text = out;
        }
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            if is_tonemark(c) {
                let end = end_of(&text, i, is_tonemark);
                out.push(text[end - 1]);
                i = end;
            } else {
                out.push(c);
                i += 1;
            }
        }
        out
    }

    /// `remove_dangling`: `^[dangling]+` to nothing, then ` +[dangling]+` to `" "`.
    pub(super) fn remove_dangling(text: &[char]) -> Vec<char> {
        let start = text.iter().take_while(|c| is_dangling(**c)).count();
        let text = &text[start..];
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            if c != ' ' {
                out.push(c);
                i += 1;
                continue;
            }
            let marks = end_of(text, i, |c| c == ' ');
            let end = end_of(text, marks, is_dangling);
            if end > marks {
                out.push(' ');
            } else {
                out.extend_from_slice(&text[i..marks]);
            }
            i = end;
        }
        out
    }
}
