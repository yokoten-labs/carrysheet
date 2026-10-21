//! Read and write compound files (Microsoft's MS-CFB, also called OLE2 or
//! structured storage): the container of `.xls` workbooks, `vbaProject.bin`
//! and encrypted Office files.
//!
//! Written from the published MS-CFB specification. Every input is treated
//! as hostile: reading returns an error and never panics, so no
//! `catch_unwind` is needed (and on `wasm32`, where a panic aborts, none
//! would help).
//!
//! ```
//! use carrysheet_cfb::{Builder, CompoundFile, Version};
//!
//! # fn demo() -> Result<(), carrysheet_cfb::CfbError> {
//! // Write: storages and streams by path, then the bytes of the file.
//! let mut b = Builder::new(Version::V3);
//! b.create_storage("/VBA")?;
//! b.create_stream("/VBA/dir", b"\x01\x02".to_vec())?;
//! let bytes = b.to_bytes()?;
//!
//! // Read: open from bytes, walk storages, read a stream by path.
//! let cf = CompoundFile::open(&bytes)?;
//! let walk = cf.walk("/")?;
//! for (i, item) in walk.items().iter().enumerate() {
//!     let path = walk.path(i).unwrap_or_default();
//!     println!("{path} {:?} {} bytes", item.entry.kind(), item.entry.size());
//! }
//! assert_eq!(cf.read_stream("/vba/DIR")?, b"\x01\x02");
//! # Ok(())
//! # }
//! # demo().unwrap();
//! ```
//!
//! # Hostile input
//!
//! Everything a reader allocates is backed by bytes that are really in the
//! file, and [`Limits`] bound the rest:
//!
//! - every sector chain (FAT, mini FAT, DIFAT, directory, streams) is
//!   checked for loops and for sectors past the end of the file, so it is
//!   never longer than the file has sectors;
//! - the number of FAT sectors the DIFAT lists is at most the number of
//!   sectors in the file;
//! - directory entries are capped at [`Limits::max_entries`]; each entry
//!   may be reached once (a second link is a loop), and storages nest at
//!   most [`Limits::max_depth`] deep;
//! - a stream (or the mini stream) that says it is larger than the file
//!   is refused before anything is allocated for it;
//! - all reads from one [`CompoundFile`] together produce at most
//!   [`Limits::max_output_ratio`] times the file size; a read is charged
//!   before it follows any chain, and a read that fails keeps its charge;
//! - a walk holds parent links, not paths, so deep storages cost no more
//!   than shallow ones.
//!
//! # Keeping what it does not understand
//!
//! Class ids, state bits and times are kept as raw values, entries of an
//! unknown type keep their type byte, and allocated entries that no
//! storage links to are kept as detached entries. [`Builder::from_file`]
//! copies all of it, and [`Builder::to_bytes`] writes it back, so a
//! read-then-write keeps every stream byte for byte. The sector layout is
//! new: the file is rewritten, not patched.
//!
//! Names keep their exact UTF-16 units, also a name stored without its
//! terminating null, and storages keep their stored start and size
//! fields. What a copy cannot keep (stream bytes past what the file
//! holds, a child link on a stream, links on detached entries) is listed
//! in [`Builder::losses`]; what it keeps although MS-CFB forbids it is
//! listed in [`Builder::notes`].

mod error;
mod name;
mod read;
mod upper_table;
mod write;

#[doc(hidden)]
pub mod fuzz_targets;

pub use error::CfbError;
pub use read::{CompoundFile, Entry, Walk, WalkItem};
pub use write::{Builder, Loss, LossKind, NodeId, Note};

/// The OLE2 signature at the start of every compound file.
pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Streams smaller than this live in the mini stream (64-byte sectors).
pub const MINI_STREAM_CUTOFF: u64 = 4096;

/// Whether `bytes` starts with the compound file signature.
pub fn is_compound_file(bytes: &[u8]) -> bool {
    bytes.starts_with(&SIGNATURE)
}

/// The two versions MS-CFB defines. They differ only in sector size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
    /// Version 3: 512-byte sectors. What Excel writes for `.xls`.
    V3,
    /// Version 4: 4096-byte sectors.
    V4,
}

impl Version {
    /// The sector size in bytes.
    pub fn sector_len(self) -> usize {
        match self {
            Version::V3 => 512,
            Version::V4 => 4096,
        }
    }

    pub(crate) fn number(self) -> u16 {
        match self {
            Version::V3 => 3,
            Version::V4 => 4,
        }
    }

    pub(crate) fn shift(self) -> u16 {
        match self {
            Version::V3 => 9,
            Version::V4 => 12,
        }
    }
}

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// The root storage (always entry 0).
    Root,
    /// A storage: a folder of entries.
    Storage,
    /// A stream: bytes.
    Stream,
    /// A type byte MS-CFB does not define (early OLE wrote 3 and 4). Kept
    /// as found: its children are walked and its bytes read like a
    /// stream's, so a save can write it back.
    Unknown(u8),
}

impl EntryKind {
    pub(crate) fn from_byte(b: u8) -> Option<EntryKind> {
        match b {
            0 => None,
            1 => Some(EntryKind::Storage),
            2 => Some(EntryKind::Stream),
            5 => Some(EntryKind::Root),
            other => Some(EntryKind::Unknown(other)),
        }
    }

    pub(crate) fn byte(self) -> u8 {
        match self {
            EntryKind::Storage => 1,
            EntryKind::Stream => 2,
            EntryKind::Root => 5,
            EntryKind::Unknown(b) => b,
        }
    }

    /// Whether entries of this kind may hold bytes.
    pub(crate) fn has_data(self) -> bool {
        matches!(self, EntryKind::Stream | EntryKind::Unknown(_))
    }
}

/// The values of a directory entry that say nothing about its place or
/// its bytes, kept raw so a save writes them back unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Meta {
    /// The class id (a GUID as its 16 stored bytes). MS-CFB says it is
    /// zero for streams; a non-zero one is kept anyway.
    pub clsid: [u8; 16],
    /// User-defined state bits.
    pub state_bits: u32,
    /// Creation time as a Windows FILETIME (100 ns since 1601-01-01 UTC);
    /// 0 when not set.
    pub created: u64,
    /// Modification time, like `created`.
    pub modified: u64,
}

/// Bounds on what a reader may produce. The defaults are far above any
/// real file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Directory entries (128 bytes each in the file), used or not.
    pub max_entries: usize,
    /// How deep storages may nest inside each other.
    pub max_depth: usize,
    /// All reads from one opened file together produce at most this many
    /// times the file's size. Reading every stream once produces less
    /// than the file size.
    pub max_output_ratio: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 100_000,
            max_depth: 64,
            max_output_ratio: 4,
        }
    }
}
