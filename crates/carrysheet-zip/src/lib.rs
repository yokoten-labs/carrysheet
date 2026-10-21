//! Reading zip archives that may be hostile, written from PKWARE's
//! APPNOTE.TXT (the .ZIP File Format Specification).
//!
//! Office packages (xlsx, xlsm, xlsb), Numbers files and Power Query
//! packages are zip archives. A file from someone else can be built to
//! make a zip reader search, allocate or inflate without end, or to show
//! one reader a different file than another. This reader does less than
//! a general one, on purpose:
//!
//! - The end record is looked for once, backwards, in the last 65,557
//!   bytes (22 plus the longest comment). The last valid one is used; no
//!   other is ever tried.
//! - A zip64 end record is read only through the locator right before the
//!   end record, and must sit exactly where the locator points.
//! - The archive starts at byte 0: the central directory must end right
//!   where the end records begin, and the first entry must start at 0.
//!   Data after the archive is allowed.
//! - The entry count and the directory size are checked against
//!   [`Limits`] before anything is allocated; the directory is read once.
//! - Entries may not overlap, and (unless the caller asks for a report)
//!   no two may share a name.
//! - Only Stored and Deflate, no encryption. Each local header must agree
//!   with its directory entry.
//! - Compressed bytes are charged to a budget before they are inflated,
//!   and the output is capped per entry and in total.
//! - Inflating stops when the input runs ahead of the output (empty
//!   deflate blocks cost time without producing anything), or when one
//!   run of input gives no output for too long, and a caller
//!   can stop it at any point with [`Archive::set_interrupt`]. The limits
//!   bound bytes, not time: callers reading files from others must set
//!   that interrupt (see [`Limits`]).
//!
//! Error messages never hold text from the file (such as entry names), so
//! callers can show them as they are.
//!
//! ```
//! use std::io::Cursor;
//! use carrysheet_zip::{Archive, Limits};
//!
//! // The smallest archive: an end record with no entries.
//! let mut empty = b"PK\x05\x06".to_vec();
//! empty.resize(22, 0);
//! let archive = Archive::open(Cursor::new(empty), Limits::default()).unwrap();
//! assert_eq!(archive.entries().len(), 0);
//! ```

mod cp437;
mod data;
mod directory;
mod error;

use std::collections::HashMap;
use std::io::{Read, Seek};

pub use error::Error;

/// How much an archive may make the reader do. The defaults suit Office
/// packages; real workbooks stay far below them.
///
/// These limits bound memory and bytes, not time. A hostile entry can
/// make the inflater work hard for each byte it is allowed to consume:
/// with miniz_oxide, an entry of empty fixed-Huffman blocks padded up to
/// 1.25 times its output (the most [`Archive::read`] lets through) costs
/// about 141 seconds per worst-case entry, and the default
/// [`max_total_read`](Limits::max_total_read) of 256 MiB about 8 minutes
/// per archive (at 1 GiB it was about 30). Callers that read files from
/// others must set an interrupt with [`Archive::set_interrupt`] (such as
/// a time limit), or lower `max_total_read` to what they can afford.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Entries in the central directory.
    pub max_entries: usize,
    /// Bytes of the central directory, read in one piece.
    pub max_directory_bytes: u64,
    /// Bytes one entry may inflate to.
    pub max_entry_bytes: u64,
    /// Bytes all reads together may produce.
    pub max_total_bytes: u64,
    /// Compressed bytes all reads together may consume (empty deflate
    /// blocks cost input without producing output). Default 256 MiB.
    /// This bounds work only loosely; see the time note above.
    pub max_total_read: u64,
    /// Compressed bytes one [`Archive::read_prefix`] call may consume.
    pub max_prefix_read: u64,
    /// Compressed bytes an entry may consume in a row without producing
    /// output. Real deflate gives output within a block header (under
    /// 300 bytes); a long run of empty blocks between bits of output is
    /// padding. Without this cap such runs are bounded only by the
    /// output earned so far (a quarter more input than output, see
    /// [`Archive::read`]). Default 16 KiB.
    pub max_read_without_output: u64,
    /// What to do when two entries share a name.
    pub duplicate_names: DuplicateNames,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 50_000,
            max_directory_bytes: 16 << 20,
            max_entry_bytes: 256 << 20,
            max_total_bytes: 1 << 30,
            max_total_read: 256 << 20,
            max_prefix_read: 64 << 10,
            max_read_without_output: 16 << 10,
            duplicate_names: DuplicateNames::Refuse,
        }
    }
}

/// Two entries with one name: readers disagree on which one counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateNames {
    /// Refuse the archive as damaged.
    Refuse,
    /// Open it, and list the pairs in [`Archive::duplicates`].
    /// [`Archive::find`] returns the first entry with the name.
    Report,
}

/// How an entry is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Not compressed (method 0).
    Stored,
    /// Deflate (method 8).
    Deflated,
    /// Any other method; such entries cannot be read.
    Other(u16),
}

impl Method {
    fn from_code(code: u16) -> Method {
        match code {
            0 => Method::Stored,
            8 => Method::Deflated,
            n => Method::Other(n),
        }
    }
}

