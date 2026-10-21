//! Zip files: the `.numbers` file itself, or `Index.zip` in a package.
//!
//! Opened with carrysheet-zip, the hardened reader every Carrysheet reader
//! shares (issue 40): it looks for the end record once, near the end of
//! the file, checks the entry count and directory size before it takes
//! memory, refuses overlapping entries and duplicate names, and checks
//! each entry's size and CRC-32 while it inflates.

use std::io::{Cursor, Read, Seek, SeekFrom};

use carrysheet_zip::{Archive, DuplicateNames, Error as ZipError, Limits as ZipLimits};

use super::{BUILD_HISTORY, Budget, Limits, PROPERTIES, Part, Parts, Use, use_of};
use crate::error::{NumbersError, damaged};

/// Reads the parts the reader uses from a zip file.
pub(super) fn read<R: Read + Seek>(
    mut reader: R,
    label: &str,
    limits: &Limits,
    budget: &mut Budget,
    parts: &mut Parts,
    allow_nested: bool,
) -> Result<(), NumbersError> {
    let starts_with_pk = {
        let mut magic = [0u8; 2];
        reader.seek(SeekFrom::Start(0))?;
        reader.read(&mut magic)? == 2 && magic == *b"PK"
    };
    let mut archive = Archive::open(reader, zip_limits(limits)).map_err(|e| match e {
        ZipError::Damaged(_) if !starts_with_pk => NumbersError::NotNumbers {
            reason: "it is neither a zip file nor a package folder",
        },
        e => zip_error(label, limits, e),
    })?;
    for i in 0..archive.entries().len() {
        let entry = &archive.entries()[i];
        let name = entry.name.clone();
        let (size, is_dir, encrypted) = (entry.size, entry.is_dir(), entry.is_encrypted());
        let usage = use_of(&name);
        match usage {
            Use::Skip => continue,
            Use::Password => return Err(NumbersError::PasswordProtected),
            Use::NestedIndex if !allow_nested => continue,
            _ => {}
        }
        if is_dir {
            continue;
        }
        if encrypted {
            return Err(NumbersError::PasswordProtected);
        }
        // Property lists are small; a large one is skipped unread. The
        // reader checks that an entry inflates to exactly its size, so the
        // size can be trusted for memory.
        if matches!(usage, Use::Properties | Use::BuildHistory) {
            let limit = crate::plist::MAX_INPUT as u64;
            if size > limit {
                // The usual name, not the entry's, which may be anything.
                let usual = if matches!(usage, Use::Properties) {
                    PROPERTIES
                } else {
                    BUILD_HISTORY
                };
                parts.skipped.push(usual.to_owned());
                continue;
            }
            if size > budget.left() {
                return Err(budget.exceeded());
            }
            let data = archive
                .read(i, limit)
                .map_err(|e| zip_error(&name, limits, e))?;
            budget.take(data.len() as u64)?;
            match usage {
                Use::Properties => parts.properties = Some(data),
                _ => parts.build_history = Some(data),
            }
            continue;
        }
        // A nested Index.zip is read into memory, so it is held to the
        // input limit as well as the budget.
        let left = budget.left();
        let nested = matches!(usage, Use::NestedIndex);
        let most = if nested {
            left.min(limits.max_input_bytes)
        } else {
            left
        };
        if size > most {
            return Err(if nested && limits.max_input_bytes < left {
                NumbersError::TooLarge {
                    what: "bytes in the nested Index.zip",
                    limit: limits.max_input_bytes,
                }
            } else {
                budget.exceeded()
            });
        }
        let data = archive
            .read(i, most)
            .map_err(|e| zip_error(&name, limits, e))?;
        budget.take(data.len() as u64)?;
        match usage {
            Use::Archive => parts.archives.push(Part { name, data }),
            Use::NestedIndex => read(Cursor::new(&data[..]), &name, limits, budget, parts, false)?,
            Use::Properties | Use::BuildHistory | Use::Password | Use::Skip => {}
        }
    }
    Ok(())
}

/// The zip reader's limits from the document's. What entries inflate to
/// is charged to the document's budget, here and in [`read`]; entries
/// cannot overlap, so the compressed bytes read are bounded by the input
/// size, which [`Limits::max_input_bytes`] bounds.
fn zip_limits(limits: &Limits) -> ZipLimits {
    ZipLimits {
        max_entries: limits.max_zip_entries,
        max_entry_bytes: limits.max_total_bytes,
        max_total_bytes: limits.max_total_bytes,
        max_total_read: limits
            .max_input_bytes
            .max(ZipLimits::default().max_total_read),
        // Two entries with one name: other readers may pick the other.
        duplicate_names: DuplicateNames::Refuse,
        ..ZipLimits::default()
    }
}

/// A zip reader error as a document error. Its texts hold nothing from the
/// file; `part` names the entry or the zip file (empty for the document).
fn zip_error(part: &str, limits: &Limits, e: ZipError) -> NumbersError {
    match e {
        ZipError::Io(e) => NumbersError::Io(e),
        ZipError::LimitExceeded {
            what: "zip entries",
            ..
        } => NumbersError::TooLarge {
            what: "entries in the zip file",
            limit: limits.max_zip_entries as u64,
        },
        ZipError::LimitExceeded { what, max } => NumbersError::TooLarge { what, limit: max },
        ZipError::Unsupported(_) => NumbersError::Unsupported {
            feature: "zip features Numbers does not write (a compression method other than Deflate, encryption, or several disks)",
        },
        e if e.is_slow_to_unpack() => damaged(
            part,
            format!(
                "{e}; Numbers does not write such data, so the file was made to slow readers down"
            ),
        ),
        e => damaged(part, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_errors_become_document_errors() {
        let limits = Limits::default();
        let e = zip_error(
            "",
            &limits,
            ZipError::LimitExceeded {
                what: "zip entries",
                max: 3,
            },
        );
        assert!(
            matches!(e, NumbersError::TooLarge { what, limit } if what == "entries in the zip file" && limit == 20_000),
            "{e}"
        );
        let e = zip_error("", &limits, ZipError::Unsupported("x".into()));
        assert!(matches!(e, NumbersError::Unsupported { .. }), "{e}");
        let e = zip_error("Index/A.iwa", &limits, ZipError::Damaged("bad".into()));
        assert!(e.what().contains("Index/A.iwa"), "{}", e.what());
    }
}
