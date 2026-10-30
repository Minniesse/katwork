//! File names. A name is a relative path; a "folder" is a name prefix.
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
/// white space (Unicode `White_Space`, so a no-break space counts) and no `Cc` or `Cf`
/// character (control, bidi and zero-width characters); the whole path at most 512 bytes.
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

fn hidden(ch: char) -> bool {
    matches!(
        ch.general_category(),
        GeneralCategory::Control | GeneralCategory::Format
    )
}

fn valid_segment(segment: &str) -> bool {
    (1..=MAX_SEGMENT_BYTES).contains(&segment.len())
        && segment != "."
        && segment != ".."
        && !segment.contains('\\')
        && !segment.starts_with(char::is_whitespace)
        && !segment.ends_with(char::is_whitespace)
}
