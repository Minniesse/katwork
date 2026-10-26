//! The package guard: a strict zip reader for OOXML packages. It reads the end record itself
//! before the `zip` crate parses anything, refuses what readers could disagree on, and inflates
//! every entry itself under a cap on the bytes actually inflated.
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
};

use zip::{
    CompressionMethod, ZipArchive,
    read::{ArchiveOffset, Config},
};

use super::{DocError, DocLimits, deadline};

const END: &[u8; 4] = b"PK\x05\x06";
const ZIP64_LOCATOR: &[u8; 4] = b"PK\x06\x07";
const ZIP64_END: &[u8; 4] = b"PK\x06\x06";
const LOCAL: &[u8; 4] = b"PK\x03\x04";
const CENTRAL: &[u8; 4] = b"PK\x01\x02";
const END_LEN: usize = 22;
const CHUNK: usize = 64 << 10;
/// Entries at most this large are spared the ratio check (a small sparse part is normal).
const RATIO_EXEMPT_BYTES: u64 = 1 << 20;
/// Flag bits 0 (encrypted), 6 (strong encryption) and 13 (masked directory).
const ENCRYPTION_FLAGS: u16 = 0x0001 | 0x0040 | 0x2000;
/// The Info-ZIP Unicode Path extra field: a second name for an entry.
const UNICODE_PATH: u16 = 0x7075;
/// The longest entry name accepted. Part names are short paths; a reader copies its part's name
/// into what it returns.
const MAX_NAME_BYTES: usize = 256;
/// `to_stored_zip` writes ZIP32 records, so a package never outgrows them.
const ZIP32_BYTES: u64 = 1 << 31;

thread_local! {
    static INFLATED: Cell<u64> = const { Cell::new(0) };
}

/// The entries `open_package` has inflated on this thread, for tests.
#[doc(hidden)]
pub fn inflated_entries() -> u64 {
    INFLATED.get()
}

/// A package's entries, inflated and checked, in their original order with their original names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    entries: Vec<(String, Vec<u8>)>,
    /// ASCII-lowercased name to entry index.
    index: HashMap<String, usize>,
}

