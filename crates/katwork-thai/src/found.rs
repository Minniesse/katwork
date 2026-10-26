//! What the scanners return (a value with its span, confidence and flags), and the plumbing
//! they share: the length limit and the normalized view of a text they match on.

use std::fmt;
use std::ops::Range;

use crate::digits::{Script, script};

/// The longest text `find_numbers` and `find_dates` scan, in code points.
///
/// A longer text is refused whole with [`TooLong`], never scanned in part: a scan
/// that stopped early would drop later figures in silence, and a quote that cuts
/// one of them would then pass.
pub const MAX_SCAN_CODE_POINTS: usize = 200_000;

/// A text longer than [`MAX_SCAN_CODE_POINTS`], refused by the scanners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong {
    pub code_points: usize,
}

impl fmt::Display for TooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "text of {} code points is longer than {MAX_SCAN_CODE_POINTS}",
            self.code_points
        )
    }
}

impl std::error::Error for TooLong {}

pub(crate) fn check_len(text: &str) -> Result<(), TooLong> {
    // Counting stops one past the limit, so a huge text costs no more than a long one.
    let code_points = text.chars().take(MAX_SCAN_CODE_POINTS + 1).count();
    if code_points > MAX_SCAN_CODE_POINTS {
        return Err(TooLong {
            code_points: text.chars().count(),
        });
    }
    Ok(())
}

/// One reading found in a text.
#[derive(Clone, Debug, PartialEq)]
pub struct Found<T> {
    /// First code point of the reading in the text the scanner was given.
    pub start: u32,
    /// One past its last code point; the span never splits a grapheme cluster.
    pub end: u32,
    /// The text of `start..end`, as written.
    pub raw: String,
    pub value: T,
    /// Other readings of the same text, each with its confidence.
    pub alts: Vec<(T, f32)>,
    pub conf: f32,
    pub flags: Flags,
}

/// Facts about how a reading was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(pub u16);

impl Flags {
    /// At least one Thai digit.
    pub const THAI_DIGITS: Flags = Flags(1);
    /// Digits of more than one script.
    pub const MIXED_DIGITS: Flags = Flags(1 << 1);
    /// Thai number words (`Form::Words` or `Form::Mixed`).
    pub const WORDS: Flags = Flags(1 << 2);
    /// A two-digit year, read in the century nearest the anchor.
    pub const TWO_DIGIT_YEAR: Flags = Flags(1 << 3);
    /// No era marker: the era came from the year's range or the anchor.
    pub const ERA_INFERRED: Flags = Flags(1 << 4);
    /// A fiscal year or quarter, 1 October to 30 September.
    pub const FISCAL: Flags = Flags(1 << 5);
    /// A Buddhist-Era year before 2484 (1941), when the Thai year began on 1 April.
    pub const PRE_1941: Flags = Flags(1 << 6);
    /// The Rattanakosin or Chula Sakarat era.
    pub const OTHER_ERA: Flags = Flags(1 << 7);
    /// An accounting negative written in parentheses.
    pub const PAREN_NEGATIVE: Flags = Flags(1 << 8);
    /// A clock time.
    pub const TIME: Flags = Flags(1 << 9);
    /// Number words in parentheses that restate the figure before them.
    pub const AMOUNT_IN_WORDS: Flags = Flags(1 << 10);

    /// True when every flag of `other` is set.
    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;

