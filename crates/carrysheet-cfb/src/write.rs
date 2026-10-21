//! Writing: build a tree of storages and streams, then lay it out as a
//! compound file (MS-CFB 2.2-2.6).

use std::collections::BTreeMap;

use crate::error::CfbError;
use crate::name::{MAX_NAME_UNITS, checked, compare, key, split};
use crate::read::{
    DIFSECT, ENDOFCHAIN, ENTRY_LEN, FATSECT, FREESECT, HEADER_DIFAT, HeaderKeep, MAX_REGULAR,
    MINI_LEN, NOSTREAM,
};
use crate::{CompoundFile, Entry, EntryKind, MINI_STREAM_CUTOFF, Meta, SIGNATURE, Version};

/// [`Builder::canonical`]: per entry, (depth, name, name terminated,
/// kind, meta, a storage's stored start and size, bytes).
pub(crate) type Canonical<'b> = Vec<(
    usize,
    &'b [u16],
    bool,
    EntryKind,
    Meta,
    (u32, u64),
    &'b [u8],
)>;

const RED: u8 = 0;
const BLACK: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    units: Vec<u16>,
    /// Whether the name is written with its terminating null. Only a copy
    /// of a name stored without one (see [`Note`]) is not.
    terminated: bool,
    /// A copied storage's starting sector and size fields as stored, so
    /// the copy writes them back (MS-CFB says 0; some writers put the
    /// end-of-chain mark). New storages write 0.
    stored: (u32, u64),
    kind: EntryKind,
    meta: Meta,
    data: Vec<u8>,
    children: Vec<usize>,
    /// Children by name in compare form ([`key`]), so finding a name costs
    /// O(log n) however many entries a storage holds. A copied file may
    /// hold one name twice; the index then names the first. A BTreeMap,
    /// not a HashMap: on wasm32-unknown-unknown std seeds its hasher from
    /// memory addresses, which are the same every run, so a crafted file
    /// could pick names that all collide.
    index: BTreeMap<Vec<u16>, usize>,
}

impl Node {
    fn new(units: Vec<u16>, kind: EntryKind, meta: Meta, data: Vec<u8>) -> Node {
        Node {
            units,
            terminated: true,
            stored: (0, 0),
            kind,
            meta,
            data,
            children: Vec::new(),
            index: BTreeMap::new(),
        }
    }

    /// A node copied from `e`, its name in the form it was stored in.
    fn copy_of(e: &Entry, data: Vec<u8>) -> Node {
        let mut n = Node::new(e.name_utf16().to_vec(), e.kind(), e.meta(), data);
        n.terminated = e.name_terminated();
        if e.kind() == EntryKind::Storage {
            n.stored = e.stored_start_size();
        }
        n
    }

    fn holds_entries(&self) -> bool {
        !matches!(self.kind, EntryKind::Stream)
    }
}

/// An entry of a [`Builder`], for adding children by exact name with
/// [`Builder::create_child`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(usize);

impl NodeId {
    /// The root storage.
    pub const ROOT: NodeId = NodeId(0);
}

/// Something [`Builder::from_file`] could not keep: writing the copy
/// changes it. [`Loss::kind`] says what.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Loss {
    /// What was lost.
    pub kind: LossKind,
    /// The entry's number in the source file's directory.
    pub id: u32,
    /// Its name (shown as text).
    pub name: String,
    /// For [`LossKind::StreamBytes`], the size its directory entry
    /// declares; otherwise 0.
    pub declared: u64,
    /// For [`LossKind::StreamBytes`], the bytes kept; otherwise 0.
    pub kept: u64,
    /// What happened and what the copy writes instead.
    pub why: String,
}

/// What a [`Loss`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LossKind {
    /// A stream holds less than its size says: the bytes the file holds
    /// are kept.
    StreamBytes,
    /// A stream links to a child, which a stream cannot have: the entries
    /// it reaches are kept as detached entries, without the link.
    StreamChild,
    /// A detached entry links to other entries: each is kept on its own,
    /// without the links.
    DetachedLinks,
}

/// Something odd [`Builder::from_file`] kept exactly as it was: the copy
/// writes it the same way, so nothing is lost, but the source file does
/// not follow MS-CFB there.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Note {
    /// The entry's number in the source file's directory.
    pub id: u32,
    /// Its name (shown as text).
    pub name: String,
    /// What is odd, and how the copy keeps it.
    pub why: String,
}

