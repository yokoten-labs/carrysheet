//! Every file the program touches: `--root`, bounded reads, atomic writes,
//! and paths shown with `~` (record 0010, decision 2). The library only
//! sees bytes.
//!
//! A client path is checked twice. [`plan`] works on text alone, before the
//! system sees the path: device paths, other shares and drives, `..` above
//! the root and names Windows reserves are refused (opening `\\host\share`
//! would contact that computer). Then [`Root::walk`] follows links one name
//! at a time and plans every target the same way. `cap-std` was not taken:
//! it refuses an absolute link that points inside the root.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use carrysheet::error::{CommandError, ErrorCode};
use carrysheet::open::ReadFailure;

/// One step below the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Name(String),
    Parent,
}

/// Why a path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    DeviceNamespace,
    OtherShare,
    OtherDrive,
    DriveRelative,
    RootRelative,
    NotUnderRoot,
    BadName,
    ParentAfterMissing,
    Escapes,
    BadLink,
}

impl Refusal {
    fn why(self, root: &str) -> String {
        match self {
            Refusal::DeviceNamespace => "device and volume paths such as \\\\.\\ and \\\\?\\GLOBALROOT are never opened".into(),
            Refusal::OtherShare => format!("network paths are refused unless they are on the root folder's own share ({root}), so no other computer is contacted"),
            Refusal::OtherDrive => format!("it is on another drive than the root folder {root}"),
            Refusal::DriveRelative => "a drive letter without a backslash, like C:book.xlsx, depends on a hidden current folder".into(),
            Refusal::RootRelative => "a path that starts with \\ but has no drive depends on the current drive".into(),
            Refusal::NotUnderRoot => format!("only files inside {root} may be used"),
            Refusal::BadName => "it has a name Windows reserves (like NUL or COM1) or a character file names cannot hold (like : or a final dot)".into(),
            Refusal::ParentAfterMissing => "it goes up with .. after a folder that does not exist".into(),
            Refusal::Escapes => format!("following .. or a link leads outside {root}"),
            Refusal::BadLink => "a link on the way cannot be followed safely".into(),
        }
    }
}

/// The start of a path: relative, `/`, `C:\` (upper case) or a share.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    Relative,
    Slash,
    Drive(char),
    Share(String, String),
}

/// A path's start and its parts, without empty and `.` parts. Verbatim
/// prefixes are accepted only before a plain drive or UNC path.
fn parse(text: &str, windows: bool) -> Result<(Start, Vec<String>), Refusal> {
    if !windows {
        let start = if text.starts_with('/') {
            Start::Slash
        } else {
            Start::Relative
        };
        return Ok((start, parts(text, '/')));
    }
    let mut s = text.replace('/', "\\");
    if let Some(len) = [r"\\?\", r"\??\"]
        .iter()
        .find(|p| s.starts_with(*p))
        .map(|p| p.len())
    {
        let rest = s[len..].to_owned();
        if rest
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\"))
        {
            s = format!(r"\\{}", &rest[4..]);
        } else if is_drive(&rest) {
            s = rest;
        } else {
            return Err(Refusal::DeviceNamespace);
        }
    } else if s.starts_with(r"\\.\") || s == r"\\." || s == r"\\?" {
        return Err(Refusal::DeviceNamespace);
    }
    if let Some(rest) = s.strip_prefix(r"\\") {
        let mut it = rest.split('\\');
        let (host, share) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if host.is_empty() || share.is_empty() || host == "." || host == "?" {
            return Err(Refusal::DeviceNamespace);
        }
        let rest: Vec<&str> = it.collect();
        let start = Start::Share(host.to_lowercase(), share.to_lowercase());
        return Ok((start, parts(&rest.join("\\"), '\\')));
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        if b.len() == 2 || b[2] != b'\\' {
            return Err(Refusal::DriveRelative);
        }
        return Ok((
            Start::Drive(char::from(b[0].to_ascii_uppercase())),
            parts(&s[3..], '\\'),
        ));
    }
    if s.starts_with('\\') {
        return Err(Refusal::RootRelative);
    }
    Ok((Start::Relative, parts(&s, '\\')))
}

fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\')
}