impl Package {
    /// The entry named `name` under ASCII-only case folding, as OPC compares part names.
    pub fn part(&self, name: &str) -> Option<&[u8]> {
        self.index
            .get(&name.to_ascii_lowercase())
            .map(|&at| self.entries[at].1.as_slice())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// A new stored-only archive of exactly these bytes in their original order, for libraries
    /// that open their own zip. It is written by hand with fixed timestamps, so it cannot fail
    /// and the same package always gives the same bytes.
    pub fn to_stored_zip(&self) -> Vec<u8> {
        // `open_package` keeps every size, offset and count inside ZIP32's fields.
        let u32_of = |n: usize| u32::try_from(n).expect("open_package bounds the package size");
        let u16_of = |n: usize| u16::try_from(n).expect("open_package bounds names and entries");
        // 1980-01-01 00:00:00 in MS-DOS date and time.
        let (time, date): (u16, u16) = (0, (1 << 5) | 1);
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in &self.entries {
            let offset = u32_of(out.len());
            let crc = crc32fast::hash(data);
            let (name_len, size) = (u16_of(name.len()), u32_of(data.len()));
            out.extend_from_slice(LOCAL);
            for field in [10u16, 0, 0, time, date] {
                out.extend_from_slice(&field.to_le_bytes());
            }
            for field in [crc, size, size] {
                out.extend_from_slice(&field.to_le_bytes());
            }
            out.extend_from_slice(&name_len.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            central.extend_from_slice(CENTRAL);
            for field in [20u16, 10, 0, 0, time, date] {
                central.extend_from_slice(&field.to_le_bytes());
            }
            for field in [crc, size, size] {
                central.extend_from_slice(&field.to_le_bytes());
            }
            for field in [name_len, 0, 0, 0, 0] {
                central.extend_from_slice(&field.to_le_bytes());
            }
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let (directory_offset, directory_len) = (u32_of(out.len()), u32_of(central.len()));
        let count = u16_of(self.entries.len());
        out.extend_from_slice(&central);
        out.extend_from_slice(END);
        for field in [0u16, 0, count, count] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&directory_len.to_le_bytes());
        out.extend_from_slice(&directory_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }
}

/// What the end record says, read before the `zip` crate parses anything.
struct EndRecord {
    entries: u64,
    directory_offset: u64,
    directory_size: u64,
    /// Where the directory must end: at the ZIP64 end record when there is one, else at the end
    /// record.
    directory_end: u64,
}

/// Opens an OOXML package: one end record ending exactly at the end of the file, at most
/// `max_zip_entries` entries (refused before the directory is parsed), no bytes before the
/// archive, a directory whose headers fill exactly the size the end record gives (no gap, no
/// header past the counted ones), local headers that agree with the directory (their DOS time
/// aside), no overlapping entries, unique ASCII names of at most 256 bytes under ASCII case
/// folding with no `..`, `\` or leading `/`, no encryption, only stored and deflated entries,
/// and at most `max_unzipped_bytes` actually inflated.
pub fn open_package(bytes: &[u8], limits: &DocLimits) -> Result<Package, DocError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocError::TooLarge("input"));
    }
    let end = end_record(bytes)?;
    if end.entries > u64::from(limits.max_zip_entries) || end.entries > u64::from(u16::MAX) {
        return Err(DocError::TooComplex("zip_entries"));
    }
    // A reader that walks the directory by its size would see other headers than the count
    // gives: bytes left over, or a header past the counted ones.
    if end.directory_offset.checked_add(end.directory_size) != Some(end.directory_end) {
        return Err(DocError::Malformed("zip_directory"));
    }
    let config = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
    // The input with each local header's DOS time made its central header's (`same_times`).
    let retimed: Vec<u8>;
    let mut archive = ZipArchive::with_config(config, Cursor::new(bytes))
        .map_err(|_| DocError::Malformed("zip"))?;
    // Byte-equal names fold into one entry of the crate's index.
    if archive.has_duplicate_names() {
        return Err(DocError::Malformed("zip_duplicate_name"));
    }
    // The crate falls back to other end records when one fails; it must have read ours.
    if archive.offset() != 0
        || archive.central_directory_start() != end.directory_offset
        || archive.len() as u64 != end.entries
    {
        return Err(DocError::Malformed("zip_directory"));
    }
    let mut folded = HashSet::with_capacity(archive.len());
    let mut names = Vec::with_capacity(archive.len());
    // Where the next central header must start: the headers follow one another from the
    // directory's start, and the last ends where the directory does.
    let mut next = end.directory_offset;
    // Each entry's local header and central header, for `same_times`.
    let mut headers = Vec::with_capacity(archive.len());
    for at in 0..archive.len() {
        if at % 1024 == 1023 {
            deadline::check()?;
        }
        let entry = archive
            .by_index_data(at)
            .map_err(|_| DocError::Malformed("zip"))?;
        let name = entry_name(entry.name_raw())?;
        if !folded.insert(name.to_ascii_lowercase()) {
            return Err(DocError::Malformed("zip_duplicate_name"));
        }
        if entry.flags().as_u16() & ENCRYPTION_FLAGS != 0 {
            return Err(DocError::Encrypted);
        }
        let method = entry.compression();
        if method != CompressionMethod::STORE && method != CompressionMethod::DEFLATE {
            return Err(DocError::Malformed("zip_method"));
        }
        if entry.central_header_start() != next {
            return Err(DocError::Malformed("zip_directory"));
        }
        let (header_end, unicode_path) = central_header(bytes, next)?;
        if unicode_path {
            return Err(DocError::Malformed("zip_name"));
        }
        headers.push((entry.header_start(), next));
        next = header_end;
        names.push((name, entry.size(), entry.compressed_size()));
    }
    if next != end.directory_end {
        return Err(DocError::Malformed("zip_directory"));
    }
    if let Some(copy) = same_times(bytes, &headers) {
        retimed = copy;
        archive = ZipArchive::with_config(config, Cursor::new(retimed.as_slice()))
            .map_err(|_| DocError::Malformed("zip"))?;
    }
    archive
        .validate_header_consistency()
        .map_err(|_| DocError::Malformed("zip_header_mismatch"))?;

    let budget = limits.max_unzipped_bytes.min(ZIP32_BYTES);
    let mut total = 0u64;
    let mut entries = Vec::with_capacity(names.len());
    for (at, (name, declared, compressed)) in names.into_iter().enumerate() {
        deadline::check()?;
        let remaining = budget - total;
        if declared > remaining {
            return Err(DocError::TooComplex("unzipped_bytes"));
        }
        let data = inflate(&mut archive, at, declared, compressed, remaining, limits)?;
        total += data.len() as u64;
        INFLATED.set(INFLATED.get() + 1);
        entries.push((name, data));
    }
    let index = entries
        .iter()
        .enumerate()
        .map(|(at, (name, _))| (name.to_ascii_lowercase(), at))
        .collect();
    Ok(Package { entries, index })
}

