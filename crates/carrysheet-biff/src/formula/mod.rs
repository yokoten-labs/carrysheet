//! Turns parsed-expression tokens (Ptg, [MS-XLS] 2.5.198) into en-US A1
//! formula text.
//!
//! BIFF8 and BIFF5 store references differently: BIFF8 keeps the relative
//! flags in the column word and uses 16-bit columns, while BIFF2 to BIFF5 keep
//! them in the top bits of the row word, use 8-bit columns, and spell 3D
//! references with an EXTERNSHEET index plus sheet numbers.

mod functions;

use encoding_rs::Encoding;

use crate::codepage;
use crate::model::{BiffVersion, CellError, FormulaText, column_name};

/// An XTI entry of the BIFF8 EXTERNSHEET record.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Xti {
    pub supbook: u16,
    pub first: i16,
    pub last: i16,
}

/// A BIFF8 SUPBOOK record and the EXTERNNAME records after it.
#[derive(Debug, Clone)]
pub(crate) struct SupBook {
    pub kind: SupBookKind,
    pub names: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum SupBookKind {
    /// This workbook.
    Internal,
    /// Add-in functions.
    AddIn,
    /// Another workbook, by file name, with its sheet names.
    External { book: String, sheets: Vec<String> },
    /// DDE or OLE links.
    Other,
}

/// A BIFF2 to BIFF5 EXTERNSHEET record.
#[derive(Debug, Clone)]
pub(crate) enum ExternSheet {
    /// A sheet of this workbook.
    Own(String),
    /// This workbook as a whole.
    OwnBook,
    /// A sheet of another workbook.
    External {
        book: String,
        sheet: String,
    },
    Unknown,
}

/// What the renderer needs to know about the workbook.
pub(crate) struct Env<'a> {
    pub version: BiffVersion,
    pub sheet_names: &'a [String],
    /// Display text of each NAME record (PtgName index - 1).
    pub names: &'a [String],
    pub xti: &'a [Xti],
    pub supbooks: &'a [SupBook],
    pub extern_sheets: &'a [ExternSheet],
    /// BIFF5 EXTERNNAME records, grouped by the EXTERNSHEET they follow.
    pub extern_names: &'a [Vec<String>],
    pub enc: &'static Encoding,
    pub max_text: usize,
}

struct Fail {
    reason: String,
    at: usize,
}

type R<T> = Result<T, Fail>;

struct Toks<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Toks<'a> {
    fn fail(&self, reason: impl Into<String>) -> Fail {
        Fail {
            reason: reason.into(),
            at: self.pos,
        }
    }
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if self.data.len() - self.pos < n {
            return Err(self.fail(format!(
                "a token needs {n} more bytes but only {} remain",
                self.data.len() - self.pos
            )));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> R<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> R<i16> {
        Ok(self.u16()? as i16)
    }
    fn f64(&mut self) -> R<f64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(f64::from_le_bytes(a))
    }
}

/// A cell reference as stored: position plus relative flags.
#[derive(Clone, Copy)]
struct Loc {
    row: u32,
    col: u16,
    row_rel: bool,
    col_rel: bool,
}

/// Renders `tokens` (with `extra` holding array constants) for a formula in
/// cell `base` (row, col). `base` matters only for the relative tokens of
/// shared formulas (PtgRefN, PtgAreaN).
pub(crate) fn render(tokens: &[u8], extra: &[u8], base: (u32, u16), env: &Env<'_>) -> FormulaText {
    text(render_inner(tokens, extra, base, env, false))
}

/// [`render`] for a shared formula (SHRFMLA): there, PtgRef3d and PtgArea3d
/// hold offsets from the cell too ([MS-XLS] RgceLocRel), like PtgRefN.
pub(crate) fn render_shared(
    tokens: &[u8],
    extra: &[u8],
    base: (u32, u16),
    env: &Env<'_>,
) -> FormulaText {
    text(render_inner(tokens, extra, base, env, true))
}

fn text(r: R<String>) -> FormulaText {
    match r {
        Ok(s) => FormulaText::Parsed(s),
        Err(f) => FormulaText::Unparsed {
            reason: f.reason,
            at: f.at,
        },
    }
}

fn is_biff8(env: &Env<'_>) -> bool {
    env.version == BiffVersion::Biff8
}