fn parts(text: &str, separator: char) -> Vec<String> {
    text.split(separator)
        .filter(|p| !p.is_empty() && *p != ".")
        .map(str::to_owned)
        .collect()
}

/// A name a file may have; on Windows not a device, no `<>:"|?*` or
/// control characters, no final dot or space.
fn is_usable_name(name: &str, windows: bool) -> bool {
    !name.contains('\0')
        && (!windows
            || !is_windows_device_name(name)
                && !name.ends_with(['.', ' '])
                && !name
                    .chars()
                    .any(|c| c.is_control() || "<>:\"|?*".contains(c)))
}

/// True for names Windows treats as devices in every folder, such as `NUL`,
/// `nul:`, `com1.txt` or `COM¹`.
pub fn is_windows_device_name(name: &str) -> bool {
    let stem = name
        .split(['.', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    let digit = chars.next();
    (head == "COM" || head == "LPT")
        && chars.next().is_none()
        && digit.is_some_and(|d| d.is_ascii_digit() || "¹²³".contains(d))
}

/// The steps from `root` (absolute) to `given`, on text alone, refusing
/// anything that could reach outside the root.
pub fn plan(root: &str, given: &str, windows: bool) -> Result<Vec<Step>, Refusal> {
    let (root_start, root_parts) = parse(root, windows)?;
    if root_start == Start::Relative {
        return Err(Refusal::NotUnderRoot);
    }
    let (start, parts) = parse(given, windows)?;
    let below = match start {
        Start::Relative => parts,
        Start::Share(..) if start != root_start => return Err(Refusal::OtherShare),
        _ if start != root_start => return Err(Refusal::OtherDrive),
        _ => {
            let same =
                |a: &String, b: &String| a == b || windows && a.to_lowercase() == b.to_lowercase();
            if parts.len() < root_parts.len()
                || !root_parts.iter().zip(&parts).all(|(a, b)| same(a, b))
            {
                return Err(Refusal::NotUnderRoot);
            }
            parts[root_parts.len()..].to_vec()
        }
    };
    let mut depth = 0usize;
    let planned = below.into_iter().map(|part| {
        depth = match part.as_str() {
            ".." => depth.checked_sub(1).ok_or(Refusal::NotUnderRoot)?,
            _ => depth + 1,
        };
        to_steps(vec![part], windows).map(|mut s| s.remove(0))
    });
    planned.collect()
}

fn to_steps(parts: Vec<String>, windows: bool) -> Result<Vec<Step>, Refusal> {
    parts
        .into_iter()
        .map(|p| match p.as_str() {
            ".." => Ok(Step::Parent),
            _ if is_usable_name(&p, windows) => Ok(Step::Name(p)),
            _ => Err(Refusal::BadName),
        })
        .collect()
}

/// The folder every path a client gives must stay inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// As the system resolves it (`\\?\C:\…` on Windows).
    canonical: PathBuf,
    /// The plain form, for [`plan`].
    text: String,
    /// The root as given, made absolute, when that differs (`/var/x` for
    /// `/private/var/x`, or a mapped drive).
    alias: Option<String>,
}

impl Root {
    /// Uses `dir`, which must be an existing folder.
    pub fn new(dir: &Path) -> Result<Root, CommandError> {
        let shown = display_path(dir);
        let fix = "Give --root an existing folder, or leave it out to use the current folder.";
        let error = |code, what: &str, why: String| {
            CommandError::new(code, format!("the root folder {shown} {what}"), why, fix)
        };
        let canonical = std::fs::canonicalize(dir)
            .map_err(|e| error(ErrorCode::FileNotFound, "cannot be used", e.to_string()))?;
        if !canonical.is_dir() {
            return Err(error(
                ErrorCode::InvalidArgument,
                "is not a folder",
                "--root names the folder whose files may be used".into(),
            ));
        }
        let is_absolute =
            |t: &&str| parse(t, cfg!(windows)).is_ok_and(|(s, _)| s != Start::Relative);
        let text = plain_path(&canonical)
            .to_str()
            .filter(is_absolute)
            .map(str::to_owned);
        let text = text.ok_or_else(|| {
            error(
                ErrorCode::InvalidArgument,
                "cannot be used",
                "its path is not plain text, or is a device or volume path".into(),
            )
        })?;
        let alias = std::path::absolute(dir)
            .ok()
            .and_then(|a| a.to_str().map(str::to_owned));
        let alias = alias.filter(|a| *a != text && parse(a, cfg!(windows)).is_ok());
        Ok(Root {
            canonical,
            text,
            alias,
        })
    }

    /// The root folder, fit for showing and opening.
    pub fn dir(&self) -> PathBuf {
        plain_path(&self.canonical)
    }

    /// True for a whole drive or file system (`/`, `C:\`, `\\host\share`).
    pub fn is_filesystem_root(&self) -> bool {
        self.canonical.parent().is_none()
    }

    pub fn is_folder(&self, dir: &Path) -> bool {
        std::fs::canonicalize(dir).is_ok_and(|c| c == self.canonical)
    }

    /// `given` (relative to the root, or absolute) as a path inside the root
    /// with every link followed. The file itself need not exist.
    pub fn resolve(&self, given: &str) -> Result<PathBuf, CommandError> {
        let windows = cfg!(windows);
        let walked = self
            .plan(given, windows)
            .and_then(|steps| self.walk(steps, windows));
        walked
            .map(|p| plain_path(&p))
            .map_err(|refusal| self.refuse(given, refusal))
    }

    /// [`plan`] against the canonical root, else against the alias.
    fn plan(&self, given: &str, windows: bool) -> Result<Vec<Step>, Refusal> {
        match (plan(&self.text, given, windows), &self.alias) {
            (
                Err(Refusal::NotUnderRoot | Refusal::OtherDrive | Refusal::OtherShare),
                Some(alias),
            ) => plan(alias, given, windows),
            (result, _) => result,
        }
    }

    /// Follows `steps` from the root, looking only at paths inside it.
    fn walk(&self, steps: Vec<Step>, windows: bool) -> Result<PathBuf, Refusal> {
        let (mut current, mut queue) = (self.canonical.clone(), VecDeque::from(steps));
        let (mut missing, mut links) = (false, 0);
        while let Some(step) = queue.pop_front() {
            let name = match step {
                Step::Parent if missing => return Err(Refusal::ParentAfterMissing),
                // `current` holds no links, so its parent is the real one.
                Step::Parent if current == self.canonical || !current.pop() => {
                    return Err(Refusal::Escapes);
                }
                Step::Parent => continue,
                Step::Name(name) => name,
            };
            let next = current.join(&name);
            match std::fs::symlink_metadata(&next) {
                Ok(meta) if !missing && meta.file_type().is_symlink() => {
                    links += 1;
                    let target = std::fs::read_link(&next).map_err(|_| Refusal::BadLink)?;
                    let target = target
                        .to_str()
                        .filter(|_| links <= 40)
                        .ok_or(Refusal::BadLink)?;
                    let mut expanded = if parse(target, windows)?.0 == Start::Relative {
                        VecDeque::from(to_steps(parse(target, windows)?.1, windows)?)
                    } else {
                        current = self.canonical.clone();
                        VecDeque::from(self.plan(target, windows)?)
                    };
                    expanded.extend(queue);
                    queue = expanded;
                }
                found => (missing, current) = (missing || found.is_err(), next),
            }
            if !current.starts_with(&self.canonical) {
                return Err(Refusal::Escapes);
            }
        }
        Ok(current)
    }

    fn refuse(&self, given: &str, refusal: Refusal) -> CommandError {
        let mut shown = display_path(Path::new(given));
        if shown.chars().count() > 80 {
            shown = format!("{}...", shown.chars().take(80).collect::<String>());
        }
        CommandError::new(
            ErrorCode::OutsideRoot,
            format!("{shown} is outside the root folder"),
            refusal.why(&display_path(&self.dir())),
            "Use a path inside the root folder, like book.xlsx or reports/q1.xlsx, or start the server with --root set to a folder that holds the file.",
        )
    }
}

/// Drops the `\\?\` of a canonical Windows path when the plain form names
/// the same file (not for long paths or names ending in a dot or space).
pub fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_str().unwrap_or("");
    let plain = match (text.strip_prefix(r"\\?\UNC\"), text.strip_prefix(r"\\?\")) {
        (Some(rest), _) => format!(r"\\{rest}"),
        (None, Some(rest)) if is_drive(rest) => rest.to_owned(),
        _ => return path.to_path_buf(),
    };
    let names_are_plain = plain
        .split('\\')
        .filter(|p| !p.is_empty())
        .skip(1)
        .all(|p| !p.ends_with(['.', ' ']));
    if plain.len() < 260 && names_are_plain {
        PathBuf::from(plain)
    } else {
        path.to_path_buf()
    }
}

/// Reads a regular file of at most `max` bytes. The reported size is not
/// trusted; anything but a regular file is refused before it is opened,
/// because a pipe waits for a writer and a device may never end.
pub fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, ReadFailure> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(ReadFailure::NotAFile);
    }
    if meta.len() > max {
        return Err(ReadFailure::TooLarge);
    }
    let file = std::fs::File::open(path)?;
    // The path may have been swapped since the checks above.
    if !file.metadata()?.is_file() {
        return Err(ReadFailure::NotAFile);
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(ReadFailure::TooLarge);
    }
    Ok(bytes)
}