/// A compound file being built in memory: storages and streams by path,
/// with their class ids, state bits and times. [`Builder::to_bytes`] lays
/// it out as a file.
///
/// Start empty with [`Builder::new`], or copy an opened file with
/// [`Builder::from_file`] to change it and write it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builder {
    version: Version,
    header: HeaderKeep,
    /// The tree; `nodes[0]` is the root.
    nodes: Vec<Node>,
    /// Entries no storage links to, kept from a copied file.
    detached: Vec<Node>,
    /// What a copy could not keep.
    losses: Vec<Loss>,
    /// What a copy kept although it does not follow MS-CFB.
    notes: Vec<Note>,
}

/// Where a stream's bytes go.
#[derive(Clone, Copy)]
enum Place {
    None,
    Mini(u32),
    Regular(u32),
}

/// A stream's bytes for a copy: all of them, or as many as the file holds
/// with a [`Loss`] that says why. Only a budget error stops the copy.
fn copy_data(
    file: &CompoundFile<'_>,
    e: &Entry,
    losses: &mut Vec<Loss>,
) -> Result<Vec<u8>, CfbError> {
    if !e.kind().has_data() {
        return Ok(Vec::new());
    }
    let (data, why) = match file.read_lenient(e) {
        Ok(d) if d.len() as u64 == e.size() => return Ok(d),
        Ok(d) => {
            let why = format!(
                "it says it holds {} bytes, but its sector chain holds {}",
                e.size(),
                d.len()
            );
            (d, why)
        }
        Err(err @ CfbError::LimitExceeded { .. }) => return Err(err),
        Err(err) => (Vec::new(), err.why()),
    };
    losses.push(Loss {
        kind: LossKind::StreamBytes,
        id: e.id(),
        name: e.name().to_string(),
        declared: e.size(),
        kept: data.len() as u64,
        why,
    });
    Ok(data)
}

/// What a copy of `e` cannot keep about its place in the tree, and what
/// it keeps that MS-CFB does not allow. `detached` is whether no storage
/// links to it.
fn check_entry(
    e: &Entry,
    version: Version,
    detached: bool,
    losses: &mut Vec<Loss>,
    notes: &mut Vec<Note>,
) {
    let loss = |kind, why: String| Loss {
        kind,
        id: e.id(),
        name: e.name().to_string(),
        declared: 0,
        kept: 0,
        why,
    };
    let (left, right, child) = e.links();
    if detached && [left, right, child].iter().any(|&l| l != NOSTREAM) {
        losses.push(loss(
            LossKind::DetachedLinks,
            format!(
                "no storage links to it, but it links to entries {} (left, right, child); \
                 the copy keeps it without those links",
                [left, right, child]
                    .map(|l| if l == NOSTREAM {
                        "none".to_string()
                    } else {
                        l.to_string()
                    })
                    .join(", ")
            ),
        ));
    } else if e.kind() == EntryKind::Root && (left != NOSTREAM || right != NOSTREAM) {
        losses.push(loss(
            LossKind::DetachedLinks,
            format!(
                "it is the root, which has no siblings, but it links to entries {} (left, \
                 right); the copy writes no sibling links",
                [left, right]
                    .map(|l| if l == NOSTREAM {
                        "none".to_string()
                    } else {
                        l.to_string()
                    })
                    .join(", ")
            ),
        ));
    } else if e.kind() == EntryKind::Stream && child != NOSTREAM {
        losses.push(loss(
            LossKind::StreamChild,
            format!(
                "it is a stream but links to child entry {child}; the copy keeps what that \
                 link reaches as detached entries"
            ),
        ));
    }
    let mut note = |why: String| {
        notes.push(Note {
            id: e.id(),
            name: e.name().to_string(),
            why,
        })
    };
    if !e.name_terminated() {
        note(format!(
            "its name ({} code units) has no terminating null; the copy writes it \
             the same way",
            e.name_utf16().len()
        ));
    }
    if e.name_utf16().is_empty() && e.stored_name_len() != 0 {
        note(format!(
            "its name is empty but its name length is {} bytes; the copy writes length 0, \
             as for any empty name",
            e.stored_name_len()
        ));
    }
    let (start, size) = e.stored_start_size();
    if e.kind() == EntryKind::Storage && (start != 0 || size != 0) {
        note(format!(
            "it is a storage with starting sector 0x{start:08X} and size {size}, where \
             MS-CFB requires 0; readers ignore them, and the copy writes them the same way"
        ));
    }
    if version == Version::V3 && e.kind() != EntryKind::Storage && size >> 32 != 0 {
        note(format!(
            "its size field has 0x{:08X} in the high 32 bits, which MS-CFB tells version 3 \
             readers to ignore; the copy uses the low 32 bits ({}) and writes 0 above them",
            size >> 32,
            size & 0xFFFF_FFFF
        ));
    }
}

