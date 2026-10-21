//! A small, strict XML reader on top of quick-xml for hostile input.
//!
//! - A `<!DOCTYPE>` is refused, so no DTD and no entity can be defined; only
//!   the five predefined entities and character references are resolved.
//! - Nesting deeper than a limit is refused.
//! - Text is decoded from UTF-8 or UTF-16 (with or without a byte order
//!   mark) before parsing, so the reader never depends on quick-xml's
//!   optional encoding support.

use std::borrow::Cow;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// One element's start (or an empty element).
#[derive(Debug, Clone)]
pub(crate) struct Element {
    /// The local name, without a prefix.
    pub name: String,
    /// The namespace URI the element is in (empty when none).
    pub namespace: String,
    /// The prefix as written, to resolve once namespaces are known.
    prefix: String,
    /// Attributes as (qualified name, value), values unescaped.
    attributes: Vec<(String, String)>,
    /// `<a/>`: no matching end will follow.
    pub empty: bool,
}

/// SpreadsheetML's main namespace, transitional and strict.
const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const STRICT_MAIN: &str = "http://purl.oclc.org/ooxml/spreadsheetml/main";

impl Element {
    /// True for an element in SpreadsheetML's main namespace (or in no
    /// namespace, to be lenient with hand-made files); false for extensions
    /// such as `x15:connection`.
    pub fn is_main(&self) -> bool {
        matches!(self.namespace.as_str(), MAIN | STRICT_MAIN | "")
    }

    /// The value of the unprefixed attribute `name`.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The value of a prefixed attribute whose local name is `local`, such as
    /// `r:id`, whatever the prefix is.
    pub fn prefixed_attr(&self, local: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| {
                key.split_once(':').is_some_and(|(prefix, name)| {
                    prefix != "xmlns" && prefix != "xml" && name == local
                })
            })
            .map(|(_, value)| value.as_str())
    }

    /// A boolean attribute (`1`, `true`, `0`, `false`), or `default` when it
    /// is absent or not a boolean.
    pub fn flag(&self, name: &str, default: bool) -> bool {
        match self.attr(name) {
            Some("1" | "true") => true,
            Some("0" | "false") => false,
            _ => default,
        }
    }

    /// An unsigned number attribute.
    pub fn number(&self, name: &str) -> Option<u32> {
        self.attr(name).and_then(|v| v.trim().parse().ok())
    }
}

/// What the reader returns.
#[derive(Debug)]
pub(crate) enum Node<'a> {
    /// An element start, with its depth (the root element is 1).
    Start(Element, usize),
    /// An element end, with the depth of the element it closes.
    End(usize),
    /// A piece of text (character data, CDATA or one resolved reference)
    /// inside an element at the given depth. Text without references is
    /// borrowed from the part, not copied.
    Text(Cow<'a, str>, usize),
}

/// Most namespace declarations one part may make, in all its elements
/// together. Office parts make a few dozen; the cap keeps each element's
/// lookup short however many elements there are.
const MAX_NAMESPACES: usize = 256;

pub(crate) struct XmlReader<'a> {
    reader: Reader<&'a [u8]>,
    depth: usize,
    max_depth: usize,
    /// Namespace declarations in scope: (depth declared at, prefix, URI).
    namespaces: Vec<(usize, String, String)>,
    /// Declarations made so far in the part.
    declared: usize,
}

impl<'a> XmlReader<'a> {
    pub fn new(text: &'a str, max_depth: usize) -> Self {
        let mut reader = Reader::from_str(text);
        let config = reader.config_mut();
        config.expand_empty_elements = false;
        config.check_end_names = true;
        config.allow_dangling_amp = false;
        Self {
            reader,
            depth: 0,
            max_depth,
            namespaces: Vec::new(),
            declared: 0,
        }
    }

    /// The next node, `Ok(None)` at the end of the document.
    pub fn next(&mut self) -> Result<Option<Node<'a>>, String> {
        loop {
            let event = self.reader.read_event().map_err(|e| {
                format!(
                    "the XML is malformed near byte {}: {e}",
                    self.reader.buffer_position()
                )
            })?;
            match event {
                Event::Start(start) => {
                    self.depth += 1;
                    if self.depth > self.max_depth {
                        return Err(format!(
                            "elements nest deeper than {} levels",
                            self.max_depth
                        ));
                    }
                    let element = self.resolve(element(&start, false)?, self.depth, true)?;
                    return Ok(Some(Node::Start(element, self.depth)));
                }
                Event::Empty(start) => {
                    if self.depth + 1 > self.max_depth {
                        return Err(format!(
                            "elements nest deeper than {} levels",
                            self.max_depth
                        ));
                    }
                    let element = self.resolve(element(&start, true)?, self.depth + 1, false)?;
                    return Ok(Some(Node::Start(element, self.depth + 1)));
                }
                Event::End(_) => {
                    let closed = self.depth;
                    self.depth = self.depth.saturating_sub(1);
                    while self
                        .namespaces
                        .last()
                        .is_some_and(|(depth, _, _)| *depth >= closed)
                    {
                        self.namespaces.pop();
                    }
                    return Ok(Some(Node::End(closed)));
                }
                Event::Text(text) => {
                    let raw = utf8(text.into_inner(), "the text is not valid UTF-8")?;
                    let text = if raw.contains('&') {
                        Cow::Owned(unescape(&raw, false)?)
                    } else {
                        raw
                    };
                    return Ok(Some(Node::Text(text, self.depth)));
                }
                Event::CData(data) => {
                    let raw = utf8(data.into_inner(), "a CDATA section is not valid UTF-8")?;
                    return Ok(Some(Node::Text(raw, self.depth)));
                }
                Event::GeneralRef(reference) => {
                    let name = std::str::from_utf8(&reference)
                        .map_err(|_| "an entity name is not valid UTF-8".to_string())?;
                    return Ok(Some(Node::Text(
                        Cow::Owned(resolve_reference(name)?.to_string()),
                        self.depth,
                    )));
                }
                Event::DocType(_) => {
                    return Err("the part has a <!DOCTYPE>, which Office files never have and which can define entities".to_string());
                }
                Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
                Event::Eof => {
                    if self.depth != 0 {
                        return Err("the XML ends before all elements are closed".to_string());
                    }
                    return Ok(None);
                }
            }
        }
    }
}

