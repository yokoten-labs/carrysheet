//! Reading: open from bytes, walk storages, read streams (MS-CFB 2.2-2.6).

use std::cell::Cell;

use crate::error::CfbError;
use crate::name::{same, split};
use crate::{EntryKind, Limits, MINI_STREAM_CUTOFF, Meta, SIGNATURE, Version};

/// Largest number that names a real sector (MAXREGSECT).
pub(crate) const MAX_REGULAR: u32 = 0xFFFF_FFFA;
/// FAT value of a DIFAT sector.
pub(crate) const DIFSECT: u32 = 0xFFFF_FFFC;
/// FAT value of a FAT sector.
pub(crate) const FATSECT: u32 = 0xFFFF_FFFD;
/// Last sector of a chain.
pub(crate) const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
/// An unused sector.
pub(crate) const FREESECT: u32 = 0xFFFF_FFFF;
/// No sibling or child.
pub(crate) const NOSTREAM: u32 = 0xFFFF_FFFF;
/// Bytes in a directory entry.
pub(crate) const ENTRY_LEN: usize = 128;
/// Bytes in a mini sector.
pub(crate) const MINI_LEN: usize = 64;
/// FAT sector numbers held by the header itself.
pub(crate) const HEADER_DIFAT: usize = 109;

/// Header values the reader does not use, kept so a save writes them back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HeaderKeep {
    /// Header CLSID (MS-CFB: reserved, zero).
    pub clsid: [u8; 16],
    /// Minor version (0x003E in files Microsoft writes).
    pub minor_version: u16,
    /// The 6 reserved bytes after the mini sector shift.
    pub reserved: [u8; 6],
    /// Transaction signature number.
    pub transaction: u32,
}

/// One storage or stream of an opened compound file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    id: u32,
    name: String,
    units: Vec<u16>,
    /// Whether the stored name ends with a null code unit, as MS-CFB
    /// requires.
    terminated: bool,
    /// The name length field as stored, in bytes.
    name_len: u16,
    kind: EntryKind,
    meta: Meta,
    start: u32,
    size: u64,
    /// The size field as stored, before a storage's is read as 0.
    stored_size: u64,
    left: u32,
    right: u32,
    child: u32,
    /// Children in tree order (the order MS-CFB sorts names in).
    children: Vec<u32>,
}

impl Entry {
    /// The name. Code units that are not valid UTF-16 show as U+FFFD; the
    /// exact units are in [`Entry::name_utf16`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name exactly as stored, without the terminating null.
    pub fn name_utf16(&self) -> &[u16] {
        &self.units
    }

    /// Whether the stored name ends with the terminating null MS-CFB
    /// requires. When it does not, every stored code unit is in the name
    /// (up to 32); a copy that cannot keep that form must refuse it.
    pub fn name_terminated(&self) -> bool {
        self.terminated
    }

    /// The name length field as stored, in bytes (the null included).
    pub(crate) fn stored_name_len(&self) -> u16 {
        self.name_len
    }

    /// The left sibling, right sibling and child links as stored.
    pub(crate) fn links(&self) -> (u32, u32, u32) {
        (self.left, self.right, self.child)
    }

    /// The starting sector and size fields as stored (a storage's are
    /// read as 0 but may hold other values).
    pub(crate) fn stored_start_size(&self) -> (u32, u64) {
        (self.start, self.stored_size)
    }

    /// What the entry is.
    pub fn kind(&self) -> EntryKind {
        self.kind
    }

    /// Whether it is a stream.
    pub fn is_stream(&self) -> bool {
        self.kind == EntryKind::Stream
    }

    /// Whether it is a storage (or the root storage).
    pub fn is_storage(&self) -> bool {
        matches!(self.kind, EntryKind::Storage | EntryKind::Root)
    }

    /// The size in bytes the entry declares: a stream's length, or for the
    /// root the mini stream's. Storages have 0. Reading checks it against
    /// the file before trusting it.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Class id, state bits and times, as stored.
    pub fn meta(&self) -> Meta {
        self.meta
    }

    /// The class id as its 16 stored bytes.
    pub fn clsid(&self) -> [u8; 16] {
        self.meta.clsid
    }

    /// User-defined state bits.
    pub fn state_bits(&self) -> u32 {
        self.meta.state_bits
    }

    /// Creation time as a Windows FILETIME; 0 when not set.
    pub fn created(&self) -> u64 {
        self.meta.created
    }

    /// Modification time as a Windows FILETIME; 0 when not set.
    pub fn modified(&self) -> u64 {
        self.meta.modified
    }