impl Builder {
    /// An empty compound file: only the root storage, which has no class
    /// id and no times.
    pub fn new(version: Version) -> Builder {
        Builder {
            version,
            header: HeaderKeep {
                minor_version: 0x003E,
                ..HeaderKeep::default()
            },
            nodes: vec![Node::new(
                "Root Entry".encode_utf16().collect(),
                EntryKind::Root,
                Meta::default(),
                Vec::new(),
            )],
            detached: Vec::new(),
            losses: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Copies everything in an opened file: every stream's bytes, every
    /// entry's exact name, type, class id, state bits and times, the
    /// detached entries, and the header values the format leaves unused.
    /// Writing the copy gives a file with the same content in a new layout.
    ///
    /// A stream whose sector chain holds less than its size says is kept
    /// as far as it goes and listed in [`Builder::losses`]; a caller that
    /// must not change data refuses to save when that list is not empty.
    /// Only a [`Limits`](crate::Limits) bound stops the copy.
    pub fn from_file(file: &CompoundFile<'_>) -> Result<Builder, CfbError> {
        let root = file.root();
        let mut b = Builder {
            version: file.version(),
            header: file.header(),
            nodes: vec![Node::copy_of(root, Vec::new())],
            detached: Vec::new(),
            losses: Vec::new(),
            notes: Vec::new(),
        };
        check_entry(root, b.version, false, &mut b.losses, &mut b.notes);
        let mut todo = vec![(root, 0usize)];
        while let Some((entry, at)) = todo.pop() {
            for c in file.children(entry) {
                check_entry(c, b.version, false, &mut b.losses, &mut b.notes);
                let data = copy_data(file, c, &mut b.losses)?;
                let i = b.push(at, Node::copy_of(c, data));
                todo.push((c, i));
            }
        }
        for d in file.detached() {
            check_entry(d, b.version, true, &mut b.losses, &mut b.notes);
            let data = copy_data(file, d, &mut b.losses)?;
            b.detached.push(Node::copy_of(d, data));
        }
        Ok(b)
    }

    /// What [`Builder::from_file`] could not keep: stream bytes past what
    /// the file holds, and links and fields the copy writes differently.
    /// Empty when the copy is exact.
    pub fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// What [`Builder::from_file`] kept exactly although it does not follow
    /// MS-CFB, such as a name without its terminating null.
    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    /// Adds a node under `parent`; returns its index.
    fn push(&mut self, parent: usize, node: Node) -> usize {
        let i = self.nodes.len();
        let k = key(&node.units);
        self.nodes.push(node);
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(i);
            p.index.entry(k).or_insert(i);
        }
        i
    }

    /// The version the file will be written as.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Changes the version (sector size) the file will be written as.
    pub fn set_version(&mut self, version: Version) {
        self.version = version;
    }

    fn child(&self, parent: usize, units: &[u16]) -> Option<usize> {
        self.nodes.get(parent)?.index.get(&key(units)).copied()
    }

    fn find(&self, path: &str) -> Option<usize> {
        let mut cur = 0usize;
        for part in split(path) {
            let units: Vec<u16> = part.encode_utf16().collect();
            cur = self.child(cur, &units)?;
        }
        Some(cur)
    }

    /// The entry at a path, for [`Builder::create_child`].
    pub fn node(&self, path: &str) -> Option<NodeId> {
        self.find(path).map(NodeId)
    }

    /// Adds an entry under `parent` with its name exactly as given in
    /// UTF-16, as a copy from another file needs: any code units, even
    /// `/` or half a surrogate pair, up to 31 of them. The name must not
    /// be in the storage yet (compared without case).
    pub fn create_child(
        &mut self,
        parent: NodeId,
        units: &[u16],
        kind: EntryKind,
        meta: Meta,
        data: Vec<u8>,
    ) -> Result<NodeId, CfbError> {
        let name = || String::from_utf16_lossy(units);
        if kind == EntryKind::Root {
            return Err(CfbError::WrongKind {
                path: name(),
                expected: "storage or stream (there is one root)",
            });
        }
        if units.len() > MAX_NAME_UNITS {
            return Err(CfbError::InvalidName {
                name: name(),
                why: "it is longer than 31 UTF-16 code units, the most a directory entry holds",
            });
        }
        if !self.nodes.get(parent.0).is_some_and(Node::holds_entries) {
            return Err(CfbError::WrongKind {
                path: name(),
                expected: "storage",
            });
        }
        if self.child(parent.0, units).is_some() {
            return Err(CfbError::AlreadyExists { path: name() });
        }
        let data = if kind.has_data() { data } else { Vec::new() };
        Ok(NodeId(self.push(
            parent.0,
            Node::new(units.to_vec(), kind, meta, data),
        )))
    }

    /// The parent of a new entry and the new entry's name.
    fn place_for(&self, path: &str) -> Result<(usize, Vec<u16>), CfbError> {
        let parts: Vec<&str> = split(path).collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(CfbError::AlreadyExists {
                path: path.to_string(),
            });
        };
        let parent_path = parents.join("/");
        let parent = self.find(&parent_path).ok_or_else(|| CfbError::NotFound {
            path: format!("/{parent_path}"),
        })?;
        if !self.nodes.get(parent).is_some_and(Node::holds_entries) {
            return Err(CfbError::WrongKind {
                path: format!("/{parent_path}"),
                expected: "storage",
            });
        }
        Ok((parent, checked(name)?))
    }

