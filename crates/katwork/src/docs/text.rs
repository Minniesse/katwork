//! The plain-text reader (`text` version 1): UTF-8, or UTF-16 with a byte-order mark, decoded
//! strictly; one block per paragraph, paragraphs split on blank lines.
use std::borrow::Cow;

use super::{
    Block, DocError, DocLimits, Extracted, ExtractorId, Locator, deadline, replace_nul,
    split_pieces,
};

pub(super) fn read(bytes: &[u8], limits: &DocLimits) -> Result<Extracted, DocError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocError::TooLarge("input"));
    }
    let text = decode(bytes)?;
    let mut paragraphs = Paragraphs {
        text: &text,
        limits,
        blocks: Vec::new(),
        ordinal: 0,
    };
    // The open paragraph's start (a byte and a code-point offset) and the byte where its last
    // line's content ends.
    let mut open: Option<(usize, usize)> = None;
    let mut last_end = 0;
    let (mut at, mut chars) = (0usize, 0usize);
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index % 1024 == 1023 {
            deadline::check()?;
        }
        // A line ends at `\n`; a `\r` right before it belongs to the line ending.
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if content.chars().all(char::is_whitespace) {
            if let Some(start) = open.take() {
                paragraphs.push(start, last_end)?;
            }
        } else {
            open.get_or_insert((at, chars));
            last_end = at + content.len();
        }
        at += line.len();
        chars += line.chars().count();
    }
    if let Some(start) = open {
        paragraphs.push(start, last_end)?;
    }
    Ok(Extracted {
        extractor: ExtractorId::TEXT,
        blocks: paragraphs.blocks,
        pages: Vec::new(),
        warnings: Vec::new(),
        workbook: None,
    })
}

struct Paragraphs<'a> {
    text: &'a str,
    limits: &'a DocLimits,
    blocks: Vec<Block>,
    ordinal: u32,
}

impl Paragraphs<'_> {
    /// One paragraph, from `start` (a byte and a code-point offset) to the byte `end`, as
    /// blocks whose ranges count code points of the decoded file.
    fn push(&mut self, start: (usize, usize), end: usize) -> Result<(), DocError> {
        deadline::check()?;
        self.ordinal += 1;
        let paragraph = &self.text[start.0..end];
        let mut chars = start.1;
        for (piece, range) in split_pieces(paragraph)?.into_iter().enumerate() {
            if self.blocks.len() as u64 >= u64::from(self.limits.max_blocks) {
                return Err(DocError::TooComplex("blocks"));
            }
            let text = &paragraph[range];
            let len = text.chars().count();
            self.blocks.push(Block {
                ordinal: self.ordinal,
                piece: piece as u32,
                text: replace_nul(text),
                locator: Locator::Text {
                    start: offset(chars)?,
                    end: offset(chars + len)?,
                },
                lines: Vec::new(),
                cell: None,
                unit_context: None,
                hidden: None,
                suspect: None,
            });
            chars += len;
        }
        Ok(())
    }
}

fn offset(chars: usize) -> Result<u32, DocError> {
    u32::try_from(chars).map_err(|_| DocError::TooComplex("text_length"))
}

/// The decoded file, after its byte-order mark. Offsets count code points of this text.
fn decode(bytes: &[u8]) -> Result<Cow<'_, str>, DocError> {
    let malformed = DocError::Malformed("text_encoding");
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| -> Result<Cow<'_, str>, DocError> {
        if !rest.len().is_multiple_of(2) {
            return Err(malformed);
        }
        let mut text = String::with_capacity(rest.len());
        let units = rest.as_chunks::<2>().0.iter().map(|&pair| unit(pair));
        for (index, decoded) in char::decode_utf16(units).enumerate() {
            if index % 65_536 == 65_535 {
                deadline::check()?;
            }
            text.push(decoded.map_err(|_| malformed)?);
        }
        Ok(Cow::Owned(text))
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, u16::from_le_bytes)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, u16::from_be_bytes)
    } else {
        let rest = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        std::str::from_utf8(rest)
            .map(Cow::Borrowed)
            .map_err(|_| malformed)
    }
}
