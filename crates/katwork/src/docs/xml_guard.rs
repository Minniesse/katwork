//! A strict well-formedness pass over one XML part before any reader builds anything from it.
//! quick-xml's ill-formed errors are recoverable by design; the guard stops at the first one.
use std::borrow::Cow;

use quick_xml::{
    NsReader, XmlVersion,
    events::{BytesRef, BytesStart, Event},
    name::{NamespaceError, ResolveResult},
};

use super::{DocError, DocLimits, deadline};

/// Attributes one element may carry, namespace declarations included. Word writes a few dozen at
/// most (the most on a part's root); more is refused before any reader walks them.
pub const MAX_XML_ATTRIBUTES: usize = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XmlStats {
    pub elements: u64,
    /// The elements whose local name is the `count` passed to `scan`, whatever their prefix.
    pub counted: u64,
}

/// What is left of one package's `max_xml_elements`. Every part `scan` passes takes its elements
/// from the same budget, so a package of many parts is held to the total one part would be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementBudget {
    left: u64,
}

impl ElementBudget {
    pub fn new(limits: &DocLimits) -> ElementBudget {
        ElementBudget {
            left: limits.max_xml_elements,
        }
    }

    fn take(&mut self) -> Result<(), DocError> {
        self.left = self
            .left
            .checked_sub(1)
            .ok_or(DocError::TooComplex("xml_elements"))?;
        Ok(())
    }
}

/// Checks that `part` is one well-formed UTF-8 XML document with no DTD, at most
/// `max_xml_depth` deep, whose entity references are the five predefined ones or character
/// references to XML `Char`s, and whose elements fit in what is left of `budget`; counts the
/// elements named `count`.
pub fn scan(
    part: &[u8],
    limits: &DocLimits,
    count: Option<&[u8]>,
    budget: &mut ElementBudget,
) -> Result<XmlStats, DocError> {
    if part.starts_with(&[0xFF, 0xFE])
        || part.starts_with(&[0xFE, 0xFF])
        || part.starts_with(&[0, b'<'])
        || part.starts_with(&[b'<', 0])
    {
        return Err(DocError::Malformed("xml_encoding"));
    }
    let mut reader = reader(part);
    let mut stats = XmlStats::default();
    let (mut depth, mut roots) = (0u32, 0u32);
    for events in 0u64.. {
        if events.is_multiple_of(1024) {
            deadline::check()?;
        }
        match reader.read_event().map_err(map_error)? {
            Event::Decl(decl) => {
                if let Some(encoding) = decl.encoding() {
                    let encoding = encoding.map_err(|_| DocError::Malformed("xml_encoding"))?;
                    if !encoding.eq_ignore_ascii_case("utf-8") {
                        return Err(DocError::Malformed("xml_encoding"));
                    }
                }
            }
            Event::DocType(_) => return Err(DocError::Malformed("doctype")),
            Event::Start(start) => {
                budget.take()?;
                element(&reader, &start, limits, depth, &mut roots)?;
                stats.count(&start, count);
                depth += 1;
            }
            Event::Empty(start) => {
                budget.take()?;
                element(&reader, &start, limits, depth, &mut roots)?;
                stats.count(&start, count);
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(DocError::Malformed("xml_structure"))?;
            }
            Event::Text(text) => {
                if depth == 0 && !text.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n')) {
                    return Err(DocError::Malformed("xml_structure"));
                }
                check_chars(&text, "xml_char")?;
            }
            Event::CData(data) => {
                if depth == 0 {
                    return Err(DocError::Malformed("xml_structure"));
                }
                check_chars(&data, "xml_char")?;
            }
            Event::GeneralRef(reference) => {
                if depth == 0 {
                    return Err(DocError::Malformed("xml_structure"));
                }
                resolve_ref(&reference)?;
            }
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof => {
                if depth != 0 || roots == 0 {
                    return Err(DocError::Malformed("xml_structure"));
                }
                return Ok(stats);
            }
        }
    }
    unreachable!("the event loop returns at Eof")
}