    /// Adds an entry of any kind except the root; the storage it goes in
    /// must exist and must not hold the name yet.
    pub fn create_entry(
        &mut self,
        path: &str,
        kind: EntryKind,
        meta: Meta,
        data: Vec<u8>,
    ) -> Result<(), CfbError> {
        if kind == EntryKind::Root {
            return Err(CfbError::WrongKind {
                path: path.to_string(),
                expected: "storage or stream (there is one root)",
            });
        }
        let (parent, units) = self.place_for(path)?;
        if self.child(parent, &units).is_some() {
            return Err(CfbError::AlreadyExists {
                path: path.to_string(),
            });
        }
        let data = if kind.has_data() { data } else { Vec::new() };
        self.push(parent, Node::new(units, kind, meta, data));
        Ok(())
    }

    /// Adds a storage. Its parent must exist.
    pub fn create_storage(&mut self, path: &str) -> Result<(), CfbError> {
        self.create_entry(path, EntryKind::Storage, Meta::default(), Vec::new())
    }

    /// Adds a storage and any storages above it that are missing.
    pub fn create_storage_all(&mut self, path: &str) -> Result<(), CfbError> {
        let mut sofar = String::new();
        for part in split(path) {
            sofar.push('/');
            sofar.push_str(part);
            match self.find(&sofar) {
                Some(i) if self.nodes.get(i).is_some_and(Node::holds_entries) => {}
                Some(_) => {
                    return Err(CfbError::WrongKind {
                        path: sofar,
                        expected: "storage",
                    });
                }
                None => self.create_storage(&sofar)?,
            }
        }
        Ok(())
    }

    /// Adds a new stream. Fails if the path is taken.
    pub fn create_stream(&mut self, path: &str, data: Vec<u8>) -> Result<(), CfbError> {
        self.create_entry(path, EntryKind::Stream, Meta::default(), data)
    }

    /// Sets a stream's bytes, adding the stream if it is not there. Its
    /// class id, state bits and times stay as they were.
    pub fn put_stream(&mut self, path: &str, data: Vec<u8>) -> Result<(), CfbError> {
        match self.find(path) {
            None => self.create_stream(path, data),
            Some(i) => match self.nodes.get_mut(i) {
                Some(n) if n.kind == EntryKind::Stream => {
                    n.data = data;
                    Ok(())
                }
                _ => Err(CfbError::WrongKind {
                    path: path.to_string(),
                    expected: "stream",
                }),
            },
        }
    }

    /// The bytes of a stream in the builder.
    pub fn stream(&self, path: &str) -> Option<&[u8]> {
        let n = self.nodes.get(self.find(path)?)?;
        n.kind.has_data().then_some(n.data.as_slice())
    }

