//! Builds OOXML packages from hand-written WordprocessingML and SpreadsheetML for the reader
//! tests, and zip archives byte by byte for the hostile cases. Nothing hostile is committed;
//! every such file is made here, inside the test that uses it.
#![allow(dead_code)]

use std::io::{Cursor, Write};

pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
pub const STRICT_W: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const RELATIONSHIP: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const PACKAGE_RELATIONSHIPS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// The namespace declarations Word puts on a document's root.
pub const NAMESPACES: &str = concat!(
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" "#,
    r#"xmlns:o="urn:schemas-microsoft-com:office:office" "#,
    r#"mc:Ignorable="w14""#,
);

/// A `w:document` whose body is `body`, with `before` (a `w:background`) ahead of it.
pub fn document_with(before: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NAMESPACES}>{before}<w:body>{body}</w:body></w:document>"#
    )
}

pub fn document(body: &str) -> String {
    document_with("", body)
}

/// One paragraph of one run of text, `props` inside the run's `w:rPr`.
pub fn run(props: &str, text: &str) -> String {
    format!(r#"<w:r><w:rPr>{props}</w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>"#)
}

pub fn para(props: &str, runs: &str) -> String {
    format!("<w:p><w:pPr>{props}</w:pPr>{runs}</w:p>")
}

/// A theme part with the default Office colour scheme, `replace` swapping slots' XML (for
/// example `("accent1", "")` drops `a:accent1`).
pub fn theme(replace: &[(&str, &str)]) -> String {
    let slots = [
        ("dk1", r#"<a:sysClr val="windowText" lastClr="000000"/>"#),
        ("lt1", r#"<a:sysClr val="window" lastClr="FFFFFF"/>"#),
        ("dk2", r#"<a:srgbClr val="44546A"/>"#),
        ("lt2", r#"<a:srgbClr val="E7E6E6"/>"#),
        ("accent1", r#"<a:srgbClr val="4472C4"/>"#),
        ("accent2", r#"<a:srgbClr val="ED7D31"/>"#),
        ("accent3", r#"<a:srgbClr val="A5A5A5"/>"#),
        ("accent4", r#"<a:srgbClr val="FFC000"/>"#),
        ("accent5", r#"<a:srgbClr val="5B9BD5"/>"#),
        ("accent6", r#"<a:srgbClr val="70AD47"/>"#),
        ("hlink", r#"<a:srgbClr val="0563C1"/>"#),
        ("folHlink", r#"<a:srgbClr val="954F72"/>"#),
    ];
    let mut scheme = String::new();
    for (slot, colour) in slots {
        match replace.iter().find(|(name, _)| *name == slot) {
            Some((_, "")) => {}
            Some((_, xml)) => scheme.push_str(xml),
            None => scheme.push_str(&format!("<a:{slot}>{colour}</a:{slot}>")),
        }
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme"><a:themeElements><a:clrScheme name="Office">{scheme}</a:clrScheme></a:themeElements></a:theme>"#
    )
}

pub fn styles(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {NAMESPACES}>{inner}</w:styles>"#
    )
}

pub fn settings(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:settings {NAMESPACES}>{inner}</w:settings>"#
    )
}

/// A DOCX package: the main part, the parts it relates to, and any other entries.
#[derive(Clone)]
pub struct Docx {
    main: String,
    document: String,
    /// The main part's relationships: (type suffix, target, part XML or `None` when the target
    /// is not added).
    related: Vec<(String, String, Option<Vec<u8>>)>,
    /// `_rels/.rels` written as is, instead of the one office-document relationship.
    root_rels: Option<String>,
    extra: Vec<(String, Vec<u8>)>,
}

impl Docx {
    pub fn new(body: &str) -> Docx {
        Docx::from_document(&document(body))
    }

    pub fn from_document(document: &str) -> Docx {
        Docx {
            main: "word/document.xml".into(),
            document: document.into(),
            related: Vec::new(),
            root_rels: None,
            extra: Vec::new(),
        }
    }

    /// Stores the main part under `name` (relative to the package root).
    pub fn main_part(mut self, name: &str) -> Docx {
        self.main = name.into();
        self
    }

    pub fn styles(self, inner: &str) -> Docx {
        self.related("styles", "styles.xml", styles(inner).as_bytes())
    }

    pub fn theme(self, xml: &str) -> Docx {
        self.related("theme", "theme/theme1.xml", xml.as_bytes())
    }

    pub fn settings(self, inner: &str) -> Docx {
        self.related("settings", "settings.xml", settings(inner).as_bytes())
    }

    /// Adds a part the main part relates to with `kind` (`header`, `endnotes` ...).
    pub fn related(mut self, kind: &str, target: &str, xml: &[u8]) -> Docx {
        self.related
            .push((kind.into(), target.into(), Some(xml.to_vec())));
        self
    }

    /// A relationship whose target part is not in the package.
    pub fn related_missing(mut self, kind: &str, target: &str) -> Docx {
        self.related.push((kind.into(), target.into(), None));
        self
    }

    pub fn root_rels(mut self, relationships: &str) -> Docx {
        self.root_rels = Some(format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PACKAGE_RELATIONSHIPS}">{relationships}</Relationships>"#
        ));
        self
    }

    pub fn entry(mut self, name: &str, bytes: &[u8]) -> Docx {
        self.extra.push((name.into(), bytes.to_vec()));
        self
    }

    pub fn entries(&self) -> Vec<(String, Vec<u8>)> {
        let mut entries = vec![(
            "[Content_Types].xml".to_owned(),
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/></Types>"#.to_vec(),
        )];
        let root_rels = self.root_rels.clone().unwrap_or_else(|| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PACKAGE_RELATIONSHIPS}"><Relationship Id="rId1" Type="{RELATIONSHIP}officeDocument" Target="{}"/></Relationships>"#,
                self.main
            )
        });
        entries.push(("_rels/.rels".into(), root_rels.into_bytes()));
        entries.push((self.main.clone(), self.document.clone().into_bytes()));
        let (dir, file) = self.main.rsplit_once('/').unwrap_or(("", &self.main));
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        if !self.related.is_empty() {
            let mut rels = String::new();
            for (at, (kind, target, xml)) in self.related.iter().enumerate() {
                let kind = if kind.starts_with("http") {
                    kind.clone()
                } else {
                    format!("{RELATIONSHIP}{kind}")
                };
                rels.push_str(&format!(
                    r#"<Relationship Id="rIdX{at}" Type="{kind}" Target="{target}"/>"#
                ));
                if let Some(xml) = xml {
                    entries.push((format!("{prefix}{target}"), xml.clone()));
                }
            }
            entries.push((
                format!("{prefix}_rels/{file}.rels"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PACKAGE_RELATIONSHIPS}">{rels}</Relationships>"#
                )
                .into_bytes(),
            ));
        }
        entries.extend(self.extra.iter().cloned());
        entries
    }

    pub fn build(&self) -> Vec<u8> {
        let entries = self.entries();
        zip(
            &entries
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
                .collect::<Vec<_>>(),
            true,
        )
    }
}

/// A minimal XLSX package of one sheet whose `sheetData` is `rows`.
pub fn xlsx(rows: &str) -> Vec<u8> {
    let main = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let entries: Vec<(&str, String)> = vec![
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/></Types>"#.into(),
        ),
        (
            "_rels/.rels",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PACKAGE_RELATIONSHIPS}"><Relationship Id="rId1" Type="{RELATIONSHIP}officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
        ),
        (
            "xl/workbook.xml",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="{main}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PACKAGE_RELATIONSHIPS}"><Relationship Id="rId1" Type="{RELATIONSHIP}worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{main}"><sheetData>{rows}</sheetData></worksheet>"#),
        ),
    ];
    zip(
        &entries
            .iter()
            .map(|(name, xml)| (*name, xml.as_bytes()))
            .collect::<Vec<_>>(),
        true,
    )
}

/// An honest zip of `entries`, deflated or stored, with fixed timestamps.
pub fn zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let method = if deflate {
        zip::CompressionMethod::DEFLATE
    } else {
        zip::CompressionMethod::STORE
    };
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(method)
        .last_modified_time(zip::DateTime::default());
    for (name, bytes) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// One entry written byte by byte: `compressed` is stored as the entry's data whatever the
/// method says, and the sizes and CRC are whatever the test declares.
#[derive(Clone)]
pub struct RawEntry {
    pub name: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub compressed: Vec<u8>,
    pub size: u32,
}

impl RawEntry {
    pub fn stored(name: &str, data: &[u8]) -> RawEntry {
        RawEntry {
            name: name.as_bytes().to_vec(),
            method: 0,
            flags: 0,
            crc: crc32fast::hash(data),
            compressed: data.to_vec(),
            size: data.len() as u32,
        }
    }

    pub fn deflated(name: &str, data: &[u8]) -> RawEntry {
        RawEntry {
            method: 8,
            compressed: deflate(data),
            ..RawEntry::stored(name, data)
        }
    }
}

/// Local headers, data, central directory and end record for `entries`, then `comment`.
pub fn raw_zip(entries: &[RawEntry], comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let offset = out.len() as u32;
        let compressed = entry.compressed.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        for field in [20u16, entry.flags, entry.method, 0, 0x21] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for field in [entry.crc, compressed, entry.size] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.compressed);

        central.extend_from_slice(b"PK\x01\x02");
        for field in [20u16, 20, entry.flags, entry.method, 0, 0x21] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        for field in [entry.crc, compressed, entry.size] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        for field in [entry.name.len() as u16, 0, 0, 0, 0] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&entry.name);
    }
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&end_record(
        entries.len() as u16,
        central.len() as u32,
        directory_offset,
        comment.len() as u16,
    ));
    out.extend_from_slice(comment);
    out
}