/// One entry of the central directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name: UTF-8 when flagged or valid, else code page 437. Text
    /// from the file: quote it before showing it.
    pub name: String,
    pub method: Method,
    /// General purpose bit flags.
    pub flags: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    /// The size the entry claims to inflate to; checked while reading.
    pub size: u64,
    /// Where the local header starts.
    pub header_offset: u64,
    /// The name as stored, to compare with the local header.
    raw_name: Box<[u8]>,
    /// Where the next entry (or the central directory) starts: this
    /// entry's data must end by then.
    data_limit: u64,
}

impl Entry {
    /// True when the entry is encrypted (traditional or strong).
    pub fn is_encrypted(&self) -> bool {
        self.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0
    }

    /// True for a folder entry (its name ends with `/`).
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

const FLAG_ENCRYPTED: u16 = 1;
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
const FLAG_UTF8: u16 = 1 << 11;

/// A check the reader calls while it inflates; returning true stops the
/// read with [`Error::Interrupted`].
pub type Interrupt = Box<dyn FnMut() -> bool>;

/// An open zip archive.
pub struct Archive<R> {
    reader: R,
    limits: Limits,
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    by_lower: HashMap<String, usize>,
    duplicates: Vec<(usize, usize)>,
    case_twins: Vec<(usize, usize)>,
    read_total: u64,
    out_total: u64,
    interrupt: Option<Interrupt>,
}

impl<R> std::fmt::Debug for Archive<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("entries", &self.entries.len())
            .field("read_total", &self.read_total)
            .field("out_total", &self.out_total)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> Archive<R> {
    /// Opens an archive: finds the end record, checks it, and reads the
    /// central directory. Entry data is read later, on request. For bytes
    /// in memory, pass a `std::io::Cursor`.
    pub fn open(mut reader: R, limits: Limits) -> Result<Archive<R>, Error> {
        let entries = directory::read(&mut reader, &limits)?;
        let mut by_name = HashMap::with_capacity(entries.len());
        let mut by_lower = HashMap::with_capacity(entries.len());
        let mut duplicates = Vec::new();
        let mut case_twins = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if let Some(&first) = by_name.get(&e.name) {
                if limits.duplicate_names == DuplicateNames::Refuse {
                    return Err(Error::Damaged(format!(
                        "entries {first} and {i} have the same name"
                    )));
                }
                duplicates.push((first, i));
                continue;
            }
            by_name.insert(e.name.clone(), i);
            let lower = e.name.to_ascii_lowercase();
            match by_lower.get(&lower) {
                Some(&first) => case_twins.push((first, i)),
                None => {
                    by_lower.insert(lower, i);
                }
            }
        }
        Ok(Archive {
            reader,
            limits,
            entries,
            by_name,
            by_lower,
            duplicates,
            case_twins,
            read_total: 0,
            out_total: 0,
            interrupt: None,
        })
    }

    /// The entries, in central directory order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The index of the entry with exactly this name (the first, when
    /// names repeat).
    pub fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// The index of the entry with this name in any ASCII case, as Office
    /// package part names compare: an exact match first.
    pub fn find_ignore_case(&self, name: &str) -> Option<usize> {
        self.find(name)
            .or_else(|| self.by_lower.get(&name.to_ascii_lowercase()).copied())
    }

    /// Pairs of entries with the same name (first, later), when
    /// [`DuplicateNames::Report`] was asked for.
    pub fn duplicates(&self) -> &[(usize, usize)] {
        &self.duplicates
    }

    /// Pairs of entries whose names differ only in ASCII case.
    pub fn case_twins(&self) -> &[(usize, usize)] {
        &self.case_twins
    }

    /// Compressed bytes read so far, charged against
    /// [`Limits::max_total_read`].
    pub fn bytes_read(&self) -> u64 {
        self.read_total
    }

    /// Reads a whole entry, at most `max_bytes` long (and at most
    /// [`Limits::max_entry_bytes`]), and checks its CRC-32.
    ///
    /// Without an interrupt ([`Archive::set_interrupt`]) a hostile entry
    /// can take minutes: see [`Limits`]. Set one, such as a time limit,
    /// when the archive comes from someone else.
    pub fn read(&mut self, index: usize, max_bytes: u64) -> Result<Vec<u8>, Error> {
        data::read(self, index, max_bytes)
    }

    /// Reads the first `n` bytes of an entry (fewer when it is shorter),
    /// inflating only as far as needed. Input is charged as it is read,
    /// at most [`Limits::max_prefix_read`] per call. When `n` covers the
    /// whole entry this is [`Archive::read`], with its size and CRC-32
    /// checks.
    pub fn read_prefix(&mut self, index: usize, n: usize) -> Result<Vec<u8>, Error> {
        data::read_prefix(self, index, n)
    }

    /// Sets a check called often while inflating (every 512 compressed
    /// bytes); when it returns true the read stops with
    /// [`Error::Interrupted`]. Callers use it for a time limit, which
    /// this crate cannot keep itself (it also builds for the browser).
    pub fn set_interrupt(&mut self, check: Interrupt) {
        self.interrupt = Some(check);
    }

    /// Gives back the reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}
