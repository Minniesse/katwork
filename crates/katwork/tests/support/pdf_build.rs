//! Test-only PDF builders. `Raw` writes objects at explicit offsets with a classic xref table, an
//! xref stream or incremental sections, so a test can make a file lie (a freed object, a broken
//! `startxref`, a `/Prev` loop) in ways lopdf's own writer never would. `Doc` builds ordinary
//! pages on top of it: `/F1` is Helvetica (WinAnsi) and `/U` is a Type 0 font whose code for a
//! character is its UTF-16 unit, so any text can be shown with `Doc::utf16`. No hostile file is
//! committed; each test builds its own.
#![allow(dead_code)]

use std::io::Write;

/// A PDF assembled from object bodies.
#[derive(Clone, Default)]
pub struct Raw {
    /// `(number, body)`, where the body is what goes between `n 0 obj` and `endobj`.
    objects: Vec<(u32, Vec<u8>)>,
    next: u32,
}

/// A stream object's body: its dictionary (without `/Length`) and data.
pub fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut body = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

pub fn flate(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// Zlib data that inflates to `size` zero bytes, written in 1 MiB chunks.
pub fn zeros(size: u64) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let chunk = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        let take = left.min(chunk.len() as u64) as usize;
        encoder.write_all(&chunk[..take]).unwrap();
        left -= take as u64;
    }
    encoder.finish().unwrap()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

impl Raw {
    pub fn new() -> Raw {
        Raw {
            objects: Vec::new(),
            next: 1,
        }
    }

    /// A new object number.
    pub fn reserve(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }

    pub fn add(&mut self, body: impl Into<Vec<u8>>) -> u32 {
        // A number just reserved has no body yet.
        let id = self.reserve();
        self.objects.push((id, body.into()));
        id
    }

    pub fn set(&mut self, id: u32, body: impl Into<Vec<u8>>) {
        let body = body.into();
        match self.objects.iter_mut().find(|(number, _)| *number == id) {
            Some(slot) => slot.1 = body,
            None => self.objects.push((id, body)),
        }
        self.next = self.next.max(id + 1);
    }

    pub fn size(&self) -> u32 {
        self.next
    }

    /// The header, every object in order, and the offset of each.
    fn body(&self) -> (Vec<u8>, Vec<(u32, usize)>) {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::new();
        for (id, body) in &self.objects {
            offsets.push((*id, out.len()));
            out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        (out, offsets)
    }

    /// A classic xref table for `offsets` (every listed number in use, the rest free).
    fn table(size: u32, offsets: &[(u32, usize)]) -> String {
        let mut by_number = vec![None; size as usize];
        for (id, offset) in offsets {
            by_number[*id as usize] = Some(*offset);
        }
        let mut table = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for offset in &by_number[1..] {
            match offset {
                Some(offset) => table.push_str(&format!("{offset:010} 00000 n \n")),
                None => table.push_str("0000000000 00000 f \n"),
            }
        }
        table
    }

    /// The file with one classic xref section; `trailer` holds extra trailer entries.
    pub fn build(&self, root: u32, trailer: &str) -> Vec<u8> {
        let (mut out, offsets) = self.body();
        let start = out.len();
        out.extend_from_slice(Raw::table(self.size(), &offsets).as_bytes());
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root {root} 0 R {trailer} >>\nstartxref\n{start}\n%%EOF\n", self.size()).as_bytes(),
        );
        out
    }

