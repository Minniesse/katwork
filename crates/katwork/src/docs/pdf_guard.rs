//! The PDF guard: nothing reaches lopdf or pdf-extract without a bound. Before lopdf parses, a
//! byte scan refuses encrypted files and long cross-reference chains. During the load,
//! `load_filter` counts objects and object streams and decodes each object stream itself under
//! one decoded-byte budget per file (`Budget`, a thread-local, because lopdf's filter is a plain
//! `fn`). After it, `prescan` walks the page tree and the Form XObjects, decodes every stream
//! (image data is dropped, never decoded) with a capped decoder, counts content operators and
//! records scripts and attachments, so pdf-extract only ever reads decoded bytes.
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    io::Read,
    panic::{self, AssertUnwindSafe},
};

use lopdf::{
    Dictionary, Document, LoadOptions, Object, ObjectId, Stream,
    content::Content,
    xref::{XrefEntry, XrefRecovery},
};

use super::{DocError, DocLimits, ExtractWarning, deadline};

/// `%PDF-` must start within this many bytes, as viewers require.
const HEADER_WINDOW: usize = 1_024;
/// Cross-reference sections lopdf may read (the first plus each `/Prev` and `/XRefStm`).
const MAX_XREF_SECTIONS: u32 = 32;
/// Entries of one page's `/Contents` array.
pub(crate) const MAX_CONTENTS: usize = 1_024;
/// Decoded bytes between deadline checks.
const CHUNK: usize = 64 << 10;
/// `/Columns` and `/Colors` of a predictor.
const MAX_COLUMNS: u64 = 65_536;
const MAX_COLORS: u64 = 32;
/// Objects the filter counts between deadline checks.
const CHECK_EVERY: u64 = 256;

/// The decoded-byte budget of one PDF read: load-time object-stream reservations plus every
/// stream the pre-scan decodes, with the objects and object streams the load counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    pub decoded: u64,
    pub objects: u64,
    pub object_streams: u32,
}

struct Open {
    budget: Budget,
    limits: DocLimits,
    /// The limit `load_filter` hit, for `load` to return.
    error: Option<DocError>,
}

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    static DECODED: Cell<u64> = const { Cell::new(0) };
}

/// The bytes the capped decoder has produced on this thread, for tests.
#[doc(hidden)]
pub fn decoded_bytes() -> u64 {
    DECODED.get()
}

fn count_decoded(bytes: usize) {
    DECODED.set(DECODED.get().saturating_add(bytes as u64));
}

/// Opens this thread's budget for one read and clears it when dropped, on a panic too.
pub(crate) struct BudgetScope(Option<Open>);

impl BudgetScope {
    pub(crate) fn open(limits: &DocLimits) -> BudgetScope {
        let outer = OPEN.with_borrow_mut(|open| {
            open.replace(Open {
                budget: Budget::default(),
                limits: limits.clone(),
                error: None,
            })
        });
        BudgetScope(outer)
    }
}

impl Drop for BudgetScope {
    fn drop(&mut self) {
        let outer = self.0.take();
        OPEN.with_borrow_mut(|open| *open = outer);
    }
}

/// This thread's budget so far.
pub fn budget() -> Budget {
    OPEN.with_borrow(|open| open.as_ref().map(|open| open.budget).unwrap_or_default())
}

/// Adds `bytes` to the open budget's decoded bytes.
fn charge(bytes: u64) -> Result<(), DocError> {
    OPEN.with_borrow_mut(|open| {
        let Some(open) = open.as_mut() else {
            return Ok(());
        };
        open.budget.decoded = open.budget.decoded.saturating_add(bytes);
        if open.budget.decoded > open.limits.max_decoded_bytes {
            return Err(DocError::TooComplex("decoded_bytes"));
        }
        Ok(())
    })
}

/// The payload `load_filter` unwinds with to end lopdf's load at once: lopdf has no other way
/// to stop. `resume_unwind` skips the panic hook, and `load` catches it.
struct Abort;