fn render_inner(
    tokens: &[u8],
    extra: &[u8],
    base: (u32, u16),
    env: &Env<'_>,
    shared: bool,
) -> R<String> {
    let mut t = Toks {
        data: tokens,
        pos: 0,
    };
    let mut x = Toks {
        data: extra,
        pos: 0,
    };
    let mut stack = Stack::default();
    let v8 = is_biff8(env);
    let old = matches!(
        env.version,
        BiffVersion::Biff2 | BiffVersion::Biff3 | BiffVersion::Biff4
    );
    while t.pos < t.data.len() {
        let start = t.pos;
        let ptg = t.u8()?;
        let fail_at = |reason: String| Fail { reason, at: start };
        match ptg {
            0x01 | 0x02 => {
                return Err(fail_at(
                    "a shared, array or table formula reference (PtgExp/PtgTbl) inside a formula"
                        .into(),
                ));
            }
            0x03..=0x11 => {
                let op = match ptg {
                    0x03 => "+",
                    0x04 => "-",
                    0x05 => "*",
                    0x06 => "/",
                    0x07 => "^",
                    0x08 => "&",
                    0x09 => "<",
                    0x0A => "<=",
                    0x0B => "=",
                    0x0C => ">=",
                    0x0D => ">",
                    0x0E => "<>",
                    0x0F => " ",
                    0x10 => ",",
                    _ => ":",
                };
                let b = pop(&mut stack, start)?;
                let a = pop(&mut stack, start)?;
                push(&mut stack, format!("{a}{op}{b}"), env, start)?;
            }
            0x12 => {
                let a = pop(&mut stack, start)?;
                push(&mut stack, format!("+{a}"), env, start)?;
            }
            0x13 => {
                let a = pop(&mut stack, start)?;
                push(&mut stack, format!("-{a}"), env, start)?;
            }
            0x14 => {
                let a = pop(&mut stack, start)?;
                push(&mut stack, format!("{a}%"), env, start)?;
            }
            0x15 => {
                let a = pop(&mut stack, start)?;
                push(&mut stack, format!("({a})"), env, start)?;
            }
            0x16 => push(&mut stack, String::new(), env, start)?,
            0x17 => {
                let s = read_short_string(&mut t, env)?;
                push(&mut stack, quote_string(&s), env, start)?;
            }
            0x19 => attr(&mut t, &mut stack, env, start)?,
            0x1C => {
                let e = t.u8()?;
                push(
                    &mut stack,
                    CellError::from_code(e).as_str().to_string(),
                    env,
                    start,
                )?;
            }
            0x1D => {
                let b = t.u8()?;
                push(
                    &mut stack,
                    if b != 0 { "TRUE" } else { "FALSE" }.into(),
                    env,
                    start,
                )?;
            }
            0x1E => {
                let n = t.u16()?;
                push(&mut stack, n.to_string(), env, start)?;
            }
            0x1F => {
                let n = t.f64()?;
                push(&mut stack, format_number(n), env, start)?;
            }
            0x20..=0x7F => {
                let base_ptg = (ptg & 0x1F) | 0x20;
                match base_ptg {
                    0x20 => {
                        if old {
                            return Err(fail_at(
                                "array constants (PtgArray) in BIFF2 to BIFF4 are not supported yet"
                                    .into(),
                            ));
                        }
                        t.take(7)?;
                        let s = read_array(&mut x, env).map_err(|f| Fail {
                            reason: format!("array constant: {}", f.reason),
                            at: start,
                        })?;
                        push(&mut stack, s, env, start)?;
                    }
                    0x21 => {
                        let idx = if matches!(env.version, BiffVersion::Biff2 | BiffVersion::Biff3)
                        {
                            t.u8()? as u16
                        } else {
                            t.u16()?
                        };
                        let Some((name, min, _)) = functions::function(idx) else {
                            return Err(fail_at(format!("unknown function number {idx}")));
                        };
                        call(&mut stack, name.to_string(), min as usize, env, start)?;
                    }
                    0x22 => {
                        let argc = (t.u8()? & 0x7F) as usize;
                        let (idx, command) =
                            if matches!(env.version, BiffVersion::Biff2 | BiffVersion::Biff3) {
                                (t.u8()? as u16, false)
                            } else {
                                let w = t.u16()?;
                                (w & 0x7FFF, w & 0x8000 != 0)
                            };
                        if idx == 255 && !command {
                            // A user-defined or add-in function: the first
                            // argument is its name.
                            if argc == 0 {
                                return Err(fail_at(
                                    "an external function call without a name".into(),
                                ));
                            }
                            let args = pop_n(&mut stack, argc, start)?;
                            let text = format!("{}({})", args[0], args[1..].join(","));
                            push(&mut stack, text, env, start)?;
                        } else {
                            let found = if command {
                                functions::command(idx)
                            } else {
                                functions::function(idx)
                            };
                            let Some((name, _, _)) = found else {
                                return Err(fail_at(format!(
                                    "unknown {}function number {idx}",
                                    if command { "command " } else { "" }
                                )));
                            };
                            call(&mut stack, name.to_string(), argc, env, start)?;
                        }
                    }
                    0x23 => {
                        let idx = match env.version {
                            BiffVersion::Biff8 => {
                                let i = t.u16()?;
                                t.take(2)?;
                                i
                            }
                            BiffVersion::Biff5 => {
                                let i = t.u16()?;
                                t.take(12)?;
                                i
                            }
                            _ => {
                                return Err(fail_at(
                                    "name tokens (PtgName) in BIFF2 to BIFF4 are not supported yet"
                                        .into(),
                                ));
                            }
                        };
                        let Some(name) = idx.checked_sub(1).and_then(|i| env.names.get(i as usize))
                        else {
                            return Err(fail_at(format!("name index {idx} has no NAME record")));
                        };
                        push(&mut stack, name.clone(), env, start)?;
                    }
                    0x24 | 0x2C => {
                        let rel = base_ptg == 0x2C;
                        let loc = read_loc(&mut t, env, rel, base)?;
                        push(&mut stack, loc_text(loc), env, start)?;
                    }
                    0x25 | 0x2D => {
                        let rel = base_ptg == 0x2D;
                        let (a, b) = read_area(&mut t, env, rel, base)?;
                        push(&mut stack, area_text(a, b, env), env, start)?;
                    }
                    0x26..=0x28 => {
                        // Memory tokens: a header, then the tokens of the
                        // subexpression follow as usual.
                        if env.version == BiffVersion::Biff2 {
                            return Err(fail_at(
                                "memory tokens in BIFF2 are not supported yet".into(),
                            ));
                        }
                        t.take(6)?;
                        if base_ptg == 0x26 {
                            skip_mem_area(&mut x, env).map_err(|f| Fail {
                                reason: f.reason,
                                at: start,
                            })?;
                        }
                    }
                    0x29 | 0x2E | 0x2F => {
                        if env.version == BiffVersion::Biff2 {
                            return Err(fail_at(
                                "memory tokens in BIFF2 are not supported yet".into(),
                            ));
                        }
                        t.take(2)?;
                    }
                    0x2A => {
                        t.take(if v8 { 4 } else { 3 })?;
                        push(&mut stack, "#REF!".into(), env, start)?;
                    }
                    0x2B => {
                        t.take(if v8 { 8 } else { 6 })?;
                        push(&mut stack, "#REF!".into(), env, start)?;
                    }
                    0x39 => {
                        let text = name_x(&mut t, env).map_err(|f| Fail {
                            reason: f.reason,
                            at: start,
                        })?;
                        push(&mut stack, text, env, start)?;
                    }
                    0x3A..=0x3D => {
                        if old {
                            return Err(fail_at(format!(
                                "3D reference token 0x{ptg:02X} is not valid before BIFF5"
                            )));
                        }
                        let text =
                            ref_3d(&mut t, base_ptg, env, base, shared).map_err(|f| Fail {
                                reason: f.reason,
                                at: start,
                            })?;
                        push(&mut stack, text, env, start)?;
                    }
                    _ => return Err(fail_at(format!("unknown token 0x{ptg:02X}"))),
                }
            }
            _ => return Err(fail_at(format!("unknown token 0x{ptg:02X}"))),
        }
    }
    match stack.items.len() {
        0 => Ok(String::new()),
        1 => Ok(stack.items.pop().unwrap_or_default()),
        n => Err(Fail {
            reason: format!("the tokens leave {n} values instead of one"),
            at: tokens.len(),
        }),
    }
}