    /// The file with an object stream holding the objects `packed` and an xref stream listing
    /// them as compressed (the rest as normal objects).
    pub fn build_packed(&self, root: u32, packed: &[u32]) -> Vec<u8> {
        let mut plain = self.clone();
        let mut index = String::new();
        let mut data = Vec::new();
        for id in packed {
            let body = &self.objects.iter().find(|(number, _)| number == id).unwrap().1;
            index.push_str(&format!("{id} {} ", data.len()));
            data.extend_from_slice(body);
            data.push(b'\n');
        }
        plain.objects.retain(|(id, _)| !packed.contains(id));
        let container = plain.reserve();
        let mut content = index.into_bytes();
        let first = content.len();
        content.extend_from_slice(&data);
        plain.set(
            container,
            stream(&format!("/Type /ObjStm /N {} /First {first} /Filter /FlateDecode", packed.len()), &flate(&content)),
        );
        let (mut out, offsets) = plain.body();
        let xref_id = plain.size();
        let size = xref_id + 1;
        let mut rows = Vec::new();
        for number in 0..size {
            let row: [u8; 7] = if let Some((_, offset)) = offsets.iter().find(|(id, _)| *id == number) {
                let offset = *offset as u32;
                [1, (offset >> 24) as u8, (offset >> 16) as u8, (offset >> 8) as u8, offset as u8, 0, 0]
            } else if let Some(position) = packed.iter().position(|id| *id == number) {
                [2, (container >> 24) as u8, (container >> 16) as u8, (container >> 8) as u8, container as u8, 0, position as u8]
            } else if number == xref_id {
                let offset = out.len() as u32;
                [1, (offset >> 24) as u8, (offset >> 16) as u8, (offset >> 8) as u8, offset as u8, 0, 0]
            } else {
                [0, 0, 0, 0, 0, 0xFF, 0xFF]
            };
            rows.extend_from_slice(&row);
        }
        let start = out.len();
        out.extend_from_slice(format!("{xref_id} 0 obj\n").as_bytes());
        out.extend_from_slice(&stream(
            &format!("/Type /XRef /Size {size} /W [1 4 2] /Root {root} 0 R /Filter /FlateDecode"),
            &flate(&rows),
        ));
        out.extend_from_slice(format!("\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes());
        out
    }
}

impl Raw {
    /// A hybrid-reference file: an object stream holding the objects `packed`, a classic table
    /// that marks them free (for readers that know no object streams), and a `/XRefStm` stream,
    /// listed in no section, that gives them as compressed.
    pub fn build_hybrid(&self, root: u32, packed: &[u32]) -> Vec<u8> {
        let mut plain = self.clone();
        let mut index = String::new();
        let mut data = Vec::new();
        for id in packed {
            let body = &self.objects.iter().find(|(number, _)| number == id).unwrap().1;
            index.push_str(&format!("{id} {} ", data.len()));
            data.extend_from_slice(body);
            data.push(b'\n');
        }
        plain.objects.retain(|(id, _)| !packed.contains(id));
        let container = plain.reserve();
        let mut content = index.into_bytes();
        let first = content.len();
        content.extend_from_slice(&data);
        plain.set(
            container,
            stream(&format!("/Type /ObjStm /N {} /First {first} /Filter /FlateDecode", packed.len()), &flate(&content)),
        );
        let (mut out, offsets) = plain.body();
        let size = plain.size() + 1;
        let mut rows = Vec::new();
        let mut subsections = String::new();
        for (position, id) in packed.iter().enumerate() {
            subsections.push_str(&format!("{id} 1 "));
            rows.extend_from_slice(&[2, (container >> 24) as u8, (container >> 16) as u8, (container >> 8) as u8, container as u8, 0, position as u8]);
        }
        let stream_at = out.len();
        out.extend_from_slice(format!("{} 0 obj\n", plain.size()).as_bytes());
        out.extend_from_slice(&stream(
            &format!("/Type /XRef /Size {size} /W [1 4 2] /Index [{subsections}] /Filter /FlateDecode"),
            &flate(&rows),
        ));
        out.extend_from_slice(b"\nendobj\n");
        let start = out.len();
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", plain.size());
        for number in 1..plain.size() {
            match offsets.iter().find(|(id, _)| *id == number) {
                Some((_, offset)) => table.push_str(&format!("{offset:010} 00000 n \n")),
                None => table.push_str("0000000000 00000 f \n"),
            }
        }
        out.extend_from_slice(table.as_bytes());
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root {root} 0 R /XRefStm {stream_at} >>\nstartxref\n{start}\n%%EOF\n").as_bytes(),
        );
        out
    }
}

/// An incremental update appended to `base`: new or changed `objects`, a table marking `freed`
/// numbers free, and a trailer whose `/Prev` points at `prev`.
pub fn append_update(base: &[u8], objects: &[(u32, Vec<u8>)], freed: &[u32], size: u32, root: u32, prev: usize) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut entries: Vec<(u32, String)> = Vec::new();
    for (id, body) in objects {
        entries.push((*id, format!("{:010} 00000 n \n", out.len())));
        out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    for id in freed {
        entries.push((*id, "0000000000 00001 f \n".to_owned()));
    }
    entries.sort();
    let start = out.len();
    let mut table = String::from("xref\n0 1\n0000000000 65535 f \n");
    for (id, row) in &entries {
        table.push_str(&format!("{id} 1\n{row}"));
    }
    out.extend_from_slice(table.as_bytes());
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R /Prev {prev} >>\nstartxref\n{start}\n%%EOF\n").as_bytes(),
    );
    out
}

/// The offset `startxref` names.
pub fn startxref(file: &[u8]) -> usize {
    let at = file.windows(9).rposition(|window| window == b"startxref").unwrap();
    let rest = std::str::from_utf8(&file[at + 9..]).unwrap();
    rest.split_whitespace().next().unwrap().parse().unwrap()
}

/// Pages on `Raw`, with a catalog, a page tree, `/F1` (Helvetica) and `/U` (identity UTF-16).
pub struct Doc {
    pub raw: Raw,
    pub catalog: u32,
    pub pages: u32,
    pub helvetica: u32,
    pub unicode: u32,
    kids: Vec<u32>,
    catalog_extra: String,
    /// Page size `[w h]` for new pages.
    pub size: (f32, f32),
}

