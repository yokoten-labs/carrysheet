//! Keeping paths inside a root folder (`mcp serve --root`).
//!
//! A path from a client is checked in two stages:
//!
//! 1. [`plan`], on text alone, before anything touches the filesystem:
//!    device namespaces (`\\.\`, `\\?\GLOBALROOT`, `\\?\Volume{…}`), network
//!    paths on another share, other drives, drive-relative and root-relative
//!    paths, absolute paths not under the root, `..` above the root, and
//!    names Windows reserves are refused. Opening `\\host\share` would make
//!    Windows connect to that computer with the user's credentials, so this
//!    must happen first. It is a pure function of text, so the Windows rules
//!    are tested on every system.
//! 2. A walk from the root, one name at a time, that follows links (and, on
//!    Windows, junctions) itself and checks every target the same way. It
//!    only ever looks at paths that are already inside the root, and `..`
//!    after a folder that does not exist is refused on every system.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::commands::error::{CommandError, ErrorCode};
use crate::commands::paths::display_path;

/// How many links a path may pass through, like the usual system limit.
const MAX_LINKS: usize = 40;

/// The longest path shown in an error, so the fix stays near the start.
const MAX_SHOWN: usize = 80;

/// One step below the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Go into this name.
    Name(String),
    /// Go up (`..`).
    Parent,
}

/// Why a path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `\\.\…`, `\\?\GLOBALROOT\…`, `\\?\Volume{…}\…` and the like.
    DeviceNamespace,
    /// A network path (`\\host\share\…`) not on the root's share.
    OtherShare,
    /// A drive other than the root's.
    OtherDrive,
    /// `C:book.xlsx`: relative to a hidden current folder of drive C.
    DriveRelative,
    /// `\book.xlsx`: relative to the current drive.
    RootRelative,
    /// An absolute path not under the root, or `..` above it.
    NotUnderRoot,
    /// A name Windows reserves (`NUL`, `COM1`) or cannot store (`a:b`, `a.`).
    BadName,
    /// `..` after a folder that does not exist.
    ParentAfterMissing,
    /// A link or `..` leads out of the root.
    Escapes,
    /// A link that cannot be read, is not text, or is part of a long chain.
    BadLink,
}

impl Refusal {
    fn why(self, root: &str) -> String {
        match self {
            Refusal::DeviceNamespace => {
                "device and volume paths such as \\\\.\\ and \\\\?\\GLOBALROOT are never opened".into()
            }
            Refusal::OtherShare => format!(
                "network paths are refused unless they are on the root folder's own share ({root}), so no other computer is contacted"
            ),
            Refusal::OtherDrive => format!("it is on another drive than the root folder {root}"),
            Refusal::DriveRelative => {
                "a drive letter without a backslash, like C:book.xlsx, depends on a hidden current folder".into()
            }
            Refusal::RootRelative => {
                "a path that starts with \\ but has no drive depends on the current drive".into()
            }
            Refusal::NotUnderRoot => format!("only files inside {root} may be used"),
            Refusal::BadName => {
                "it has a name Windows reserves (like NUL or COM1) or a character file names cannot hold (like : or a final dot)".into()
            }
            Refusal::ParentAfterMissing => "it goes up with .. after a folder that does not exist".into(),
            Refusal::Escapes => format!("following .. or a link leads outside {root}"),
            Refusal::BadLink => "a link on the way cannot be followed safely".into(),
        }
    }
}

/// The start of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    Relative,
    /// `/` on Unix.
    Slash,
    /// `C:\`, upper case.
    Drive(char),
    /// `\\host\share`, lower case.
    Share(String, String),
}