/// A 22-byte end-of-central-directory record.
pub fn end_record(
    entries: u16,
    directory_len: u32,
    directory_offset: u32,
    comment: u16,
) -> Vec<u8> {
    let mut record = b"PK\x05\x06".to_vec();
    for field in [0u16, 0, entries, entries] {
        record.extend_from_slice(&field.to_le_bytes());
    }
    record.extend_from_slice(&directory_len.to_le_bytes());
    record.extend_from_slice(&directory_offset.to_le_bytes());
    record.extend_from_slice(&comment.to_le_bytes());
    record
}

/// Raw deflate, best compression.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// Raw deflate of `data`, padded in front with empty stored blocks until it is `ratio` times
/// smaller than `data` (to the byte).
pub fn deflate_with_ratio(data: &[u8], ratio: usize) -> Vec<u8> {
    let stream = deflate(data);
    let target = data.len() / ratio;
    assert!(stream.len() <= target, "data does not compress {ratio}x");
    let mut padded = Vec::new();
    // A non-final stored block of length 0: header bits 000, padding to the byte, LEN, NLEN.
    while padded.len() + 5 <= target - stream.len() {
        padded.extend_from_slice(&[0x00, 0x00, 0x00, 0xFF, 0xFF]);
    }
    padded.extend_from_slice(&stream);
    padded
}

