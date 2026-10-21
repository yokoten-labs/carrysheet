//! The VBA project as it sits in the file, kept byte for byte.
//!
//! In `.xlsm` and `.xlsb` files the project is one part, `vbaProject.bin`,
//! which is kept as the exact bytes. In `.xls` files it is a storage
//! (`_VBA_PROJECT_CUR`) inside the workbook's own compound file; there the
//! unit that survives a save is each stream's bytes plus each entry's class
//! id, state bits and storage times, which [`KeptStorage`] holds and writes
//! back unchanged.
//!
//! Class ids, state bits and times are kept as the raw values in the file
//! (also on streams, where the format says they are zero), and entries of
//! a type the format does not define keep their type and bytes, so
//! [`KeptStorage::write_into`] writes back every value it read.
//!
//! A signed `.xls` keeps its VBA signature outside this storage, in the
//! `\x05DigitalSignature` and `\x05DigitalSignatureEx` streams at the root
//! of the workbook's compound file; `.xlsm` keeps it in the
//! `xl/vbaProjectSignature.bin` part. Both are the save path's to copy.

use std::collections::HashMap;

use carrysheet_cfb::{Builder, CfbError, CompoundFile, Entry, EntryKind, NodeId};

use crate::error::VbaError;
use crate::{Limits, VbaProject};

/// The storage that holds the VBA project inside an `.xls` file.
pub const XLS_VBA_STORAGE: &str = "/_VBA_PROJECT_CUR";

fn container_error(e: CfbError) -> VbaError {
    VbaError::Container {
        detail: e.to_string(),
    }
}

/// The VBA project of a workbook, exactly as it was in the file.
///
/// Reading macros never changes these bytes, and nothing in this crate
/// re-encodes them: what goes in is what [`VbaPart::bin_bytes`] or
/// [`KeptStorage::write_into`] gives back.
#[derive(Debug, Clone)]
pub struct VbaPart {
    inner: Inner,
}

#[derive(Debug, Clone)]
enum Inner {
    Bin(Vec<u8>),
    Storage(KeptStorage),
}

impl VbaPart {
    /// Keeps the bytes of a `vbaProject.bin` part (from `.xlsm`, `.xlsb`,
    /// `.xltm`, `.xlam`). Nothing is parsed or checked here; call
    /// [`VbaPart::read`] to list the macros.
    pub fn from_bin(bytes: Vec<u8>) -> VbaPart {
        VbaPart {
            inner: Inner::Bin(bytes),
        }
    }

    /// Copies a VBA storage, such as `/_VBA_PROJECT_CUR`, out of an open
    /// compound file.
    pub fn from_storage(
        file: &CompoundFile<'_>,
        storage: &str,
        limits: &Limits,
    ) -> Result<VbaPart, VbaError> {
        let kept = KeptStorage::copy_from(file, storage, limits)?;
        Ok(VbaPart {
            inner: Inner::Storage(kept),
        })
    }

    /// Finds and copies the VBA project of a whole `.xls` file.
    ///
    /// Returns `Ok(None)` when the workbook has no macros.
    pub fn from_xls(xls: &[u8], limits: &Limits) -> Result<Option<VbaPart>, VbaError> {
        let file = open_compound(xls, limits)?;
        if !file.is_storage(XLS_VBA_STORAGE) {
            return Ok(None);
        }
        let kept = KeptStorage::copy_from(&file, XLS_VBA_STORAGE, limits)?;
        Ok(Some(VbaPart {
            inner: Inner::Storage(kept),
        }))
    }

    /// The original `vbaProject.bin` bytes, when the part came from
    /// [`VbaPart::from_bin`].
    pub fn bin_bytes(&self) -> Option<&[u8]> {
        match &self.inner {
            Inner::Bin(b) => Some(b),
            Inner::Storage(_) => None,
        }
    }

    /// The kept storage, when the part came from an `.xls` file.
    pub fn storage(&self) -> Option<&KeptStorage> {
        match &self.inner {
            Inner::Bin(_) => None,
            Inner::Storage(s) => Some(s),
        }
    }

