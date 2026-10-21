//! Builds small Numbers documents byte by byte, valid or hostile: a tiny
//! protobuf writer, IWA chunks, Snappy literals and zip files.

#![allow(dead_code)]

use std::io::{Cursor, Write};

use carrysheet_numbers::{Document, NumbersError};
use zip::write::SimpleFileOptions;

// --- A tiny protobuf writer -------------------------------------------------

pub fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[derive(Clone, Default)]
pub struct Msg(pub Vec<u8>);

impl Msg {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn uint(mut self, field: u64, v: u64) -> Self {
        varint(field << 3, &mut self.0);
        varint(v, &mut self.0);
        self
    }
    pub fn bytes(mut self, field: u64, b: &[u8]) -> Self {
        varint(field << 3 | 2, &mut self.0);
        varint(b.len() as u64, &mut self.0);
        self.0.extend_from_slice(b);
        self
    }
    pub fn msg(self, field: u64, m: Msg) -> Self {
        self.bytes(field, &m.0)
    }
    pub fn fixed32(mut self, field: u64, v: u32) -> Self {
        varint(field << 3 | 5, &mut self.0);
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
}

pub fn reference(id: u64) -> Msg {
    Msg::new().uint(1, id)
}

/// One archive: header (`TSP.ArchiveInfo`) and one object.
pub fn archive(id: u64, kind: u64, object: &Msg) -> Vec<u8> {
    let info = Msg::new()
        .uint(1, id)
        .msg(2, Msg::new().uint(1, kind).uint(3, object.0.len() as u64));
    let mut out = Vec::new();
    varint(info.0.len() as u64, &mut out);
    out.extend_from_slice(&info.0);
    out.extend_from_slice(&object.0);
    out
}

/// A raw Snappy block made only of literals.
pub fn snappy(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(data.len() as u64, &mut out);
    for chunk in data.chunks(60_000) {
        let n = chunk.len() - 1;
        if n < 60 {
            out.push((n as u8) << 2);
        } else {
            out.push(61 << 2);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        out.extend_from_slice(chunk);
    }
    out
}

/// An `.iwa` part: one chunk for each 8 MiB of payload (a chunk holds at
/// most 16 MiB).
pub fn iwa(payload: &[u8]) -> Vec<u8> {
    if payload.is_empty() {
        return chunk(&snappy(payload));
    }
    payload
        .chunks(8 << 20)
        .flat_map(|part| chunk(&snappy(part)))
        .collect()
}

pub fn chunk(block: &[u8]) -> Vec<u8> {
    let len = block.len() as u32;
    let mut out = vec![0, len as u8, (len >> 8) as u8, (len >> 16) as u8];
    out.extend_from_slice(block);
    out
}

pub fn zip(entries: &[(&str, Vec<u8>)], method: zip::CompressionMethod) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        w.start_file(*name, options).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

pub fn stored(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    zip(entries, zip::CompressionMethod::Stored)
}

// --- A small valid document -------------------------------------------------

/// A cell storage buffer: version 5, a type, flags, then the fields.
pub fn cell(kind: u8, flags: u32, fields: &[u8]) -> Vec<u8> {
    let mut b = vec![5, kind, 0, 0, 0, 0, 0, 0];
    b.extend_from_slice(&flags.to_le_bytes());
    b.extend_from_slice(fields);
    b
}

pub fn text_cell(key: u32) -> Vec<u8> {
    cell(3, 0x8, &key.to_le_bytes())
}

pub fn formula_cell(key: u32) -> Vec<u8> {
    let mut fields = 7.0f64.to_le_bytes().to_vec();
    fields.extend_from_slice(&key.to_le_bytes());
    // A duration result (double) with a formula.
    cell(7, 0x2 | 0x200, &fields)
}

/// A row of cells: offsets and buffer.
pub fn row(index: u64, cells: &[Option<Vec<u8>>]) -> Msg {
    let mut buffer = Vec::new();
    let mut offsets = Vec::new();
    for c in cells {
        match c {
            Some(c) => {
                offsets.extend_from_slice(&(buffer.len() as i16).to_le_bytes());
                buffer.extend_from_slice(c);
            }
            None => offsets.extend_from_slice(&(-1i16).to_le_bytes()),
        }
    }
    Msg::new()
        .uint(1, index)
        .uint(2, cells.len() as u64)
        .bytes(3, b"")
        .bytes(4, b"")
        .bytes(6, &buffer)
        .bytes(7, &offsets)
}

pub fn ast_node(kind: u64) -> Msg {
    Msg::new().uint(1, kind)
}

/// `TSCE.FormulaArchive` with the given nodes.
pub fn formula(nodes: &[Msg]) -> Msg {
    let mut array = Msg::new();
    for n in nodes {
        array = array.msg(1, n.clone());
    }
    Msg::new().msg(1, array)
}

pub struct Doc {
    pub rows: u64,
    pub columns: u64,
    pub rows_msgs: Vec<Msg>,
    pub strings: Vec<(u64, &'static str)>,
    pub formulas: Vec<(u64, Msg)>,
    pub merges: Vec<(u32, u32)>,
    pub table_model_ref: u64,
    pub tile_repeats: usize,
    pub string_list_extra: Option<Msg>,
    /// Fields added to the table model (and to every extra table's model).
    pub model_extra: Option<Msg>,
    /// More tables on the sheet, each with its own model that shares the
    /// first table's tile and lists.
    pub extra_tables: usize,
    /// More objects: (id, type, message).
    pub extra_objects: Vec<(u64, u64, Msg)>,
}

impl Default for Doc {
    fn default() -> Self {
        Self {
            rows: 2,
            columns: 2,
            rows_msgs: vec![row(0, &[Some(text_cell(1)), Some(formula_cell(1))])],
            strings: vec![(1, "hi")],
            formulas: vec![(
                1,
                formula(&[ast_node(17).uint(43, 0x3040_0000_0000_0000).uint(42, 7)]),
            )],
            merges: Vec::new(),
            table_model_ref: 4,
            tile_repeats: 1,
            string_list_extra: None,
            model_extra: None,
            extra_tables: 0,
            extra_objects: Vec::new(),
        }
    }
}

impl Doc {
    pub fn objects(&self) -> Vec<Vec<u8>> {
        let document = Msg::new().msg(1, reference(2));
        let sheet = Msg::new().bytes(1, b"Sheet").msg(2, reference(3));
        let info = Msg::new()
            .msg(1, Msg::new().msg(2, reference(2)))
            .msg(2, reference(self.table_model_ref));
        let mut tiles = Msg::new();
        for _ in 0..self.tile_repeats {
            tiles = tiles.msg(1, Msg::new().uint(1, 0).msg(2, reference(5)));
        }
        tiles = tiles.uint(2, 256);
        let mut store = Msg::new()
            .msg(3, tiles)
            .msg(4, reference(6))
            .msg(6, reference(7));
        if !self.merges.is_empty() {
            store = store.msg(13, reference(8));
        }
        let mut model = Msg::new()
            .bytes(1, b"id")
            .msg(4, store)
            .uint(6, self.rows)
            .uint(7, self.columns)
            .bytes(8, b"Table");
        if let Some(extra) = &self.model_extra {
            model.0.extend_from_slice(&extra.0);
        }
        let mut tile = Msg::new()
            .uint(1, 0)
            .uint(2, 0)
            .uint(3, 0)
            .uint(4, 1)
            .uint(7, 1);
        for r in &self.rows_msgs {
            tile = tile.msg(5, r.clone());
        }
        let mut strings = Msg::new().uint(1, 1).uint(2, 10);
        for (key, s) in &self.strings {
            strings = strings.msg(
                3,
                Msg::new().uint(1, *key).uint(2, 1).bytes(3, s.as_bytes()),
            );
        }
        if let Some(extra) = &self.string_list_extra {
            strings.0.extend_from_slice(&extra.0);
        }
        let mut formulas = Msg::new().uint(1, 3).uint(2, 10);
        for (key, f) in &self.formulas {
            formulas = formulas.msg(3, Msg::new().uint(1, *key).uint(2, 1).msg(5, f.clone()));
        }
        let mut merges = Msg::new();
        for &(origin, size) in &self.merges {
            merges = merges.msg(
                1,
                Msg::new()
                    .msg(1, Msg::new().fixed32(1, origin))
                    .msg(2, Msg::new().fixed32(1, size)),
            );
        }
        let mut objects = vec![
            archive(1, 1, &document),
            archive(2, 2, &sheet),
            archive(3, 6000, &info),
            archive(4, 6001, &model),
            archive(5, 6002, &tile),
            archive(6, 6005, &strings),
            archive(7, 6005, &formulas),
            archive(8, 6144, &merges),
        ];
        for i in 0..self.extra_tables as u64 {
            let info = Msg::new()
                .msg(1, Msg::new().msg(2, reference(2)))
                .msg(2, reference(1_000_000 + i));
            objects.push(archive(100 + i, 6000, &info));
            objects.push(archive(1_000_000 + i, 6001, &model));
        }
        for (id, kind, m) in &self.extra_objects {
            objects.push(archive(*id, *kind, m));
        }
        objects
    }

    pub fn bytes(&self) -> Vec<u8> {
        document(&self.objects())
    }
}

/// `Metadata/Properties.plist` of a document saved by Numbers 14.3.
pub const PROPERTIES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>fileFormatVersion</key><string>14.2.2</string>
</dict></plist>"#;

/// A zip document with these objects and a version in its metadata.
pub fn document(objects: &[Vec<u8>]) -> Vec<u8> {
    stored(&[
        ("Index/Document.iwa", iwa(&objects.concat())),
        ("Metadata/Properties.plist", PROPERTIES.as_bytes().to_vec()),
    ])
}

pub fn read(bytes: &[u8]) -> Result<Document, NumbersError> {
    Document::from_bytes(bytes)
}