    /// The class id, state bits and times of an entry.
    pub fn meta(&self, path: &str) -> Option<Meta> {
        Some(self.nodes.get(self.find(path)?)?.meta)
    }

    /// Sets the class id, state bits and times of an entry (`/` is the
    /// root).
    pub fn set_meta(&mut self, path: &str, meta: Meta) -> Result<(), CfbError> {
        let i = self.find(path).ok_or_else(|| CfbError::NotFound {
            path: path.to_string(),
        })?;
        if let Some(n) = self.nodes.get_mut(i) {
            n.meta = meta;
        }
        Ok(())
    }

    /// Each storage's entries in MS-CFB's name order (a stable sort, so
    /// equal names keep their order), so the layout depends only on the
    /// content.
    fn sorted_children(&self) -> Vec<Vec<usize>> {
        self.nodes
            .iter()
            .map(|n| {
                let mut kids = n.children.clone();
                kids.sort_by(|&a, &b| match (self.nodes.get(a), self.nodes.get(b)) {
                    (Some(x), Some(y)) => compare(&x.units, &y.units),
                    _ => std::cmp::Ordering::Equal,
                });
                kids
            })
            .collect()
    }

    /// The content in a form that does not depend on the order entries
    /// were added in: the tree depth first in name order, then the
    /// detached entries. Two builders with the same canonical form write
    /// the same bytes.
    pub(crate) fn canonical(&self) -> Canonical<'_> {
        let sorted = self.sorted_children();
        let mut out = Vec::with_capacity(self.nodes.len() + self.detached.len());
        let mut todo = vec![(0usize, 0usize)];
        while let Some((i, depth)) = todo.pop() {
            let Some(n) = self.nodes.get(i) else { continue };
            out.push((
                depth,
                n.units.as_slice(),
                n.terminated,
                n.kind,
                n.meta,
                n.stored,
                n.data.as_slice(),
            ));
            if let Some(kids) = sorted.get(i) {
                todo.extend(kids.iter().rev().map(|&k| (k, depth + 1)));
            }
        }
        for n in &self.detached {
            out.push((
                0,
                n.units.as_slice(),
                n.terminated,
                n.kind,
                n.meta,
                n.stored,
                n.data.as_slice(),
            ));
        }
        out
    }

    /// Lays the file out and returns its bytes. The layout is fixed: FAT,
    /// DIFAT, directory, mini FAT, mini stream, then the large streams,
    /// each contiguous; names in each storage form a balanced red-black
    /// tree.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CfbError> {
        let ss = self.version.sector_len();
        let eps = ss / 4;

        let sorted = self.sorted_children();
        // Directory order: the tree depth first, then detached entries.
        let mut order: Vec<&Node> = Vec::with_capacity(self.nodes.len() + self.detached.len());
        let mut ids = vec![NOSTREAM; self.nodes.len()];
        let mut todo = vec![0usize];
        while let Some(i) = todo.pop() {
            let Some(n) = self.nodes.get(i) else { continue };
            if let Some(slot) = ids.get_mut(i) {
                *slot = order.len() as u32;
            }
            order.push(n);
            if let Some(kids) = sorted.get(i) {
                todo.extend(kids.iter().rev());
            }
        }
        order.extend(self.detached.iter());
        if order.len() >= MAX_REGULAR as usize {
            return Err(CfbError::TooLarge {
                detail: format!(
                    "{} entries are more than a directory can number",
                    order.len()
                ),
            });
        }

        // Where each stream goes.
        let mut places = vec![Place::None; order.len()];
        let mut mini_sectors = 0u64;
        let mut big_sectors = 0u64;
        for (n, place) in order.iter().zip(places.iter_mut()) {
            if !n.kind.has_data() || n.data.is_empty() {
                continue;
            }
            let len = n.data.len() as u64;
            if self.version == Version::V3 && len > u64::from(u32::MAX) {
                return Err(CfbError::TooLarge {
                    detail: format!(
                        "a stream of {len} bytes does not fit a version 3 file, which stores sizes in 32 bits"
                    ),
                });
            }
            if len < MINI_STREAM_CUTOFF {
                *place = Place::Mini(mini_sectors as u32);
                mini_sectors += len.div_ceil(MINI_LEN as u64);
            } else {
                *place = Place::Regular(big_sectors as u32);
                big_sectors += len.div_ceil(ss as u64);
            }
        }
        let too_large = || CfbError::TooLarge {
            detail: "the streams need more sectors than MS-CFB can number".into(),
        };
        let mini_bytes = mini_sectors * MINI_LEN as u64;
        let dir = (order.len() * ENTRY_LEN).div_ceil(ss) as u64;
        let minifat = (mini_sectors * 4).div_ceil(ss as u64);
        let ministream = mini_bytes.div_ceil(ss as u64);
        let fixed = dir + minifat + ministream + big_sectors;
        let (mut fat, mut difat) = (0u64, 0u64);
        loop {
            let total = fixed + fat + difat;
            let nf = total.div_ceil(eps as u64);
            let nd = nf
                .saturating_sub(HEADER_DIFAT as u64)
                .div_ceil(eps as u64 - 1);
            if nf == fat && nd == difat {
                break;
            }
            (fat, difat) = (nf, nd);
            if fat > u64::from(MAX_REGULAR) {
                return Err(too_large());
            }
        }
        let total = fixed + fat + difat;
        if total >= u64::from(MAX_REGULAR) {
            return Err(too_large());
        }
        let file_len = usize::try_from((total + 1) * ss as u64).map_err(|_| too_large())?;
        // Every count below fits u32, as `total` does.
        let (fat, difat, dir, minifat, ministream) = (
            fat as u32,
            difat as u32,
            dir as u32,
            minifat as u32,
            ministream as u32,
        );
        let difat_at = fat;
        let dir_at = difat_at + difat;
        let minifat_at = dir_at + dir;
        let ministream_at = minifat_at + minifat;
        let big_at = ministream_at + ministream;

        let mut out = vec![0u8; file_len];
        let sector_at = |s: u32| (s as usize + 1) * ss;

        // FAT.
        let mut table = vec![FREESECT; fat as usize * eps];
        let mut set = |i: u32, v: u32| {
            if let Some(x) = table.get_mut(i as usize) {
                *x = v;
            }
        };
        for s in 0..fat {
            set(s, FATSECT);
        }
        for s in difat_at..dir_at {
            set(s, DIFSECT);
        }
        chain(&mut set, dir_at, dir);
        chain(&mut set, minifat_at, minifat);
        chain(&mut set, ministream_at, ministream);
        for (n, place) in order.iter().zip(&places) {
            if let Place::Regular(first) = *place {
                let count = (n.data.len() as u64).div_ceil(ss as u64) as u32;
                chain(&mut set, big_at + first, count);
            }
        }
        for (k, words) in table.chunks(eps).enumerate() {
            put_words(&mut out, sector_at(k as u32), words);
        }

        // DIFAT: the first 109 FAT sectors in the header, the rest in DIFAT
        // sectors, each ending with the next one's number.
        let fat_ids: Vec<u32> = (0..fat).collect();
        let (in_header, rest) = fat_ids.split_at(fat_ids.len().min(HEADER_DIFAT));
        for (j, ids) in rest.chunks(eps - 1).enumerate() {
            let s = difat_at + j as u32;
            let mut words = ids.to_vec();
            words.resize(eps - 1, FREESECT);
            words.push(if s + 1 < dir_at { s + 1 } else { ENDOFCHAIN });
            put_words(&mut out, sector_at(s), &words);
        }

        // Mini FAT and mini stream.
        let mut mini_table = vec![FREESECT; minifat as usize * eps];
        let mut set_mini = |i: u32, v: u32| {
            if let Some(x) = mini_table.get_mut(i as usize) {
                *x = v;
            }
        };
        let ministream_off = sector_at(ministream_at);
        for (n, place) in order.iter().zip(&places) {
            match *place {
                Place::Mini(first) => {
                    let count = (n.data.len() as u64).div_ceil(MINI_LEN as u64) as u32;
                    chain(&mut set_mini, first, count);
                    put(
                        &mut out,
                        ministream_off + first as usize * MINI_LEN,
                        &n.data,
                    );
                }
                Place::Regular(first) => put(&mut out, sector_at(big_at + first), &n.data),
                Place::None => {}
            }
        }
        if minifat > 0 {
            put_words(&mut out, sector_at(minifat_at), &mini_table);
        }

        // Directory.
        let dir_off = sector_at(dir_at);
        let mut links = vec![(NOSTREAM, NOSTREAM, NOSTREAM, BLACK); order.len()];
        for (i, kids) in sorted.iter().enumerate() {
            if kids.is_empty() {
                continue;
            }
            let kid_ids: Vec<u32> = kids.iter().filter_map(|&c| ids.get(c).copied()).collect();
            let top = balanced(&kid_ids, &mut links);
            if let Some(l) = ids.get(i).and_then(|&id| links.get_mut(id as usize)) {
                l.2 = top;
            }
        }
        for (id, (n, place)) in order.iter().zip(&places).enumerate() {
            let (start, size) = match (n.kind, *place) {
                (EntryKind::Root, _) if mini_bytes > 0 => (ministream_at, mini_bytes),
                (EntryKind::Root, _) => (ENDOFCHAIN, 0),
                (EntryKind::Storage, _) => n.stored,
                (_, Place::Mini(s)) => (s, n.data.len() as u64),
                (_, Place::Regular(s)) => (big_at + s, n.data.len() as u64),
                (_, Place::None) => (ENDOFCHAIN, 0),
            };
            // A name without its null (kept from a copy) may use all 32.
            if n.units.len() > MAX_NAME_UNITS + usize::from(!n.terminated) {
                return Err(CfbError::InvalidName {
                    name: String::from_utf16_lossy(&n.units),
                    why: "it is longer than 31 UTF-16 code units, the most a directory entry holds",
                });
            }
            let (left, right, child, color) = links
                .get(id)
                .copied()
                .unwrap_or((NOSTREAM, NOSTREAM, NOSTREAM, BLACK));
            let mut e = [0u8; ENTRY_LEN];
            for (k, u) in n.units.iter().enumerate() {
                e[2 * k..2 * k + 2].copy_from_slice(&u.to_le_bytes());
            }
            let name_len = if n.units.is_empty() {
                0
            } else {
                (n.units.len() as u16 + u16::from(n.terminated)) * 2
            };
            e[64..66].copy_from_slice(&name_len.to_le_bytes());
            e[66] = n.kind.byte();
            e[67] = color;
            e[68..72].copy_from_slice(&left.to_le_bytes());
            e[72..76].copy_from_slice(&right.to_le_bytes());
            e[76..80].copy_from_slice(&child.to_le_bytes());
            e[80..96].copy_from_slice(&n.meta.clsid);
            e[96..100].copy_from_slice(&n.meta.state_bits.to_le_bytes());
            e[100..108].copy_from_slice(&n.meta.created.to_le_bytes());
            e[108..116].copy_from_slice(&n.meta.modified.to_le_bytes());
            e[116..120].copy_from_slice(&start.to_le_bytes());
            e[120..128].copy_from_slice(&size.to_le_bytes());
            put(&mut out, dir_off + id * ENTRY_LEN, &e);
        }
        // Unused slots at the end of the last directory sector.
        let mut unused = [0u8; ENTRY_LEN];
        unused[68..80].fill(0xFF);
        let slots = dir as usize * ss / ENTRY_LEN;
        for id in order.len()..slots {
            put(&mut out, dir_off + id * ENTRY_LEN, &unused);
        }

        // Header.
        let mut h = [0u8; 512];
        h[..8].copy_from_slice(&SIGNATURE);
        h[8..24].copy_from_slice(&self.header.clsid);
        h[24..26].copy_from_slice(&self.header.minor_version.to_le_bytes());
        h[26..28].copy_from_slice(&self.version.number().to_le_bytes());
        h[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
        h[30..32].copy_from_slice(&self.version.shift().to_le_bytes());
        h[32..34].copy_from_slice(&6u16.to_le_bytes());
        h[34..40].copy_from_slice(&self.header.reserved);
        let dir_count = if self.version == Version::V4 { dir } else { 0 };
        h[40..44].copy_from_slice(&dir_count.to_le_bytes());
        h[44..48].copy_from_slice(&fat.to_le_bytes());
        h[48..52].copy_from_slice(&dir_at.to_le_bytes());
        h[52..56].copy_from_slice(&self.header.transaction.to_le_bytes());
        h[56..60].copy_from_slice(&(MINI_STREAM_CUTOFF as u32).to_le_bytes());
        let first_minifat = if minifat > 0 { minifat_at } else { ENDOFCHAIN };
        h[60..64].copy_from_slice(&first_minifat.to_le_bytes());
        h[64..68].copy_from_slice(&minifat.to_le_bytes());
        let first_difat = if difat > 0 { difat_at } else { ENDOFCHAIN };
        h[68..72].copy_from_slice(&first_difat.to_le_bytes());
        h[72..76].copy_from_slice(&difat.to_le_bytes());
        let mut header_difat = in_header.to_vec();
        header_difat.resize(HEADER_DIFAT, FREESECT);
        for (k, v) in header_difat.iter().enumerate() {
            h[76 + 4 * k..80 + 4 * k].copy_from_slice(&v.to_le_bytes());
        }
        put(&mut out, 0, &h);
        Ok(out)
    }
}