/// The operand stack. It counts the bytes it holds, so that many operands
/// that are each under the text limit cannot add up to much more.
#[derive(Default)]
struct Stack {
    items: Vec<String>,
    bytes: usize,
}

fn pop(stack: &mut Stack, at: usize) -> R<String> {
    let s = stack.items.pop().ok_or(Fail {
        reason: "an operator or function has too few operands".into(),
        at,
    })?;
    stack.bytes = stack.bytes.saturating_sub(s.len() + 1);
    Ok(s)
}

fn pop_n(stack: &mut Stack, n: usize, at: usize) -> R<Vec<String>> {
    if stack.items.len() < n {
        return Err(Fail {
            reason: format!(
                "a function needs {n} arguments but only {} values are on the stack",
                stack.items.len()
            ),
            at,
        });
    }
    let args = stack.items.split_off(stack.items.len() - n);
    let freed: usize = args.iter().map(|a| a.len() + 1).sum();
    stack.bytes = stack.bytes.saturating_sub(freed);
    Ok(args)
}

fn push(stack: &mut Stack, s: String, env: &Env<'_>, at: usize) -> R<()> {
    // Each operand ends up in the one result, so all of them together may
    // not pass the limit either (each item counts one byte more, so that a
    // flood of empty operands is bounded too).
    let bytes = stack.bytes.saturating_add(s.len() + 1);
    if bytes > env.max_text {
        return Err(Fail {
            reason: format!("the formula text is longer than {} bytes", env.max_text),
            at,
        });
    }
    stack.bytes = bytes;
    stack.items.push(s);
    Ok(())
}

