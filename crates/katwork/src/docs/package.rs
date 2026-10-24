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
}

/// Opens an OOXML package: one end record ending exactly at the end of the file, at most
/// `max_zip_entries` entries (refused before the directory is parsed), no bytes before the
/// archive, local headers that agree with the directory, no overlapping entries, unique ASCII
/// names under ASCII case folding with no `..`, `\` or leading `/`, no encryption, only stored
/// and deflated entries, and at most `max_unzipped_bytes` actually inflated.
pub fn open_package(bytes: &[u8], limits: &DocLimits) -> Result<Package, DocError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocError::TooLarge("input"));
    }
    let end = end_record(bytes)?;
    if end.entries > u64::from(limits.max_zip_entries) || end.entries > u64::from(u16::MAX) {
        return Err(DocError::TooComplex("zip_entries"));
    }
    let config = Config {
        archive_offset: ArchiveOffset::Known(0),
    };
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
        if has_unicode_path(bytes, entry.central_header_start())? {
            return Err(DocError::Malformed("zip_name"));
        }
        names.push((name, entry.size(), entry.compressed_size()));
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

/// Finds the one end record, refusing a second one (a decoy in the comment included) anywhere
/// from the central directory on, and follows a ZIP64 locator when the counts overflow.
fn end_record(bytes: &[u8]) -> Result<EndRecord, DocError> {
    let malformed = DocError::Malformed("zip_end_record");
    if bytes.len() < END_LEN {
        return Err(DocError::Malformed("zip"));
    }
    let floor = bytes.len().saturating_sub(END_LEN + usize::from(u16::MAX));
    let signatures: Vec<usize> = (floor..=bytes.len() - END_LEN)
        .filter(|&at| &bytes[at..at + 4] == END)
        .collect();
    let mut ending_here = signatures
        .iter()
        .copied()
        .filter(|&at| at + END_LEN + usize::from(u16_at(bytes, at + 20)) == bytes.len());
    let (Some(at), None) = (ending_here.next(), ending_here.next()) else {
        return Err(malformed);
    };
    let (disk, directory_disk) = (u16_at(bytes, at + 4), u16_at(bytes, at + 6));
    let (disk_entries, entries) = (u16_at(bytes, at + 8), u16_at(bytes, at + 10));
    let directory_offset = u32_at(bytes, at + 16);
    let mut record = EndRecord {
        entries: u64::from(entries),
        directory_offset: u64::from(directory_offset),
    };
    let mut directory_end = at;
    if entries == u16::MAX || disk_entries == u16::MAX || directory_offset == u32::MAX {
        // The ZIP64 locator sits right before the end record and points at the ZIP64 record.
        let locator = at.checked_sub(20).ok_or(malformed)?;
        if &bytes[locator..locator + 4] != ZIP64_LOCATOR || u32_at(bytes, locator + 4) != 0 {
            return Err(malformed);
        }
        let zip64 = usize::try_from(u64_at(bytes, locator + 8)).map_err(|_| malformed)?;
        if zip64.checked_add(56).is_none_or(|end| end > locator)
            || &bytes[zip64..zip64 + 4] != ZIP64_END
            || u32_at(bytes, zip64 + 16) != 0
            || u32_at(bytes, zip64 + 20) != 0
            || u64_at(bytes, zip64 + 24) != u64_at(bytes, zip64 + 32)
        {
            return Err(malformed);
        }
        record.entries = u64_at(bytes, zip64 + 32);
        record.directory_offset = u64_at(bytes, zip64 + 48);
        directory_end = zip64;
    } else if disk != 0 || directory_disk != 0 || disk_entries != entries {
        return Err(malformed);
    }
    if record.directory_offset > directory_end as u64 {
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

/// OPC part names are ASCII; the guard also refuses what a path could escape or alias with.
fn entry_name(raw: &[u8]) -> Result<String, DocError> {
    let name = std::str::from_utf8(raw).map_err(|_| DocError::Malformed("zip_name"))?;
    if name.is_empty()
        || !name.bytes().all(|byte| (0x20..0x7F).contains(&byte))
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(DocError::Malformed("zip_name"));
    }
    Ok(name.to_owned())
}

/// Whether the central header at `header` carries a Unicode Path extra field, which would give
/// the entry a second name that other readers use.
fn has_unicode_path(bytes: &[u8], header: u64) -> Result<bool, DocError> {
    let malformed = DocError::Malformed("zip_directory");
    let header = usize::try_from(header).map_err(|_| malformed)?;
    if header.checked_add(46).is_none_or(|end| end > bytes.len()) {
        return Err(malformed);
    }
    let name_len = usize::from(u16_at(bytes, header + 28));
    let extra_len = usize::from(u16_at(bytes, header + 30));
    let start = header + 46 + name_len;
    let extra = bytes.get(start..start + extra_len).ok_or(malformed)?;
    let mut fields = extra;
    while fields.len() >= 4 {
        let (id, len) = (u16_at(fields, 0), usize::from(u16_at(fields, 2)));
        if id == UNICODE_PATH {
            return Ok(true);
        }
        fields = fields.get(4 + len..).unwrap_or_default();
    }
    Ok(false)
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
