//! A small XML scanner for the flat parts of a zip package: relationships
//! (`.rels`) and `[Content_Types].xml`.
//!
//! It finds start tags and reads their attributes, and skips everything a
//! decoy could hide in: comments, CDATA sections and processing
//! instructions. A `<!DOCTYPE` is refused, as packages must not have one.
//! Every step moves forward, so the work is linear in the input, and the
//! number of elements returned is capped.

use std::collections::HashMap;

/// Why a part could not be scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlError {
    /// The part has a document type declaration (OPC forbids them).
    Doctype,
    /// More wanted elements than the cap.
    TooMany,
}

/// A start or empty-element tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element<'a> {
    /// The local name, without a namespace prefix.
    pub name: &'a str,
    /// The text between the name and the closing `>`.
    raw: &'a str,
}

impl<'a> Element<'a> {
    /// An unprefixed attribute's value, with character references
    /// replaced. Linear in the tag's length.
    ///
    /// The package attributes (`Id`, `Type`, `Target`, `TargetMode`,
    /// `Extension`, `PartName`, `ContentType`) have no namespace, so only
    /// an exact, unprefixed name matches: never a namespace declaration
    /// such as `xmlns:Target`, nor a foreign `o:Target`, which Office
    /// ignores.
    pub fn attr(&self, wanted: &str) -> Option<String> {
        attrs(self.raw)
            .find(|(name, _)| *name == wanted)
            .map(|(_, value)| unescape(value))
    }
}

/// The attributes in a tag's text, as (name, raw value) pairs, in order.
/// Stops at the first thing that is not `name="value"`.
fn attrs(raw: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut s = raw;
    std::iter::from_fn(move || {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        let eq = s.find('=')?;
        let name = s[..eq].trim();
        s = s[eq + 1..].trim_start();
        let quote = s.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        s = &s[1..];
        let close = s.find(quote)?;
        let value = &s[..close];
        s = &s[close + 1..];
        Some((name, value))
    })
}

/// The namespace a tag binds `prefix` to (`None` for the default
/// namespace), if it declares it. An empty value undeclares it. Linear
/// in the tag's length.
fn declared(raw: &str, prefix: Option<&str>) -> Option<String> {
    attrs(raw)
        .find(|(name, _)| match prefix {
            None => *name == "xmlns",
            Some(p) => name.strip_prefix("xmlns:") == Some(p),
        })
        .map(|(_, value)| unescape(value))
}

/// The root's namespace declarations, read once, so that looking a
/// child's prefix up costs the same however long the root's tag is.
#[derive(Default)]
struct Scope<'a> {
    default: Option<String>,
    prefixes: HashMap<&'a str, String>,
}

impl<'a> Scope<'a> {
    fn of(raw: &'a str) -> Scope<'a> {
        let mut scope = Scope::default();
        for (name, value) in attrs(raw) {
            if name == "xmlns" {
                scope.default.get_or_insert_with(|| unescape(value));
            } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                scope
                    .prefixes
                    .entry(prefix)
                    .or_insert_with(|| unescape(value));
            }
        }
        scope
    }

    fn get(&self, prefix: Option<&str>) -> Option<&str> {
        match prefix {
            None => self.default.as_deref(),
            Some(p) => self.prefixes.get(p).map(String::as_str),
        }
    }
}

/// A qualified name split at its first colon: (prefix, local name).
fn split_name(name: &str) -> (Option<&str>, &str) {
    match name.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, name),
    }
}