fn call(stack: &mut Stack, name: String, argc: usize, env: &Env<'_>, at: usize) -> R<()> {
    let args = pop_n(stack, argc, at)?;
    push(stack, format!("{name}({})", args.join(",")), env, at)
}

fn attr(t: &mut Toks<'_>, stack: &mut Stack, env: &Env<'_>, start: usize) -> R<()> {
    let grbit = t.u8()?;
    let w = if env.version == BiffVersion::Biff2 {
        t.u8()? as u16
    } else {
        t.u16()?
    };
    if grbit & 0x04 != 0 {
        // attrChoose: a jump table of w + 1 offsets follows.
        let width = if env.version == BiffVersion::Biff2 {
            1
        } else {
            2
        };
        t.take((w as usize + 1) * width)?;
    }
    if grbit & 0x10 != 0 {
        // attrSum: SUM with one argument.
        let a = pop(stack, start)?;
        push(stack, format!("SUM({a})"), env, start)?;
    }
    // attrVolatile, attrIf, attrGoto, attrBaxcel and attrSpace change
    // nothing in the text. Spaces and line breaks are dropped.
    Ok(())
}

fn read_short_string(t: &mut Toks<'_>, env: &Env<'_>) -> R<String> {
    let cch = t.u8()? as usize;
    if is_biff8(env) {
        let flag = t.u8()?;
        if flag & 1 != 0 {
            Ok(codepage::utf16le(t.take(cch * 2)?))
        } else {
            Ok(codepage::latin1(t.take(cch)?))
        }
    } else {
        Ok(codepage::decode(env.enc, t.take(cch)?))
    }
}

fn quote_string(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Formats a number the way Excel shows it in a formula.
pub(crate) fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return "#NUM!".into();
    }
    let a = v.abs();
    if a != 0.0 && !(1e-9..1e15).contains(&a) {
        let s = format!("{v:E}");
        match s.split_once('E') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}E+{e}"),
            _ => s,
        }
    } else if v == 0.0 {
        "0".into()
    } else {
        format!("{v}")
    }
}

fn read_loc(t: &mut Toks<'_>, env: &Env<'_>, rel: bool, base: (u32, u16)) -> R<Loc> {
    if is_biff8(env) {
        let rw = t.u16()?;
        let cw = t.u16()?;
        Ok(loc8(rw, cw, rel, base))
    } else {
        let rw = t.u16()?;
        let col = t.u8()?;
        Ok(loc5(rw, col, rel, base))
    }
}

fn loc8(rw: u16, cw: u16, rel: bool, base: (u32, u16)) -> Loc {
    let row_rel = cw & 0x8000 != 0;
    let col_rel = cw & 0x4000 != 0;
    let mut row = rw as u32;
    let mut col = cw & 0x3FFF;
    if rel {
        if row_rel {
            row = (base.0 as i64 + rw as i16 as i64).rem_euclid(65536) as u32;
        }
        if col_rel {
            col = (base.1 as i64 + (cw & 0xFF) as u8 as i8 as i64).rem_euclid(256) as u16;
        }
    }
    Loc {
        row,
        col,
        row_rel,
        col_rel,
    }
}