/// lopdf's load filter. It counts every object against `max_pdf_objects`, checks the deadline,
/// and for each `/Type /ObjStm` stream counts it against `max_object_streams`, reserves
/// `max_objstm_bytes` of the budget and decodes it with the capped decoder before lopdf parses
/// it. Past any limit it records the error and ends the load.
pub fn load_filter(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    let outcome = OPEN.with_borrow_mut(|open| {
        let Some(open) = open.as_mut() else {
            return Ok(());
        };
        open.budget.objects += 1;
        if open.budget.objects > u64::from(open.limits.max_pdf_objects) {
            return Err(DocError::TooComplex("objects"));
        }
        if open.budget.objects.is_multiple_of(CHECK_EVERY) {
            deadline::check()?;
        }
        let Object::Stream(stream) = object else {
            return Ok(());
        };
        if !stream.dict.has_type(b"ObjStm") {
            return Ok(());
        }
        open.budget.object_streams += 1;
        if open.budget.object_streams > open.limits.max_object_streams {
            return Err(DocError::TooComplex("object_streams"));
        }
        open.budget.decoded = open.budget.decoded.saturating_add(open.limits.max_objstm_bytes);
        if open.budget.decoded > open.limits.max_decoded_bytes {
            return Err(DocError::TooComplex("decoded_bytes"));
        }
        let decoded = decode(None, stream, open.limits.max_objstm_bytes).map_err(|error| match error {
            DocError::TooComplex(_) => DocError::TooComplex("objstm_bytes"),
            DocError::Malformed("stream_filter") => DocError::Malformed("stream_filter"),
            DocError::Malformed(_) => DocError::Malformed("object_stream"),
            other => other,
        })?;
        stream.set_plain_content(decoded);
        Ok(())
    });
    match outcome {
        // lopdf keeps a top-level object as the filter left it and an object-stream member as
        // the filter returns it; a stream is never a member, so it needs no copy.
        Ok(()) => match object {
            Object::Stream(_) => Some((id, Object::Null)),
            other => Some((id, other.clone())),
        },
        Err(error) => {
            OPEN.with_borrow_mut(|open| {
                if let Some(open) = open.as_mut() {
                    open.error.get_or_insert(error);
                }
            });
            panic::resume_unwind(Box::new(Abort))
        }
    }
}

/// A name's bytes as lopdf reads them: regular characters, with `#` and two hex digits for one
/// byte; any other `#` ends the name. Reads at most `max` decoded bytes.
fn name_at(bytes: &[u8], mut at: usize, max: usize) -> (Vec<u8>, usize) {
    let mut name = Vec::new();
    while let Some(&byte) = bytes.get(at) {
        if name.len() > max || is_white(byte) || is_delimiter(byte) {
            break;
        }
        if byte == b'#' {
            let hex = |at: usize| bytes.get(at).and_then(|byte| (*byte as char).to_digit(16));
            match (hex(at + 1), hex(at + 2)) {
                (Some(high), Some(low)) => {
                    name.push((high * 16 + low) as u8);
                    at += 3;
                    continue;
                }
                _ => break,
            }
        }
        name.push(byte);
        at += 1;
    }
    (name, at)
}

fn is_white(byte: u8) -> bool {
    b" \t\n\r\0\x0C".contains(&byte)
}

fn is_delimiter(byte: u8) -> bool {
    b"()<>[]{}/%".contains(&byte)
}

/// Whether an integer follows at `at` (after white space), and where it ends.
fn integer_at(bytes: &[u8], mut at: usize) -> Option<usize> {
    while bytes.get(at).is_some_and(|byte| is_white(*byte)) {
        at += 1;
    }
    let start = at;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    (at > start).then_some(at)
}

