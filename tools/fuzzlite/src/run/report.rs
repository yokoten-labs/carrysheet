//! Findings: the JSON line and the files in the output folder.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::child::Kind;
use super::paths::display_path;
use super::sink::write_text;
use crate::rng::fnv64;

/// A crash or blow-up, after minimising.
#[derive(Debug, Clone)]
pub struct Finding {
    pub target: String,
    pub kind: Kind,
    /// The panic message, the abort reason, or the time and memory used.
    pub detail: String,
    /// The format-aware mutations that led here, or `bytes`.
    pub family: String,
    pub ops: Vec<String>,
    pub seed_file: String,
    pub rng_seed: u64,
    pub case: u64,
    pub original_bytes: usize,
    pub input: Vec<u8>,
    pub millis: u64,
    pub peak_kib: Option<u64>,
    /// The minimised input on disk, once written.
    pub file: Option<PathBuf>,
}

impl Finding {
    /// A short name from the content and the signature group, for file
    /// names: the same bytes get the same id whether they timed out or
    /// finished slowly.
    pub fn id(&self) -> String {
        format!("{}-{:016x}", self.kind.group(), fnv64(&self.input))
    }

    /// One line of JSON.
    pub fn json(&self) -> String {
        JsonLine::new()
            .str("target", &self.target)
            .str("kind", self.kind.as_str())
            .str("detail", &self.detail)
            .str("family", &self.family)
            .strs("ops", &self.ops)
            .str("seed_file", &self.seed_file)
            .num("rng_seed", self.rng_seed)
            .num("case", self.case)
            .num("original_bytes", self.original_bytes as u64)
            .num("input_bytes", self.input.len() as u64)
            .num("millis", self.millis)
            .opt_num("peak_mib", self.peak_kib.map(|k| k.div_ceil(1024)))
            .opt_str("file", self.file.as_deref().map(display_path).as_deref())
            .finish()
    }

    /// Writes `<id>.bin` (minimised), `<id>.orig.bin` and `<id>.json`
    /// under `out/<target>/`.
    pub fn write(&mut self, out: &Path, original: &[u8]) -> std::io::Result<()> {
        let dir = out.join(&self.target);
        std::fs::create_dir_all(&dir)?;
        let id = self.id();
        let file = dir.join(format!("{id}.bin"));
        std::fs::write(&file, &self.input)?;
        std::fs::write(dir.join(format!("{id}.orig.bin")), original)?;
        self.file = Some(file);
        write_text(&dir.join(format!("{id}.json")), &(self.json() + "\n"))
    }
}

/// Builds one JSON object on one line. Hiding the home folder happens
/// where it is written (`sink`).
pub struct JsonLine(String);

impl Default for JsonLine {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonLine {
    pub fn new() -> JsonLine {
        JsonLine(String::from("{"))
    }

    fn key(&mut self, key: &str) {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        self.0.push_str(&quote(key));
        self.0.push(':');
    }

    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.key(key);
        self.0.push_str(&quote(value));
        self
    }

    pub fn opt_str(mut self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.str(key, v),
            None => {
                self.key(key);
                self.0.push_str("null");
                self
            }
        }
    }

    pub fn strs(mut self, key: &str, values: &[String]) -> Self {
        self.key(key);
        self.0.push('[');
        for (i, v) in values.iter().enumerate() {
            if i > 0 {
                self.0.push(',');
            }
            self.0.push_str(&quote(v));
        }
        self.0.push(']');
        self
    }

    pub fn num(mut self, key: &str, value: u64) -> Self {
        self.key(key);
        let _ = write!(self.0, "{value}");
        self
    }

    pub fn opt_num(mut self, key: &str, value: Option<u64>) -> Self {
        self.key(key);
        match value {
            Some(v) => {
                let _ = write!(self.0, "{v}");
            }
            None => self.0.push_str("null"),
        }
        self
    }

    pub fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

/// A JSON string.
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
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Finding {
        Finding {
            target: "biff".into(),
            kind: Kind::Panic,
            detail: "a \"b\"\n\u{1}".into(),
            family: "biff:sst".into(),
            ops: vec!["biff:sst".into(), "bytes:flip".into()],
            seed_file: "x.xls".into(),
            rng_seed: 1,
            case: 2,
            original_bytes: 3,
            input: vec![1, 2],
            millis: 4,
            peak_kib: Some(1025),
            file: None,
        }
    }

    #[test]
    fn json_escapes_and_has_every_field() {
        let j = finding().json();
        assert!(j.starts_with('{') && j.ends_with('}'), "{j}");
        assert!(j.contains(r#""detail":"a \"b\"\n\u0001""#), "{j}");
        assert!(j.contains(r#""ops":["biff:sst","bytes:flip"]"#), "{j}");
        assert!(j.contains(r#""peak_mib":2,"file":null"#), "{j}");
        assert!(!j.contains('\n'));
    }

    #[test]
    fn json_never_carries_the_home_folder() {
        let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) else {
            return;
        };
        let mut f = finding();
        f.seed_file = format!("{home}/seeds/a.xls");
        f.detail = format!("{home}/.cargo/registry/src/cfb/src/lib.rs:1:1: boom");
        f.ops = vec![format!("{home}/x")];
        f.file = Some(PathBuf::from(&home).join("out").join("a.bin"));
        // What reaches stdout or the .json file: through the sink.
        let j = crate::run::sink::scrub(&f.json());
        assert!(!j.contains(&home), "{j}");
        assert!(j.contains(r#""seed_file":"~/seeds/a.xls""#), "{j}");
    }
}