fn loc5(rw: u16, col: u8, rel: bool, base: (u32, u16)) -> Loc {
    let row_rel = rw & 0x8000 != 0;
    let col_rel = rw & 0x4000 != 0;
    let mut row = (rw & 0x3FFF) as u32;
    let mut c = col as u16;
    if rel {
        if row_rel {
            // A 14-bit signed offset.
            let off = (((rw & 0x3FFF) << 2) as i16 >> 2) as i64;
            row = (base.0 as i64 + off).rem_euclid(16384) as u32;
        }
        if col_rel {
            c = (base.1 as i64 + col as i8 as i64).rem_euclid(256) as u16;
        }
    }
    Loc {
        row,
        col: c,
        row_rel,
        col_rel,
    }
}

fn read_area(t: &mut Toks<'_>, env: &Env<'_>, rel: bool, base: (u32, u16)) -> R<(Loc, Loc)> {
    if is_biff8(env) {
        let r1 = t.u16()?;
        let r2 = t.u16()?;
        let c1 = t.u16()?;
        let c2 = t.u16()?;
        Ok((loc8(r1, c1, rel, base), loc8(r2, c2, rel, base)))
    } else {
        let r1 = t.u16()?;
        let r2 = t.u16()?;
        let c1 = t.u8()?;
        let c2 = t.u8()?;
        Ok((loc5(r1, c1, rel, base), loc5(r2, c2, rel, base)))
    }
}

fn col_text(l: Loc) -> String {
    format!("{}{}", if l.col_rel { "" } else { "$" }, column_name(l.col))
}

fn row_text(l: Loc) -> String {
    format!("{}{}", if l.row_rel { "" } else { "$" }, l.row as u64 + 1)
}

fn loc_text(l: Loc) -> String {
    format!("{}{}", col_text(l), row_text(l))
}

fn area_text(a: Loc, b: Loc, env: &Env<'_>) -> String {
    let max_row = if is_biff8(env) { 65535 } else { 16383 };
    if a.row == 0 && b.row == max_row {
        // Whole columns, such as A:C.
        format!("{}:{}", col_text(a), col_text(b))
    } else if a.col == 0 && b.col == 255 {
        // Whole rows, such as 1:3.
        format!("{}:{}", row_text(a), row_text(b))
    } else {
        format!("{}:{}", loc_text(a), loc_text(b))
    }
}

/// Quotes a sheet name for use before "!" when Excel would.
pub(crate) fn sheet_prefix(name: &str) -> String {
    if needs_quotes(name) {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_string()
    }
}

fn needs_quotes(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let first = name.chars().next().unwrap_or('0');
    if first.is_ascii_digit() || first == '.' {
        return true;
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return true;
    }
    // Names that read as a cell reference ("A1", "R1C1") need quotes.
    let upper = name.to_ascii_uppercase();
    let letters = upper
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    let rest = &upper[letters..];
    if (1..=3).contains(&letters) && !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    upper.starts_with('R')
        && upper[1..].chars().any(|c| c == 'C')
        && upper
            .chars()
            .skip(1)
            .all(|c| c.is_ascii_digit() || c == 'C')
}

fn sheets_text(first: &str, last: &str) -> String {
    if first == last {
        sheet_prefix(first)
    } else if needs_quotes(first) || needs_quotes(last) {
        format!(
            "'{}:{}'",
            first.replace('\'', "''"),
            last.replace('\'', "''")
        )
    } else {
        format!("{first}:{last}")
    }
}

/// "[Book.xls]Sheet1", or "[Book.xls]Sheet1:Sheet3" for a sheet range.
fn external_text(book: &str, first: &str, last: &str) -> String {
    let inner = if last.is_empty() || first == last {
        format!("[{book}]{first}")
    } else {
        format!("[{book}]{first}:{last}")
    };
    let stem = book.trim_end_matches(".xls").trim_end_matches(".xlw");
    if needs_quotes(stem) || needs_quotes(first) || needs_quotes(last) {
        format!("'{}'", inner.replace('\'', "''"))
    } else {
        inner
    }
}