/// The children of the root element with one of the `wanted` local
/// names, in document order; at most `max` of them.
///
/// Package parts are flat: a root such as `<Relationships>` in
/// `namespace`, and children in the same namespace. Names are resolved
/// the namespace-aware way, as a conforming XML reader (and so Office)
/// does: a prefix or the default namespace is looked up in the
/// element's own declarations, then the root's. So
/// `<r:Relationship xmlns:r="…relationships">` counts like
/// `<Relationship>`, and none of these do: a root in another namespace
/// (then nothing counts), elements whose prefix or default namespace
/// is another one (`<d:Relationship xmlns:d="urn:decoy">`,
/// `xmlns="urn:decoy"`, `xmlns=""`), names with an undeclared prefix,
/// and anything nested deeper. Namespace names compare exactly, as
/// the XML Namespaces spec says: `"<namespace> "` is another one.
pub fn elements<'a>(
    xml: &'a str,
    namespace: &str,
    wanted: &[&str],
    max: usize,
) -> Result<Vec<Element<'a>>, XmlError> {
    let mut found = Vec::new();
    let mut rest = xml;
    let mut depth = 0usize;
    // The root's declarations, which its children inherit.
    let mut root: Option<Scope> = None;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = skip_past(after, "-->");
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            rest = skip_past(after, "]]>");
        } else if let Some(after) = rest.strip_prefix("<?") {
            rest = skip_past(after, "?>");
        } else if rest.starts_with("<!") {
            return Err(XmlError::Doctype);
        } else if let Some(after) = rest.strip_prefix("</") {
            rest = skip_past(after, ">");
            depth = depth.saturating_sub(1);
        } else {
            let body = &rest[1..];
            let end = tag_end(body);
            let tag = &body[..end];
            rest = body.get(end + 1..).unwrap_or("");
            let name_end = tag
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(tag.len());
            let (prefix, local) = split_name(&tag[..name_end]);
            let raw = &tag[name_end..];
            // Names are resolved only where they count: the root, and its
            // children. Only the root's declarations and the element's own
            // are in scope there.
            if depth == 0 {
                let scope = Scope::of(raw);
                // The root: it must be in the part's namespace.
                if root.is_some() || scope.get(prefix) != Some(namespace) {
                    return Ok(Vec::new());
                }
                root = Some(scope);
            } else if depth == 1 && wanted.contains(&local) {
                let own = declared(raw, prefix);
                let resolved = match &own {
                    Some(ns) => Some(ns.as_str()),
                    None => root.as_ref().and_then(|r| r.get(prefix)),
                };
                if resolved == Some(namespace) {
                    if found.len() == max {
                        return Err(XmlError::TooMany);
                    }
                    found.push(Element { name: local, raw });
                }
            }
            if !tag.ends_with('/') {
                depth += 1;
            }
        }
    }
    Ok(found)
}

/// The text after the first `end`, or nothing when it never comes.
fn skip_past<'a>(s: &'a str, end: &str) -> &'a str {
    match s.find(end) {
        Some(i) => &s[i + end.len()..],
        None => "",
    }
}

/// The index of the `>` that closes a tag, outside quoted values; the
/// length of `s` when there is none.
fn tag_end(s: &str) -> usize {
    let mut quote = None;
    for (i, b) in s.bytes().enumerate() {
        match (quote, b) {
            (None, b'"' | b'\'') => quote = Some(b),
            (Some(q), b) if b == q => quote = None,
            (None, b'>') => return i,
            _ => {}
        }
    }
    s.len()
}

