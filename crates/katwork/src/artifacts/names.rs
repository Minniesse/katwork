//! File names. A name is a relative path; a "folder" is a name prefix.
use std::sync::LazyLock;

use regex_syntax::hir::{Class, ClassUnicode, HirKind};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::error::KatError;

const MAX_NAME_BYTES: usize = 512;
const MAX_SEGMENT_BYTES: usize = 255;
const MAX_SEGMENTS: usize = 8;

/// The stored form of a file name, or `FileNameInvalid`.
///
/// NFC first (never NFKC, which splits SARA AM), then: a `/`-separated relative path of 1 to 8
/// segments, each 1 to 255 UTF-8 bytes, not `.` or `..`, with no `\`, no leading or trailing
/// white space (Unicode `White_Space`, so a no-break space counts) and no hidden character (see
/// `hidden`); the whole path at most 512 bytes.
pub fn validate_name(raw: &str) -> Result<String, KatError> {
    let name: String = raw.nfc().collect();
    if name.len() > MAX_NAME_BYTES || name.chars().any(hidden) {
        return Err(KatError::FileNameInvalid);
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() > MAX_SEGMENTS || !segments.into_iter().all(valid_segment) {
        return Err(KatError::FileNameInvalid);
    }
    Ok(name)
}

/// A folder prefix to list by: NFC, at most 512 bytes, none of the characters a name refuses.
/// Any other text is allowed, so a prefix may end in `/` or stop inside a segment.
pub fn validate_prefix(raw: &str) -> Result<String, KatError> {
    let prefix: String = raw.nfc().collect();
    if prefix.len() > MAX_NAME_BYTES || prefix.chars().any(hidden) {
        return Err(KatError::FileNameInvalid);
    }
    Ok(prefix)
}

/// A character a name never holds, since it hides or breaks what a person reads: `Cc` and `Cf`
/// (control, bidi and zero-width characters, the plan's rule); line and paragraph separators
/// (`Zl`, `Zp`), which start a new line wherever the name is shown; U+FFFD, the mark of bytes
/// lost in decoding; and the rest of Unicode's `Default_Ignorable_Code_Point`, which fonts draw
/// as nothing (the Hangul fillers, the combining grapheme joiner, the variation selectors).
fn hidden(ch: char) -> bool {
    static IGNORABLE: LazyLock<ClassUnicode> = LazyLock::new(|| {
        let hir = regex_syntax::Parser::new()
            .parse(r"\p{Default_Ignorable_Code_Point}")
            .expect("a valid Unicode property");
        let HirKind::Class(Class::Unicode(class)) = hir.kind() else {
            unreachable!("a property parses to a class")
        };
        class.clone()
    });
    if matches!(
        ch.general_category(),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    ) || ch == char::REPLACEMENT_CHARACTER
    {
        return true;
    }
    let ranges = IGNORABLE.ranges();
    let index = ranges.partition_point(|range| range.end() < ch);
    ranges.get(index).is_some_and(|range| range.start() <= ch)
}

fn valid_segment(segment: &str) -> bool {
    (1..=MAX_SEGMENT_BYTES).contains(&segment.len())
        && segment != "."
        && segment != ".."
        && !segment.contains('\\')
        && !segment.starts_with(char::is_whitespace)
        && !segment.ends_with(char::is_whitespace)
}