    /// The entry's number in the directory.
    pub fn id(&self) -> u32 {
        self.id
    }
}

/// The mini stream: where it lies in regular sectors, and its length.
#[derive(Debug)]
struct Mini {
    sectors: Vec<u32>,
    len: u64,
}

/// A compound file opened from bytes. Reading never changes the bytes.
///
/// Open with [`CompoundFile::open`]; find entries by path with
/// [`CompoundFile::entry`] or [`CompoundFile::walk`]; read streams with
/// [`CompoundFile::read_stream`] and [`CompoundFile::read`]. To change a
/// file, copy it into a [`Builder`](crate::Builder).
#[derive(Debug)]
pub struct CompoundFile<'a> {
    bytes: &'a [u8],
    version: Version,
    header: HeaderKeep,
    sector_len: usize,
    fat: Vec<u32>,
    /// The mini FAT, or why it cannot be read (only small streams need it).
    minifat: Result<Vec<u32>, CfbError>,
    mini: Result<Mini, CfbError>,
    root: Entry,
    /// Every directory slot but the root's; `None` for unused slots.
    entries: Vec<Option<Entry>>,
    detached: Vec<u32>,
    limits: Limits,
    produced: Cell<u64>,
}

fn le16(b: &[u8], at: usize) -> u16 {
    match b.get(at..at + 2) {
        Some(&[x, y]) => u16::from_le_bytes([x, y]),
        _ => 0,
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    match b.get(at..at + 4) {
        Some(&[a, b2, c, d]) => u32::from_le_bytes([a, b2, c, d]),
        _ => 0,
    }
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from(le32(b, at)) | (u64::from(le32(b, at + 4)) << 32)
}

fn words(b: &[u8]) -> impl Iterator<Item = u32> + '_ {
    b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c))
}

/// How a special sector value reads in a message.
fn special(v: u32) -> String {
    match v {
        FREESECT => "the free-sector mark".into(),
        ENDOFCHAIN => "the end-of-chain mark".into(),
        FATSECT => "the FAT-sector mark".into(),
        DIFSECT => "the DIFAT-sector mark".into(),
        v => format!("the reserved value 0x{v:08X}"),
    }
}

/// Follows a chain of sector numbers through `table` from `start`, for
/// at most `max` links. Returns the sectors in order; the chain may end
/// sooner. A link outside the table or back to an earlier sector is an
/// error. Memory is bounded by `max` and by the table's length.
fn follow(
    table: &[u32],
    start: u32,
    max: usize,
    part: &str,
    unit: &str,
) -> Result<Vec<u32>, CfbError> {
    let mut out = Vec::new();
    let mut cur = start;
    // One more than the table holds, so a loop through every sector shows.
    let limit = max.min(table.len() + 1);
    while out.len() < limit && cur != ENDOFCHAIN {
        let Some(&next) = table.get(cur as usize) else {
            let detail = if cur > MAX_REGULAR {
                format!(
                    "its chain reaches {} where a {unit} number belongs",
                    special(cur)
                )
            } else {
                format!(
                    "its chain points to {unit} {cur}, but there are only {} {unit}s",
                    table.len()
                )
            };
            return Err(CfbError::damaged(part, detail));
        };
        out.push(cur);
        cur = next;
    }
    // A chain cannot visit a sector twice: that is a loop.
    let mut sorted = out.clone();
    sorted.sort_unstable();
    if let Some(w) = sorted.windows(2).find(|w| w[0] == w[1]) {
        return Err(CfbError::damaged(
            part,
            format!("its chain loops back to {unit} {}", w[0]),
        ));
    }
    Ok(out)
}

/// A plain bit set over `0..n`.
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }

    /// Sets bit `i`; returns whether it was set before.
    fn test_and_set(&mut self, i: usize) -> bool {
        match self.0.get_mut(i / 64) {
            Some(w) => {
                let bit = 1u64 << (i % 64);
                let was = *w & bit != 0;
                *w |= bit;
                was
            }
            None => true,
        }
    }
}

/// What bytes that are not a compound file are instead.
fn not_cfb(bytes: &[u8]) -> CfbError {
    let detail = if bytes.is_empty() {
        "they are empty".to_string()
    } else if bytes.starts_with(b"PK\x03\x04") {
        "they are a ZIP package, which is how .xlsx, .xlsm and .xlsb files are stored".into()
    } else {
        "they do not start with the OLE2 signature D0 CF 11 E0 A1 B1 1A E1".into()
    };
    CfbError::NotCompoundFile { detail }
}

