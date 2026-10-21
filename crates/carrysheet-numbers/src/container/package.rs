//! Package folders from older Numbers: `Index.zip` or an `Index/` folder,
//! and a `Metadata/` folder.
//!
//! Nothing inside the package may lead the reader outside it: symbolic
//! links are never followed, at any level below the package folder. Each
//! file is checked before it is opened and again after, so a file swapped
//! for a link in between is noticed: on Unix by its device and inode
//! numbers, on Windows because the link itself is opened, not followed. A folder swapped for a link between its check and its listing
//! cannot be caught with the standard library alone; the window is small.

use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

use super::{BUILD_HISTORY, Budget, Limits, PASSWORD_PARTS, PROPERTIES, Part, Parts, zip};
use crate::error::NumbersError;

pub(super) fn read(
    path: &Path,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Parts, NumbersError> {
    let mut parts = Parts::default();
    for name in PASSWORD_PARTS {
        if regular_file(&path.join(name)) {
            return Err(NumbersError::PasswordProtected);
        }
    }
    let index_zip = path.join("Index.zip");
    if regular_file(&index_zip) {
        let file = open_regular(&index_zip)?;
        if file.metadata()?.len() > limits.max_input_bytes {
            return Err(super::input_too_large(limits));
        }
        zip::read(
            BufReader::new(file),
            "Index.zip",
            limits,
            budget,
            &mut parts,
            false,
        )?;
    } else {
        let index = path.join("Index");
        if !real_folder(&index) {
            return Err(NumbersError::NotNumbers {
                reason: "the folder has neither Index.zip nor an Index folder",
            });
        }
        let mut seen = 0;
        read_folder(&index, "Index", 0, limits, budget, &mut seen, &mut parts)?;
    }
    // Metadata/ must be a real folder too, not a link to one elsewhere.
    if real_folder(&path.join("Metadata")) {
        let mut skipped = Vec::new();
        for (name, slot) in [
            (PROPERTIES, &mut parts.properties),
            (BUILD_HISTORY, &mut parts.build_history),
        ] {
            let file = path.join(name);
            if regular_file(&file) {
                // Property lists are small; a large one is skipped unread.
                let limit = (crate::plist::MAX_INPUT as u64).min(budget.left());
                match read_limited(&file, limit) {
                    Ok(data) => {
                        budget.take(data.len() as u64)?;
                        *slot = Some(data);
                    }
                    Err(NumbersError::TooLarge { .. }) => skipped.push(name.to_owned()),
                    Err(e) => return Err(e),
                }
            }
        }
        parts.skipped = skipped;
    }
    Ok(parts)
}

/// True for a regular file that is not a symbolic link.
fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// True for a folder that is not a symbolic link.
fn real_folder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Opens a regular file that is not a symbolic link, and checks that the
/// file opened is the one that was checked.
fn open_regular(path: &Path) -> Result<fs::File, NumbersError> {
    let swapped = || NumbersError::NotNumbers {
        reason: "a file in the package is a link or changed while it was read",
    };
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() {
        return Err(swapped());
    }
    let file = open_no_follow(path)?;
    let after = file.metadata()?;
    if !after.is_file() || after.is_symlink() || !same_file(&before, &after) {
        return Err(swapped());
    }
    Ok(file)
}

/// Opens a file; on Windows, a link (reparse point) is opened itself
/// rather than followed, so the check after opening sees it.
#[cfg(windows)]
fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    /// `FILE_FLAG_OPEN_REPARSE_POINT`.
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(windows))]
fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    // Without Unix inode numbers, compare what std gives on every platform.
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, NumbersError> {
    let file = open_regular(path)?;
    let mut data = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(NumbersError::TooLarge {
            what: "bytes after decompression",
            limit,
        });
    }
    Ok(data)
}

/// Walks an `Index/` folder of a package. Symbolic links are skipped.
fn read_folder(
    dir: &Path,
    prefix: &str,
    depth: u32,
    limits: &Limits,
    budget: &mut Budget,
    seen: &mut usize,
    parts: &mut Parts,
) -> Result<(), NumbersError> {
    const MAX_DEPTH: u32 = 8;
    if depth > MAX_DEPTH {
        return Err(NumbersError::TooLarge {
            what: "levels of folders in the package",
            limit: u64::from(MAX_DEPTH),
        });
    }
    // Checked again right before listing, to keep the window small.
    if !real_folder(dir) {
        return Ok(());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        *seen += 1;
        if *seen > limits.max_zip_entries {
            return Err(NumbersError::TooLarge {
                what: "files in the package",
                limit: limits.max_zip_entries as u64,
            });
        }
        entries.push(entry);
    }
    // read_dir order depends on the file system; sort for repeatable results.
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        // file_type does not follow links: a link is neither dir nor file.
        let kind = entry.file_type()?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let name = format!("{prefix}/{file_name}");
        if kind.is_dir() {
            read_folder(&entry.path(), &name, depth + 1, limits, budget, seen, parts)?;
        } else if kind.is_file() && name.ends_with(".iwa") {
            let data = read_limited(&entry.path(), budget.left())?;
            budget.take(data.len() as u64)?;
            parts.archives.push(Part { name, data });
        }
    }
    Ok(())
}