/// The "Sheet1!" or "'[Book.xls]Sheet 1'!" part of a 3D reference, or
/// `None` for a reference to a deleted sheet.
fn prefix_8(ixti: u16, env: &Env<'_>) -> R<Option<String>> {
    let fail = |reason: String| Fail { reason, at: 0 };
    let xti = env
        .xti
        .get(ixti as usize)
        .ok_or_else(|| fail(format!("EXTERNSHEET entry {ixti} does not exist")))?;
    let book = env
        .supbooks
        .get(xti.supbook as usize)
        .ok_or_else(|| fail(format!("SUPBOOK {} does not exist", xti.supbook)))?;
    if xti.first < 0 || xti.last < 0 {
        return Ok(None);
    }
    match &book.kind {
        SupBookKind::Internal => {
            let f = env.sheet_names.get(xti.first as usize);
            let l = env.sheet_names.get(xti.last as usize);
            match (f, l) {
                (Some(f), Some(l)) => Ok(Some(sheets_text(f, l))),
                _ => Ok(None),
            }
        }
        SupBookKind::External { book, sheets } => {
            let first = sheets.get(xti.first as usize).cloned().unwrap_or_default();
            let last = sheets.get(xti.last as usize).cloned().unwrap_or_default();
            Ok(Some(external_text(book, &first, &last)))
        }
        SupBookKind::AddIn | SupBookKind::Other => Err(fail(
            "a cell reference points into an add-in or DDE link".into(),
        )),
    }
}

/// BIFF5 3D prefix from an EXTERNSHEET index and sheet numbers.
fn prefix_5(ixals: i16, first: i16, last: i16, env: &Env<'_>) -> R<Option<String>> {
    if ixals < 0 {
        // A reference into this workbook: the sheet numbers say where.
        if first < 0 || last < 0 {
            return Ok(None);
        }
        let f = env.sheet_names.get(first as usize);
        let l = env.sheet_names.get(last as usize);
        if let (Some(f), Some(l)) = (f, l) {
            return Ok(Some(sheets_text(f, l)));
        }
        // Fall back to the EXTERNSHEET record's own sheet name.
        let i = (-(ixals as i32) - 1) as usize;
        return match env.extern_sheets.get(i) {
            Some(ExternSheet::Own(name)) => Ok(Some(sheet_prefix(name))),
            _ => Ok(None),
        };
    }
    let i = (ixals as usize).wrapping_sub(1);
    match env.extern_sheets.get(i) {
        Some(ExternSheet::Own(name)) => Ok(Some(sheet_prefix(name))),
        Some(ExternSheet::External { book, sheet }) => Ok(Some(external_text(book, sheet, sheet))),
        Some(ExternSheet::OwnBook) | Some(ExternSheet::Unknown) | None => Err(Fail {
            reason: format!("EXTERNSHEET record {ixals} cannot be used for a cell reference"),
            at: 0,
        }),
    }
}

fn ref_3d(
    t: &mut Toks<'_>,
    base_ptg: u8,
    env: &Env<'_>,
    base: (u32, u16),
    shared: bool,
) -> R<String> {
    // In a shared formula the location is an offset from the cell
    // (RgceLocRel), read the same way as PtgRefN.
    let prefix = if is_biff8(env) {
        let ixti = t.u16()?;
        prefix_8(ixti, env)?
    } else {
        let ixals = t.i16()?;
        t.take(8)?;
        let first = t.i16()?;
        let last = t.i16()?;
        prefix_5(ixals, first, last, env)?
    };
    let body = match base_ptg {
        0x3A => Some(loc_text(read_loc(t, env, shared, base)?)),
        0x3B => {
            let (a, b) = read_area(t, env, shared, base)?;
            Some(area_text(a, b, env))
        }
        0x3C => {
            t.take(if is_biff8(env) { 4 } else { 3 })?;
            None
        }
        _ => {
            t.take(if is_biff8(env) { 8 } else { 6 })?;
            None
        }
    };
    Ok(match (prefix, body) {
        (Some(p), Some(b)) => format!("{p}!{b}"),
        (Some(p), None) => format!("{p}!#REF!"),
        (None, _) => "#REF!".to_string(),
    })
}