/// Replaces XML's character references. A reference is at most 12 bytes
/// long, so each `&` looks no further: linear in the input.
pub fn unescape(s: &str) -> String {
    let Some(first) = s.find('&') else {
        return s.to_owned();
    };
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    // Text from `plain` up to `i` is copied as is when a reference is met.
    let mut plain = 0;
    let mut i = first;
    while i < b.len() {
        if b[i] != b'&' {
            i += 1;
            continue;
        }
        let end = (i + 12).min(b.len());
        let mut j = i + 1;
        while j < end && b[j] != b';' {
            j += 1;
        }
        if j < end
            && let Some(c) = entity(&s[i + 1..j])
        {
            out.push_str(&s[plain..i]);
            out.push(c);
            i = j + 1;
            plain = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&s[plain..]);
    out
}

/// The character an entity name (between `&` and `;`) stands for.
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => name
            .strip_prefix("#x")
            .or_else(|| name.strip_prefix("#X"))
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
            .and_then(char::from_u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const WANTED: &[&str] = &["Relationship"];

    /// A relationships part around `body`.
    fn rels(body: &str) -> String {
        format!(r#"<?xml version="1.0"?><Relationships xmlns="{NS}">{body}</Relationships>"#)
    }

    fn targets(xml: &str) -> Vec<String> {
        elements(xml, NS, WANTED, 10)
            .unwrap()
            .iter()
            .map(|e| e.attr("Target").unwrap_or_default())
            .collect()
    }

    #[test]
    fn comments_cdata_and_instructions_hide_nothing() {
        let xml = rels(
            r#"<?pi <Relationship Target="pi"/>?><!-- <Relationship Target="comment"/> --><![CDATA[<Relationship Target="cdata"/>]]><Relationship Id="a" Target='x&amp;y.bin' Note="a > b"/><Relationship Target="second"></Relationship><RelationshipX Target="no"/>"#,
        );
        assert_eq!(targets(&xml), ["x&y.bin", "second"]);
        let found = elements(&xml, NS, WANTED, 10).unwrap();
        assert_eq!(found[0].attr("Note").as_deref(), Some("a > b"));
        assert_eq!(found[0].attr("Missing"), None);
    }

    /// Reviewer cases element_decoy and default_ns_decoy: elements that
    /// look like relationships but are in another namespace.
    #[test]
    fn only_unprefixed_children_in_the_namespace_count() {
        let real = r#"<Relationship Id="r9" Type="t" Target="real.bin"/>"#;
        for decoy in [
            r#"<d:Relationship xmlns:d="urn:decoy" Id="r8" Type="t" Target="decoy.bin"/>"#,
            r#"<Relationship xmlns="urn:decoy" Id="r8" Type="t" Target="decoy.bin"/>"#,
            r#"<x xmlns="urn:decoy"><Relationship Target="decoy.bin"/></x>"#,
            r#"<Relationships><Relationship Target="decoy.bin"/></Relationships>"#,
        ] {
            let xml = rels(&format!("{decoy}{real}"));
            assert_eq!(targets(&xml), ["real.bin"], "{decoy}");
        }
        // Declaring the same namespace again changes nothing.
        let same = format!(r#"<Relationship xmlns="{NS}" Target="same.bin"/>"#);
        assert_eq!(targets(&rels(&same)), ["same.bin"]);
        // A root in another namespace, or missing its namespace; a
        // prefixed root whose unprefixed children have no namespace.
        for xml in [
            r#"<Relationships xmlns="urn:decoy"><Relationship Target="a"/></Relationships>"#
                .to_owned(),
            format!(
                r#"<p:Relationships xmlns:p="{NS}"><Relationship Target="a"/></p:Relationships>"#
            ),
            r#"<Relationships><Relationship Target="a"/></Relationships>"#.to_owned(),
        ] {
            assert!(targets(&xml).is_empty(), "{xml}");
        }
    }

    /// Round-4 review: namespace names compare exactly. A space or a
    /// newline makes another namespace, so a decoy part declared that way
    /// is not a relationships part, and a child declared that way is not
    /// a relationship.
    #[test]
    fn namespace_names_compare_exactly() {
        for near in [
            format!("{NS} "),
            format!("\n{NS}"),
            format!(" {NS}\t"),
            format!("{NS}&#32;"),
            format!("&#10;{NS}"),
        ] {
            let root = format!(
                r#"<Relationships xmlns="{near}"><Relationship Target="decoy.bin"/></Relationships>"#
            );
            assert!(targets(&root).is_empty(), "{near:?}");
            let child = rels(&format!(
                r#"<Relationship xmlns="{near}" Target="decoy.bin"/><Relationship Target="real.bin"/>"#
            ));
            assert_eq!(targets(&child), ["real.bin"], "{near:?}");
            let prefixed = rels(&format!(
                r#"<r:Relationship xmlns:r="{near}" Target="decoy.bin"/><Relationship Target="real.bin"/>"#
            ));
            assert_eq!(targets(&prefixed), ["real.bin"], "{near:?}");
        }
        // Character references are replaced before comparing.
        let escaped = NS.replace('/', "&#47;");
        let xml = format!(
            r#"<Relationships xmlns="{escaped}"><Relationship Target="real.bin"/></Relationships>"#
        );
        assert_eq!(targets(&xml), ["real.bin"]);
    }

    /// Prefixes resolve the namespace-aware way: a prefix bound to the
    /// relationships namespace, on the root or on the element, is the
    /// same as no prefix under that default namespace.
    #[test]
    fn prefixes_resolve_like_a_namespace_aware_reader() {
        let prefixed_root = format!(
            r#"<r:Relationships xmlns:r="{NS}"><r:Relationship Target="a.bin"/><Relationship Target="no-ns.bin"/><d:Relationship Target="unbound.bin"/></r:Relationships>"#
        );
        assert_eq!(targets(&prefixed_root), ["a.bin"]);
        let found = elements(&prefixed_root, NS, WANTED, 10).unwrap();
        assert_eq!(found[0].name, "Relationship");
        // Declared on the element itself, under the default namespace.
        let own = rels(&format!(
            r#"<q:Relationship xmlns:q="{NS}" Target="own.bin"/>"#
        ));
        assert_eq!(targets(&own), ["own.bin"]);
        // Both a default and a prefix on the root.
        let both = format!(
            r#"<Relationships xmlns="{NS}" xmlns:r="{NS}"><r:Relationship Target="r.bin"/><Relationship Target="plain.bin"/></Relationships>"#
        );
        assert_eq!(targets(&both), ["r.bin", "plain.bin"]);
        // A prefix rebound on the element wins over the root's.
        let rebound = format!(
            r#"<r:Relationships xmlns:r="{NS}"><r:Relationship xmlns:r="urn:decoy" Target="decoy.bin"/><r:Relationship Target="real.bin"/></r:Relationships>"#
        );
        assert_eq!(targets(&rebound), ["real.bin"]);
        // `xmlns=""` takes an element out of the default namespace.
        assert_eq!(
            targets(&rels(
                r#"<Relationship xmlns="" Target="decoy.bin"/><Relationship Target="real.bin"/>"#
            )),
            ["real.bin"]
        );
        // A prefixed attribute is not the package attribute, even with
        // the prefix bound to the namespace.
        let attr = format!(
            r#"<r:Relationships xmlns:r="{NS}"><r:Relationship r:Target="decoy.bin"/></r:Relationships>"#
        );
        assert_eq!(targets(&attr), [""]);
    }

    /// Reviewer cases: namespace declarations and prefixed attributes
    /// named like the package attributes.
    #[test]
    fn only_unprefixed_attributes_match() {
        for tag in [
            r#"<Relationship xmlns:Target="decoy.bin" Type="t" Target="real.bin"/>"#,
            r#"<Relationship xmlns:Target="urn:decoy" Target="real.bin"/>"#,
            r#"<Relationship xmlns:o="urn:o" o:Target="decoy.bin" Target="real.bin"/>"#,
            r#"<Relationship Target="real.bin" xmlns:Target="decoy.bin"/>"#,
        ] {
            assert_eq!(targets(&rels(tag)), ["real.bin"], "{tag}");
        }
        let xml = rels(
            r#"<Relationship xmlns:Type="urn:x" xmlns:TargetMode="External" o:TargetMode="External" Type="real"/>"#,
        );
        let found = elements(&xml, NS, WANTED, 10).unwrap();
        assert_eq!(found[0].attr("Type").as_deref(), Some("real"));
        assert_eq!(found[0].attr("TargetMode"), None);
        assert_eq!(found[0].attr("xmlns:Type").as_deref(), Some("urn:x"));
    }

    #[test]
    fn doctype_and_too_many_are_refused() {
        assert_eq!(
            elements("<!DOCTYPE x><Relationship/>", NS, WANTED, 10),
            Err(XmlError::Doctype)
        );
        let many = rels(&"<Relationship/>".repeat(11));
        assert_eq!(elements(&many, NS, WANTED, 10), Err(XmlError::TooMany));
    }

    #[test]
    fn broken_xml_never_panics() {
        for body in [
            "<",
            "<!--",
            "<![CDATA[",
            "<?",
            "</",
            "<Relationship",
            "<Relationship Id=",
            "<Relationship Id=\"x",
            "<Relationship Id=x>",
            "<Relationship =\"\" />",
            "<Relationship é='\u{1F600}'>",
            "</a></b></c><Relationship/>",
        ] {
            for xml in [body.to_owned(), rels(body)] {
                if let Ok(found) = elements(&xml, NS, WANTED, 10) {
                    for e in found {
                        let _ = e.attr("Id");
                    }
                }
            }
        }
        assert_eq!(
            unescape("&#65;&#x42;&lt;&zz;&#xFFFFFFFF;&"),
            "AB<&zz;&#xFFFFFFFF;&"
        );
    }

    /// Budget: hostile parts finish in linear time.
    #[test]
    fn hostile_parts_stay_fast() {
        let amps = rels(&format!(
            "<Relationship Type=\"x\" Target=\"{}\"/>",
            "&".repeat(800 * 1024)
        ));
        let comments = rels(&"<!--".repeat(200_000));
        let open_tags = rels(&"<Relationship a='".repeat(100_000));
        let deep = rels(&"<a>".repeat(200_000));
        // A root with many declarations, and many children whose prefix
        // is looked up in them (and found in none, so all are scanned):
        // each lookup costs the same.
        let decls: String = (0..20_000).map(|i| format!(" xmlns:p{i}=\"u\"")).collect();
        let wide_root = format!(
            r#"<Relationships xmlns="{NS}"{decls}>{}</Relationships>"#,
            "<q:Relationship/><q:Relationship xmlns:z='u'/>".repeat(20_000)
        );
        for (what, xml) in [
            ("amps", &amps),
            ("comments", &comments),
            ("open tags", &open_tags),
            ("deep", &deep),
            ("wide root", &wide_root),
        ] {
            let started = Instant::now();
            if let Ok(found) = elements(xml, NS, WANTED, 10) {
                for e in found {
                    let _ = e.attr("Target");
                }
            }
            let took = started.elapsed();
            eprintln!("{what}: {took:?}");
            // Linear inputs take about 100 ms in a debug build on a shared CI
            // runner; a quadratic one takes seconds. One second keeps the test
            // about the shape of the cost, not the speed of the machine.
            assert!(took < Duration::from_secs(1), "{what}: {took:?}");
        }
    }
}
