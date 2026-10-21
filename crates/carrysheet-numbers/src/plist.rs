//! Just enough of Apple's property lists to read the version metadata.
//!
//! `Metadata/Properties.plist` (usually binary) holds `fileFormatVersion`;
//! `Metadata/BuildVersionHistory.plist` (usually XML) is an array of the
//! app builds that saved the document. Only strings, arrays and dictionaries
//! are read; anything else is skipped. Both readers are bounded: a bad list
//! gives `None`, never a panic or a large allocation. Real lists are a few
//! kilobytes; larger input is ignored, and a binary list, where one string
//! can be referred to many times, may produce at most [`MAX_TEXT`] bytes of
//! strings in all.

/// The values this reader understands.
#[derive(Debug, PartialEq)]
pub(crate) enum Value {
    String(String),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
    Other,
}

impl Value {
    /// A string value for `key` in a dictionary.
    pub(crate) fn string_for(&self, key: &str) -> Option<&str> {
        match self {
            Self::Dict(entries) => entries.iter().find_map(|(k, v)| match v {
                Value::String(s) if k == key => Some(s.as_str()),
                _ => None,
            }),
            _ => None,
        }
    }

    /// The strings of an array.
    pub(crate) fn strings(&self) -> Vec<String> {
        match self {
            Self::Array(items) => items
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Parses a binary or XML property list.
pub(crate) fn parse(data: &[u8]) -> Option<Value> {
    if data.len() > MAX_INPUT {
        return None;
    }
    if data.starts_with(b"bplist00") {
        Binary::new(data)?.root()
    } else {
        parse_xml(std::str::from_utf8(data).ok()?)
    }
}

const MAX_DEPTH: u32 = 8;
const MAX_ITEMS: usize = 10_000;
/// The largest property list read.
pub(crate) const MAX_INPUT: usize = 1 << 20;
/// The most bytes of strings one binary list may produce.
const MAX_TEXT: usize = 1 << 20;

/// What a binary list may still produce.
struct Left {
    items: usize,
    text: usize,
}

impl Left {
    fn text(&mut self, len: usize) -> Option<()> {
        self.text = self.text.checked_sub(len)?;
        Some(())
    }
}

struct Binary<'a> {
    data: &'a [u8],
    offsets: Vec<usize>,
    ref_size: usize,
    root: usize,
}

impl<'a> Binary<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let trailer = data.get(data.len().checked_sub(32)?..)?;
        let offset_size = usize::from(trailer[6]);
        let ref_size = usize::from(trailer[7]);
        let count = be(&trailer[8..16]);
        let root = be(&trailer[16..24]);
        let table = be(&trailer[24..32]);
        if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) || count > MAX_ITEMS {
            return None;
        }
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            let at = table.checked_add(i.checked_mul(offset_size)?)?;
            offsets.push(be(data.get(at..at.checked_add(offset_size)?)?));
        }
        Some(Self {
            data,
            offsets,
            ref_size,
            root,
        })
    }

    fn root(&self) -> Option<Value> {
        let mut left = Left {
            items: MAX_ITEMS,
            text: MAX_TEXT,
        };
        self.object(self.root, 0, &mut left)
    }

    fn object(&self, index: usize, depth: u32, left: &mut Left) -> Option<Value> {
        if depth > MAX_DEPTH || left.items == 0 {
            return None;
        }
        left.items -= 1;
        let at = *self.offsets.get(index)?;
        let marker = *self.data.get(at)?;
        let (kind, low) = (marker >> 4, usize::from(marker & 0x0f));
        let (len, start) = match kind {
            0x5 | 0x6 | 0xa | 0xd => self.length(at, low)?,
            _ => return Some(Value::Other),
        };
        match kind {
            0x5 => {
                let bytes = self.data.get(start..start.checked_add(len)?)?;
                left.text(len)?;
                Some(Value::String(
                    bytes.iter().map(|&b| char::from(b)).collect(),
                ))
            }
            0x6 => {
                let bytes = self
                    .data
                    .get(start..start.checked_add(len.checked_mul(2)?)?)?;
                // Each UTF-16 unit becomes up to 3 bytes of UTF-8.
                left.text(len.checked_mul(3)?)?;
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_be_bytes(c))
                    .collect();
                Some(Value::String(String::from_utf16_lossy(&units)))
            }
            0xa => {
                let refs = self.refs(start, len)?;
                let items = refs
                    .into_iter()
                    .map(|r| self.object(r, depth + 1, left))
                    .collect::<Option<Vec<_>>>()?;
                Some(Value::Array(items))
            }
            _ => {
                let refs = self.refs(start, len.checked_mul(2)?)?;
                let (keys, values) = refs.split_at(len);
                let mut entries = Vec::with_capacity(len);
                for (&k, &v) in keys.iter().zip(values) {
                    if let Some(Value::String(key)) = self.object(k, depth + 1, left) {
                        entries.push((key, self.object(v, depth + 1, left)?));
                    }
                }
                Some(Value::Dict(entries))
            }
        }
    }

    /// The length of an object and where its contents start.
    fn length(&self, at: usize, low: usize) -> Option<(usize, usize)> {
        if low != 0x0f {
            return Some((low, at + 1));
        }
        let marker = *self.data.get(at + 1)?;
        if marker >> 4 != 0x1 {
            return None;
        }
        if marker & 0x0f > 3 {
            return None;
        }
        let size = 1usize << (marker & 0x0f);
        let start = at + 2;
        let len = be(self.data.get(start..start.checked_add(size)?)?);
        (len <= self.data.len()).then_some((len, start + size))
    }

    fn refs(&self, start: usize, count: usize) -> Option<Vec<usize>> {
        if count > MAX_ITEMS {
            return None;
        }
        let bytes = self
            .data
            .get(start..start.checked_add(count.checked_mul(self.ref_size)?)?)?;
        Some(bytes.chunks_exact(self.ref_size).map(be).collect())
    }
}