impl<'a> CompoundFile<'a> {
    /// Opens a compound file with the default [`Limits`].
    pub fn open(bytes: &'a [u8]) -> Result<CompoundFile<'a>, CfbError> {
        CompoundFile::open_with(bytes, Limits::default())
    }

    /// Opens a compound file: reads the header, the FAT, the directory and
    /// its tree, and checks them. Stream bytes are read later, on demand.
    pub fn open_with(bytes: &'a [u8], limits: Limits) -> Result<CompoundFile<'a>, CfbError> {
        if !bytes.starts_with(&SIGNATURE) {
            return Err(not_cfb(bytes));
        }
        if bytes.len() < 512 {
            return Err(CfbError::damaged(
                "header",
                format!(
                    "the file is {} bytes long, shorter than the 512-byte header",
                    bytes.len()
                ),
            ));
        }
        let h = &bytes[..512];
        let major = le16(h, 26);
        let order = le16(h, 28);
        let shift = le16(h, 30);
        let mini_shift = le16(h, 32);
        if order != 0xFFFE {
            return Err(CfbError::Unsupported {
                detail: format!("the byte order mark is 0x{order:04X}, not 0xFFFE"),
            });
        }
        let version = match major {
            3 => Version::V3,
            4 => Version::V4,
            other => {
                return Err(CfbError::Unsupported {
                    detail: format!("the major version is {other}; MS-CFB defines 3 and 4"),
                });
            }
        };
        if shift != version.shift() {
            return Err(CfbError::Unsupported {
                detail: format!(
                    "version {major} files have {}-byte sectors, but the sector shift is {shift}",
                    version.sector_len()
                ),
            });
        }
        if mini_shift != 6 {
            return Err(CfbError::Unsupported {
                detail: format!(
                    "the mini sector shift is {mini_shift}; MS-CFB uses 6 (64-byte mini sectors)"
                ),
            });
        }
        let cutoff = le32(h, 56);
        if u64::from(cutoff) != MINI_STREAM_CUTOFF {
            return Err(CfbError::Unsupported {
                detail: format!("the mini stream cutoff is {cutoff} bytes; MS-CFB uses 4096"),
            });
        }
        let mut header = HeaderKeep {
            minor_version: le16(h, 24),
            transaction: le32(h, 52),
            ..HeaderKeep::default()
        };
        header.clsid.copy_from_slice(&h[8..24]);
        header.reserved.copy_from_slice(&h[34..40]);
        let num_fat = le32(h, 44) as usize;
        let first_dir = le32(h, 48);
        let first_minifat = le32(h, 60);
        let first_difat = le32(h, 68);

        let ss = version.sector_len();
        if bytes.len() <= ss {
            return Err(CfbError::damaged(
                "header",
                format!(
                    "the file ends after {} bytes, before its first {ss}-byte sector",
                    bytes.len()
                ),
            ));
        }
        let num_sectors = (bytes.len() - ss).div_ceil(ss);
        if num_sectors > MAX_REGULAR as usize {
            return Err(CfbError::Unsupported {
                detail: "the file has more sectors than MS-CFB can number".into(),
            });
        }
        let sector = |s: u32, part: &str| -> Result<&'a [u8], CfbError> {
            let start = (s as usize).saturating_add(1).saturating_mul(ss);
            bytes.get(start..start.saturating_add(ss)).ok_or_else(|| {
                CfbError::damaged(
                    part,
                    format!("the file ends inside sector {s}, which holds part of it"),
                )
            })
        };

        // DIFAT: where the FAT sectors are. The header holds the first 109;
        // DIFAT sectors hold the rest.
        let mut difat = Vec::new();
        for i in 0..HEADER_DIFAT {
            let v = le32(h, 76 + 4 * i);
            if v == FREESECT {
                break;
            }
            if v > MAX_REGULAR {
                return Err(CfbError::damaged(
                    "header",
                    format!("its FAT sector list holds {} at place {i}", special(v)),
                ));
            }
            difat.push(v);
        }
        let mut seen = Bits::new(num_sectors);
        let mut cur = first_difat;
        while cur != ENDOFCHAIN && cur != FREESECT {
            if cur as usize >= num_sectors {
                let detail = if cur > MAX_REGULAR {
                    format!(
                        "its chain reaches {} where a sector number belongs",
                        special(cur)
                    )
                } else {
                    format!(
                        "its chain points to sector {cur}, but the file has only {num_sectors} sectors"
                    )
                };
                return Err(CfbError::damaged("DIFAT", detail));
            }
            if seen.test_and_set(cur as usize) {
                return Err(CfbError::damaged(
                    "DIFAT",
                    format!("its chain loops back to sector {cur}"),
                ));
            }
            let sec = sector(cur, "DIFAT")?;
            let (list, next) = sec.split_at(ss - 4);
            for v in words(list) {
                if v != FREESECT && v > MAX_REGULAR {
                    return Err(CfbError::damaged(
                        "DIFAT",
                        format!("its FAT sector list holds {}", special(v)),
                    ));
                }
                difat.push(v);
            }
            // Every DIFAT sector is a sector of the file, so this list
            // grows by at most one sector's worth per sector of the file.
            cur = le32(next, 0);
        }
        // Some writers pad the last DIFAT sector with zeros instead of
        // free marks; zeros past the header's FAT sector count are padding.
        while difat.len() > HEADER_DIFAT && difat.len() > num_fat && difat.last() == Some(&0) {
            difat.pop();
        }
        while difat.last() == Some(&FREESECT) {
            difat.pop();
        }
        if difat.len() > num_sectors {
            return Err(CfbError::damaged(
                "DIFAT",
                format!(
                    "it lists {} FAT sectors, but the whole file has only {num_sectors} sectors",
                    difat.len()
                ),
            ));
        }

        // FAT: the next sector of every sector. Entries past the end of the
        // file mean nothing, so the table has one entry per sector.
        let mut fat = Vec::with_capacity(num_sectors);
        for (k, &s) in difat.iter().enumerate() {
            if fat.len() >= num_sectors {
                break;
            }
            if s as usize >= num_sectors {
                let detail = if s == FREESECT {
                    format!("FAT sector {k} is marked free, but later FAT sectors are listed")
                } else {
                    format!(
                        "it puts FAT sector {k} at sector {s}, but the file has only {num_sectors} sectors"
                    )
                };
                return Err(CfbError::damaged("DIFAT", detail));
            }
            fat.extend(words(sector(s, "FAT")?));
        }
        fat.truncate(num_sectors);
        fat.resize(num_sectors, FREESECT);

        // Directory.
        let per_sector = ss / ENTRY_LEN;
        let max_dir_sectors = limits.max_entries.div_ceil(per_sector).saturating_add(1);
        let dir_sectors = follow(&fat, first_dir, max_dir_sectors, "directory", "sector")?;
        if dir_sectors.is_empty() {
            return Err(CfbError::damaged(
                "directory",
                "the header names no directory sector",
            ));
        }
        if dir_sectors.len() * per_sector > limits.max_entries.max(per_sector) {
            return Err(CfbError::limit(
                "directory entries",
                limits.max_entries as u64,
            ));
        }
        let mut entries = Vec::with_capacity(dir_sectors.len() * per_sector);
        for &s in &dir_sectors {
            for raw in sector(s, "directory")?.as_chunks::<ENTRY_LEN>().0 {
                let id = entries.len() as u32;
                entries.push(parse_entry(id, raw, version)?);
            }
        }
        let root = match entries.first_mut().and_then(Option::take) {
            Some(r) if r.kind == EntryKind::Root => r,
            other => {
                let what = match other {
                    None => "unused".to_string(),
                    Some(e) => format!("of type {}", e.kind.byte()),
                };
                return Err(CfbError::damaged(
                    "directory",
                    format!("entry 0 must be the root storage, but it is {what}"),
                ));
            }
        };
        let mut file = CompoundFile {
            bytes,
            version,
            header,
            sector_len: ss,
            fat,
            minifat: Ok(Vec::new()),
            mini: Ok(Mini {
                sectors: Vec::new(),
                len: 0,
            }),
            root,
            entries,
            detached: Vec::new(),
            limits,
            produced: Cell::new(0),
        };
        file.build_tree()?;

        // The mini FAT and mini stream are needed only for small streams,
        // so a damaged one fails those reads, not the open.
        file.minifat =
            follow(&file.fat, first_minifat, usize::MAX, "mini FAT", "sector").and_then(|chain| {
                let mut table = Vec::with_capacity(chain.len() * (ss / 4));
                for s in chain {
                    table.extend(words(sector(s, "mini FAT")?));
                }
                Ok(table)
            });
        file.mini = file.mini_stream();
        Ok(file)
    }

    /// Links every entry to its storage, checking the red-black trees:
    /// every link must name a used entry, no entry may be linked twice
    /// (a loop), and storages nest at most `max_depth` deep.
    fn build_tree(&mut self) -> Result<(), CfbError> {
        let n = self.entries.len();
        let mut visited = Bits::new(n);
        visited.test_and_set(0);
        let mut todo: Vec<(u32, usize)> = vec![(0, 0)];
        while let Some((id, depth)) = todo.pop() {
            let top = self.get(id).map_or(NOSTREAM, |e| e.child);
            let kids = self.siblings(id, top, &mut visited)?;
            for &k in &kids {
                if let Some(e) = self.get(k) {
                    if e.kind == EntryKind::Root {
                        return Err(CfbError::damaged(
                            "directory",
                            format!("entry {k} is a second root storage"),
                        ));
                    }
                    // A stream has no children; a child link on one is
                    // ignored, and what it points to stays detached.
                    if e.kind != EntryKind::Stream && e.child != NOSTREAM {
                        if depth + 1 > self.limits.max_depth {
                            return Err(CfbError::limit(
                                "levels of nested storages",
                                self.limits.max_depth as u64,
                            ));
                        }
                        todo.push((k, depth + 1));
                    }
                }
            }
            if id == 0 {
                self.root.children = kids;
            } else if let Some(Some(e)) = self.entries.get_mut(id as usize) {
                e.children = kids;
            }
        }
        for (i, e) in self.entries.iter().enumerate() {
            if e.is_some() && !visited.test_and_set(i) {
                self.detached.push(i as u32);
            }
        }
        Ok(())
    }

    /// The entries of one storage's tree, in order (left, node, right).
    fn siblings(&self, parent: u32, top: u32, visited: &mut Bits) -> Result<Vec<u32>, CfbError> {
        let n = self.entries.len();
        let mut out = Vec::new();
        let mut stack = Vec::new();
        let mut cur = top;
        loop {
            while cur != NOSTREAM {
                let Some(e) = self.get(cur).filter(|_| cur != 0) else {
                    let detail = if cur as usize >= n {
                        format!(
                            "the tree of entry {parent} links to entry {cur}, but there are only {n} entries"
                        )
                    } else {
                        format!(
                            "the tree of entry {parent} links to entry {cur}, which is unused or the root"
                        )
                    };
                    return Err(CfbError::damaged("directory", detail));
                };
                if visited.test_and_set(cur as usize) {
                    return Err(CfbError::damaged(
                        "directory",
                        format!("entry {cur} is linked twice, so the directory tree loops"),
                    ));
                }
                stack.push(cur);
                cur = e.left;
            }
            let Some(id) = stack.pop() else { break };
            out.push(id);
            cur = self.get(id).map_or(NOSTREAM, |e| e.right);
        }
        Ok(out)
    }

    /// Where the mini stream lies; checked against the file's size.
    fn mini_stream(&self) -> Result<Mini, CfbError> {
        let len = self.root.size;
        if len == 0 {
            return Ok(Mini {
                sectors: Vec::new(),
                len: 0,
            });
        }
        let file_len = self.bytes.len() as u64;
        if len > file_len {
            return Err(CfbError::damaged(
                "mini stream",
                format!(
                    "the root entry says it holds {len} bytes, but the whole file is {file_len} bytes"
                ),
            ));
        }
        let ss = self.sector_len as u64;
        let need = usize::try_from(len.div_ceil(ss)).unwrap_or(usize::MAX);
        let sectors = follow(&self.fat, self.root.start, need, "mini stream", "sector")?;
        let len = len.min(sectors.len() as u64 * ss);
        Ok(Mini { sectors, len })
    }

    fn get(&self, id: u32) -> Option<&Entry> {
        if id == 0 {
            Some(&self.root)
        } else {
            self.entries.get(id as usize).and_then(Option::as_ref)
        }
    }

    /// The version (sector size) of the file.
    pub fn version(&self) -> Version {
        self.version
    }

    /// The limits the file was opened with.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub(crate) fn header(&self) -> HeaderKeep {
        self.header
    }

