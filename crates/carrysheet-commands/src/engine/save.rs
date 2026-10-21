//! Saving: what a save keeps and loses (record 0003, point 3), and writing
//! files so a failure never leaves half a workbook behind.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::FileFormat;
use crate::error::{CommandError, ErrorCode};
use crate::paths::display_path;

/// Most dropped package parts listed by name; the rest are counted.
pub const MAX_LISTED_PARTS: usize = 20;

/// One kind of content a save would not keep.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Loss {
    /// What, in plain words: `charts`, `VBA project`, `formulas`.
    pub feature: String,
    /// How many the opened file has.
    pub before: u64,
    /// How many the saved file will have.
    pub after: u64,
}

/// How the package parts of the opened file fare in the saved one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct PartCounts {
    /// Written back byte for byte.
    pub identical: u64,
    /// Written again by the kernel (content kept, bytes differ).
    pub rewritten: u64,
    pub dropped: u64,
    /// New in the saved file.
    pub added: u64,
}

/// What a save will keep and lose, found before writing anything.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SaveReport {
    pub format: FileFormat,
    /// What the saved file will not have that the opened one had. Empty
    /// means nothing known is lost.
    pub lost: Vec<Loss>,
    /// Package parts of the opened file that the saved one will not have,
    /// as quoted and shortened names, at most [`MAX_LISTED_PARTS`].
    pub dropped_parts: Vec<String>,
    /// For an xlsx opened and saved as xlsx: the parts compared.
    pub parts: Option<PartCounts>,
    /// Things worth knowing that are not losses, such as a cache Excel
    /// rebuilds itself.
    pub notes: Vec<String>,
}

impl SaveReport {
    pub fn new(format: FileFormat) -> SaveReport {
        SaveReport {
            format,
            lost: Vec::new(),
            dropped_parts: Vec::new(),
            parts: None,
            notes: Vec::new(),
        }
    }

    /// True when saving would lose something, so it needs `confirm`.
    pub fn loses_something(&self) -> bool {
        !self.lost.is_empty() || !self.dropped_parts.is_empty()
    }

    /// One line naming every loss, such as `VBA project, charts (2), tables (1 of 3)`.
    pub fn summary(&self) -> String {
        let mut items: Vec<String> = self
            .lost
            .iter()
            .map(|l| match (l.before, l.after) {
                (1, 0) => l.feature.clone(),
                (n, 0) => format!("{} ({n})", l.feature),
                (n, kept) => format!("{} ({} of {n})", l.feature, n.saturating_sub(kept)),
            })
            .collect();
        let dropped = self.dropped_parts.len();
        if dropped > 0 {
            let more = if dropped >= MAX_LISTED_PARTS { "+" } else { "" };
            items.push(format!("{dropped}{more} package part(s)"));
        }
        items.join(", ")
    }
}

/// A workbook encoded for saving, and what the save keeps and loses.
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub report: SaveReport,
}

/// Text from a file, made safe to show: control, bidi and invisible
/// characters escaped, cut at `max` characters.
pub fn clean_from_file(text: &str, max: usize) -> String {
    escape(text, max, false)
}

/// [`clean_from_file`] in double quotes, with quotes and backslashes escaped,
/// for naming something from a file inside a message.
pub fn quote_from_file(text: &str, max: usize) -> String {
    format!("\"{}\"", escape(text, max, true))
}