/// A raw deflate stream of `n` zero bytes (`n` at least 4), written with fixed Huffman codes:
/// one literal, then back-references of distance 1. About 1.6 bits a byte, so a gibibyte is
/// 6.8 MB and quick to make.
pub fn zeros_stream(n: u64) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u64,
        len: u32,
    }
    impl Bits {
        /// Writes `count` bits of `value`, least significant first.
        fn put(&mut self, value: u64, count: u32) {
            self.acc |= value << self.len;
            self.len += count;
            while self.len >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.len -= 8;
            }
        }
        /// Writes a Huffman code, most significant bit first.
        fn code(&mut self, code: u64, count: u32) {
            let reversed = (0..count).fold(0, |acc, bit| {
                acc | (((code >> bit) & 1) << (count - 1 - bit))
            });
            self.put(reversed, count);
        }
    }
    let mut bits = Bits {
        out: Vec::new(),
        acc: 0,
        len: 0,
    };
    assert!(n >= 4);
    bits.put(1, 1); // the final block
    bits.put(1, 2); // fixed Huffman codes
    bits.code(0b0011_0000, 8); // the literal 0
    // Lengths 258 (code 285) and 3..=10 (codes 257..=264) carry no extra bits.
    let mut left = n - 1;
    while left > 0 {
        let length = match left {
            258.. if left != 259 && left != 260 => 258,
            11.. if left - 10 >= 3 => 10,
            11.. => left - 3,
            _ => left,
        };
        if length == 258 {
            bits.code(0b1100_0101, 8);
        } else {
            bits.code(length - 2, 7);
        }
        bits.code(0, 5); // distance 1
        left -= length;
    }
    bits.code(0, 7); // the end of the block
    if bits.len > 0 {
        bits.out.push(bits.acc as u8);
    }
    bits.out
}

/// Offsets of the central directory headers, in order.
pub fn central_headers(zip: &[u8]) -> Vec<usize> {
    let end =
        zip.len() - 22 - u16::from_le_bytes([zip[zip.len() - 2], zip[zip.len() - 1]]) as usize;
    let mut at = u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes([zip[end + 10], zip[end + 11]]);
    let mut headers = Vec::new();
    for _ in 0..count {
        headers.push(at);
        let name = u16::from_le_bytes([zip[at + 28], zip[at + 29]]) as usize;
        let extra = u16::from_le_bytes([zip[at + 30], zip[at + 31]]) as usize;
        let comment = u16::from_le_bytes([zip[at + 32], zip[at + 33]]) as usize;
        at += 46 + name + extra + comment;
    }
    headers
}

/// The local header a central header points at.
pub fn local_header(zip: &[u8], central: usize) -> usize {
    u32::from_le_bytes(zip[central + 42..central + 46].try_into().unwrap()) as usize
}

pub fn set_u16(zip: &mut [u8], at: usize, value: u16) {
    zip[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn set_u32(zip: &mut [u8], at: usize, value: u32) {
    zip[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