impl XmlReader<'_> {
    /// Fills in the element's namespace from the declarations in scope plus
    /// its own; `keep` leaves its own in scope for its children.
    fn resolve(
        &mut self,
        mut element: Element,
        depth: usize,
        keep: bool,
    ) -> Result<Element, String> {
        let before = self.namespaces.len();
        for (key, value) in &element.attributes {
            let prefix = if key == "xmlns" {
                ""
            } else if let Some(prefix) = key.strip_prefix("xmlns:") {
                prefix
            } else {
                continue;
            };
            self.declared += 1;
            if self.declared > MAX_NAMESPACES {
                return Err(format!(
                    "the part declares more than {MAX_NAMESPACES} namespaces"
                ));
            }
            self.namespaces
                .push((depth, prefix.to_string(), value.clone()));
        }
        element.namespace = self
            .namespaces
            .iter()
            .rev()
            .find(|(_, prefix, _)| *prefix == element.prefix)
            .map(|(_, _, uri)| uri.clone())
            .unwrap_or_default();
        if !keep {
            self.namespaces.truncate(before);
        }
        Ok(element)
    }
}

/// Text from bytes, borrowed when the bytes are.
fn utf8<'a>(bytes: Cow<'a, [u8]>, error: &str) -> Result<Cow<'a, str>, String> {
    match bytes {
        Cow::Borrowed(bytes) => std::str::from_utf8(bytes)
            .map(Cow::Borrowed)
            .map_err(|_| error.to_string()),
        Cow::Owned(bytes) => String::from_utf8(bytes)
            .map(Cow::Owned)
            .map_err(|_| error.to_string()),
    }
}

/// Most attributes one element may have; Office elements have a few dozen.
const MAX_ATTRIBUTES: usize = 1_000;

fn element(start: &BytesStart<'_>, empty: bool) -> Result<Element, String> {
    let qname = start.name();
    let name = std::str::from_utf8(qname.local_name().as_ref())
        .map_err(|_| "an element name is not valid UTF-8".to_string())?
        .to_string();
    let prefix = match qname.prefix() {
        Some(prefix) => std::str::from_utf8(prefix.as_ref())
            .map_err(|_| "an element prefix is not valid UTF-8".to_string())?
            .to_string(),
        None => String::new(),
    };
    let mut attributes = Vec::new();
    // Duplicate checks are quadratic in the number of attributes; the count
    // is capped instead, and the first of two equal names wins.
    for attribute in start.attributes().with_checks(false) {
        if attributes.len() >= MAX_ATTRIBUTES {
            return Err(format!(
                "<{name}> has more than {MAX_ATTRIBUTES} attributes"
            ));
        }
        let attribute =
            attribute.map_err(|e| format!("an attribute of <{name}> is malformed: {e}"))?;
        let key = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|_| "an attribute name is not valid UTF-8".to_string())?
            .to_string();
        let raw = std::str::from_utf8(&attribute.value)
            .map_err(|_| format!("the attribute {key} is not valid UTF-8"))?;
        attributes.push((key, unescape(raw, true)?));
    }
    Ok(Element {
        name,
        namespace: String::new(),
        prefix,
        attributes,
        empty,
    })
}

/// Resolves `&name;` references: the five predefined entities and character
/// references. Anything else is an error, since no DTD can define more.
/// Attribute values also get their literal tabs and line breaks turned into
/// spaces, as XML requires.
fn unescape(raw: &str, attribute: bool) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        push_normalized(&mut out, &rest[..amp], attribute);
        let after = &rest[amp + 1..];
        let semi = after
            .find(';')
            .filter(|&s| s <= 32)
            .ok_or_else(|| "a `&` is not followed by an entity name and `;`".to_string())?;
        out.push(resolve_reference(&after[..semi])?);
        rest = &after[semi + 1..];
    }
    push_normalized(&mut out, rest, attribute);
    Ok(out)
}

