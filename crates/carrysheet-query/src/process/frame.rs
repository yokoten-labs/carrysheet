//! What the parent and the worker send each other over the worker's stdin
//! and stdout: frames of a 4-byte little-endian length and a body.
//!
//! Requests (parent to worker) are binary, because the sheets can be
//! large: a load with the limits and every cell, then queries. Replies
//! (worker to parent) are a tag byte and JSON: the loaded tables, a page of
//! results, or an error.

use std::io::{self, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use carrysheet_model::{CellValue, ErrorKind};

use crate::error::QueryError;
use crate::input::{Header, QueryCell, TableInfo, TableInput};
use crate::output::{Limits, Page, QueryResult};

/// The largest request the worker accepts.
pub(crate) const MAX_FRAME: usize = 1 << 30;

/// JSON can write one byte of text as six (`\u0000`).
const JSON_GROWTH: usize = 6;
/// Room in a reply for what the limits do not count: notes, an error's
/// text, the JSON around the values.
const REPLY_SLACK: usize = 1 << 20;
/// What one column in a reply can take besides its values: a name of at
/// most 128 characters, escaped, and its type.
const COLUMN_BYTES: usize = 1024;
/// The longest JSON number or keyword a cell can be (`-1.2345678901234567e-308`).
const CELL_BYTES: usize = 32;
/// Columns a result may have (`MAX_COLUMNS` in the engine).
const RESULT_COLUMNS: usize = 2000;

/// The largest reply to a query the parent reads: a page of at most
/// `max_page_bytes` of text plus one more row (a row is at most
/// `max_value_bytes`), every byte escaped; `max_page_cells` plus one row of
/// numbers; column names, which come from the query text or from 128-character
/// headers; and slack. About 55 MiB with the default limits.
pub(crate) fn reply_cap(limits: &Limits) -> usize {
    let text = limits
        .max_page_bytes
        .saturating_add(limits.max_value_bytes)
        .saturating_add(limits.max_sql_bytes)
        .saturating_mul(JSON_GROWTH);
    let cells = limits
        .max_page_cells
        .saturating_add(RESULT_COLUMNS)
        .saturating_mul(CELL_BYTES);
    text.saturating_add(cells)
        .saturating_add(RESULT_COLUMNS * COLUMN_BYTES)
        .saturating_add(REPLY_SLACK)
        .min(MAX_FRAME)
}

/// The largest reply to a load the parent reads: the tables and their
/// columns, each column at most [`COLUMN_BYTES`].
pub(crate) fn loaded_cap(inputs: &[TableInput]) -> usize {
    inputs
        .iter()
        .map(|t| t.width().saturating_add(1))
        .fold(0usize, usize::saturating_add)
        .saturating_mul(COLUMN_BYTES)
        .saturating_add(REPLY_SLACK)
        .min(MAX_FRAME)
}

const LOAD: u8 = 1;
const QUERY: u8 = 2;
const LOADED: u8 = 1;
const RESULT: u8 = 2;
const ERROR: u8 = 3;

/// Writes one frame and flushes it.
pub(crate) fn write(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&n| n as usize <= MAX_FRAME)
        .ok_or_else(|| invalid("frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// Reads one frame of at most `max` bytes; `None` when the other side
/// closed the pipe between frames.
pub(crate) fn read(r: &mut impl Read, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..])? {
            0 if got == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > max {
        return Err(invalid("frame too large"));
    }
    // Grows as bytes arrive, so a bad length cannot allocate a gigabyte.
    let mut body = Vec::new();
    r.take(len as u64).read_to_end(&mut body)?;
    if body.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(body))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// A request to the worker.
#[derive(Debug)]
pub(crate) enum Request {
    Load(Limits, Vec<TableInput>),
    Query(String, Page),
}

/// The worker's reply to a load.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Loaded {
    pub(crate) tables: Vec<TableInfo>,
    /// What the loaded data takes in the worker's SQLite, measured.
    pub(crate) bytes: u64,
}

/// A reply from the worker.
#[derive(Debug)]
pub(crate) enum Reply {
    Loaded(Loaded),
    Result(QueryResult),
    Error(QueryError),
}

pub(crate) fn encode_load(limits: &Limits, inputs: &[TableInput]) -> Vec<u8> {
    let mut out = Out(vec![LOAD]);
    for n in [
        limits.max_input_cells,
        limits.max_steps,
        limits.max_memory,
        limits.memory_budget,
        nanos(limits.max_time),
        nanos(limits.max_load_time),
        limits.max_sql_bytes as u64,
        limits.max_value_bytes as u64,
        limits.max_page_rows as u64,
        limits.max_page_cells as u64,
        limits.max_page_bytes as u64,
    ] {
        out.u64(n);
    }
    out.u64(inputs.len() as u64);
    for input in inputs {
        out.str(&input.name);
        match &input.header {
            Header::FirstRow => out.u8(0),
            Header::Letters => out.u8(1),
            Header::Names(names) => {
                out.u8(2);
                out.u64(names.len() as u64);
                for name in names {
                    out.str(name);
                }
            }
        }
        out.u64(u64::from(input.first_column));
        out.u64(u64::from(input.first_row));
        out.u64(input.rows.len() as u64);
        for row in &input.rows {
            out.u64(row.len() as u64);
            for cell in row {
                match cell {
                    QueryCell::Value(CellValue::Empty) => out.u8(0),
                    QueryCell::Value(CellValue::Number(n)) => {
                        out.u8(1);
                        out.0.extend_from_slice(&n.to_le_bytes());
                    }
                    QueryCell::Value(CellValue::Text(s)) => {
                        out.u8(2);
                        out.str(s);
                    }
                    QueryCell::Value(CellValue::Bool(b)) => {
                        out.u8(3);
                        out.u8(u8::from(*b));
                    }
                    QueryCell::Value(CellValue::Error(e)) => {
                        out.u8(4);
                        let i = ErrorKind::ALL.iter().position(|k| k == e).unwrap_or(0);
                        out.u8(i as u8);
                    }
                    QueryCell::Date(d) => {
                        out.u8(5);
                        out.str(d);
                    }
                }
            }
        }
    }
    out.0
}

pub(crate) fn encode_query(sql: &str, page: Page) -> Vec<u8> {
    let mut out = Out(vec![QUERY]);
    out.str(sql);
    out.u64(page.offset);
    out.u64(page.max_rows as u64);
    out.0
}

pub(crate) fn decode_request(body: &[u8]) -> io::Result<Request> {
    let mut r = In(body);
    match r.u8()? {
        LOAD => {
            let mut n = [0u64; 11];
            for v in &mut n {
                *v = r.u64()?;
            }
            let limits = Limits {
                max_input_cells: n[0],
                max_steps: n[1],
                max_memory: n[2],
                memory_budget: n[3],
                max_time: Duration::from_nanos(n[4]),
                max_load_time: Duration::from_nanos(n[5]),
                max_sql_bytes: size(n[6]),
                max_value_bytes: size(n[7]),
                max_page_rows: size(n[8]),
                max_page_cells: size(n[9]),
                max_page_bytes: size(n[10]),
            };
            let tables = r.count()?;
            let mut inputs = Vec::with_capacity(tables);
            for _ in 0..tables {
                let name = r.str()?;
                let header = match r.u8()? {
                    0 => Header::FirstRow,
                    1 => Header::Letters,
                    2 => {
                        let count = r.count()?;
                        let mut names = Vec::with_capacity(count);
                        for _ in 0..count {
                            names.push(r.str()?);
                        }
                        Header::Names(names)
                    }
                    _ => return Err(invalid("bad header")),
                };
                let first_column = u32::try_from(r.u64()?).map_err(|_| invalid("bad column"))?;
                let first_row = u32::try_from(r.u64()?).map_err(|_| invalid("bad row"))?;
                let count = r.count()?;
                let mut rows = Vec::with_capacity(count);
                for _ in 0..count {
                    let width = r.count()?;
                    let mut row = Vec::with_capacity(width);
                    for _ in 0..width {
                        row.push(match r.u8()? {
                            0 => QueryCell::Value(CellValue::Empty),
                            1 => QueryCell::Value(CellValue::Number(f64::from_le_bytes(
                                r.bytes(8)?.try_into().map_err(|_| invalid("bad number"))?,
                            ))),
                            2 => QueryCell::Value(CellValue::Text(r.str()?)),
                            3 => QueryCell::Value(CellValue::Bool(r.u8()? != 0)),
                            4 => QueryCell::Value(CellValue::Error(
                                *ErrorKind::ALL
                                    .get(r.u8()? as usize)
                                    .ok_or_else(|| invalid("bad error value"))?,
                            )),
                            5 => QueryCell::Date(r.str()?),
                            _ => return Err(invalid("bad cell")),
                        });
                    }
                    rows.push(row);
                }
                inputs.push(TableInput {
                    name,
                    header,
                    first_column,
                    first_row,
                    rows,
                });
            }
            Ok(Request::Load(limits, inputs))
        }
        QUERY => {
            let sql = r.str()?;
            let offset = r.u64()?;
            let max_rows = size(r.u64()?);
            Ok(Request::Query(sql, Page { offset, max_rows }))
        }
        _ => Err(invalid("unknown request")),
    }
}

pub(crate) fn encode_reply(reply: &Reply) -> Vec<u8> {
    let (tag, json) = match reply {
        Reply::Loaded(l) => (LOADED, serde_json::to_vec(l)),
        Reply::Result(r) => (RESULT, serde_json::to_vec(r)),
        Reply::Error(e) => (ERROR, serde_json::to_vec(e)),
    };
    let mut out = vec![tag];
    // These types always serialize; an empty body decodes as an error.
    out.extend(json.unwrap_or_default());
    out
}

pub(crate) fn decode_reply(body: &[u8]) -> io::Result<Reply> {
    let (&tag, json) = body.split_first().ok_or_else(|| invalid("empty reply"))?;
    let bad = |e: serde_json::Error| invalid(&e.to_string());
    match tag {
        LOADED => serde_json::from_slice(json).map(Reply::Loaded).map_err(bad),
        RESULT => serde_json::from_slice(json).map(Reply::Result).map_err(bad),
        ERROR => serde_json::from_slice(json).map(Reply::Error).map_err(bad),
        _ => Err(invalid("unknown reply")),
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn size(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, n: u8) {
        self.0.push(n);
    }

    fn u64(&mut self, n: u64) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }

    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct In<'a>(&'a [u8]);

impl In<'_> {
    fn bytes(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn u64(&mut self) -> io::Result<u64> {
        let b = self.bytes(8)?;
        Ok(u64::from_le_bytes(
            b.try_into().map_err(|_| invalid("short"))?,
        ))
    }

    /// A count of things that follow, each at least one byte: never more
    /// than the bytes left, so a bad count cannot allocate much.
    fn count(&mut self) -> io::Result<usize> {
        let n = size(self.u64()?);
        if n > self.0.len() {
            return Err(invalid("bad count"));
        }
        Ok(n)
    }

    fn str(&mut self) -> io::Result<String> {
        let n = size(self.u64()?);
        let b = self.bytes(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| invalid("bad text"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let limits = Limits {
            max_time: Duration::from_millis(1500),
            ..Limits::default()
        };
        let inputs = vec![
            TableInput::new(
                "Sales \"Q1\" 地",
                vec![
                    vec!["a".into(), "b".into()],
                    vec![
                        1.5.into(),
                        true.into(),
                        QueryCell::Value(CellValue::Error(ErrorKind::NA)),
                        QueryCell::Date("2024-03-01".into()),
                        QueryCell::default(),
                    ],
                    vec![],
                ],
            )
            .with_first_column(3)
            .with_first_row(7),
            TableInput::new("T", vec![]).with_header(Header::Names(vec!["x".into(), "".into()])),
            TableInput::new("L", vec![vec![f64::NAN.into()]]).with_header(Header::Letters),
        ];
        let body = encode_load(&limits, &inputs);
        let Request::Load(l, i) = decode_request(&body).unwrap() else {
            panic!("not a load")
        };
        assert_eq!(l, limits);
        assert_eq!(i[..2], inputs[..2]);
        assert!(matches!(i[2].rows[0][0], QueryCell::Value(CellValue::Number(n)) if n.is_nan()));

        let page = Page {
            offset: 9,
            max_rows: 3,
        };
        let Request::Query(sql, p) = decode_request(&encode_query("SELECT 'é'", page)).unwrap()
        else {
            panic!("not a query")
        };
        assert_eq!((sql.as_str(), p), ("SELECT 'é'", page));
    }

    #[test]
    fn damaged_requests_are_errors() {
        let body = encode_load(
            &Limits::default(),
            &[TableInput::new("T", vec![vec!["a".into()]])],
        );
        for cut in 0..body.len() {
            assert!(decode_request(&body[..cut]).is_err(), "cut at {cut}");
        }
        let mut huge = vec![LOAD];
        huge.extend([0u8; 88]);
        huge.extend(u64::MAX.to_le_bytes());
        assert!(decode_request(&huge).is_err());
    }

    #[test]
    fn frames_round_trip() {
        let mut pipe = Vec::new();
        write(&mut pipe, b"hello").unwrap();
        write(&mut pipe, b"").unwrap();
        let mut r = pipe.as_slice();
        assert_eq!(read(&mut r, MAX_FRAME).unwrap().unwrap(), b"hello");
        assert_eq!(read(&mut r, MAX_FRAME).unwrap().unwrap(), b"");
        assert!(read(&mut r, MAX_FRAME).unwrap().is_none());
        let mut short: &[u8] = &[5, 0, 0, 0, b'a'];
        assert!(read(&mut short, MAX_FRAME).is_err());
        // Over the cap: refused from the length alone.
        let mut long: &[u8] = &[6, 0, 0, 0];
        assert!(read(&mut long, 5).is_err());
        let mut fits: &[u8] = &[1, 0, 0, 0, b'a'];
        assert_eq!(read(&mut fits, 1).unwrap().unwrap(), b"a");
    }
}