/// True when something, even a dangling link, has this name.
pub fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Writes through a temporary file in the same folder, synced, then renamed
/// into place. A link is written through and keeps its permissions. With a
/// `root`, the folder must still be inside it just before and after.
pub fn write_atomic(path: &Path, bytes: &[u8], root: Option<&Path>) -> Result<(), CommandError> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let keep = std::fs::metadata(&real).ok().map(|m| m.permissions());
    let dir = real
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = real
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.carrysheet-tmp", std::process::id()));
    let inside = || {
        let root = root.map(std::fs::canonicalize);
        let ok = match root {
            None => true,
            Some(root) => {
                std::fs::canonicalize(dir).is_ok_and(|d| root.is_ok_and(|r| d.starts_with(r)))
                    && std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()) == real
            }
        };
        ok.then_some(()).ok_or_else(|| CommandError::new(
            ErrorCode::OutsideRoot,
            format!("{} is outside the root folder", display_path(path)),
            "a link on the way to the file changed while it was being saved, and now leads outside the root folder",
            "Use a path inside the root folder that is not a link changed by another program; nothing was written.",
        ))
    };
    inside()?;
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Some(permissions) = keep {
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.sync_all()
    })();
    let renamed = written
        .map_err(|e| carrysheet::save::write_error(&real, &e))
        .and_then(|()| inside());
    let renamed = renamed.and_then(|()| {
        std::fs::rename(&tmp, &real).map_err(|e| carrysheet::save::write_error(&real, &e))
    });
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    renamed
}