/// Links `count` sectors from `start` into one chain.
fn chain(set: &mut dyn FnMut(u32, u32), start: u32, count: u32) {
    for i in 0..count {
        let next = if i + 1 < count {
            start + i + 1
        } else {
            ENDOFCHAIN
        };
        set(start + i, next);
    }
}

/// Copies `bytes` into `out` at `at`; never writes past the end.
fn put(out: &mut [u8], at: usize, bytes: &[u8]) {
    if let Some(dst) = out.get_mut(at..at.saturating_add(bytes.len())) {
        dst.copy_from_slice(bytes);
    }
}

fn put_words(out: &mut [u8], at: usize, words: &[u32]) {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    put(out, at, &bytes);
}

/// Builds a balanced binary tree over sorted entries and colors it as a
/// valid red-black tree: every level is full except perhaps the last,
/// whose nodes are red, so every path has the same number of black nodes.
/// Returns the tree's top entry.
fn balanced(sorted: &[u32], links: &mut [(u32, u32, u32, u8)]) -> u32 {
    let n = sorted.len();
    let deepest = n.checked_ilog2().unwrap_or(0);
    let last_level_full = (n + 1).is_power_of_two();
    let mut stack = vec![(sorted, 0u32, None::<(usize, bool)>)];
    let mut top = NOSTREAM;
    // Iterative, so a storage with many entries cannot overflow the stack.
    while let Some((part, d, parent)) = stack.pop() {
        let mid = part.len() / 2;
        let Some(&id) = part.get(mid) else { continue };
        let color = if d == deepest && !last_level_full {
            RED
        } else {
            BLACK
        };
        if let Some(l) = links.get_mut(id as usize) {
            l.0 = NOSTREAM;
            l.1 = NOSTREAM;
            l.3 = color;
        }
        match parent {
            None => top = id,
            Some((p, is_left)) => {
                if let Some(l) = links.get_mut(p) {
                    if is_left {
                        l.0 = id;
                    } else {
                        l.1 = id;
                    }
                }
            }
        }
        let (left, right) = part.split_at(mid);
        let right = right.get(1..).unwrap_or(&[]);
        stack.push((left, d + 1, Some((id as usize, true))));
        stack.push((right, d + 1, Some((id as usize, false))));
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks the red-black rules on a tree made by `balanced`: no red
    /// node has a red child, and every path down has as many black nodes.
    fn black_height(links: &[(u32, u32, u32, u8)], id: u32, parent_red: bool) -> u32 {
        if id == NOSTREAM {
            return 1;
        }
        let (l, r, _, color) = links[id as usize];
        let red = color == RED;
        assert!(!(red && parent_red), "red node {id} under a red node");
        let hl = black_height(links, l, red);
        let hr = black_height(links, r, red);
        assert_eq!(hl, hr, "black heights differ under {id}");
        hl + u32::from(!red)
    }

    #[test]
    fn balanced_trees_are_red_black_trees() {
        for n in 0..300u32 {
            let ids: Vec<u32> = (0..n).collect();
            let mut links = vec![(NOSTREAM, NOSTREAM, NOSTREAM, BLACK); n as usize];
            let top = balanced(&ids, &mut links);
            if n == 0 {
                assert_eq!(top, NOSTREAM);
                continue;
            }
            // The top of the tree may be red only if it is alone.
            black_height(&links, top, false);
            // In order, the tree gives the sorted entries back.
            let mut seen = Vec::new();
            let mut stack = Vec::new();
            let mut cur = top;
            loop {
                while cur != NOSTREAM {
                    stack.push(cur);
                    cur = links[cur as usize].0;
                }
                let Some(id) = stack.pop() else { break };
                seen.push(id);
                cur = links[id as usize].1;
            }
            assert_eq!(seen, ids, "n = {n}");
        }
    }
}