/// Finds the one end record, refusing a second one (a decoy in the comment included) from the
/// central directory's start or the scan window's start (the last 64 KiB and 22 bytes),
/// whichever is later. A ZIP64 locator right before it is always followed, and the end record's
/// fields must each be their overflow marker or the ZIP64 value; a marker with no locator is
/// refused.
fn end_record(bytes: &[u8]) -> Result<EndRecord, DocError> {
    let malformed = DocError::Malformed("zip_end_record");
    if bytes.len() < END_LEN {
        return Err(DocError::Malformed("zip"));
    }
    let floor = bytes.len().saturating_sub(END_LEN + usize::from(u16::MAX));
    let mut signatures = Vec::new();
    for at in floor..=bytes.len() - END_LEN {
        if (at - floor) % 1024 == 1023 {
            deadline::check()?;
        }
        if &bytes[at..at + 4] == END {
            signatures.push(at);
        }
    }
    let mut ending_here = signatures
        .iter()
        .copied()
        .filter(|&at| at + END_LEN + usize::from(u16_at(bytes, at + 20)) == bytes.len());
    let (Some(at), None) = (ending_here.next(), ending_here.next()) else {
        return Err(malformed);
    };
    let (disk, directory_disk) = (u16_at(bytes, at + 4), u16_at(bytes, at + 6));
    let (disk_entries, entries) = (u16_at(bytes, at + 8), u16_at(bytes, at + 10));
    let (directory_size, directory_offset) = (u32_at(bytes, at + 12), u32_at(bytes, at + 16));
    let mut record = EndRecord {
        entries: u64::from(entries),
        directory_offset: u64::from(directory_offset),
        directory_size: u64::from(directory_size),
        directory_end: at as u64,
    };
    let markers = entries == u16::MAX
        || disk_entries == u16::MAX
        || directory_size == u32::MAX
        || directory_offset == u32::MAX;
    // The ZIP64 locator sits right before the end record. Some readers follow one found there
    // whatever the markers say, so the guard does too, and the 32-bit fields must then agree
    // with the ZIP64 record they stand for.
    let locator = at
        .checked_sub(20)
        .filter(|&locator| &bytes[locator..locator + 4] == ZIP64_LOCATOR);
    match locator {
        None if markers => return Err(malformed),
        None => {
            if disk != 0 || directory_disk != 0 || disk_entries != entries {
                return Err(malformed);
            }
        }
        Some(locator) => {
            // The ZIP64 record, with no extensible data, ends where the locator starts, and the
            // locator points at it: no reader can find another one.
            let zip64 = locator.checked_sub(56).ok_or(malformed)?;
            if u32_at(bytes, locator + 4) != 0
                || u64_at(bytes, locator + 8) != zip64 as u64
                || &bytes[zip64..zip64 + 4] != ZIP64_END
                || u64_at(bytes, zip64 + 4) != 44
                || u32_at(bytes, zip64 + 16) != 0
                || u32_at(bytes, zip64 + 20) != 0
                || u64_at(bytes, zip64 + 24) != u64_at(bytes, zip64 + 32)
            {
                return Err(malformed);
            }
            record.entries = u64_at(bytes, zip64 + 32);
            record.directory_size = u64_at(bytes, zip64 + 40);
            record.directory_offset = u64_at(bytes, zip64 + 48);
            record.directory_end = zip64 as u64;
            let agrees = |field: u64, marker: u64, value: u64| field == marker || field == value;
            let (short, long) = (u64::from(u16::MAX), u64::from(u32::MAX));
            if !agrees(u64::from(entries), short, record.entries)
                || !agrees(u64::from(disk_entries), short, record.entries)
                || !agrees(u64::from(directory_size), long, record.directory_size)
                || !agrees(u64::from(directory_offset), long, record.directory_offset)
                || !agrees(u64::from(disk), short, 0)
                || !agrees(u64::from(directory_disk), short, 0)
            {
                return Err(malformed);
            }
        }
    }
    if record.directory_offset > record.directory_end {
        return Err(malformed);
    }
    if signatures
        .iter()
        .any(|&other| other != at && other as u64 >= record.directory_offset)
    {
        return Err(malformed);
    }
    Ok(record)
}