fn name_x(t: &mut Toks<'_>, env: &Env<'_>) -> R<String> {
    let fail = |reason: String| Fail { reason, at: 0 };
    match env.version {
        BiffVersion::Biff8 => {
            let ixti = t.u16()?;
            let iname = t.u16()?;
            t.take(2)?;
            let xti = env
                .xti
                .get(ixti as usize)
                .ok_or_else(|| fail(format!("EXTERNSHEET entry {ixti} does not exist")))?;
            let book = env
                .supbooks
                .get(xti.supbook as usize)
                .ok_or_else(|| fail(format!("SUPBOOK {} does not exist", xti.supbook)))?;
            if let SupBookKind::Internal = book.kind {
                // An internal NameX points at this workbook's NAME list.
                return iname
                    .checked_sub(1)
                    .and_then(|i| env.names.get(i as usize))
                    .cloned()
                    .ok_or_else(|| fail(format!("name index {iname} has no NAME record")));
            }
            let name = iname
                .checked_sub(1)
                .and_then(|i| book.names.get(i as usize))
                .ok_or_else(|| fail(format!("external name {iname} does not exist")))?;
            Ok(match &book.kind {
                SupBookKind::External { book, .. } => format!("{}!{name}", sheet_prefix(book)),
                _ => name.clone(),
            })
        }
        BiffVersion::Biff5 => {
            let ixals = t.i16()?;
            t.take(8)?;
            let iname = t.u16()?;
            t.take(12)?;
            let i = ((ixals as i32).unsigned_abs() as usize).wrapping_sub(1);
            let name = env
                .extern_names
                .get(i)
                .and_then(|names| names.get((iname as usize).wrapping_sub(1)))
                .ok_or_else(|| {
                    fail(format!(
                        "external name {iname} of EXTERNSHEET {ixals} does not exist"
                    ))
                })?;
            Ok(match env.extern_sheets.get(i) {
                Some(ExternSheet::External { book, .. }) => {
                    format!("{}!{name}", sheet_prefix(book))
                }
                _ => name.clone(),
            })
        }
        _ => Err(fail(
            "external name tokens (PtgNameX) in BIFF2 to BIFF4 are not supported yet".into(),
        )),
    }
}

fn skip_mem_area(x: &mut Toks<'_>, env: &Env<'_>) -> R<()> {
    let count = x.u16()? as usize;
    let each = if is_biff8(env) { 8 } else { 6 };
    x.take(count * each)?;
    Ok(())
}