/// Splits a path into its start and its parts, dropping empty and `.`
/// parts. On Windows `/` and `\` are both separators.
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
    // Verbatim and NT prefixes: only the plain drive and UNC forms are
    // accepted, as if written without the prefix.
    let verbatim = [r"\\?\", r"\??\"]
        .iter()
        .find(|p| s.starts_with(*p))
        .map(|p| p.len());
    if let Some(len) = verbatim {
        let rest = s[len..].to_owned();
        if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(r"UNC\") {
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
        let host = it.next().unwrap_or("");
        let share = it.next().unwrap_or("");
        if host.is_empty() || share.is_empty() || host == "." || host == "?" {
            return Err(Refusal::DeviceNamespace);
        }
        let start = Start::Share(host.to_lowercase(), share.to_lowercase());
        let rest: Vec<&str> = it.collect();
        return Ok((start, parts(&rest.join("\\"), '\\')));
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        if b.len() == 2 || b[2] != b'\\' {
            return Err(Refusal::DriveRelative);
        }
        let drive = char::from(b[0].to_ascii_uppercase());
        return Ok((Start::Drive(drive), parts(&s[3..], '\\')));
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

/// True for a name a file may have: on Windows, not a reserved device name,
/// no `<>:"|?*`, no control characters, and no final dot or space.
fn is_usable_name(name: &str, windows: bool) -> bool {
    if name.contains('\0') {
        return false;
    }
    if !windows {
        return true;
    }
    !is_windows_device_name(name)
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
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
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let mut chars = stem.chars();
            let head: String = chars.by_ref().take(3).collect();
            let digit = chars.next();
            let rest = chars.next();
            (head == "COM" || head == "LPT")
                && rest.is_none()
                && digit.is_some_and(|d| d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
        }
    }
}

/// Plans the steps from `root` to `given` on text alone, refusing anything
/// that could reach outside the root. `root` is the root folder as an
/// absolute path; `windows` selects Windows or Unix rules.
pub fn plan(root: &str, given: &str, windows: bool) -> Result<Vec<Step>, Refusal> {
    let (root_start, root_parts) = parse(root, windows)?;
    if root_start == Start::Relative {
        return Err(Refusal::NotUnderRoot);
    }
    let (start, parts) = parse(given, windows)?;
    let below = match start {
        Start::Relative => parts,
        _ if start != root_start => {
            return Err(match start {
                Start::Share(..) => Refusal::OtherShare,
                _ => Refusal::OtherDrive,
            });
        }
        _ => {
            let same = |a: &String, b: &String| {
                if windows {
                    a.to_lowercase() == b.to_lowercase()
                } else {
                    a == b
                }
            };
            if parts.len() < root_parts.len()
                || !root_parts.iter().zip(&parts).all(|(a, b)| same(a, b))
            {
                return Err(Refusal::NotUnderRoot);
            }
            parts[root_parts.len()..].to_vec()
        }
    };
    let mut depth = 0usize;
    let mut steps = Vec::with_capacity(below.len());
    for part in below {
        if part == ".." {
            depth = depth.checked_sub(1).ok_or(Refusal::NotUnderRoot)?;
            steps.push(Step::Parent);
        } else if is_usable_name(&part, windows) {
            depth += 1;
            steps.push(Step::Name(part));
        } else {
            return Err(Refusal::BadName);
        }
    }
    Ok(steps)
}

/// The folder every path a command uses must stay inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// As the operating system resolves it (`\\?\C:\…` on Windows).
    canonical: PathBuf,
    /// The plain form as text, for [`plan`].
    text: String,
    /// The root as it was given, made absolute, when that differs.
    alias: Option<String>,
}

impl Root {
    /// Uses `dir` as the root. It must be an existing folder.
    pub fn new(dir: &Path) -> Result<Root, CommandError> {
        let shown = display_path(dir);
        let fix = "Give --root an existing folder, or leave it out to use the current folder.";
        let canonical = std::fs::canonicalize(dir).map_err(|e| {
            CommandError::new(
                ErrorCode::FileNotFound,
                format!("the root folder {shown} cannot be used"),
                e.to_string(),
                fix,
            )
        })?;
        if !canonical.is_dir() {
            return Err(CommandError::new(
                ErrorCode::InvalidArgument,
                format!("the root folder {shown} is not a folder"),
                "--root names the folder whose files may be used",
                fix,
            ));
        }
        let text = plain_path(&canonical)
            .to_str()
            .filter(|t| parse(t, cfg!(windows)).is_ok_and(|(s, _)| s != Start::Relative))
            .map(str::to_owned)
            .ok_or_else(|| {
                CommandError::new(
                    ErrorCode::InvalidArgument,
                    format!("the root folder {shown} cannot be used"),
                    "its path is not plain text, or is a device or volume path",
                    fix,
                )
            })?;
        let alias = std::path::absolute(dir)
            .ok()
            .and_then(|a| a.to_str().map(str::to_owned))
            .filter(|a| *a != text && parse(a, cfg!(windows)).is_ok());
        Ok(Root {
            canonical,
            text,
            alias,
        })
    }

    /// The root folder, in a form fit for showing and opening.
    pub fn dir(&self) -> PathBuf {
        plain_path(&self.canonical)
    }

    /// True when the root is a whole drive or file system (`/`, `C:\`,
    /// `\\host\share`).
    pub fn is_filesystem_root(&self) -> bool {
        self.canonical.parent().is_none()
    }

    /// True when `dir` is this same folder.
    pub fn is_folder(&self, dir: &Path) -> bool {
        std::fs::canonicalize(dir).is_ok_and(|c| c == self.canonical)
    }

    /// Resolves `given` (relative to the root, or absolute) to a path inside
    /// the root with every link followed. The file itself need not exist.
    pub fn resolve(&self, given: &str) -> Result<PathBuf, CommandError> {
        let windows = cfg!(windows);
        self.plan(given, windows)
            .and_then(|steps| self.walk(steps, windows))
            .map(|p| plain_path(&p))
            .map_err(|refusal| self.refuse(given, refusal))
    }

    /// [`plan`] against the canonical root, or else against the root as it
    /// was given (`/var/x` for `/private/var/x`, or a mapped drive), which
    /// names the same folder.
    fn plan(&self, given: &str, windows: bool) -> Result<Vec<Step>, Refusal> {
        match (plan(&self.text, given, windows), &self.alias) {
            (
                Err(Refusal::NotUnderRoot | Refusal::OtherDrive | Refusal::OtherShare),
                Some(alias),
            ) => plan(alias, given, windows),
            (result, _) => result,
        }
    }

    /// Follows `steps` from the root. Only paths inside the root are looked
    /// at; links are read and their targets planned like client paths.
    fn walk(&self, steps: Vec<Step>, windows: bool) -> Result<PathBuf, Refusal> {
        let mut current = self.canonical.clone();
        let mut queue: VecDeque<Step> = steps.into();
        let mut missing = false;
        let mut links = 0;
        while let Some(step) = queue.pop_front() {
            match step {
                Step::Parent => {
                    if missing {
                        return Err(Refusal::ParentAfterMissing);
                    }
                    // `current` holds no links, so its parent is the real one.
                    if current == self.canonical || !current.pop() {
                        return Err(Refusal::Escapes);
                    }
                }
                Step::Name(name) => {
                    let next = current.join(&name);
                    if missing {
                        current = next;
                        continue;
                    }
                    match std::fs::symlink_metadata(&next) {
                        Err(_) => {
                            missing = true;
                            current = next;
                        }
                        Ok(meta) if meta.file_type().is_symlink() => {
                            links += 1;
                            if links > MAX_LINKS {
                                return Err(Refusal::BadLink);
                            }
                            let target = std::fs::read_link(&next).map_err(|_| Refusal::BadLink)?;
                            let target = target.to_str().ok_or(Refusal::BadLink)?;
                            let (start, _) = parse(target, windows)?;
                            let mut expanded: VecDeque<Step> = if start == Start::Relative {
                                // From the folder holding the link; going above
                                // the root is caught by the checks below.
                                relative_steps(target, windows)?.into()
                            } else {
                                current = self.canonical.clone();
                                self.plan(target, windows)?.into()
                            };
                            expanded.extend(queue);
                            queue = expanded;
                        }
                        Ok(_) => current = next,
                    }
                }
            }
            if !current.starts_with(&self.canonical) {
                return Err(Refusal::Escapes);
            }
        }
        Ok(current)
    }

    fn refuse(&self, given: &str, refusal: Refusal) -> CommandError {
        let mut shown = display_path(Path::new(given));
        if shown.chars().count() > MAX_SHOWN {
            shown = format!("{}...", shown.chars().take(MAX_SHOWN).collect::<String>());
        }
        CommandError::new(
            ErrorCode::OutsideRoot,
            format!("{shown} is outside the root folder"),
            refusal.why(&display_path(&self.dir())),
            "Use a path inside the root folder, like book.xlsx or reports/q1.xlsx, or start the server with --root set to a folder that holds the file.",
        )
    }
}

/// The steps of a relative link target, without the depth check of [`plan`].
fn relative_steps(target: &str, windows: bool) -> Result<Vec<Step>, Refusal> {
    let (_, parts) = parse(target, windows)?;
    parts
        .into_iter()
        .map(|p| {
            if p == ".." {
                Ok(Step::Parent)
            } else if is_usable_name(&p, windows) {
                Ok(Step::Name(p))
            } else {
                Err(Refusal::BadName)
            }
        })
        .collect()
}

/// Drops the `\\?\` prefix Windows' canonical paths carry when the plain form
/// means the same file, so paths can be shown (with `~`) and passed on.
/// Other paths are returned as they are.
pub fn plain_path(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\").filter(|r| is_drive(r)) {
        rest.to_owned()
    } else {
        return path.to_path_buf();
    };
    // Long paths and names ending in a dot or space need the prefix.
    let safe = plain.len() < 260
        && plain
            .split('\\')
            .filter(|p| !p.is_empty())
            .skip(1)
            .all(|p| !p.ends_with('.') && !p.ends_with(' '));
    if safe {
        PathBuf::from(plain)
    } else {
        path.to_path_buf()
    }
}
