//! What an uploaded file is: its name's extension, checked against its first bytes.
use super::MediaType;
use crate::error::KatError;

/// The encoding of a file, as far as the text types need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextProbe {
    /// The whole file is valid UTF-8 and holds no U+0000 (an empty file included).
    Utf8,
    /// The file starts with a UTF-16 byte order mark, `FF FE` or `FE FF`. It is kept as
    /// uploaded; the text reader decodes it.
    Utf16Bom,
    /// Anything else: TIS-620, Windows-874, binary data, or UTF-8 cut off mid-character.
    Other,
}

/// A streaming UTF-8 check that carries an incomplete character across chunks.
#[derive(Debug, Default)]
pub(crate) struct TextScanner {
    /// The start of a character the last chunk cut off: at most 3 bytes.
    carry: [u8; 3],
    carry_len: usize,
    /// Invalid UTF-8 or a U+0000 was seen.
    failed: bool,
}

impl TextScanner {
    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        if self.failed || chunk.is_empty() {
            return;
        }
        // U+0000 is the only UTF-8 character with a zero byte.
        if chunk.contains(&0) {
            self.failed = true;
            return;
        }
        let mut rest = chunk;
        if self.carry_len > 0 {
            let Some(needed) = sequence_len(self.carry[0]) else {
                self.failed = true;
                return;
            };
            let take = (needed - self.carry_len).min(rest.len());
            let mut joined = [0u8; 4];
            joined[..self.carry_len].copy_from_slice(&self.carry[..self.carry_len]);
            joined[self.carry_len..self.carry_len + take].copy_from_slice(&rest[..take]);
            let joined = &joined[..self.carry_len + take];
            rest = &rest[take..];
            if joined.len() < needed {
                // Still incomplete: it must still be the start of a valid character.
                match std::str::from_utf8(joined) {
                    Err(error) if error.error_len().is_none() => {
                        self.carry[..joined.len()].copy_from_slice(joined);
                        self.carry_len = joined.len();
                    }
                    _ => self.failed = true,
                }
                return;
            }
            if std::str::from_utf8(joined).is_err() {
                self.failed = true;
                return;
            }
            self.carry_len = 0;
        }
        match std::str::from_utf8(rest) {
            Ok(_) => {}
            Err(error) if error.error_len().is_none() => {
                let tail = &rest[error.valid_up_to()..];
                self.carry[..tail.len()].copy_from_slice(tail);
                self.carry_len = tail.len();
            }
            Err(_) => self.failed = true,
        }
    }

    /// The probe of the whole file, given its first bytes.
    pub(crate) fn finish(&self, head: &[u8]) -> TextProbe {
        if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
            TextProbe::Utf16Bom
        } else if self.failed || self.carry_len > 0 {
            TextProbe::Other
        } else {
            TextProbe::Utf8
        }
    }
}

/// The length of the UTF-8 sequence a lead byte starts.
fn sequence_len(lead: u8) -> Option<usize> {
    match lead {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

const PDF: &[u8] = b"%PDF-";
const ZIP: &[u8] = b"PK\x03\x04";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG: &[u8] = b"\xFF\xD8\xFF";

/// The media type of a file named `name` whose first 8 bytes are `head`. The extension is the
/// last segment's text after its last `.`, compared case-insensitively; the bytes must agree
/// with it. A text-named file must be UTF-8 without U+0000, or UTF-16 with a byte order mark.
pub fn sniff(name: &str, head: &[u8], text: TextProbe) -> Result<MediaType, KatError> {
    let segment = name.rsplit('/').next().unwrap_or(name);
    let Some((_, extension)) = segment.rsplit_once('.') else {
        return Err(KatError::FileTypeUnsupported);
    };
    let is = |candidate: &str| extension.eq_ignore_ascii_case(candidate);
    let binary = |media: MediaType, magic: &[u8]| {
        if head.starts_with(magic) {
            Ok(media)
        } else {
            Err(KatError::FileTypeUnsupported)
        }
    };
    let textual = |media: MediaType| match text {
        TextProbe::Utf8 | TextProbe::Utf16Bom => Ok(media),
        TextProbe::Other => Err(KatError::FileEncodingUnsupported),
    };
    if is("pdf") {
        binary(MediaType::Pdf, PDF)
    } else if is("docx") {
        binary(MediaType::Docx, ZIP)
    } else if is("xlsx") {
        binary(MediaType::Xlsx, ZIP)
    } else if is("png") {
        binary(MediaType::Png, PNG)
    } else if is("jpg") || is("jpeg") {
        binary(MediaType::Jpeg, JPEG)
    } else if is("txt") {
        textual(MediaType::Text)
    } else if is("md") {
        textual(MediaType::Markdown)
    } else if is("csv") {
        textual(MediaType::Csv)
    } else {
        Err(KatError::FileTypeUnsupported)
    }
}