fn read_array(x: &mut Toks<'_>, env: &Env<'_>) -> R<String> {
    let cols = x.u8()? as usize + 1;
    let rows = x.u16()? as usize + 1;
    // Every value takes at least two bytes; refuse counts the data cannot hold.
    let left = x.data.len() - x.pos;
    if cols.saturating_mul(rows) > left / 2 {
        return Err(x.fail(format!(
            "{cols} x {rows} values do not fit in the remaining {left} bytes"
        )));
    }
    let mut out = String::from("{");
    for r in 0..rows {
        if r > 0 {
            out.push(';');
        }
        for c in 0..cols {
            if c > 0 {
                out.push(',');
            }
            let typ = x.u8()?;
            match typ {
                0x00 => {
                    x.take(8)?;
                }
                0x01 => out.push_str(&format_number(x.f64()?)),
                0x02 => {
                    let s = if is_biff8(env) {
                        let cch = x.u16()? as usize;
                        let flag = x.u8()?;
                        if flag & 1 != 0 {
                            codepage::utf16le(x.take(cch * 2)?)
                        } else {
                            codepage::latin1(x.take(cch)?)
                        }
                    } else {
                        let cch = x.u8()? as usize;
                        codepage::decode(env.enc, x.take(cch)?)
                    };
                    out.push_str(&quote_string(&s));
                }
                0x04 => {
                    let b = x.u8()?;
                    x.take(7)?;
                    out.push_str(if b != 0 { "TRUE" } else { "FALSE" });
                }
                0x10 => {
                    let e = x.u8()?;
                    x.take(7)?;
                    out.push_str(CellError::from_code(e).as_str());
                }
                _ => return Err(x.fail(format!("unknown array value type 0x{typ:02X}"))),
            }
            if out.len() > env.max_text {
                return Err(x.fail("the array constant is too long"));
            }
        }
    }
    out.push('}');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(version: BiffVersion) -> Env<'static> {
        static SHEETS: [String; 0] = [];
        Env {
            version,
            sheet_names: &SHEETS,
            names: &[],
            xti: &[],
            supbooks: &[],
            extern_sheets: &[],
            extern_names: &[],
            enc: encoding_rs::WINDOWS_1252,
            max_text: 10_000,
        }
    }

    fn text(tokens: &[u8], version: BiffVersion) -> String {
        match render(tokens, &[], (0, 0), &env(version)) {
            FormulaText::Parsed(s) => s,
            FormulaText::Unparsed { reason, .. } => format!("UNPARSED: {reason}"),
        }
    }

    #[test]
    fn biff8_operators_and_refs() {
        // A1+$B$2*3
        let t = [
            0x24, 0, 0, 0, 0xC0, 0x24, 1, 0, 1, 0, 0x1E, 3, 0, 0x05, 0x03,
        ];
        assert_eq!(text(&t, BiffVersion::Biff8), "A1+$B$2*3");
        // SUM(A1:B2) through PtgFuncVar.
        let t = [0x25, 0, 0, 1, 0, 0, 0xC0, 1, 0xC0, 0x22, 1, 4, 0];
        assert_eq!(text(&t, BiffVersion::Biff8), "SUM(A1:B2)");
        // Whole columns.
        let t = [0x25, 0, 0, 0xFF, 0xFF, 0, 0xC0, 2, 0xC0];
        assert_eq!(text(&t, BiffVersion::Biff8), "A:C");
    }

    #[test]
    fn biff5_refs_keep_flags_in_the_row() {
        // B15 relative (row 14, col 1) in BIFF4/5 layout.
        let t = [0x44, 0x0E, 0xC0, 0x01];
        assert_eq!(text(&t, BiffVersion::Biff4), "B15");
        let t = [0x44, 0x0E, 0x00, 0x01];
        assert_eq!(text(&t, BiffVersion::Biff5), "$B$15");
    }

    #[test]
    fn shared_formula_offsets() {
        // PtgRefN with row offset -1 and column offset +1 from C3: D2.
        let e = env(BiffVersion::Biff8);
        let t = [0x2C, 0xFF, 0xFF, 0x01, 0xC0];
        assert_eq!(render(&t, &[], (2, 2), &e).as_str(), Some("D2"));
        let e = env(BiffVersion::Biff5);
        let t = [0x2C, 0xFF, 0xFF, 0x01];
        assert_eq!(render(&t, &[], (2, 2), &e).as_str(), Some("D2"));
    }

    #[test]
    fn strings_numbers_errors() {
        let t = [
            0x17, 3, 0, b'a', b'"', b'b', 0x1F, 0, 0, 0, 0, 0, 0, 0xE0, 0x3F, 0x08,
        ];
        assert_eq!(text(&t, BiffVersion::Biff8), "\"a\"\"b\"&0.5");
        assert_eq!(text(&[0x1C, 0x2A], BiffVersion::Biff8), "#N/A");
        assert_eq!(format_number(1e20), "1E+20");
        assert_eq!(format_number(-2.5e-12), "-2.5E-12");
        assert_eq!(format_number(42.0), "42");
    }

    #[test]
    fn unknown_tokens_are_reported_not_fatal() {
        let t = [0x1E, 1, 0, 0xFF];
        match render(&t, &[], (0, 0), &env(BiffVersion::Biff8)) {
            FormulaText::Unparsed { at, .. } => assert_eq!(at, 3),
            other => panic!("{other:?}"),
        }
        // Too few operands.
        assert!(text(&[0x03], BiffVersion::Biff8).starts_with("UNPARSED"));
    }

    #[test]
    fn array_constant() {
        let e = env(BiffVersion::Biff8);
        let t = [0x60, 0, 0, 0, 0, 0, 0, 0];
        let mut x = vec![1u8, 0, 0];
        x.push(0x01);
        x.extend_from_slice(&1.0f64.to_le_bytes());
        x.extend_from_slice(&[0x02, 1, 0, 0, b'x']);
        assert_eq!(render(&t, &x, (0, 0), &e).as_str(), Some("{1,\"x\"}"));
    }

    #[test]
    fn sheet_names_are_quoted_when_needed() {
        assert_eq!(sheet_prefix("Sheet1"), "Sheet1");
        assert_eq!(sheet_prefix("My Sheet"), "'My Sheet'");
        assert_eq!(sheet_prefix("A1"), "'A1'");
        assert_eq!(sheet_prefix("2019"), "'2019'");
        assert_eq!(sheet_prefix("O'Neil"), "'O''Neil'");
        assert_eq!(sheet_prefix("中文"), "中文");
    }
}