    /// The root storage.
    pub fn root(&self) -> &Entry {
        &self.root
    }

    /// The entries of a storage, in the order MS-CFB sorts names (shorter
    /// first, then by upper-cased code units). Empty for a stream.
    pub fn children<'s>(&'s self, entry: &'s Entry) -> impl Iterator<Item = &'s Entry> + 's {
        entry.children.iter().filter_map(|&id| self.get(id))
    }

    /// Used entries that no storage links to. A save keeps them (as
    /// entries without links), though no path reaches them.
    pub fn detached(&self) -> impl Iterator<Item = &Entry> + '_ {
        self.detached.iter().filter_map(|&id| self.get(id))
    }

    /// The entry at a path such as `/VBA/dir`. Names compare without case;
    /// `/` (or an empty path) is the root.
    pub fn entry(&self, path: &str) -> Option<&Entry> {
        let mut cur = &self.root;
        for part in split(path) {
            let units: Vec<u16> = part.encode_utf16().collect();
            cur = self.children(cur).find(|c| same(&c.units, &units))?;
        }
        Some(cur)
    }

    /// Whether something is at the path.
    pub fn exists(&self, path: &str) -> bool {
        self.entry(path).is_some()
    }

    /// Whether a stream is at the path.
    pub fn is_stream(&self, path: &str) -> bool {
        self.entry(path).is_some_and(Entry::is_stream)
    }

    /// Whether a storage (or the root) is at the path.
    pub fn is_storage(&self, path: &str) -> bool {
        self.entry(path).is_some_and(Entry::is_storage)
    }

    /// The storage at `path` and everything under it, parents before
    /// children; the storage itself comes first.
    ///
    /// Each item names its parent by index, so a walk costs a few words per
    /// entry however deep storages nest. [`Walk::path`] builds one item's
    /// path when it is needed.
    pub fn walk(&self, path: &str) -> Result<Walk<'_>, CfbError> {
        let top = self.entry(path).ok_or_else(|| CfbError::NotFound {
            path: path.to_string(),
        })?;
        let mut items = Vec::new();
        let mut todo = vec![(None, 0usize, top)];
        while let Some((parent, depth, entry)) = todo.pop() {
            let me = items.len();
            items.push(WalkItem {
                parent,
                depth,
                entry,
            });
            let kids: Vec<&Entry> = self.children(entry).collect();
            for c in kids.into_iter().rev() {
                todo.push((Some(me), depth + 1, c));
            }
        }
        Ok(Walk { items })
    }

    /// The whole stream at a path.
    pub fn read_stream(&self, path: &str) -> Result<Vec<u8>, CfbError> {
        let e = self.entry(path).ok_or_else(|| CfbError::NotFound {
            path: path.to_string(),
        })?;
        if !e.kind.has_data() {
            return Err(CfbError::WrongKind {
                path: path.to_string(),
                expected: "stream",
            });
        }
        self.read(e)
    }

    /// The whole stream. Fails if its sector chain holds fewer bytes than
    /// its size says.
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, CfbError> {
        self.read_impl(entry, u64::MAX, false)
    }

    /// The first `max` bytes of the stream (all of it if it is shorter).
    /// Only those bytes must be in the file: a stream that says it is
    /// larger than the file still gives its first `max` bytes when its
    /// sector chain holds them.
    pub fn read_prefix(&self, entry: &Entry, max: u64) -> Result<Vec<u8>, CfbError> {
        self.read_impl(entry, max, false)
    }

    /// The stream, or as much of it as its sector chain holds when the
    /// chain ends before the size it declares (some writers store a wrong
    /// size). The caller checks the bytes against its own length fields.
    pub fn read_lenient(&self, entry: &Entry) -> Result<Vec<u8>, CfbError> {
        self.read_impl(entry, u64::MAX, true)
    }

    /// Counts bytes against the output budget before any work is done for
    /// them. A read that then fails keeps its charge, so failing reads
    /// cannot be repeated for free.
    fn charge(&self, n: u64) -> Result<(), CfbError> {
        let budget = (self.bytes.len() as u64).saturating_mul(self.limits.max_output_ratio);
        let total = self.produced.get().saturating_add(n);
        if total > budget {
            return Err(CfbError::limit("bytes of stream data", budget));
        }
        self.produced.set(total);
        Ok(())
    }

    fn read_impl(&self, e: &Entry, max: u64, short_ok: bool) -> Result<Vec<u8>, CfbError> {
        if !e.kind.has_data() {
            return Err(CfbError::WrongKind {
                path: e.name.clone(),
                expected: "stream",
            });
        }
        let want = e.size.min(max);
        if want == 0 {
            return Ok(Vec::new());
        }
        let part = format!("stream {}", e.name);
        let file_len = self.bytes.len() as u64;
        // Only the bytes asked for must exist: a prefix of a stream whose
        // size is wrong can still be read. (`want` is at most the size, so
        // the message holds.)
        if want > file_len && !short_ok {
            return Err(CfbError::damaged(
                part,
                format!(
                    "it says it holds {} bytes, but the whole file is {file_len} bytes",
                    e.size
                ),
            ));
        }
        // Charge what the read may produce (at most the file's size) before
        // following any chain; give back what a successful read did not use.
        let cost = want.min(file_len);
        self.charge(cost)?;
        let result = if e.size < MINI_STREAM_CUTOFF {
            self.read_mini(e, want, cost, short_ok, &part)
        } else {
            self.read_regular(e, want, cost, short_ok, &part)
        };
        if let Ok(out) = &result {
            let unused = cost.saturating_sub(out.len() as u64);
            self.produced
                .set(self.produced.get().saturating_sub(unused));
        }
        result
    }

    fn short(&self, e: &Entry, part: &str, got: u64) -> CfbError {
        CfbError::damaged(
            part,
            format!(
                "it says it holds {} bytes, but its sector chain ends after {got} bytes",
                e.size
            ),
        )
    }

    fn read_regular(
        &self,
        e: &Entry,
        want: u64,
        cost: u64,
        short_ok: bool,
        part: &str,
    ) -> Result<Vec<u8>, CfbError> {
        let ss = self.sector_len;
        // No more links than the charged bytes need.
        let need = usize::try_from(want.min(cost).div_ceil(ss as u64)).unwrap_or(usize::MAX);
        let chain = follow(&self.fat, e.start, need, part, "sector")?;
        let avail = want.min(chain.len() as u64 * ss as u64);
        if avail < want && !short_ok {
            return Err(self.short(e, part, avail));
        }
        let avail = avail as usize; // at most the chain's bytes, so it fits
        let mut out = Vec::with_capacity(avail);
        for s in chain {
            let take = (avail - out.len()).min(ss);
            let start = (s as usize + 1) * ss;
            let have = self.bytes.get(start..).unwrap_or(&[]);
            match have.get(..take) {
                Some(b) => out.extend_from_slice(b),
                None if short_ok => {
                    out.extend_from_slice(have);
                    break;
                }
                None => {
                    return Err(CfbError::damaged(
                        part,
                        format!("the file ends inside sector {s}, which holds part of it"),
                    ));
                }
            }
        }
        Ok(out)
    }

    fn read_mini(
        &self,
        e: &Entry,
        want: u64,
        cost: u64,
        short_ok: bool,
        part: &str,
    ) -> Result<Vec<u8>, CfbError> {
        let minifat = self.minifat.as_ref().map_err(Clone::clone)?;
        let mini = self.mini.as_ref().map_err(Clone::clone)?;
        let need = usize::try_from(want.min(cost).div_ceil(MINI_LEN as u64)).unwrap_or(usize::MAX);
        let chain = follow(minifat, e.start, need, part, "mini sector")?;
        let avail = want.min(chain.len() as u64 * MINI_LEN as u64);
        if avail < want && !short_ok {
            return Err(self.short(e, part, avail));
        }
        let avail = avail as usize; // below the 4096-byte cutoff
        let ss = self.sector_len;
        let mut out = Vec::with_capacity(avail);
        for m in chain {
            let take = (avail - out.len()).min(MINI_LEN);
            let off = m as u64 * MINI_LEN as u64;
            let bytes = (off + take as u64 <= mini.len)
                .then(|| {
                    let s = *mini.sectors.get((off / ss as u64) as usize)?;
                    let start = (s as usize + 1) * ss + (off % ss as u64) as usize;
                    self.bytes.get(start..start + take)
                })
                .flatten();
            match bytes {
                Some(b) => out.extend_from_slice(b),
                None if short_ok => break,
                None => {
                    return Err(CfbError::damaged(
                        part,
                        format!(
                            "its mini sector {m} lies past the end of the mini stream ({} bytes)",
                            mini.len
                        ),
                    ));
                }
            }
        }
        Ok(out)
    }
}