/// The current user's home folder, if the system says.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|p| !p.as_os_str().is_empty())
}

/// A path with `~` for the home folder, so no output carries the user name.
pub fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    home_dir().map_or_else(
        || text.clone(),
        |home| display_path_text(&text, &home.display().to_string(), cfg!(windows)),
    )
}

/// [`display_path`] on text, for Windows (`windows: true`) or Unix rules.
pub fn display_path_text(path: &str, home: &str, windows: bool) -> String {
    let Some(home) = usable_home(home, windows) else {
        return path.to_owned();
    };
    let plain = if windows {
        strip_verbatim(path)
    } else {
        Cow::Borrowed(path)
    };
    let rest = match_prefix(&plain, &home, windows).map(|end| &plain[end..]);
    match rest.map(|r| (r, r.chars().next())) {
        Some((_, None)) => "~".to_owned(),
        Some((rest, Some(c))) if is_separator(c, windows) => format!("~{rest}"),
        // No match, or only part of a name, like /home/alicex for /home/alice.
        _ => path.to_owned(),
    }
}

/// Replaces every mention of the home folder in free text with `~`.
pub fn redact_home(text: &str) -> String {
    home_dir().map_or_else(
        || text.to_owned(),
        |home| redact_home_text(text, &home.display().to_string(), cfg!(windows)),
    )
}