/// The checks on raw bytes before lopdf reads anything: the header, encryption (an `/Encrypt`
/// name anywhere, however its characters are escaped: trailers and cross-reference stream
/// dictionaries are never inside object streams, so a real one is always plain bytes), and the
/// number of cross-reference sections lopdf would follow.
fn scan_bytes(bytes: &[u8], limits: &DocLimits) -> Result<(), DocError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocError::TooLarge("input"));
    }
    let window = &bytes[..bytes.len().min(HEADER_WINDOW)];
    if !window.windows(5).any(|five| five == b"%PDF-") {
        return Err(DocError::Malformed("pdf_header"));
    }
    let mut sections = 1u32;
    let mut at = 0;
    let mut names = 0u64;
    while let Some(offset) = bytes[at..].iter().position(|byte| *byte == b'/') {
        names += 1;
        if names.is_multiple_of(4096) {
            deadline::check()?;
        }
        let (name, end) = name_at(bytes, at + offset + 1, 8);
        at = end.max(at + offset + 1);
        match name.as_slice() {
            b"Encrypt" => return Err(DocError::Encrypted),
            // A trailer's /Prev and /XRefStm are direct offsets; an outline's /Prev is `n g R`.
            b"Prev" | b"XRefStm" => {
                let Some(number) = integer_at(bytes, at) else {
                    continue;
                };
                let reference = integer_at(bytes, number).is_some_and(|generation| {
                    let rest = &bytes[generation..];
                    let skip = rest.iter().take_while(|byte| is_white(**byte)).count();
                    rest.get(skip) == Some(&b'R')
                });
                if !reference {
                    sections += 1;
                    if sections > MAX_XREF_SECTIONS {
                        return Err(DocError::TooComplex("xref_sections"));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Loads `bytes` with lopdf under the open budget (`BudgetScope`), then refuses an encrypted
/// result and, when lopdf read the cross-reference table as written, drops every object the
/// table does not list as in use, so a freed object's old copy in an object stream is never
/// read.
pub(crate) fn load(bytes: &[u8], limits: &DocLimits) -> Result<Document, DocError> {
    scan_bytes(bytes, limits)?;
    let options = LoadOptions {
        password: None,
        filter: Some(load_filter),
        strict: false,
        max_decompressed_size: Some(usize::try_from(limits.max_objstm_bytes).unwrap_or(usize::MAX)),
    };
    let loaded = panic::catch_unwind(AssertUnwindSafe(|| {
        Document::load_mem_with_options(bytes, options)
    }));
    let recorded = OPEN.with_borrow_mut(|open| open.as_mut().and_then(|open| open.error.take()));
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(payload) if payload.is::<Abort>() => return Err(recorded.unwrap_or(DocError::Internal)),
        Err(payload) => panic::resume_unwind(payload),
    };
    if let Some(error) = recorded {
        return Err(error);
    }
    let mut doc = loaded.map_err(|_| DocError::Malformed("pdf"))?;
    if doc.trailer.get(b"Encrypt").is_ok() || doc.was_encrypted() || doc.is_encrypted() {
        return Err(DocError::Encrypted);
    }
    // An object stream lopdf could not parse is dropped with its members; never read a file
    // with objects silently missing.
    for (number, entry) in &doc.reference_table.entries {
        if matches!(entry, XrefEntry::Compressed { .. }) && !doc.objects.contains_key(&(*number, 0)) {
            return Err(DocError::Malformed("object_stream"));
        }
    }
    if !matches!(doc.xref_recovery, Some(XrefRecovery::Reconstructed)) {
        let entries = &doc.reference_table.entries;
        doc.objects.retain(|id, _| match entries.get(&id.0) {
            Some(XrefEntry::Normal { generation, .. }) => *generation == id.1,
            Some(XrefEntry::Compressed { .. }) => id.1 == 0,
            _ => false,
        });
    }
    Ok(doc)
}

/// One decode step's output: kept up to `cap` bytes; one more is an error.
struct Capped {
    data: Vec<u8>,
    cap: u64,
}

impl Capped {
    fn new(cap: u64) -> Capped {
        Capped {
            data: Vec::new(),
            cap,
        }
    }

    fn room(&self) -> u64 {
        self.cap.saturating_sub(self.data.len() as u64)
    }

    fn push(&mut self, byte: u8) -> Result<(), DocError> {
        if self.room() == 0 {
            return Err(DocError::TooComplex("stream_bytes"));
        }
        self.data.push(byte);
        count_decoded(1);
        if self.data.len().is_multiple_of(CHUNK) {
            deadline::check()?;
        }
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), DocError> {
        if bytes.len() as u64 > self.room() {
            return Err(DocError::TooComplex("stream_bytes"));
        }
        self.data.extend_from_slice(bytes);
        count_decoded(bytes.len());
        Ok(())
    }

    /// Reads `reader` to its end in chunks, checking the deadline before each.
    fn read_from(&mut self, mut reader: impl Read) -> Result<(), DocError> {
        let mut chunk = vec![0u8; CHUNK];
        loop {
            deadline::check()?;
            let want = usize::try_from(self.room()).unwrap_or(usize::MAX).min(CHUNK);
            if want == 0 {
                // At the cap: one more byte of output is too many.
                let mut probe = [0u8; 1];
                return match reader.read(&mut probe) {
                    Ok(0) => Ok(()),
                    Ok(_) => Err(DocError::TooComplex("stream_bytes")),
                    Err(_) => Err(DocError::Malformed("stream_decode")),
                };
            }
            let read = reader
                .read(&mut chunk[..want])
                .map_err(|_| DocError::Malformed("stream_decode"))?;
            if read == 0 {
                return Ok(());
            }
            self.extend(&chunk[..read])?;
        }
    }
}

/// `/Filter` and `/DecodeParms` resolved: each filter name with its parameters.
fn filters(doc: Option<&Document>, dict: &Dictionary) -> Result<Vec<(Vec<u8>, Option<Dictionary>)>, DocError> {
    let malformed = DocError::Malformed("stream_filter");
    let resolve = |object: &Object| -> Result<Object, DocError> {
        match object {
            Object::Reference(_) => doc
                .and_then(|doc| doc.dereference(object).ok())
                .map(|(_, object)| object.clone())
                .ok_or(malformed),
            other => Ok(other.clone()),
        }
    };
    let names: Vec<Vec<u8>> = match dict.get(b"Filter").ok().map(resolve).transpose()? {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Name(name)) => vec![name],
        Some(Object::Array(items)) => items
            .iter()
            .map(|item| match resolve(item)? {
                Object::Name(name) => Ok(name),
                _ => Err(malformed),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(malformed),
    };
    let params = dict.get(b"DecodeParms").ok().map(resolve).transpose()?;
    let param = |index: usize| -> Result<Option<Dictionary>, DocError> {
        let entry = match &params {
            Some(Object::Array(items)) => items.get(index).map(resolve).transpose()?,
            Some(other) => Some(other.clone()),
            None => None,
        };
        match entry {
            None | Some(Object::Null) => Ok(None),
            Some(Object::Dictionary(dict)) => Ok(Some(dict)),
            Some(_) => Err(DocError::Malformed("stream_decode")),
        }
    };
    names
        .into_iter()
        .enumerate()
        .map(|(index, name)| Ok((name, param(index)?)))
        .collect()
}

/// Decodes `stream` through its filters, each step capped at `cap` bytes. Only the five
/// general filters are decoded; any other (an image codec, `Crypt`, Brotli, an unknown name)
/// is `Malformed("stream_filter")`.
fn decode(doc: Option<&Document>, stream: &Stream, cap: u64) -> Result<Vec<u8>, DocError> {
    let mut data = stream.content.clone();
    for (name, params) in filters(doc, &stream.dict)? {
        let mut out = Capped::new(cap);
        match name.as_slice() {
            b"FlateDecode" | b"Fl" => {
                inflate(&data, &mut out)?;
                out = predict(out, params.as_ref(), cap)?;
            }
            b"LZWDecode" | b"LZW" => {
                lzw(&data, params.as_ref(), &mut out)?;
                out = predict(out, params.as_ref(), cap)?;
            }
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(&data, &mut out)?,
            b"ASCII85Decode" | b"A85" => ascii85(&data, &mut out)?,
            b"RunLengthDecode" | b"RL" => run_length(&data, &mut out)?,
            _ => return Err(DocError::Malformed("stream_filter")),
        }
        data = out.data;
    }
    Ok(data)
}

/// Deflate data, with or without its zlib wrapper. The checksum is not checked, as viewers do
/// not; corrupt deflate data is `Malformed`.
fn inflate(input: &[u8], out: &mut Capped) -> Result<(), DocError> {
    let raw = match input {
        [cmf, flg, rest @ ..] if cmf & 0x0F == 8 && ((u16::from(*cmf) << 8) | u16::from(*flg)) % 31 == 0 => {
            if flg & 0x20 != 0 {
                // A preset dictionary: never in a PDF.
                return Err(DocError::Malformed("stream_decode"));
            }
            rest
        }
        _ => input,
    };
    out.read_from(flate2::read::DeflateDecoder::new(raw))
}

/// A writer that keeps at most `room` bytes and refuses the rest.
struct Limited<'a> {
    out: &'a mut Capped,
    over: bool,
}

impl std::io::Write for Limited<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.out.extend(bytes).is_err() {
            self.over = true;
            return Err(std::io::Error::other("stream_bytes"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn lzw(input: &[u8], params: Option<&Dictionary>, out: &mut Capped) -> Result<(), DocError> {
    let early = params
        .and_then(|params| params.get(b"EarlyChange").ok())
        .and_then(|value| value.as_i64().ok())
        .is_none_or(|value| value != 0);
    let mut decoder = if early {
        weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
    } else {
        weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8)
    };
    let mut sink = Limited { out, over: false };
    let result = decoder.into_stream(&mut sink).decode_all(input);
    if sink.over {
        return Err(DocError::TooComplex("stream_bytes"));
    }
    // A stream without its end code is common; keep what decoded.
    if result.status.is_err() && result.bytes_written == 0 {
        return Err(DocError::Malformed("stream_decode"));
    }
    deadline::check()
}

fn ascii_hex(input: &[u8], out: &mut Capped) -> Result<(), DocError> {
    let mut high: Option<u8> = None;
    for &byte in input {
        if byte == b'>' {
            break;
        }
        if is_white(byte) {
            continue;
        }
        let nibble = (byte as char).to_digit(16).ok_or(DocError::Malformed("stream_decode"))? as u8;
        match high.take() {
            Some(high) => out.push(high << 4 | nibble)?,
            None => high = Some(nibble),
        }
    }
    if let Some(high) = high {
        out.push(high << 4)?;
    }
    Ok(())
}

fn ascii85(input: &[u8], out: &mut Capped) -> Result<(), DocError> {
    let malformed = DocError::Malformed("stream_decode");
    let mut group: Vec<u32> = Vec::with_capacity(5);
    let flush = |group: &[u32], out: &mut Capped| -> Result<(), DocError> {
        let mut padded = [84u32; 5];
        padded[..group.len()].copy_from_slice(group);
        let value = padded.iter().try_fold(0u64, |value, digit| Some(value * 85 + u64::from(*digit)));
        let value = value.and_then(|value| u32::try_from(value).ok()).ok_or(malformed)?;
        out.extend(&value.to_be_bytes()[..group.len() - 1])
    };
    let mut bytes = input.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'~' => break,
            b'z' if group.is_empty() => out.extend(&[0; 4])?,
            b'!'..=b'u' => {
                group.push(u32::from(byte - b'!'));
                if group.len() == 5 {
                    flush(&group, out)?;
                    group.clear();
                }
            }
            byte if is_white(byte) => {}
            _ => return Err(malformed),
        }
    }
    match group.len() {
        0 => Ok(()),
        1 => Err(malformed),
        _ => flush(&group, out),
    }
}

fn run_length(input: &[u8], out: &mut Capped) -> Result<(), DocError> {
    let mut at = 0;
    while let Some(&length) = input.get(at) {
        at += 1;
        match length {
            128 => break,
            0..=127 => {
                let end = (at + usize::from(length) + 1).min(input.len());
                out.extend(&input[at..end])?;
                at = end;
            }
            _ => {
                let Some(&byte) = input.get(at) else { break };
                at += 1;
                for _ in 0..(257 - usize::from(length)) {
                    out.push(byte)?;
                }
            }
        }
    }
    Ok(())
}

/// Undoes a TIFF (2) or PNG (10 to 15) predictor. Its output is never longer than its input.
fn predict(input: Capped, params: Option<&Dictionary>, cap: u64) -> Result<Capped, DocError> {
    let number = |key: &[u8], default: i64| -> Result<u64, DocError> {
        match params.and_then(|params| params.get(key).ok()) {
            None => Ok(default as u64),
            Some(value) => value
                .as_i64()
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(DocError::Malformed("stream_predictor")),
        }
    };
    let predictor = number(b"Predictor", 1)?;
    if predictor == 1 {
        return Ok(input);
    }
    let colors = number(b"Colors", 1)?;
    let bits = number(b"BitsPerComponent", 8)?;
    let columns = number(b"Columns", 1)?;
    if !(1..=MAX_COLORS).contains(&colors) || !(1..=MAX_COLUMNS).contains(&columns) || ![1, 2, 4, 8, 16].contains(&bits) {
        return Err(DocError::Malformed("stream_predictor"));
    }
    let row = usize::try_from((colors * bits * columns).div_ceil(8)).map_err(|_| DocError::Malformed("stream_predictor"))?;
    let pixel = usize::try_from((colors * bits).div_ceil(8)).map_err(|_| DocError::Malformed("stream_predictor"))?;
    let data = input.data;
    let mut out = Capped::new(cap);
    match predictor {
        2 => {
            for line in data.chunks(row) {
                deadline::check()?;
                let mut line = line.to_vec();
                tiff_row(&mut line, colors as usize, bits as usize);
                out.extend(&line)?;
            }
        }
        10..=15 => {
            let mut previous = vec![0u8; row];
            for line in data.chunks(row + 1) {
                deadline::check()?;
                let (&kind, encoded) = line.split_first().ok_or(DocError::Malformed("stream_predictor"))?;
                let mut current = vec![0u8; encoded.len()];
                for index in 0..encoded.len() {
                    let left = if index >= pixel { current[index - pixel] } else { 0 };
                    let up = previous[index];
                    let up_left = if index >= pixel { previous[index - pixel] } else { 0 };
                    let value = encoded[index];
                    current[index] = match kind {
                        0 => value,
                        1 => value.wrapping_add(left),
                        2 => value.wrapping_add(up),
                        3 => value.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                        4 => value.wrapping_add(paeth(left, up, up_left)),
                        _ => return Err(DocError::Malformed("stream_predictor")),
                    };
                }
                out.extend(&current)?;
                previous[..current.len()].copy_from_slice(&current);
            }
        }
        _ => return Err(DocError::Malformed("stream_predictor")),
    }
    Ok(out)
}

fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let (a, b, c) = (i16::from(left), i16::from(up), i16::from(up_left));
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        up
    } else {
        up_left
    }
}

/// TIFF predictor 2 on one row: each sample adds the one `colors` samples before it.
fn tiff_row(row: &mut [u8], colors: usize, bits: usize) {
    let samples = row.len() * 8 / bits;
    let mask = if bits == 16 { 0xFFFF } else { (1u32 << bits) - 1 };
    let get = |row: &[u8], index: usize| -> u32 {
        match bits {
            16 => u32::from(u16::from_be_bytes([row[index * 2], row[index * 2 + 1]])),
            8 => u32::from(row[index]),
            _ => {
                let bit = index * bits;
                u32::from(row[bit / 8] >> (8 - bits - bit % 8)) & mask
            }
        }
    };
    for index in colors..samples {
        let value = (get(row, index) + get(row, index - colors)) & mask;
        match bits {
            16 => row[index * 2..index * 2 + 2].copy_from_slice(&(value as u16).to_be_bytes()),
            8 => row[index] = value as u8,
            _ => {
                let bit = index * bits;
                let shift = 8 - bits - bit % 8;
                let byte = &mut row[bit / 8];
                *byte = (*byte & !((mask as u8) << shift)) | ((value as u8) << shift);
            }
        }
    }
}

/// One page as the pre-scan found it, in reading order.
#[derive(Debug, Clone, PartialEq)]
pub struct PrescanPage {
    pub number: u32,
    pub id: ObjectId,
    /// Normalised, inherited.
    pub media_box: [f32; 4],
    /// Normalised, inherited, defaulting to the media box and cut to it.
    pub crop_box: [f32; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrescanReport {
    pub pages: Vec<PrescanPage>,
    /// Scripts, actions, XFA forms and attachments, once per kind.
    pub warnings: Vec<ExtractWarning>,
    /// lopdf rebuilt the cross-reference table by scanning the file, so objects may come from an
    /// older revision than a viewer shows.
    pub xref_rebuilt: bool,
    /// Content operators in the page streams and each Form XObject they draw, counted once.
    pub content_ops: u64,
}

fn rect_of(doc: &Document, object: Option<&Object>) -> Option<[f32; 4]> {
    let Some(Object::Array(items)) = object.and_then(|object| doc.dereference(object).ok()).map(|(_, object)| object) else {
        return None;
    };
    let values: Option<Vec<f32>> = items
        .iter()
        .map(|item| match doc.dereference(item).ok()?.1 {
            Object::Integer(value) => Some(*value as f32),
            Object::Real(value) => Some(*value),
            _ => None,
        })
        .collect();
    match values?.as_slice() {
        [a, b, c, d] if [a, b, c, d].iter().all(|value| value.is_finite()) => {
            Some([a.min(*c), b.min(*d), a.max(*c), b.max(*d)])
        }
        _ => None,
    }
}

fn resolve<'a>(doc: &'a Document, object: &'a Object) -> Option<(Option<ObjectId>, &'a Object)> {
    doc.dereference(object).ok()
}

fn dict_of<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    match resolve(doc, object)?.1 {
        Object::Dictionary(dict) => Some(dict),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}

/// The page tree in reading order, iteratively: a node seen twice is `page_tree_cycle`, depth
/// past `max_page_tree_depth` is `TooComplex`, and the pages are counted against `max_pages`
/// before any page is read. Media and crop boxes inherit down the tree.
fn page_tree(doc: &Document, limits: &DocLimits) -> Result<Vec<PrescanPage>, DocError> {
    let no_pages = DocError::Malformed("no_pages");
    let root = doc.trailer.get(b"Root").ok().and_then(|root| dict_of(doc, root)).ok_or(no_pages)?;
    let top = root.get(b"Pages").ok().ok_or(no_pages)?;
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut pages = Vec::new();
    // (node, depth, inherited media box, inherited crop box)
    let mut stack: Vec<(&Object, u32, Option<[f32; 4]>, Option<[f32; 4]>)> = vec![(top, 1, None, None)];
    let mut visits = 0u64;
    while let Some((node, depth, media, crop)) = stack.pop() {
        visits += 1;
        if visits.is_multiple_of(1024) {
            deadline::check()?;
        }
        if depth > limits.max_page_tree_depth {
            return Err(DocError::TooComplex("page_tree_depth"));
        }
        let Some((id, Object::Dictionary(dict))) = resolve(doc, node) else {
            continue;
        };
        if let Some(id) = id {
            if !seen.insert(id) {
                return Err(DocError::Malformed("page_tree_cycle"));
            }
        }
        let media = rect_of(doc, dict.get(b"MediaBox").ok()).or(media);
        let crop = rect_of(doc, dict.get(b"CropBox").ok()).or(crop);
        let kids = match dict.get(b"Kids").ok().and_then(|kids| resolve(doc, kids)) {
            Some((_, Object::Array(kids))) => Some(kids),
            _ => None,
        };
        let is_tree = dict.has_type(b"Pages") || (!dict.has_type(b"Page") && kids.is_some());
        match (is_tree, kids) {
            (true, Some(kids)) => {
                for kid in kids.iter().rev() {
                    stack.push((kid, depth + 1, media, crop));
                }
            }
            (true, None) => {}
            (false, _) => {
                let Some(id) = id else { continue };
                if pages.len() as u64 >= u64::from(limits.max_pages) {
                    return Err(DocError::TooComplex("pages"));
                }
                // US Letter when no media box is named, as the walker reads it.
                let media = media.unwrap_or([0.0, 0.0, 612.0, 792.0]);
                let crop = crop.map_or(media, |crop| {
                    [crop[0].max(media[0]), crop[1].max(media[1]), crop[2].min(media[2]), crop[3].min(media[3])]
                });
                pages.push(PrescanPage {
                    number: pages.len() as u32 + 1,
                    id,
                    media_box: media,
                    crop_box: crop,
                });
            }
        }
    }
    if pages.is_empty() {
        return Err(no_pages);
    }
    Ok(pages)
}

/// Decodes every stream in the document in place and charges its bytes to the budget: image
/// XObjects and streams whose data is in an external file (`/F`) are emptied, never decoded;
/// object streams are already plain. Afterwards no stream has a `/Filter`.
fn decode_all(doc: &mut Document, limits: &DocLimits) -> Result<(), DocError> {
    let ids: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| matches!(object, Object::Stream(_)))
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        deadline::check()?;
        let Some(Object::Stream(stream)) = doc.objects.get(&id) else {
            continue;
        };
        let image = stream.dict.get(b"Subtype").ok().and_then(|subtype| resolve(doc, subtype)).is_some_and(|(_, subtype)| subtype.as_name().ok() == Some(b"Image"));
        let decoded = if image || stream.dict.has(b"F") {
            Vec::new()
        } else if !stream.dict.has(b"Filter") {
            continue;
        } else {
            let decoded = decode(Some(doc), stream, limits.max_stream_bytes)?;
            charge(decoded.len() as u64)?;
            decoded
        };
        if let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) {
            stream.set_plain_content(decoded);
        }
    }
    Ok(())
}