/// One entry of a [`Walk`].
#[derive(Debug, Clone, Copy)]
pub struct WalkItem<'f> {
    /// The index of the storage that holds it in the walk; `None` for the
    /// first item, where the walk started.
    pub parent: Option<usize>,
    /// How many storages down from the start (0 for the start).
    pub depth: usize,
    /// The entry.
    pub entry: &'f Entry,
}

/// A storage and everything under it, from [`CompoundFile::walk`]:
/// parents before children, siblings in name order.
#[derive(Debug, Clone)]
pub struct Walk<'f> {
    items: Vec<WalkItem<'f>>,
}

impl<'f> Walk<'f> {
    /// The items; the first is the storage the walk started at.
    pub fn items(&self) -> &[WalkItem<'f>] {
        &self.items
    }

    /// How many entries the walk holds, the start included.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the walk is empty (it never is: the start is in it).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The entries in walk order.
    pub fn entries(&self) -> impl Iterator<Item = &'f Entry> + '_ {
        self.items.iter().map(|i| i.entry)
    }

    /// The path of item `i` relative to the start, names joined with `/`
    /// (empty for the start). Names are shown as text, so a name holding
    /// `/` or invalid UTF-16 does not round-trip through it; use the
    /// entries' [`Entry::name_utf16`] and `parent` links to copy exactly.
    pub fn path(&self, i: usize) -> Option<String> {
        let mut names = Vec::new();
        let mut cur = self.items.get(i)?;
        while let Some(p) = cur.parent {
            names.push(cur.entry.name());
            cur = self.items.get(p)?;
        }
        names.reverse();
        Some(names.join("/"))
    }
}