    fn bitor(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Flags {
    fn bitor_assign(&mut self, other: Flags) {
        self.0 |= other.0;
    }
}

/// The text a scanner reads: the input, its `normalize_th` text as code points, and the map
/// between them. Scanners match on the normalized code points and report spans of the input.
pub(crate) struct View {
    original: Vec<char>,
    normalized: crate::Normalized,
    pub(crate) chars: Vec<char>,
}

impl View {
    pub(crate) fn new(text: &str) -> View {
        let normalized = crate::normalize_th(text);
        let chars = normalized.text.chars().collect();
        View {
            original: text.chars().collect(),
            normalized,
            chars,
        }
    }

    /// The input code points the normalized code points `range` came from, tight to them and
    /// widened to grapheme clusters.
    pub(crate) fn source(&self, range: &Range<usize>) -> Range<u32> {
        self.normalized.source_range(range.start, range.end)
    }

    /// The input as written under a normalized range, without the invisible code points.
    pub(crate) fn written(&self, range: &Range<usize>) -> String {
        let source = self.source(range);
        self.slice(&source)
            .chars()
            .filter(|c| !crate::normalize::is_invisible(*c))
            .collect()
    }

    /// A space that was a figure, punctuation, thin or hair space or a narrow no-break space in
    /// the input: one that can sit between the digit groups of one figure.
    pub(crate) fn group_space(&self, at: usize) -> bool {
        self.chars.get(at) == Some(&' ')
            && self
                .written(&(at..at + 1))
                .chars()
                .any(|c| matches!(c, '\u{2007}'..='\u{200A}' | '\u{202F}'))
    }

    /// Step 1 deleted, right before normalized code point `at`, a code point a reader may still
    /// see (`breaks_figure`): a control or a Hangul filler.
    pub(crate) fn broken_before(&self, at: usize) -> bool {
        let gap = self.normalized.gap_before(at);
        let start = usize::try_from(gap.start).unwrap_or(usize::MAX);
        let end = usize::try_from(gap.end).unwrap_or(usize::MAX);
        work(1);
        self.original
            .get(start..end.min(self.original.len()))
            .is_some_and(|deleted| deleted.iter().copied().any(breaks_figure))
    }

    fn slice(&self, range: &Range<u32>) -> String {
        let start = usize::try_from(range.start).unwrap_or(usize::MAX);
        let end = usize::try_from(range.end).unwrap_or(usize::MAX);
        self.original
            .get(start..end.min(self.original.len()))
            .map(|chars| chars.iter().collect())
            .unwrap_or_default()
    }

    /// As `source`, starting at the code points step 1 deleted right before the range.
    pub(crate) fn source_with_gap(&self, range: &Range<usize>) -> Range<u32> {
        let source = self.source(range);
        let gap = self.normalized.gap_before(range.start);
        self.normalized.cluster_start(gap.start.min(source.start))..source.end
    }

    /// A reading over the normalized range, with its span, raw text and digit flags.
    pub(crate) fn found<T>(
        &self,
        range: &Range<usize>,
        value: T,
        alts: Vec<(T, f32)>,
        conf: f32,
        flags: Flags,
    ) -> Found<T> {
        self.found_at(self.source(range), value, alts, conf, flags)
    }

    /// A reading over the input range `source`, with its raw text and digit flags.
    pub(crate) fn found_at<T>(
        &self,
        source: Range<u32>,
        value: T,
        alts: Vec<(T, f32)>,
        conf: f32,
        flags: Flags,
    ) -> Found<T> {
        let raw = self.slice(&source);
        let scripts: Vec<Script> = raw.chars().filter_map(script).collect();
        let mut flags = flags;
        if scripts.contains(&Script::Thai) {
            flags |= Flags::THAI_DIGITS;
        }
        if scripts.windows(2).any(|pair| pair[0] != pair[1]) {
            flags |= Flags::MIXED_DIGITS;
        }
        Found {
            start: source.start,
            end: source.end,
            raw,
            value,
            alts,
            conf,
            flags,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Work the scanners did on this thread, for the test that checks it grows linearly.
    pub(crate) static WORK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Counts `units` of scanning work in test builds.
#[cfg(test)]
pub(crate) fn work(units: usize) {
    WORK.with(|work| {
        work.set(
            work.get()
                .saturating_add(u64::try_from(units).unwrap_or(u64::MAX)),
        )
    });
}

#[cfg(not(test))]
pub(crate) fn work(_units: usize) {}

pub(crate) fn has_at(chars: &[char], at: usize, word: &str) -> bool {
    work(1);
    (at..)
        .zip(word.chars())
        .all(|(index, c)| chars.get(index) == Some(&c))
}

pub(crate) fn has_before(chars: &[char], end: usize, word: &str) -> bool {
    let len = word.chars().count();
    end.checked_sub(len)
        .is_some_and(|start| has_at(chars, start, word))
}

pub(crate) fn char_len(word: &str) -> usize {
    word.chars().count()
}

pub(crate) fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// A decimal digit, or another numeric code point (a digit of a script `normalize_th` does not
/// fold, a superscript, a fraction): something a written figure can hold.
pub(crate) fn is_numeral(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && c.is_numeric())
}

/// A code point that, between two digits, may be part of one written figure the grammar does not
/// read: a rule, not a list, so a separator nobody listed cannot split a figure into a head and a
/// confident tail. It is anything but a boundary the reader knows: white space, an ASCII
/// character other than a letter, a digit, `'` and `` ` `` (both look like a thousands mark), a
/// Thai letter that is not a combining mark, a dash or an arrow. So U+FFFD, the middle dot, the
/// Arabic separators, curly quotes, U+2044, a combining mark and a private-use code point all
/// count. Letters and the digits of other scripts make a code by their own rules.
pub(crate) fn foreign_separator(c: char) -> bool {
    if crate::normalize::is_thai_mark(c) || c == '\'' || c == '`' {
        return true;
    }
    !(c.is_whitespace() || c.is_alphanumeric() || c.is_ascii() || is_thai(c) || dash_or_arrow(c))
}

/// A dash or an arrow, which joins the two ends of a range (`10―15`, `1,000 ⇒ 1,250`): the dash
/// punctuation `normalize_th` leaves (it folds the hyphens and minus signs to `-`), the tilde and
/// wave dashes, the arrow blocks, and the triangles drawn as arrows. One rule for the lexer,
/// where it splits two figures, and for the signal table.
pub(crate) fn dash_or_arrow(c: char) -> bool {
    matches!(
        c,
        '-' | '~'
            | '\u{058A}'
            | '\u{05BE}'
            | '\u{1400}'
            | '\u{1806}'
            | '\u{2010}'..='\u{2015}'
            | '\u{2E17}'
            | '\u{2E1A}'
            | '\u{2E3A}'
            | '\u{2E3B}'
            | '\u{2E40}'
            | '\u{2E5D}'
            | '\u{301C}'
            | '\u{3030}'
            | '\u{30A0}'
            | '\u{FE31}'
            | '\u{FE32}'
            | '\u{FE58}'
            | '\u{FE63}'
            | '\u{FF0D}'
            | '\u{FF5E}'
            | '\u{10EAD}'
            | '\u{2190}'..='\u{21FF}'
            | '\u{25B6}'
            | '\u{25BA}'
            | '\u{2794}'..='\u{27BF}'
            | '\u{27F0}'..='\u{27FF}'
            | '\u{2900}'..='\u{297F}'
            | '\u{2B00}'..='\u{2BFF}'
    )
}

/// A glyph the text layer could not name: U+FFFD, U+FFFC or a private-use code point. A figure it
/// touches may have lost a digit to it, so the reader cannot place the figure.
pub(crate) fn unknown_glyph(c: char) -> bool {
    matches!(
        c,
        '\u{FFFD}'
            | '\u{FFFC}'
            | '\u{E000}'..='\u{F8FF}'
            | '\u{F0000}'..='\u{FFFFD}'
            | '\u{100000}'..='\u{10FFFD}'
    )
}

/// A code point step 1 deletes that a reader may still see as a mark or a gap: a control (U+0096
/// is an en dash decoded with the wrong code page) or a Hangul filler, which fonts draw as a
/// blank. Inside or right before a figure, the reader cannot place the figure.
pub(crate) fn breaks_figure(c: char) -> bool {
    (c.is_control() && !c.is_whitespace())
        || matches!(c, '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}')
}

/// A Thai consonant, vowel or mark: a code point that continues a Thai word.
pub(crate) fn is_thai(c: char) -> bool {
    matches!(c, '\u{0E01}'..='\u{0E4E}')
}

pub(crate) fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// A letter that makes a figure it touches a code: any letter but a Thai one.
pub(crate) fn code_letter(c: char) -> bool {
    c.is_alphabetic() && !is_thai(c)
}

/// Where a run of numerals (`is_numeral`) starting at `from` ends.
pub(crate) fn numerals_end(chars: &[char], from: usize) -> usize {
    let end = chars.get(from..).map_or(from, |rest| {
        from + rest.iter().take_while(|c| is_numeral(**c)).count()
    });
    work(end - from + 1);
    end
}

/// Where a digit run starting at `from` ends.
pub(crate) fn digits_end(chars: &[char], from: usize) -> usize {
    let end = chars.get(from..).map_or(from, |rest| {
        from + rest.iter().take_while(|c| is_digit(**c)).count()
    });
    work(end - from + 1);
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a over which scalar values each rule of the scanners counts, so a change to a rule
    /// shows up here as well as in the behaviour tests.
    #[test]
    fn scanner_character_rules_are_frozen() {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for c in (0..=0x10_FFFF).filter_map(char::from_u32) {
            let rules = u8::from(foreign_separator(c))
                | (u8::from(unknown_glyph(c)) << 1)
                | (u8::from(breaks_figure(c)) << 2)
                | (u8::from(code_letter(c)) << 3);
            if rules != 0 {
                for byte in u32::from(c).to_le_bytes().into_iter().chain([rules]) {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        assert_eq!(
            hash, RULES,
            "a character rule changed (fingerprint {hash:#018x})"
        );
    }

    /// The fingerprint of the rules as written in fix round 2.
    const RULES: u64 = 0xcdd7_3bfb_5990_53cf;
}