/// The streams a page's `/Contents` names, in order, at most `MAX_CONTENTS`.
fn contents(doc: &Document, page: &Dictionary) -> Result<Vec<ObjectId>, DocError> {
    let mut streams = Vec::new();
    match page.get(b"Contents").ok() {
        Some(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Stream(_)) => streams.push(*id),
            Ok(Object::Array(items)) => streams.extend(items.iter().filter_map(|item| item.as_reference().ok())),
            _ => {}
        },
        Some(Object::Array(items)) => streams.extend(items.iter().filter_map(|item| item.as_reference().ok())),
        _ => {}
    }
    if streams.len() > MAX_CONTENTS {
        return Err(DocError::TooComplex("contents"));
    }
    Ok(streams)
}

/// A page attribute inherited through `/Parent`, as pdf-extract reads it.
fn inherited<'a>(doc: &'a Document, page: &'a Dictionary, key: &[u8], limits: &DocLimits) -> Option<&'a Object> {
    let mut dict = page;
    let mut seen = HashSet::new();
    for _ in 0..=limits.max_page_tree_depth {
        if let Ok(value) = dict.get(key) {
            return resolve(doc, value).map(|(_, value)| value);
        }
        let parent = dict.get(b"Parent").ok()?.as_reference().ok()?;
        if !seen.insert(parent) {
            return None;
        }
        dict = doc.get_dictionary(parent).ok()?;
    }
    None
}

