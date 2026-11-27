//! The first line of each cell note of a workbook, by `<Sheet>!<A1>`: where an XLSX claim cell's
//! `kw-claim-<n>` marker sits (SP2a Task 9; Task 25 reads the same markers). KatWork's XLSX
//! reader does not read notes, so read-back reads them here, from the opened package.
use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};

use crate::docs::{DocError, deadline, package::Package, xml_guard};

const MAX_LINE_CHARS: usize = 64;

/// The value of `start`'s attribute whose local name is `local`.
fn attribute(start: &BytesStart, local: &str) -> Result<Option<String>, DocError> {
    for attr in start.attributes() {
        let attr = attr.map_err(|_| DocError::Malformed("xml_attribute"))?;
        if attr.key.local_name().as_ref() == local {
            return Ok(Some(xml_guard::attribute_value(&attr.value)?.into_owned()));
        }
    }
    Ok(None)
}

/// What `elements` reports.
enum Node<'a, 'b> {
    Start(&'a str, &'a BytesStart<'b>, bool),
    Text(&'a str),
    End(&'a str),
}

/// Calls `each` with every element start (and whether it is empty), text and element end of
/// `part`, by local name.
fn elements(
    part: &[u8],
    mut each: impl FnMut(Node) -> Result<(), DocError>,
) -> Result<(), DocError> {
    let mut reader = xml_guard::reader(part);
    let mut buf = Vec::new();
    let mut events = 0u32;
    loop {
        events = events.wrapping_add(1);
        if events.is_multiple_of(1024) {
            deadline::check()?;
        }
        buf.clear();
        match reader
            .read_event_into(&mut buf)
            .map_err(xml_guard::map_error)?
        {
            Event::Start(start) => each(Node::Start(start.local_name().as_ref(), &start, false))?,
            Event::Empty(start) => each(Node::Start(start.local_name().as_ref(), &start, true))?,
            Event::End(e) => each(Node::End(e.local_name().as_ref()))?,
            Event::Text(t) => each(Node::Text(&t.xml10_content()))?,
            Event::GeneralRef(reference) => {
                let c = xml_guard::resolve_ref(&reference)?;
                each(Node::Text(c.encode_utf8(&mut [0; 4])))?;
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// `Id` to target of a relationships part, each target resolved against `base` (the folder of
/// the part the relationships belong to).
fn relationships(
    package: &Package,
    part: &str,
    base: &str,
) -> Result<HashMap<String, (String, String)>, DocError> {
    let mut out = HashMap::new();
    let Some(xml) = package.part(part) else {
        return Ok(out);
    };
    elements(xml, |node| {
        if let Node::Start("Relationship", start, _) = node
            && let (Some(id), Some(target), Some(kind)) = (
                attribute(start, "Id")?,
                attribute(start, "Target")?,
                attribute(start, "Type")?,
            )
        {
            out.insert(id, (resolve(base, &target), kind));
        }
        Ok(())
    })?;
    Ok(out)
}

/// A relationship target as a package part name.
fn resolve(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_owned();
    } else {
        base.split('/').filter(|p| !p.is_empty()).collect()
    };
    for piece in target.split('/') {
        match piece {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            piece => parts.push(piece),
        }
    }
    parts.join("/")
}

/// The folder of a part name (`xl/worksheets/sheet1.xml` is `xl/worksheets`).
fn folder(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The first line of every note, keyed `<Sheet>!<A1>`, each line at most 64 characters (a
/// marker is short; a longer line is kept cut, which matches no marker).
pub fn xlsx_note_markers(package: &Package) -> Result<HashMap<String, String>, DocError> {
    let workbook = "xl/workbook.xml";
    let Some(xml) = package.part(workbook) else {
        return Err(DocError::Malformed("xlsx_workbook"));
    };
    let mut sheets = Vec::new();
    elements(xml, |node| {
        if let Node::Start("sheet", start, _) = node
            && let (Some(sheet), Some(id)) = (attribute(start, "name")?, attribute(start, "id")?)
        {
            sheets.push((sheet, id));
        }
        Ok(())
    })?;
    let book_rels = relationships(package, "xl/_rels/workbook.xml.rels", folder(workbook))?;
    let mut out = HashMap::new();
    for (sheet, id) in sheets {
        let Some((sheet_part, _)) = book_rels.get(&id) else {
            continue;
        };
        let rels_part = format!(
            "{}/_rels/{}.rels",
            folder(sheet_part),
            sheet_part.rsplit('/').next().unwrap_or_default()
        );
        let sheet_rels = relationships(package, &rels_part, folder(sheet_part))?;
        for (comments, kind) in sheet_rels.values() {
            if !kind.ends_with("/comments") {
                continue;
            }
            let Some(xml) = package.part(comments) else {
                continue;
            };
            let mut current: Option<(String, String)> = None;
            let mut in_text = 0u32;
            let mut found = Vec::new();
            elements(xml, |node| {
                match node {
                    Node::Start("comment", start, false) => {
                        current = attribute(start, "ref")?.map(|r| (r, String::new()));
                    }
                    Node::Start("t", _, false) => in_text += 1,
                    Node::Text(text) => {
                        if in_text > 0
                            && let Some((_, line)) = &mut current
                            && !line.contains('\n')
                            && line.chars().count() <= MAX_LINE_CHARS
                        {
                            line.push_str(text);
                        }
                    }
                    Node::End("t") => in_text = in_text.saturating_sub(1),
                    Node::End("comment") => found.extend(current.take()),
                    _ => {}
                }
                Ok(())
            })?;
            for (cell, text) in found {
                let line: String = text
                    .split('\n')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(MAX_LINE_CHARS)
                    .collect();
                out.insert(format!("{sheet}!{cell}"), line);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_targets_resolve() {
        assert_eq!(
            resolve("xl", "worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve("xl/worksheets", "../comments1.xml"),
            "xl/comments1.xml"
        );
        assert_eq!(resolve("xl", "/xl/x.xml"), "xl/x.xml");
        assert_eq!(folder("xl/worksheets/sheet1.xml"), "xl/worksheets");
    }
}
