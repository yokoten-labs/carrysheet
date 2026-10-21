//! Errors with three parts: what happened, why, and how to fix it.

use std::fmt;

/// Everything that can stop a VBA project from being read or kept.
///
/// Each error explains itself in three parts: [`VbaError::what`],
/// [`VbaError::why`] and [`VbaError::fix`]. `Display` joins all three.
/// Errors never quote macro source or passwords, so they are safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VbaError {
    /// The bytes are not an OLE2 compound file (the container of
    /// `vbaProject.bin` and `.xls`).
    NotCompoundFile {
        /// What the bytes look like instead.
        detail: String,
    },
    /// The compound file is damaged, or its reader gave up on it.
    Container {
        /// What the compound file reader reported.
        detail: String,
    },
    /// A stream or storage the VBA project needs is not there.
    MissingStream {
        /// The path of the stream, relative to the VBA project storage,
        /// such as `VBA/dir`.
        stream: String,
    },
    /// A compressed container (MS-OVBA 2.4.1) cannot be decompressed.
    BadCompression {
        /// The stream that holds the container, when known.
        stream: String,
        /// Byte offset inside the compressed data.
        offset: usize,
        /// What is wrong at that offset.
        detail: String,
    },
    /// The `dir` stream (MS-OVBA 2.3.4.2) does not follow the format.
    BadDir {
        /// Byte offset inside the decompressed `dir` stream.
        offset: usize,
        /// What is wrong at that offset.
        detail: String,
    },
    /// A module's source offset points past the end of its stream.
    BadModuleOffset {
        /// The module name.
        module: String,
        /// The offset from the `dir` stream (MODULEOFFSET).
        offset: u64,
        /// The length of the module stream.
        stream_len: u64,
    },
    /// A safety limit from [`Limits`](crate::Limits) was reached.
    LimitExceeded {
        /// Which limit, such as `decompressed bytes per stream`.
        limit: &'static str,
        /// The limit's value.
        max: u64,
    },
    /// Writing the kept VBA storage into another compound file failed.
    WriteFailed {
        /// What the compound file writer reported.
        detail: String,
    },
}

impl VbaError {
    /// What happened, in one sentence.
    pub fn what(&self) -> String {
        match self {
            VbaError::NotCompoundFile { .. } => {
                "The VBA project is not an OLE2 compound file.".into()
            }
            VbaError::Container { .. } => {
                "The compound file that holds the macros is damaged.".into()
            }
            VbaError::MissingStream { stream } => {
                format!("The VBA project has no `{}` stream.", printable(stream))
            }
            VbaError::BadCompression { stream, offset, .. } => {
                if stream.is_empty() {
                    format!("Compressed VBA data is broken at byte {offset}.")
                } else {
                    format!(
                        "The compressed `{}` stream is broken at byte {offset}.",
                        printable(stream)
                    )
                }
            }
            VbaError::BadDir { offset, .. } => {
                format!("The VBA `dir` stream is broken at byte {offset}.")
            }
            VbaError::BadModuleOffset { module, .. } => {
                format!(
                    "Module `{}` says its source starts past the end of its stream.",
                    printable(module)
                )
            }
            VbaError::LimitExceeded { limit, max } => {
                format!("The VBA project is over the limit on {limit} ({max}).")
            }
            VbaError::WriteFailed { .. } => {
                "The macros could not be copied into the new file.".into()
            }
        }
    }

    /// Why it happened.
    pub fn why(&self) -> String {
        match self {
            VbaError::NotCompoundFile { detail } => format!("The bytes {}.", printable(detail)),
            VbaError::Container { detail } => {
                format!("The compound file reader said: {}.", printable(detail))
            }
            VbaError::MissingStream { stream } => {
                if stream.eq_ignore_ascii_case("VBA/dir") {
                    "Every VBA project lists its modules in `VBA/dir`; without it no module can be found, so the project was cut or damaged.".into()
                } else if is_module_stream(stream) {
                    "The module list names this stream, but it is not in the file, so neither the module's source nor its compiled code is there.".into()
                } else {
                    "The storage that holds the VBA project is not in the file.".into()
                }
            }
            VbaError::BadCompression { stream, detail, .. } => {
                if is_module_stream(stream) {
                    format!(
                        "{}; the module's source is damaged or was deliberately hidden.",
                        printable(detail)
                    )
                } else if stream.is_empty() {
                    format!("{}.", printable(detail))
                } else {
                    format!("{}; the list of modules cannot be read.", printable(detail))
                }
            }
            VbaError::BadDir { detail, .. } => format!("{}.", printable(detail)),
            VbaError::BadModuleOffset {
                offset, stream_len, ..
            } => format!(
                "The module list gives source offset {offset}, but the module stream has only {stream_len} bytes; the source is damaged or was deliberately hidden."
            ),
            VbaError::LimitExceeded { .. } => {
                "Limits stop damaged or hostile files from using all memory; real VBA projects stay far below them.".into()
            }
            VbaError::WriteFailed { detail } => {
                format!("The compound file writer said: {}.", printable(detail))
            }
        }
    }