    /// Lists the project's modules, source and risk flags. Read-only.
    pub fn read(&self, limits: &Limits) -> Result<VbaProject, VbaError> {
        match &self.inner {
            Inner::Bin(bytes) => {
                let file = open_compound(bytes, limits)?;
                let kept = KeptStorage::copy_from(&file, "/", limits)?;
                crate::read_kept(&kept, limits)
            }
            Inner::Storage(kept) => crate::read_kept(kept, limits),
        }
    }
}

pub(crate) fn open_compound<'a>(
    bytes: &'a [u8],
    limits: &Limits,
) -> Result<CompoundFile<'a>, VbaError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(VbaError::limit("input bytes", limits.max_input_bytes));
    }
    if !carrysheet_cfb::is_compound_file(bytes) {
        let detail = if bytes.is_empty() {
            "are empty".to_string()
        } else if bytes.starts_with(b"PK\x03\x04") {
            "are a ZIP package (.xlsm, .xlsb); the macros are in its xl/vbaProject.bin part".into()
        } else {
            "do not start with the OLE2 signature D0 CF 11 E0 A1 B1 1A E1".into()
        };
        return Err(VbaError::NotCompoundFile { detail });
    }
    CompoundFile::open(bytes).map_err(container_error)
}

/// One storage or stream of a kept VBA storage.
#[derive(Debug, Clone)]
pub struct KeptEntry {
    path: String,
    parent: Option<usize>,
    meta: Entry,
    data: Option<Vec<u8>>,
}

impl KeptEntry {
    /// The path relative to the VBA storage, with `/` between names; empty
    /// for the VBA storage itself. Example: `VBA/Module1`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The index of the storage this entry is in, among
    /// [`KeptStorage::entries`]; `None` for the VBA storage itself.
    pub fn parent(&self) -> Option<usize> {
        self.parent
    }

    /// The stream bytes, or `None` for a storage. (An entry of a type the
    /// format does not define keeps the bytes it has, too.)
    pub fn data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }

    /// The class id (all zeros for streams in files Excel writes).
    pub fn clsid(&self) -> [u8; 16] {
        self.meta.clsid()
    }

    /// The user-defined state bits.
    pub fn state_bits(&self) -> u32 {
        self.meta.state_bits()
    }

    /// The compound file entry, with its times (Windows FILETIMEs).
    pub fn entry(&self) -> &Entry {
        &self.meta
    }
}

/// A VBA storage copied out of a compound file: every stream's bytes and
/// every entry's metadata, in the file's order.
#[derive(Debug, Clone)]
pub struct KeptStorage {
    entries: Vec<KeptEntry>,
    /// Stream paths in upper case, to their entry.
    index: HashMap<String, usize>,
}

fn path_key(path: &str) -> String {
    path.chars().flat_map(char::to_uppercase).collect()
}

impl KeptStorage {
    /// The entries, parents before children; the first is the storage
    /// itself.
    pub fn entries(&self) -> &[KeptEntry] {
        &self.entries
    }

    /// The bytes of a stream, by path relative to the storage. Names
    /// compare without case, as in compound files.
    pub fn stream(&self, path: &str) -> Option<&[u8]> {
        let i = *self.index.get(&path_key(path))?;
        self.entries[i].data.as_deref()
    }