/// A big-endian unsigned number of up to 8 bytes, saturated to `usize`.
fn be(bytes: &[u8]) -> usize {
    let value = bytes.iter().fold(0u64, |acc, &b| acc << 8 | u64::from(b));
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A small XML plist reader: only `<dict>`, `<array>`, `<key>` and
/// `<string>` matter; other elements become [`Value::Other`].
fn parse_xml(text: &str) -> Option<Value> {
    let body = text.split_once("<plist")?.1;
    let body = body.split_once('>')?.1;
    let mut tokens = Tokens {
        rest: body,
        count: 0,
    };
    tokens.value(0)
}

struct Tokens<'a> {
    rest: &'a str,
    count: usize,
}

impl Tokens<'_> {
    /// The next tag name, such as `string`, `/array` or `true/`.
    fn next_tag(&mut self) -> Option<&str> {
        self.count += 1;
        if self.count > MAX_ITEMS {
            return None;
        }
        let start = self.rest.find('<')?;
        let end = start + self.rest[start..].find('>')?;
        let tag = self.rest[start + 1..end].trim();
        self.rest = &self.rest[end + 1..];
        Some(tag)
    }

    fn text_until(&mut self, close: &str) -> Option<String> {
        let end = self.rest.find(close)?;
        let raw = &self.rest[..end];
        self.rest = &self.rest[end + close.len()..];
        Some(unescape(raw))
    }

    fn value(&mut self, depth: u32) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        let tag = self.next_tag()?.to_owned();
        self.value_for(&tag, depth)
    }

    fn value_for(&mut self, tag: &str, depth: u32) -> Option<Value> {
        match tag {
            "string" => Some(Value::String(self.text_until("</string>")?)),
            "string/" => Some(Value::String(String::new())),
            "array" => {
                let mut items = Vec::new();
                loop {
                    let tag = self.next_tag()?.to_owned();
                    if tag == "/array" {
                        return Some(Value::Array(items));
                    }
                    items.push(self.value_for(&tag, depth + 1)?);
                }
            }
            "dict" => {
                let mut entries = Vec::new();
                loop {
                    let tag = self.next_tag()?.to_owned();
                    match tag.as_str() {
                        "/dict" => return Some(Value::Dict(entries)),
                        "key" => {
                            let key = self.text_until("</key>")?;
                            let value = self.value(depth + 1)?;
                            entries.push((key, value));
                        }
                        _ => return None,
                    }
                }
            }
            "array/" => Some(Value::Array(Vec::new())),
            "dict/" => Some(Value::Dict(Vec::new())),
            t if t.ends_with('/') => Some(Value::Other),
            t if !t.starts_with('/') => {
                // A scalar such as <integer>2</integer>: skip to its end.
                let close = format!("</{t}>");
                self.text_until(&close)?;
                Some(Value::Other)
            }
            _ => None,
        }
    }
}