fn push_normalized(out: &mut String, text: &str, attribute: bool) {
    if attribute {
        out.extend(text.chars().map(|c| {
            if matches!(c, '\t' | '\n' | '\r') {
                ' '
            } else {
                c
            }
        }));
    } else {
        out.push_str(text);
    }
}

fn resolve_reference(name: &str) -> Result<char, String> {
    let resolved = match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "apos" => Some('\''),
        "quot" => Some('"'),
        _ => name.strip_prefix('#').and_then(|number| {
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => number.parse().ok(),
            };
            code.and_then(char::from_u32)
        }),
    };
    resolved.ok_or_else(|| format!("the entity `&{name};` is not one XML predefines"))
}

/// Decodes a part's bytes to text: UTF-8, or UTF-16 with or without a byte
/// order mark. UTF-8 reuses the bytes' memory instead of copying them.
pub(crate) fn decode(mut bytes: Vec<u8>) -> Result<String, String> {
    let (utf16, little_endian, skip) = match bytes.as_slice() {
        [0xEF, 0xBB, 0xBF, ..] => (false, false, 3),
        [0xFF, 0xFE, ..] => (true, true, 2),
        [0xFE, 0xFF, ..] => (true, false, 2),
        [b'<', 0, ..] => (true, true, 0),
        [0, b'<', ..] => (true, false, 0),
        _ => (false, false, 0),
    };
    if !utf16 {
        bytes.drain(..skip);
        return String::from_utf8(bytes).map_err(|e| {
            format!(
                "the part is not valid UTF-8 (at byte {})",
                e.utf8_error().valid_up_to()
            )
        });
    }
    let body = &bytes[skip..];
    if !body.len().is_multiple_of(2) {
        return Err("the part is UTF-16 but has an odd number of bytes".to_string());
    }
    let units = body.as_chunks::<2>().0.iter().map(|&pair| {
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| "the part is UTF-16 with a broken surrogate pair".to_string())
}

#[cfg(test)]
mod tests {
    use super::{Node, XmlReader, decode};

    fn texts(xml: &str) -> Result<Vec<String>, String> {
        let mut reader = XmlReader::new(xml, 8);
        let mut out = Vec::new();
        while let Some(node) = reader.next()? {
            match node {
                Node::Text(text, _) => out.push(text.into_owned()),
                Node::Start(element, _) => out.extend(element.attr("v").map(str::to_string)),
                Node::End(_) => {}
            }
        }
        Ok(out)
    }

    #[test]
    fn references_resolve() {
        assert_eq!(
            texts("<a v='x&amp;y&#10;z'>1 &lt; 2&#x41;</a>")
                .unwrap()
                .concat(),
            "x&y\nz1 < 2A"
        );
    }

    #[test]
    fn doctype_and_unknown_entities_are_refused() {
        assert!(
            texts("<!DOCTYPE a [<!ENTITY e 'x'>]><a>&e;</a>")
                .unwrap_err()
                .contains("DOCTYPE")
        );
        assert!(texts("<a>&e;</a>").is_err());
        assert!(texts("<a v='&e;'/>").is_err());
    }

    #[test]
    fn namespaces_resolve() {
        let xml = r#"<x:a xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:ext"><e:b/><x:c xmlns:x="urn:other"><x:f/></x:c><x:d/><g/></x:a>"#;
        let mut reader = XmlReader::new(xml, 8);
        let mut seen = Vec::new();
        while let Some(node) = reader.next().unwrap() {
            if let Node::Start(element, _) = node {
                seen.push((element.name.clone(), element.is_main()));
            }
        }
        let expected = [
            ("a", true),
            ("b", false),
            ("c", false),
            ("f", false),
            ("d", true),
            ("g", true),
        ];
        let expected: Vec<(String, bool)> =
            expected.iter().map(|(n, m)| (n.to_string(), *m)).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn depth_is_limited() {
        let deep = format!("{}{}", "<a>".repeat(9), "</a>".repeat(9));
        assert!(texts(&deep).unwrap_err().contains("deeper"));
        let ok = format!("{}{}", "<a>".repeat(8), "</a>".repeat(8));
        assert!(texts(&ok).is_ok());
    }

    #[test]
    fn utf16_decodes() {
        let text = "<a>é</a>";
        let mut le = vec![0xFF, 0xFE];
        le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode(le).unwrap(), text);
        let be: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(decode(be).unwrap(), text);
        assert!(decode(vec![0xFF, 0xFE, b'<']).is_err());
        assert!(decode(vec![0xC3]).is_err());
        assert_eq!(decode(b"\xEF\xBB\xBF<a/>".to_vec()).unwrap(), "<a/>");
    }

    #[test]
    fn namespace_declarations_are_capped() {
        let some: String = (0..200).map(|i| format!(" xmlns:p{i}=\"u\"")).collect();
        assert!(texts(&format!("<a{some}/>")).is_ok());
        let many: String = (0..300).map(|i| format!(" xmlns:p{i}=\"u\"")).collect();
        assert!(
            texts(&format!("<a{many}/>"))
                .unwrap_err()
                .contains("namespaces")
        );
    }
}
