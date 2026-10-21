//! Just enough JSON to read the baseline back, so the bench needs no
//! dependency. Writing is done with `format!` and [`quote`].

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// A number, as written, so a u64 keeps every digit.
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) => n.parse().ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
}

/// A JSON string literal for `s`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `Some(n)` as a number, `None` as `null`.
pub fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".to_string(), |v| v.to_string())
}

pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("text after the end of the JSON value"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("{what} at byte {}", self.i)
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: u32) -> Result<Json, String> {
        if depth > 32 {
            return Err(self.err("nested too deeply"));
        }
        self.ws();
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.eat("}") {
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return Err(self.err("expected ':'"));
                    }
                    fields.push((key, self.value(depth + 1)?));
                    self.ws();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("}") {
                        return Ok(Json::Obj(fields));
                    }
                    return Err(self.err("expected ',' or '}'"));
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat("]") {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("]") {
                        return Ok(Json::Arr(items));
                    }
                    return Err(self.err("expected ',' or ']'"));
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(_) if self.eat("null") => Ok(Json::Null),
            Some(_) if self.eat("true") => Ok(Json::Bool(true)),
            Some(_) if self.eat("false") => Ok(Json::Bool(false)),
            Some(_) => {
                let start = self.i;
                while self.i < self.s.len()
                    && matches!(
                        self.s[self.i],
                        b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
                    )
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i])
                    .ok()
                    .filter(|t| t.parse::<f64>().is_ok())
                    .map(|t| Json::Num(t.to_string()))
                    .ok_or_else(|| self.err("expected a value"))
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if !self.eat("\"") {
            return Err(self.err("expected a string"));
        }
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return Err(self.err("string never ends"));
            };
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| self.err("not UTF-8")),
                b'\\' => {
                    let e = *self
                        .s
                        .get(self.i)
                        .ok_or_else(|| self.err("string never ends"))?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hex = self
                                .s
                                .get(self.i..self.i + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .ok_or_else(|| self.err("bad \\u escape"))?;
                            self.i += 4;
                            char::from_u32(hex).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.err("bad escape")),
                    };
                    out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let v = parse(r#"{"a": [1, 2.5, null, true], "b": "x\"y\\z\n", "c": {}}"#).unwrap();
        assert_eq!(
            v.get("a").unwrap().as_arr().unwrap()[1],
            Json::Num("2.5".into())
        );
        assert_eq!(v.get("b").unwrap().as_str(), Some("x\"y\\z\n"));
        assert_eq!(
            parse(&quote("é\"\u{1}")).unwrap().as_str(),
            Some("é\"\u{1}")
        );
        let big = parse("18446744073709551615").unwrap();
        assert_eq!(big.as_u64(), Some(u64::MAX));
        assert!(parse("[1,").is_err());
        assert!(parse("{} x").is_err());
    }
}