impl<'w, 'f> IntoIterator for &'w Walk<'f> {
    type Item = &'w WalkItem<'f>;
    type IntoIter = std::slice::Iter<'w, WalkItem<'f>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

/// One 128-byte directory entry; `None` for an unused slot.
fn parse_entry(id: u32, raw: &[u8], version: Version) -> Result<Option<Entry>, CfbError> {
    let Some(kind) = raw.get(66).and_then(|&b| EntryKind::from_byte(b)) else {
        return Ok(None);
    };
    let name_len = usize::from(le16(raw, 64));
    if name_len > 64 || name_len % 2 != 0 {
        return Err(CfbError::damaged(
            "directory",
            format!(
                "entry {id} says its name is {name_len} bytes long; a name is at most 64 bytes of whole UTF-16 code units"
            ),
        ));
    }
    // The length counts the terminating null. A name whose last unit is
    // not null keeps every unit, so a copy can keep it or refuse it.
    let all = name_len / 2;
    let terminated = all == 0 || le16(raw, 2 * (all - 1)) == 0;
    let n = if terminated {
        all.saturating_sub(1)
    } else {
        all
    };
    let units: Vec<u16> = (0..n).map(|i| le16(raw, 2 * i)).collect();
    let mut clsid = [0u8; 16];
    if let Some(c) = raw.get(80..96) {
        clsid.copy_from_slice(c);
    }
    let stored_size = le64(raw, 120);
    let mut size = stored_size;
    if version == Version::V3 {
        // MS-CFB: version 3 readers ignore the high 32 bits, which some
        // writers leave uninitialized.
        size &= 0xFFFF_FFFF;
    }
    if kind == EntryKind::Storage {
        size = 0;
    }
    Ok(Some(Entry {
        id,
        name: String::from_utf16_lossy(&units),
        units,
        terminated,
        name_len: name_len as u16,
        kind,
        meta: Meta {
            clsid,
            state_bits: le32(raw, 96),
            created: le64(raw, 100),
            modified: le64(raw, 108),
        },
        start: le32(raw, 116),
        size,
        stored_size,
        left: le32(raw, 68),
        right: le32(raw, 72),
        child: le32(raw, 76),
        children: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_stop_at_loops_and_bad_links() {
        let table = [1, 2, ENDOFCHAIN, 0];
        assert_eq!(
            follow(&table, 0, usize::MAX, "t", "sector"),
            Ok(vec![0, 1, 2])
        );
        assert_eq!(follow(&table, 0, 2, "t", "sector"), Ok(vec![0, 1]));
        let looped = [1, 2, 0];
        let e = follow(&looped, 0, usize::MAX, "t", "sector").unwrap_err();
        assert!(e.why().contains("loops back"), "{e}");
        let bad = [7];
        let e = follow(&bad, 0, usize::MAX, "t", "sector").unwrap_err();
        assert!(e.why().contains("only 1 sectors"), "{e}");
        let free = [FREESECT];
        let e = follow(&free, 0, usize::MAX, "t", "sector").unwrap_err();
        assert!(e.why().contains("free-sector mark"), "{e}");
    }

    #[test]
    fn not_a_compound_file() {
        for (bytes, hint) in [
            (&b""[..], "empty"),
            (b"PK\x03\x04", "ZIP"),
            (b"hello", "signature"),
        ] {
            let e = CompoundFile::open(bytes).unwrap_err();
            assert!(matches!(e, CfbError::NotCompoundFile { .. }), "{e}");
            assert!(e.why().contains(hint), "{e}");
        }
    }
}