    /// How to fix it.
    pub fn fix(&self) -> String {
        match self {
            VbaError::NotCompoundFile { .. } => {
                "Pass the bytes of `xl/vbaProject.bin` from inside the .xlsm or .xlsb package, or the whole .xls file to the .xls reader.".into()
            }
            VbaError::LimitExceeded { limit, .. } if limit.starts_with("compressed source bytes") => {
                "Modules here point into the same bytes again and again, which Excel never writes; treat the file as hostile and do not enable its macros.".into()
            }
            VbaError::LimitExceeded { .. } => {
                "If the file is trusted, raise the limit in `Limits`; otherwise treat the file as hostile.".into()
            }
            VbaError::WriteFailed { .. } => {
                "Check that the target storage name is free and the output is writable, then save again.".into()
            }
            VbaError::BadCompression { stream, .. } if is_module_stream(stream) => {
                MAY_RUN_HIDDEN.into()
            }
            VbaError::BadModuleOffset { .. } => MAY_RUN_HIDDEN.into(),
            VbaError::BadCompression { .. }
            | VbaError::MissingStream { .. }
            | VbaError::Container { .. }
            | VbaError::BadDir { .. } => {
                "Treat the file as untrusted and do not enable its macros. If you need the macros, restore the file from a trusted copy; the cell data is not affected.".into()
            }
        }
    }

    pub(crate) fn limit(limit: &'static str, max: usize) -> Self {
        VbaError::LimitExceeded {
            limit,
            max: max as u64,
        }
    }

    pub(crate) fn dir(offset: usize, detail: impl Into<String>) -> Self {
        VbaError::BadDir {
            offset,
            detail: detail.into(),
        }
    }

    /// Names the stream on a [`VbaError::BadCompression`] error.
    pub(crate) fn in_stream(self, name: &str) -> Self {
        match self {
            VbaError::BadCompression { offset, detail, .. } => VbaError::BadCompression {
                stream: name.to_string(),
                offset,
                detail,
            },
            other => other,
        }
    }
}

const MAY_RUN_HIDDEN: &str = "Treat the file as untrusted and do not enable its macros: Excel may still run compiled code that cannot be read here. If you need the macros, restore the file from a trusted copy; the cell data is not affected.";

/// A module's stream, as opposed to `VBA/dir`, a storage or no stream.
fn is_module_stream(stream: &str) -> bool {
    stream.len() > 4
        && stream.as_bytes()[..4].eq_ignore_ascii_case(b"VBA/")
        && !stream.eq_ignore_ascii_case("VBA/dir")
}

/// Characters of a name or detail shown in a message at most.
const MAX_SHOWN: usize = 200;

/// Makes text from the file safe to show: control, invisible (soft
/// hyphen, word joiners, tag characters, variation selectors), line
/// separator and bidirectional formatting characters are escaped
/// (`\u{1b}`), a backslash is doubled so an escape cannot be faked, and the
/// text is cut at about 200 characters (`…` marks the cut).
pub(crate) fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_SHOWN + 8));
    let mut shown = 0;
    for c in text.chars() {
        if shown >= MAX_SHOWN {
            out.push('…');
            break;
        }
        shown += 1;
        if c == '\\' {
            shown += 1;
            out.push_str("\\\\");
        } else if c.is_control()
            || matches!(
                c,
                '\u{00AD}'
                    | '\u{061C}'
                    | '\u{180E}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{2028}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FE00}'..='\u{FE0F}'
                    | '\u{FEFF}'
                    | '\u{E0000}'..='\u{E007F}'
                    | '\u{E0100}'..='\u{E01EF}'
            )
        {
            let escaped: String = c.escape_unicode().collect();
            shown += escaped.chars().count() - 1;
            out.push_str(&escaped);
        } else {
            out.push(c);
        }
    }
    out
}

impl fmt::Display for VbaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.what(), self.why(), self.fix())
    }
}

impl std::error::Error for VbaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_escaped_and_cut() {
        assert_eq!(printable("Mod\u{1b}[31m1"), "Mod\\u{1b}[31m1");
        assert_eq!(printable("a\u{202E}b"), "a\\u{202e}b");
        for c in [
            '\u{2028}',
            '\u{2029}',
            '\u{061C}',
            '\u{FEFF}',
            '\u{180E}',
            '\u{00AD}',
            '\u{2060}',
            '\u{2064}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{E0000}',
            '\u{E0041}',
            '\u{E007F}',
            '\u{E0100}',
            '\u{E01EF}',
        ] {
            assert_eq!(
                printable(&format!("a{c}b")),
                format!("a{}b", c.escape_unicode())
            );
        }
        // A real backslash cannot pass for an escape.
        assert_eq!(printable("a\\u{1b}b"), "a\\\\u{1b}b");
        assert_ne!(printable("a\\u{1b}b"), printable("a\u{1b}b"));
        let long = "x".repeat(500);
        assert_eq!(printable(&long).chars().count(), 201);
        let e = VbaError::MissingStream {
            stream: format!("VBA/{}\r\nInjected", "M".repeat(300)),
        };
        assert!(!e.to_string().contains('\r'));
        assert!(!e.fix().contains("save it again"));
    }
}

#[cfg(test)]
mod advice_tests {
    use super::*;

    #[test]
    fn advice_depends_on_what_was_read() {
        let module = VbaError::BadCompression {
            stream: "VBA/Module1".into(),
            offset: 0,
            detail: "bad".into(),
        };
        assert!(module.why().contains("module's source"));
        assert!(module.fix().contains("compiled code"));
        let dir = VbaError::BadCompression {
            stream: "VBA/dir".into(),
            offset: 0,
            detail: "bad".into(),
        };
        assert!(!dir.why().contains("source") && !dir.fix().contains("compiled code"));
        let generic = VbaError::BadCompression {
            stream: String::new(),
            offset: 0,
            detail: "bad".into(),
        };
        assert_eq!(generic.why(), "bad.");
        assert!(!generic.fix().contains("compiled code"));
        for stream in ["VBA/Module1", "VBA/dir", "_VBA_PROJECT_CUR"] {
            let e = VbaError::MissingStream {
                stream: stream.into(),
            };
            assert!(!e.fix().contains("compiled code"), "{stream}");
        }
    }
}
