//! Zip files: the `.numbers` file itself, or `Index.zip` in a package.

use std::io::{Cursor, Read, Seek, SeekFrom};

use super::{BUILD_HISTORY, Budget, Limits, PROPERTIES, Part, Parts, Use, use_of};
use crate::error::{NumbersError, damaged};

/// End of central directory record, and its zip64 form.
const END: [u8; 4] = *b"PK\x05\x06";
const END64: [u8; 4] = *b"PK\x06\x06";
/// A backstop on end records. The signature is counted anywhere in the
/// file, including inside stored entries (an embedded zip has its own), so
/// the cap is generous: with the archive offset known, the zip crate checks
/// each record it tries in place, a few small reads each, so even this many
/// cost little.
const MAX_END_RECORDS: usize = 4096;

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
    check_end_records(&mut reader, label, limits)?;
    // Numbers writes no data before the zip. A known offset of 0 makes the
    // zip crate check each end record in place, instead of searching the
    // whole file again for every one it tries.
    let config = ::zip::read::Config {
        archive_offset: ::zip::read::ArchiveOffset::Known(0),
    };
    let mut archive = ::zip::ZipArchive::with_config(config, reader).map_err(|e| {
        if starts_with_pk {
            damaged(label, format!("the zip directory cannot be read ({e})"))
        } else {
            NumbersError::NotNumbers {
                reason: "it is neither a zip file nor a package folder",
            }
        }
    })?;
    if archive.len() > limits.max_zip_entries {
        return Err(too_many_entries(limits));
    }
    for i in 0..archive.len() {
        // A name that is not valid text cannot be a part Numbers wrote.
        let name = match archive.name_for_index(i) {
            Some(Ok(name)) => name.into_owned(),
            _ => continue,
        };
        let usage = use_of(&name);
        match usage {
            Use::Skip => continue,
            Use::Password => return Err(NumbersError::PasswordProtected),
            Use::NestedIndex if !allow_nested => continue,
            _ => {}
        }
        let mut file = archive
            .by_index(i)
            .map_err(|e| damaged(&name, format!("the zip entry cannot be opened ({e})")))?;
        if file.is_dir() {
            continue;
        }
        // Property lists are small; a large one is skipped unread.
        if matches!(usage, Use::Properties | Use::BuildHistory) {
            let limit = crate::plist::MAX_INPUT as u64;
            let mut data = Vec::new();
            if file.size() <= limit {
                (&mut file)
                    .take(limit.min(budget.left()) + 1)
                    .read_to_end(&mut data)
                    .map_err(|e| {
                        damaged(&name, format!("the zip entry cannot be decompressed ({e})"))
                    })?;
            }
            if file.size() > limit || data.len() as u64 > limit {
                // The usual name, not the entry's, which may be anything.
                let usual = if matches!(usage, Use::Properties) {
                    PROPERTIES
                } else {
                    BUILD_HISTORY
                };
                parts.skipped.push(usual.to_owned());
                continue;
            }
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
        let over = || {
            if nested && limits.max_input_bytes < left {
                NumbersError::TooLarge {
                    what: "bytes in the nested Index.zip",
                    limit: limits.max_input_bytes,
                }
            } else {
                budget.exceeded()
            }
        };
        if file.size() > most {
            return Err(over());
        }
        // The size in the zip directory may lie; never trust it for memory.
        let mut data = Vec::with_capacity(file.size().min(1 << 20) as usize);
        (&mut file)
            .take(most.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|e| damaged(&name, format!("the zip entry cannot be decompressed ({e})")))?;
        if data.len() as u64 > most {
            return Err(over());
        }
        budget.take(data.len() as u64)?;
        drop(file);
        match usage {
            Use::Archive => parts.archives.push(Part { name, data }),
            Use::NestedIndex => read(Cursor::new(&data[..]), &name, limits, budget, parts, false)?,
            Use::Properties | Use::BuildHistory | Use::Password | Use::Skip => {}
        }
    }
    Ok(())
}

fn too_many_entries(limits: &Limits) -> NumbersError {
    NumbersError::TooLarge {
        what: "entries in the zip file",
        limit: limits.max_zip_entries as u64,
    }
}

/// Checks the end records before the zip crate reads them.
///
/// The zip crate sets aside memory for as many entries as the end record
/// says, before it reads one. A zip64 record can say billions, so every
/// zip64 end record in the file must be within [`Limits::max_zip_entries`],
/// even one the zip crate would reject for other reasons today. A zip32
/// record says at most 65,535; that is checked after the directory is read.
/// The zip crate also tries end records one after another when one is bad,
/// so a file with very many of them is refused.
fn check_end_records<R: Read + Seek>(
    reader: &mut R,
    label: &str,
    limits: &Limits,
) -> Result<(), NumbersError> {
    let len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut ends = 0usize;
    let mut zip64_ends = Vec::new();
    let too_many = || {
        damaged(
            label,
            format!("the zip file has more than {MAX_END_RECORDS} end-of-directory records"),
        )
    };
    // Scan in blocks; keep the last 3 bytes so no signature is split.
    let mut buf = vec![0u8; 1 << 16];
    let mut at = 0u64; // file position of buf[0]
    let mut kept = 0usize;
    let mut source = reader.by_ref().take(len);
    loop {
        let n = source.read(&mut buf[kept..])?;
        let filled = kept + n;
        // `contains` on bytes is memchr, fast even in debug builds; most
        // blocks of a real file hold a 'P', but long runs of padding do not.
        let block = &buf[..filled];
        let windows = if block.contains(&b'P') {
            block.windows(4)
        } else {
            block[..0].windows(4)
        };
        for (i, window) in windows.enumerate() {
            if window == END {
                ends += 1;
                if ends > MAX_END_RECORDS {
                    return Err(too_many());
                }
            } else if window == END64 {
                zip64_ends.push(at + i as u64);
                if zip64_ends.len() > MAX_END_RECORDS {
                    return Err(too_many());
                }
            }
        }
        if n == 0 {
            break;
        }
        let keep = filled.min(3);
        buf.copy_within(filled - keep..filled, 0);
        at += (filled - keep) as u64;
        kept = keep;
    }
    for position in zip64_ends {
        let mut record = [0u8; 56];
        reader.seek(SeekFrom::Start(position))?;
        if reader.read_exact(&mut record).is_err() {
            continue;
        }
        let field = |at: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&record[at..at + 8]);
            u64::from_le_bytes(bytes)
        };
        // Any record over the limit is refused, whether or not the zip
        // crate would accept it today: a later version may check less.
        if field(24).max(field(32)) > limits.max_zip_entries as u64 {
            return Err(too_many_entries(limits));
        }
    }
    reader.seek(SeekFrom::Start(0))?;
    Ok(())
}