    /// Total stream bytes held.
    pub fn total_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|e| e.data.as_ref().map_or(0, Vec::len))
            .sum()
    }

    fn copy_from(
        file: &CompoundFile<'_>,
        storage: &str,
        limits: &Limits,
    ) -> Result<KeptStorage, VbaError> {
        if !file.is_storage(storage) {
            return Err(VbaError::MissingStream {
                stream: storage.trim_start_matches('/').to_string(),
            });
        }
        let walk = file.walk(storage).map_err(container_error)?;
        // Count entries before building any path.
        if walk.len() > limits.max_entries {
            return Err(VbaError::limit(
                "entries in the VBA storage",
                limits.max_entries,
            ));
        }
        let over = || VbaError::limit("bytes in the VBA storage", limits.max_storage_bytes);
        let mut entries: Vec<KeptEntry> = Vec::with_capacity(walk.len());
        // Paths and stream bytes together: deep storages with long names
        // make long paths, and they count too.
        let mut total = 0usize;
        for item in &walk {
            let meta = item.entry;
            let path = match item.parent.map(|p| entries[p].path.as_str()) {
                None => String::new(),
                Some("") => meta.name().to_string(),
                Some(up) => format!("{up}/{}", meta.name()),
            };
            total = total.saturating_add(path.len());
            if total > limits.max_storage_bytes {
                return Err(over());
            }
            let data = if matches!(meta.kind(), EntryKind::Stream | EntryKind::Unknown(_)) {
                let len = meta.size();
                if len > limits.max_stream_bytes as u64 {
                    return Err(VbaError::limit("bytes per stream", limits.max_stream_bytes));
                }
                total = total.saturating_add(len as usize);
                if total > limits.max_storage_bytes {
                    return Err(over());
                }
                // The reader checks the declared length against the file
                // before it allocates. Entries of a type the format does
                // not define are read as strictly as streams: a save must
                // write back all of them, not empty bytes in their place.
                Some(file.read(meta).map_err(container_error)?)
            } else {
                None
            };
            entries.push(KeptEntry {
                path,
                parent: item.parent,
                meta: meta.clone(),
                data,
            });
        }
        let mut index = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            if e.data.is_some() {
                index.entry(path_key(&e.path)).or_insert(i);
            }
        }
        Ok(KeptStorage { entries, index })
    }

    /// Writes the storage into a compound file being built, at `dest`
    /// (such as `/_VBA_PROJECT_CUR`, or `/` to make a `vbaProject.bin`),
    /// with the same stream bytes, class ids, state bits and times.
    ///
    /// `dest` must not exist yet unless it is `/`.
    pub fn write_into(&self, file: &mut Builder, dest: &str) -> Result<(), VbaError> {
        let write_err = |e: CfbError| VbaError::WriteFailed {
            detail: e.to_string(),
        };
        let dest = dest.trim_end_matches('/');
        // A name stored without its terminating null cannot be written back
        // the same way through a Builder, so writing refuses it rather than
        // change it; reading the macros is not affected. Checked before
        // anything is written. (The kept storage's own name is not copied.)
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| e.parent.is_some() && !e.meta.name_terminated())
        {
            return Err(VbaError::WriteFailed {
                detail: format!(
                    "directory entry {} has a name with no terminating null, so a copy \
                     cannot keep it exactly",
                    e.meta.id()
                ),
            });
        }
        // Each entry's node in the builder, by its index in `entries`.
        // Names are copied as UTF-16 code units, exactly as they were.
        let mut ids: Vec<NodeId> = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            // The kept storage's own entry may be a root (from a
            // vbaProject.bin); anywhere else it is a storage.
            let kind = match e.meta.kind() {
                EntryKind::Root => EntryKind::Storage,
                other => other,
            };
            let id = match e.parent {
                None if dest.is_empty() => {
                    file.set_meta("/", e.meta.meta()).map_err(write_err)?;
                    NodeId::ROOT
                }
                None => {
                    file.create_entry(dest, kind, e.meta.meta(), Vec::new())
                        .map_err(write_err)?;
                    file.node(dest).ok_or_else(|| VbaError::WriteFailed {
                        detail: format!("the storage {dest} was not found after it was made"),
                    })?
                }
                Some(p) => {
                    let data = e.data.clone().unwrap_or_default();
                    file.create_child(ids[p], e.meta.name_utf16(), kind, e.meta.meta(), data)
                        .map_err(write_err)?
                }
            };
            ids.push(id);
        }
        Ok(())
    }
}

/// Compound file names compare without case: this is the key to compare.
pub(crate) fn name_key(name: &str) -> String {
    path_key(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compare_without_case() {
        assert_eq!(path_key("VBA/dir"), path_key("vba/DIR"));
        assert_ne!(path_key("VBA/dir"), path_key("VBA"));
        assert_eq!(name_key("Módulo1"), name_key("MÓDULO1"));
    }
}