/// A copy of `bytes` whose local headers carry their central headers' DOS time and date, when any
/// differs, or `None`. The time is no part of the content and writers do not keep the two equal,
/// so the crate's consistency check compares every other field. A local header out of place is
/// left for that check to refuse.
fn same_times(bytes: &[u8], headers: &[(u64, u64)]) -> Option<Vec<u8>> {
    // At most `max_zip_entries` headers, each read once: the walk before it checks the deadline.
    let mut copy: Option<Vec<u8>> = None;
    for &(local, central) in headers {
        let (Ok(local), Ok(central)) = (usize::try_from(local), usize::try_from(central)) else {
            continue;
        };
        let (Some(time), Some(written)) = (
            bytes.get(central + 12..central + 16),
            bytes.get(local..local + 14),
        ) else {
            continue;
        };
        if &written[..4] != LOCAL || &written[10..14] == time {
            continue;
        }
        copy.get_or_insert_with(|| bytes.to_vec())[local + 10..local + 14].copy_from_slice(time);
    }
    copy
}

/// OPC part names are ASCII; the guard also refuses what a path could escape or alias with, and a
/// name longer than `MAX_NAME_BYTES`.
fn entry_name(raw: &[u8]) -> Result<String, DocError> {
    let name = std::str::from_utf8(raw).map_err(|_| DocError::Malformed("zip_name"))?;
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || !name.bytes().all(|byte| (0x20..0x7F).contains(&byte))
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(DocError::Malformed("zip_name"));
    }
    Ok(name.to_owned())
}

/// Where the central header at `header` ends (its name, extra field and comment included), and
/// whether it carries a Unicode Path extra field, which would give the entry a second name that
/// other readers use.
fn central_header(bytes: &[u8], header: u64) -> Result<(u64, bool), DocError> {
    let malformed = DocError::Malformed("zip_directory");
    let header = usize::try_from(header).map_err(|_| malformed)?;
    if header.checked_add(46).is_none_or(|end| end > bytes.len()) {
        return Err(malformed);
    }
    let name_len = usize::from(u16_at(bytes, header + 28));
    let extra_len = usize::from(u16_at(bytes, header + 30));
    let comment_len = usize::from(u16_at(bytes, header + 32));
    let start = header + 46 + name_len;
    let extra = bytes.get(start..start + extra_len).ok_or(malformed)?;
    let end = start + extra_len + comment_len;
    if end > bytes.len() {
        return Err(malformed);
    }
    let mut fields = extra;
    while fields.len() >= 4 {
        let (id, len) = (u16_at(fields, 0), usize::from(u16_at(fields, 2)));
        if id == UNICODE_PATH {
            return Ok((end as u64, true));
        }
        fields = fields.get(4 + len..).unwrap_or_default();
    }
    Ok((end as u64, false))
}

/// Inflates entry `at` in 64 KiB chunks through a reader capped at `remaining` plus one byte,
/// stopping at the deadline, the budget, the ratio cap or any byte past the declared size.
fn inflate(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    at: usize,
    declared: u64,
    compressed: u64,
    remaining: u64,
    limits: &DocLimits,
) -> Result<Vec<u8>, DocError> {
    let file = archive
        .by_index(at)
        .map_err(|_| DocError::Malformed("zip_data"))?;
    let mut reader = file.take(remaining + 1);
    let mut data = Vec::with_capacity(declared.min(CHUNK as u64) as usize);
    let mut chunk = vec![0; CHUNK];
    loop {
        deadline::check()?;
        let read = reader.read(&mut chunk).map_err(|error| {
            // The crate refuses to read past the declared size; that is a lying directory.
            if error
                .to_string()
                .contains("larger than its declared uncompressed size")
            {
                DocError::TooComplex("zip_entry_size")
            } else {
                DocError::Malformed("zip_data")
            }
        })?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
        let inflated = data.len() as u64;
        if inflated > remaining {
            return Err(DocError::TooComplex("unzipped_bytes"));
        }
        if inflated > declared {
            return Err(DocError::TooComplex("zip_entry_size"));
        }
        if inflated > RATIO_EXEMPT_BYTES
            && inflated > compressed.saturating_mul(limits.max_entry_ratio)
        {
            return Err(DocError::TooComplex("zip_ratio"));
        }
    }
    if (data.len() as u64) < declared {
        return Err(DocError::Malformed("zip_entry_size"));
    }
    Ok(data)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