/// The Form walk's state: the forms on the current path, each (form, resources) pair's height
/// once known, and the operators counted so far.
struct FormWalk<'a> {
    doc: &'a Document,
    limits: &'a DocLimits,
    path: Vec<ObjectId>,
    heights: HashMap<(ObjectId, usize), u32>,
    ops: u64,
}

impl<'a> FormWalk<'a> {
    /// Counts `content`'s operators and walks the Forms it draws; returns the height of the
    /// deepest Form chain below it (0 for none). `depth` is the content's own nesting depth.
    fn content(&mut self, content: &[u8], resources: Option<&'a Dictionary>, depth: u32) -> Result<u32, DocError> {
        let doc = self.doc;
        let mut height = 0;
        for operation in Content::decode_iter(content) {
            let Ok(operation) = operation else { break };
            self.ops += 1;
            if self.ops > self.limits.max_content_ops {
                return Err(DocError::TooComplex("content_ops"));
            }
            if self.ops.is_multiple_of(1024) {
                deadline::check()?;
            }
            if operation.operator != "Do" {
                continue;
            }
            let Some(Object::Name(name)) = operation.operands.last() else {
                continue;
            };
            let target = resources
                .and_then(|resources| resources.get(b"XObject").ok())
                .and_then(|xobjects| dict_of(doc, xobjects))
                .and_then(|xobjects| xobjects.get(name).ok())
                .and_then(|xobject| resolve(doc, xobject));
            let Some((Some(id), Object::Stream(form))) = target else {
                continue;
            };
            if form.dict.get(b"Subtype").ok().and_then(|subtype| subtype.as_name().ok()) != Some(b"Form") {
                continue;
            }
            height = height.max(self.form(id, form, resources, depth + 1)?);
        }
        Ok(height)
    }