impl XmlStats {
    fn count(&mut self, start: &BytesStart, count: Option<&[u8]>) {
        self.elements += 1;
        if count.is_some_and(|count| start.local_name().as_ref().as_bytes() == count) {
            self.counted += 1;
        }
    }
}

/// Checks an element opened at `depth` (0 for the root): the depth cap, a single root, bound
/// prefixes, at most `MAX_XML_ATTRIBUTES` attributes and well-formed attribute values.
fn element(
    reader: &NsReader<&[u8]>,
    start: &BytesStart,
    limits: &DocLimits,
    depth: u32,
    roots: &mut u32,
) -> Result<(), DocError> {
    if depth >= limits.max_xml_depth {
        return Err(DocError::TooComplex("xml_depth"));
    }
    if depth == 0 {
        *roots += 1;
        if *roots > 1 {
            return Err(DocError::Malformed("xml_structure"));
        }
    }
    if let (ResolveResult::Unknown(_), _) = reader.resolver().resolve_element(start.name()) {
        return Err(DocError::Malformed("xml_namespace"));
    }
    for (at, attribute) in start.attributes().enumerate() {
        if at == MAX_XML_ATTRIBUTES {
            return Err(DocError::TooComplex("xml_attributes"));
        }
        let attribute = attribute.map_err(|_| DocError::Malformed("xml_attribute"))?;
        if let (ResolveResult::Unknown(_), _) = reader.resolver().resolve_attribute(attribute.key) {
            return Err(DocError::Malformed("xml_namespace"));
        }
        check_chars(&attribute.value, "xml_char")?;
        attribute_value(&attribute.value)?;
    }
    Ok(())
}

/// A reader configured the way every part is read: end names checked, comments checked, text
/// kept exactly as written.
pub(crate) fn reader(part: &[u8]) -> NsReader<&[u8]> {
    let mut reader = NsReader::from_reader(part);
    reader.config_mut().enable_all_checks(true);
    reader
}

pub(crate) fn map_error(error: quick_xml::Error) -> DocError {
    match error {
        quick_xml::Error::Namespace(
            NamespaceError::TooManyBindings(_) | NamespaceError::TooDeeplyNested(_),
        ) => DocError::TooComplex("xml_namespaces"),
        quick_xml::Error::Namespace(_) => DocError::Malformed("xml_namespace"),
        quick_xml::Error::Encoding(_) => DocError::Malformed("xml_encoding"),
        quick_xml::Error::Escape(_) => DocError::Malformed("xml_entity"),
        _ => DocError::Malformed("xml_syntax"),
    }
}

/// An attribute's value with its references resolved and its white space normalized, refusing
/// any reference but the five predefined entities and character references to XML `Char`s.
pub(crate) fn attribute_value(raw: &str) -> Result<Cow<'_, str>, DocError> {
    let attribute = quick_xml::events::attributes::Attribute {
        key: quick_xml::name::QName("v"),
        value: Cow::Borrowed(raw),
    };
    let value = attribute
        .normalized_value(XmlVersion::Implicit1_0)
        .map_err(|_| DocError::Malformed("xml_entity"))?;
    check_chars(&value, "xml_entity")?;
    Ok(value)
}

/// The character an entity or character reference stands for.
pub(crate) fn resolve_ref(reference: &BytesRef) -> Result<char, DocError> {
    let resolved = match reference.resolve_char_ref() {
        Ok(Some(c)) => Some(c),
        Ok(None) => match &**reference {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "apos" => Some('\''),
            "quot" => Some('"'),
            _ => None,
        },
        Err(_) => None,
    };
    resolved
        .filter(|&c| is_xml_char(c))
        .ok_or(DocError::Malformed("xml_entity"))
}

fn check_chars(text: &str, why: &'static str) -> Result<(), DocError> {
    if text.chars().all(is_xml_char) {
        Ok(())
    } else {
        Err(DocError::Malformed(why))
    }
}

/// XML 1.0 §2.2 `Char`.
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
}
