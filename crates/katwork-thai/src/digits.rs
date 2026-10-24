//! Decimal digits of the scripts KatWork reads.

/// The value of a decimal digit written in ASCII, Thai, Arabic-Indic, Extended
/// Arabic-Indic or full-width form; `None` for anything else, `²` and other
/// numeric symbols included.
pub fn fold_digit(c: char) -> Option<u32> {
    let zero = match c {
        '0'..='9' => '0',
        '\u{0E50}'..='\u{0E59}' => '\u{0E50}',
        '\u{0660}'..='\u{0669}' => '\u{0660}',
        '\u{06F0}'..='\u{06F9}' => '\u{06F0}',
        '\u{FF10}'..='\u{FF19}' => '\u{FF10}',
        _ => return None,
    };
    Some(u32::from(c) - u32::from(zero))
}

/// Which script a digit is written in, for the digit flags.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Script {
    Ascii,
    Thai,
    ArabicIndic,
    ExtendedArabicIndic,
    FullWidth,
}

pub(crate) fn script(c: char) -> Option<Script> {
    match c {
        '0'..='9' => Some(Script::Ascii),
        '\u{0E50}'..='\u{0E59}' => Some(Script::Thai),
        '\u{0660}'..='\u{0669}' => Some(Script::ArabicIndic),
        '\u{06F0}'..='\u{06F9}' => Some(Script::ExtendedArabicIndic),
        '\u{FF10}'..='\u{FF19}' => Some(Script::FullWidth),
        _ => None,
    }
}