    fn form(&mut self, id: ObjectId, form: &'a Stream, outer: Option<&'a Dictionary>, depth: u32) -> Result<u32, DocError> {
        if self.path.contains(&id) {
            return Err(DocError::Malformed("xobject_cycle"));
        }
        if depth > self.limits.max_xobject_depth {
            return Err(DocError::TooComplex("xobject_depth"));
        }
        let resources = form.dict.get(b"Resources").ok().and_then(|resources| dict_of(self.doc, resources)).or(outer);
        let key = (id, resources.map_or(0, |resources| resources as *const Dictionary as usize));
        if let Some(height) = self.heights.get(&key) {
            if depth + height - 1 > self.limits.max_xobject_depth {
                return Err(DocError::TooComplex("xobject_depth"));
            }
            return Ok(*height);
        }
        self.path.push(id);
        let below = self.content(&form.content, resources, depth)?;
        self.path.pop();
        let height = below + 1;
        self.heights.insert(key, height);
        Ok(height)
    }
}

/// Action types that run something: never read or run here, so each is a warning.
const ACTIVE_ACTIONS: [&[u8]; 6] = [
    b"JavaScript",
    b"Launch",
    b"SubmitForm",
    b"ImportData",
    b"Rendition",
    b"ResetForm",
];

/// Scripts, actions that run something, XFA forms and attachments anywhere in the document,
/// never read or run: `ActiveContentIgnored` (`pdf_script`, `pdf_xfa`) and
/// `EmbeddedContentIgnored` (`pdf_attachment`), once each. A link (`/URI`) or a destination is
/// not active content.
fn active_content(doc: &Document) -> Result<Vec<ExtractWarning>, DocError> {
    let (mut script, mut xfa, mut attachment) = (false, false, false);
    let mut stack: Vec<&Object> = doc.objects.values().collect();
    let mut visits = 0u64;
    while let Some(object) = stack.pop() {
        visits += 1;
        if visits.is_multiple_of(4096) {
            deadline::check()?;
        }
        let dict = match object {
            Object::Array(items) => {
                stack.extend(items.iter());
                continue;
            }
            Object::Dictionary(dict) => dict,
            Object::Stream(stream) => &stream.dict,
            _ => continue,
        };
        for (key, value) in dict.iter() {
            match key.as_slice() {
                b"JS" | b"JavaScript" | b"AA" => script = true,
                b"OpenAction" => {
                    // A destination, or an action that only goes to one, moves the view.
                    let goes_to = match resolve(doc, value) {
                        Some((_, Object::Array(_))) => true,
                        Some((_, Object::Dictionary(action))) => {
                            action.get(b"S").ok().and_then(|kind| kind.as_name().ok()) == Some(b"GoTo")
                        }
                        _ => true,
                    };
                    script |= !goes_to;
                }
                b"S" if value.as_name().is_ok_and(|kind| ACTIVE_ACTIONS.contains(&kind)) => script = true,
                b"XFA" => xfa = true,
                b"EmbeddedFiles" => attachment = true,
                b"Type" if value.as_name().ok() == Some(b"EmbeddedFile") => attachment = true,
                b"Subtype" if value.as_name().ok() == Some(b"FileAttachment") => attachment = true,
                _ => {}
            }
            stack.push(value);
        }
    }
    let mut warnings = Vec::new();
    if script {
        warnings.push(ExtractWarning::ActiveContentIgnored { what: "pdf_script" });
    }
    if xfa {
        warnings.push(ExtractWarning::ActiveContentIgnored { what: "pdf_xfa" });
    }
    if attachment {
        warnings.push(ExtractWarning::EmbeddedContentIgnored {
            what: "pdf_attachment",
        });
    }
    Ok(warnings)
}