fn unescape(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_array_and_dict() {
        let xml = r#"<?xml version="1.0"?><plist version="1.0"><dict>
            <key>fileFormatVersion</key><string>11.0.3</string>
            <key>n</key><integer>2</integer><key>t</key><true/>
            <key>list</key><array><string>A &amp; B</string><string/></array>
            </dict></plist>"#;
        let v = parse(xml.as_bytes()).unwrap();
        assert_eq!(v.string_for("fileFormatVersion"), Some("11.0.3"));
        match &v {
            Value::Dict(e) => assert_eq!(e[3].1.strings(), ["A & B", ""]),
            _ => panic!("not a dict"),
        }
    }

    #[test]
    fn bad_lists_give_none() {
        assert_eq!(parse(b"bplist00"), None);
        assert_eq!(parse(b"<plist><dict><key>a</key>"), None);
        let mut deep = String::from("<plist>");
        deep.push_str(&"<array>".repeat(100));
        assert_eq!(parse(deep.as_bytes()), None);
    }

    /// A binary list whose root array refers to one string `count` times.
    fn shared_string(count: usize, len: usize) -> Vec<u8> {
        let mut data = b"bplist00".to_vec();
        let string_at = data.len();
        data.push(0x5f);
        data.push(0x12); // a 4-byte length follows
        data.extend_from_slice(&(len as u32).to_be_bytes());
        data.resize(data.len() + len, b'x');
        let array_at = data.len();
        data.push(0xaf);
        data.push(0x12);
        data.extend_from_slice(&(count as u32).to_be_bytes());
        data.resize(data.len() + count * 2, 0); // every ref is object 0
        let table = data.len();
        data.extend_from_slice(&(string_at as u64).to_be_bytes());
        data.extend_from_slice(&(array_at as u64).to_be_bytes());
        let mut trailer = [0u8; 32];
        trailer[6] = 8; // offset size
        trailer[7] = 2; // ref size
        trailer[8..16].copy_from_slice(&2u64.to_be_bytes());
        trailer[16..24].copy_from_slice(&1u64.to_be_bytes());
        trailer[24..32].copy_from_slice(&(table as u64).to_be_bytes());
        data.extend_from_slice(&trailer);
        data
    }

    #[test]
    fn one_string_referred_to_many_times_is_bounded() {
        // Small enough: read.
        let v = parse(&shared_string(3, 10)).unwrap();
        assert_eq!(v.strings().len(), 3);
        // 9,000 references to a 60 KB string would be 540 MB of strings.
        let data = shared_string(9_000, 60_000);
        assert!(data.len() < 100_000);
        let start = std::time::Instant::now();
        assert_eq!(parse(&data), None);
        assert!(start.elapsed().as_secs() < 2);
        // Input over 1 MiB is not read at all.
        assert_eq!(parse(&vec![b' '; (1 << 20) + 1]), None);
    }
}