fn escape(text: &str, max: usize, quotes: bool) -> String {
    let mut out = String::new();
    for (shown, ch) in text.chars().enumerate() {
        if shown == max {
            out.push_str("...");
            break;
        }
        let invisible = ch.is_control()
            || matches!(ch, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}');
        if invisible {
            out.push_str(&format!("\\u{{{:x}}}", ch as u32));
        } else if quotes && (ch == '"' || ch == '\\') {
            out.push('\\');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Where `edit` writes when no output is given: beside the original, as
/// `name-edited.xlsx`, or `name-edited-2.xlsx` and so on when taken.
pub fn default_output(input: &Path) -> Result<PathBuf, CommandError> {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "book".to_owned());
    let ext = input
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = input.parent().unwrap_or(Path::new(""));
    for n in 1..=99 {
        let name = if n == 1 {
            format!("{stem}-edited{ext}")
        } else {
            format!("{stem}-edited-{n}{ext}")
        };
        let candidate = dir.join(name);
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(CommandError::new(
        ErrorCode::FileExists,
        format!("no free name beside {}", display_path(input)),
        "the names name-edited.xlsx to name-edited-99.xlsx are all taken",
        "Pass output with the file to write.",
    ))
}

/// Refuses to replace an existing file unless `overwrite` is set.
pub fn check_output(path: &Path, overwrite: bool) -> Result<(), CommandError> {
    if overwrite || std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::FileExists,
        format!("{} already exists", display_path(path)),
        "Carrysheet never replaces a file unless asked",
        "Pass a different output, or set overwrite and confirm to true to replace it.",
    ))
}

fn io_error(path: &Path, e: &std::io::Error) -> CommandError {
    let code = match e.kind() {
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::NotFound => ErrorCode::FileNotFound,
        _ => ErrorCode::Io,
    };
    CommandError::new(
        code,
        format!("{} could not be written", display_path(path)),
        e.to_string(),
        "Check that the folder exists, you may write there, and no other program has the file open.",
    )
}

thread_local! {
    /// The folder [`write_atomic`] must stay inside on this thread (the MCP
    /// root, canonical), while a [`WriteRoot`] lives.
    static WRITE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Confines [`write_atomic`] on this thread to a folder until dropped.
#[must_use = "the confinement ends when this is dropped"]
pub struct WriteRoot(Option<PathBuf>);

/// Keeps every [`write_atomic`] on this thread inside `root` while the
/// returned guard lives (review round 2, L-a): a link changed after the
/// path was resolved cannot lead the write outside.
pub fn confine_writes(root: &Path) -> WriteRoot {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    WriteRoot(WRITE_ROOT.with(|r| r.replace(Some(root))))
}

impl Drop for WriteRoot {
    fn drop(&mut self) {
        let previous = self.0.take();
        WRITE_ROOT.with(|r| *r.borrow_mut() = previous);
    }
}

/// Refuses when a write confinement is set and `target`'s folder, with
/// every link followed, is not inside it, or `target` itself now leads
/// elsewhere than `expected`.
fn still_inside(target: &Path, expected: &Path, dir: &Path) -> Result<(), CommandError> {
    let Some(root) = WRITE_ROOT.with(|r| r.borrow().clone()) else {
        return Ok(());
    };
    let inside = link_target(target) == expected
        && std::fs::canonicalize(dir).is_ok_and(|d| d.starts_with(&root));
    if inside {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::OutsideRoot,
        format!("{} is outside the root folder", display_path(target)),
        "a link on the way to the file changed while it was being saved, and now leads outside the root folder",
        "Use a path inside the root folder that is not a link changed by another program; nothing was written.",
    ))
}

/// Writes `bytes` to `path` through a temporary file in the same folder,
/// then renames it into place: a failure leaves the old file as it was.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    // A link is written through: the file it points to is replaced and the
    // link stays (review M2).
    let given = path;
    let real = link_target(path);
    let path = real.as_path();
    // An existing file keeps its permissions (a private 600 file must not
    // become readable by others).
    let keep = std::fs::metadata(path).ok().map(|m| m.permissions());
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.carrysheet-tmp", std::process::id()));
    still_inside(given, path, dir)?;
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Some(permissions) = keep {
            // Set before any byte is written.
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error(path, &e));
    }
    // Checked again just before the rename: links may change meanwhile.
    if let Err(e) = still_inside(given, path, dir) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_error(path, &e)
    })
}