/// The pre-scan, after the load and before pdf-extract reads anything: the page tree, every
/// stream decoded in place under the budget, each page's content and the Forms it draws
/// (cycles, depth, operators), repeated content charged per use, and the active-content
/// warnings.
pub fn prescan(doc: &mut Document, limits: &DocLimits) -> Result<PrescanReport, DocError> {
    let pages = page_tree(doc, limits)?;
    decode_all(doc, limits)?;
    let doc: &Document = doc;
    let mut walk = FormWalk {
        doc,
        limits,
        path: Vec::new(),
        heights: HashMap::new(),
        ops: 0,
    };
    let mut used: HashSet<ObjectId> = HashSet::new();
    for page in &pages {
        let Ok(dict) = doc.get_dictionary(page.id) else {
            continue;
        };
        let resources = inherited(doc, dict, b"Resources", limits).and_then(|resources| match resources {
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        });
        let mut content = Vec::new();
        for id in contents(doc, dict)? {
            let Ok(Object::Stream(stream)) = doc.get_object(id) else {
                continue;
            };
            // Each stream was charged once when decoded; every further use is charged again.
            if !used.insert(id) {
                charge(stream.content.len() as u64)?;
            }
            content.extend_from_slice(&stream.content);
            content.push(b'\n');
        }
        walk.content(&content, resources, 0)?;
    }
    Ok(PrescanReport {
        pages,
        warnings: active_content(doc)?,
        xref_rebuilt: matches!(doc.xref_recovery, Some(XrefRecovery::Reconstructed)),
        content_ops: walk.ops,
    })
}