impl Default for Doc {
    fn default() -> Doc {
        Doc::new()
    }
}

/// Zero-width code points for the `/U` font: Thai above and below marks.
fn zero_width(unit: u16) -> bool {
    matches!(unit, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E)
}

impl Doc {
    pub fn new() -> Doc {
        let mut raw = Raw::new();
        let catalog = raw.reserve();
        let pages = raw.reserve();
        let helvetica = raw.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
        let to_unicode = raw.add(stream("", b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n1 beginbfrange <0000> <FFFF> <0000> endbfrange"));
        let widths: Vec<String> = (0x0E31u16..=0x0E4E).filter(|unit| zero_width(*unit)).map(|unit| format!("{unit} {unit} 0")).collect();
        let descendant = raw.add(format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Uni /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 500 /W [{}] >>",
            widths.join(" ")
        ));
        let unicode = raw.add(format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /Uni /Encoding /Identity-H /DescendantFonts [{descendant} 0 R] /ToUnicode {to_unicode} 0 R >>"
        ));
        Doc {
            raw,
            catalog,
            pages,
            helvetica,
            unicode,
            kids: Vec::new(),
            catalog_extra: String::new(),
            size: (300.0, 300.0),
        }
    }

    pub fn add(&mut self, body: impl Into<Vec<u8>>) -> u32 {
        self.raw.add(body)
    }

    pub fn add_stream(&mut self, dict: &str, data: &[u8]) -> u32 {
        self.raw.add(stream(dict, data))
    }

    /// A Form XObject whose box clips nothing unless `dict` sets one.
    pub fn form(&mut self, dict: &str, content: &str) -> u32 {
        let bbox = if dict.contains("/BBox") { "" } else { "/BBox [-10000 -10000 10000 10000]" };
        let resources = if dict.contains("/Resources") { String::new() } else { self.resources("") };
        self.add_stream(&format!("/Type /XObject /Subtype /Form {bbox} {resources} {dict}"), content.as_bytes())
    }

    /// `/Resources` with both fonts and `extra` entries; fonts `extra` names join them.
    pub fn resources(&self, extra: &str) -> String {
        let fonts = format!("/F1 {} 0 R /U {} 0 R", self.helvetica, self.unicode);
        if extra.contains("/Font << ") {
            format!("/Resources << {} >>", extra.replacen("/Font << ", &format!("/Font << {fonts} "), 1))
        } else {
            format!("/Resources << /Font << {fonts} >> {extra} >>")
        }
    }

    pub fn catalog(&mut self, extra: &str) {
        self.catalog_extra.push_str(extra);
        self.catalog_extra.push(' ');
    }

    /// A page drawing `content` with the standard resources plus `resources`, and `page`'s extra
    /// page-dictionary entries.
    pub fn page_with(&mut self, page: &str, content: &str, resources: &str) -> u32 {
        let contents = self.add_stream("", content.as_bytes());
        let media = if page.contains("/MediaBox") { String::new() } else { format!("/MediaBox [0 0 {} {}]", self.size.0, self.size.1) };
        let resources = self.resources(resources);
        let id = self.add(format!("<< /Type /Page /Parent {} 0 R {media} {resources} /Contents {contents} 0 R {page} >>", self.pages));
        self.kids.push(id);
        id
    }

    pub fn page(&mut self, content: &str, resources: &str) -> u32 {
        self.page_with("", content, resources)
    }

    fn finish(&mut self) {
        let kids: Vec<String> = self.kids.iter().map(|kid| format!("{kid} 0 R")).collect();
        self.raw.set(self.pages, format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), self.kids.len()));
        self.raw.set(self.catalog, format!("<< /Type /Catalog /Pages {} 0 R {} >>", self.pages, self.catalog_extra));
    }

    pub fn build(mut self) -> Vec<u8> {
        self.finish();
        self.raw.build(self.catalog, "")
    }

    /// The finished `Raw`, for tests that write the file themselves.
    pub fn into_raw(mut self) -> (Raw, u32) {
        self.finish();
        (self.raw, self.catalog)
    }

    /// `text` as a hex string for the `/U` font.
    pub fn utf16(text: &str) -> String {
        let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        format!("<{}>", hex(&units))
    }
}

/// `BT /F1 <size> Tf <x> <y> Td (<text>) Tj ET`.
pub fn text(size: f32, x: f32, y: f32, text: &str) -> String {
    format!("BT /F1 {size} Tf {x} {y} Td ({text}) Tj ET\n")
}

/// The same with the `/U` font.
pub fn utext(size: f32, x: f32, y: f32, text: &str) -> String {
    format!("BT /U {size} Tf {x} {y} Td {} Tj ET\n", Doc::utf16(text))
}