/// The file `path` names, following symbolic links (at most 40, as the
/// system does). A link to a missing file gives the missing file's path.
fn link_target(path: &Path) -> std::path::PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..40 {
        let is_link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            break;
        }
        let Ok(to) = std::fs::read_link(&p) else {
            break;
        };
        p = match p.parent() {
            Some(dir) if to.is_relative() => dir.join(to),
            _ => to,
        };
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn confined_writes_refuse_a_link_out_of_the_root() {
        let base = std::env::temp_dir().join(format!("carrysheet-confine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // As if `sub` was swapped for a link after the path was resolved.
        std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
        let guard = confine_writes(&root);
        write_atomic(&root.join("sub/a.csv"), b"1").unwrap();
        let e = write_atomic(&root.join("away/b.csv"), b"1").unwrap_err();
        assert_eq!(e.code, ErrorCode::OutsideRoot, "{e:?}");
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        // A link to a file outside, too.
        std::fs::write(outside.join("c.csv"), b"old").unwrap();
        std::os::unix::fs::symlink(outside.join("c.csv"), root.join("c.csv")).unwrap();
        assert!(write_atomic(&root.join("c.csv"), b"new").is_err());
        assert_eq!(std::fs::read(outside.join("c.csv")).unwrap(), b"old");
        drop(guard);
        // Without a root (the CLI), links are written through as before.
        write_atomic(&root.join("c.csv"), b"new").unwrap();
        assert_eq!(std::fs::read(outside.join("c.csv")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn file_text_is_quoted_escaped_and_cut() {
        assert_eq!(quote_from_file("xl/a.xml", 64), "\"xl/a.xml\"");
        assert_eq!(
            quote_from_file("a\u{202E}b\n\"", 64),
            "\"a\\u{202e}b\\u{a}\\\"\""
        );
        assert_eq!(quote_from_file(&"x".repeat(100), 3), "\"xxx...\"");
    }

    #[test]
    fn reports_say_what_is_lost() {
        let mut r = SaveReport::new(FileFormat::Xlsx);
        assert!(!r.loses_something());
        r.lost.push(Loss {
            feature: "charts".into(),
            before: 3,
            after: 1,
        });
        assert!(r.loses_something());
        assert_eq!(r.summary(), "charts (2 of 3)");
    }

    #[cfg(unix)]
    #[test]
    fn writes_keep_permissions_and_links() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("carrysheet-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        // A private file stays private when replaced (review M2).
        let book = dir.join("real/book.xlsx");
        std::fs::write(&book, b"one").unwrap();
        std::fs::set_permissions(&book, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&book, b"two").unwrap();
        let mode = std::fs::metadata(&book).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Writing to a link replaces the file it points to; the link stays.
        let link = dir.join("link.xlsx");
        std::os::unix::fs::symlink("real/book.xlsx", &link).unwrap();
        write_atomic(&link, b"three").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&book).unwrap(), b"three");
        let mode = std::fs::metadata(&book).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_dir(dir.join("real")).unwrap().count(),
            1,
            "no temporary file left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_replace_whole_files() {
        let dir = std::env::temp_dir().join(format!("carrysheet-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        write_atomic(&book, b"one").unwrap();
        write_atomic(&book, b"two").unwrap();
        assert_eq!(std::fs::read(&book).unwrap(), b"two");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary file left"
        );
        assert_eq!(default_output(&book).unwrap(), dir.join("book-edited.xlsx"));
        std::fs::write(dir.join("book-edited.xlsx"), b"x").unwrap();
        assert_eq!(
            default_output(&book).unwrap(),
            dir.join("book-edited-2.xlsx")
        );
        assert_eq!(
            check_output(&book, false).unwrap_err().code,
            ErrorCode::FileExists
        );
        assert!(check_output(&book, true).is_ok());
        let missing = dir.join("no/such/folder/x.xlsx");
        assert!(write_atomic(&missing, b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