/// [`redact_home`] on text. Any mention goes, even inside a longer name:
/// hiding too much is better than showing a user name.
pub fn redact_home_text(text: &str, home: &str, windows: bool) -> String {
    let Some(home) = usable_home(home, windows) else {
        return text.to_owned();
    };
    // `carrysheet::text` shows `\` as `\\`, so look for that form too.
    let escaped = home.replace('\\', r"\\");
    let (mut out, mut i) = (String::with_capacity(text.len()), 0);
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        let verbatim = VERBATIM
            .iter()
            .find(|p| windows && starts_with_ignore_case(rest, p));
        let skip = verbatim.map_or(0, |p| p.len());
        let found = match_prefix(&rest[skip..], &home, windows)
            .or_else(|| match_prefix(&rest[skip..], &escaped, windows));
        if let Some(len) = found {
            out.push('~');
            i += skip + len;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// The error with the home folder shown as `~` in every part.
pub fn redact_error(e: &CommandError) -> CommandError {
    CommandError::new(
        e.code,
        redact_home(&e.what),
        redact_home(&e.why),
        redact_home(&e.fix),
    )
}

const VERBATIM: &[&str] = &[r"\\?\UNC\", r"\\?\", "//?/UNC/", "//?/"];

fn is_separator(c: char, windows: bool) -> bool {
    c == '/' || (windows && c == '\\')
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// `\\?\C:\x` as `C:\x`, `\\?\UNC\server\share` as `\\server\share`.
fn strip_verbatim(path: &str) -> Cow<'_, str> {
    match VERBATIM.iter().find(|p| starts_with_ignore_case(path, p)) {
        Some(p) if p.contains("UNC") => Cow::Owned(format!(r"\\{}", &path[p.len()..])),
        Some(p) => Cow::Borrowed(&path[p.len()..]),
        None => Cow::Borrowed(path),
    }
}

/// The home as compared; `None` for one that matches too much (`/`, `C:\`).
fn usable_home(home: &str, windows: bool) -> Option<String> {
    let home = if windows {
        strip_verbatim(home)
    } else {
        Cow::Borrowed(home)
    };
    let trimmed = home.trim_end_matches(|c| is_separator(c, windows));
    let has_separator = trimmed.chars().any(|c| is_separator(c, windows));
    (has_separator && trimmed.chars().count() >= 2).then(|| trimmed.to_owned())
}

/// How many bytes at the start of `text` match `home`, if they do.
fn match_prefix(text: &str, home: &str, windows: bool) -> Option<usize> {
    let mut chars = text.char_indices();
    for h in home.chars() {
        let (_, t) = chars.next()?;
        let same_separator = is_separator(t, true) && is_separator(h, true);
        if t != h && !(windows && (same_separator || t.to_lowercase().eq(h.to_lowercase()))) {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(i, _)| i))
}
