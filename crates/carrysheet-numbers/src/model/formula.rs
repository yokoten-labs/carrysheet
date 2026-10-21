//! Formula text from the formula syntax tree Numbers stores.
//!
//! Numbers stores a formula as its nodes in postfix order: operands first,
//! then the operator or function that uses them. The node handling follows
//! numbers-parser's `formula.py` (MIT, Copyright 2021 Jon Connell; see
//! THIRD-PARTY.md).
//!
//! Many cells can share one stored formula (a filled-down column does), and
//! its references are relative to the cell. So a formula is compiled once
//! per table into a [`Compiled`] tree, with the references kept unresolved,
//! and written out for each cell. Compiling and writing are both linear and
//! without recursion; the caller charges both to a budget for the document.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use super::cell::Date;
use super::functions::function_name;
use crate::proto::tsce::FormulaArchive;
use crate::proto::tsce::ast_node_array_archive::{AstNodeArchive, AstNodeType};

/// The longest formula text, in bytes.
const MAX_TEXT: usize = 64 * 1024;

/// Sentinels for "the whole column" (rows) and "the whole row" (columns).
const ALL_ROWS: u32 = 0x7fff_ffff;
const ALL_COLUMNS: u32 = 0x7fff;

/// What a formula needs to know about the tables of the document.
pub(crate) struct Tables {
    /// Every table of the document, in order.
    tables: Vec<TableRef>,
    /// Table index by the id cross-table references use.
    by_uid: HashMap<u128, usize>,
    /// How many tables have each name.
    name_count: HashMap<Arc<str>, usize>,
}

impl Tables {
    pub(crate) fn new(tables: Vec<TableRef>) -> Self {
        let mut by_uid = HashMap::new();
        let mut name_count = HashMap::new();
        for (i, t) in tables.iter().enumerate() {
            if let Some(uid) = t.uid {
                by_uid.entry(uid).or_insert(i);
            }
            *name_count.entry(Arc::clone(&t.name)).or_insert(0) += 1;
        }
        Self {
            tables,
            by_uid,
            name_count,
        }
    }
}

/// A table as formulas refer to it. The names are shared, not copied, so a
/// long name used by many tables or references costs nothing extra.
pub(crate) struct TableRef {
    pub(crate) sheet: usize,
    pub(crate) sheet_name: Arc<str>,
    pub(crate) name: Arc<str>,
    pub(crate) uid: Option<u128>,
}

type Fail = String;

/// `Table::` or `Sheet::Table::` before a reference to another table.
#[derive(Clone)]
struct Prefix {
    sheet: Option<Arc<str>>,
    name: Arc<str>,
}

impl Prefix {
    fn len(&self) -> usize {
        self.sheet.as_ref().map_or(0, |s| s.len() + 2) + self.name.len() + 2
    }

    fn write(&self, out: &mut String) {
        if let Some(sheet) = &self.sheet {
            out.push_str(sheet);
            out.push_str("::");
        }
        out.push_str(&self.name);
        out.push_str("::");
    }
}

/// One end of a reference: a fixed number, or an offset from the cell.
#[derive(Clone, Copy)]
struct End {
    value: i64,
    /// Shown with `$`; also means the value is not relative to the cell.
    absolute: bool,
}

impl End {
    fn at(self, base: i64) -> i64 {
        if self.absolute {
            self.value
        } else {
            base + self.value
        }
    }
}

/// A reference with the cell it is relative to still open.
#[derive(Clone, Copy)]
enum Spec {
    /// A range from a colon tract: row and column ends (`None` for "all").
    Tract([Option<End>; 4]),
    /// A single cell, row or column; `None` for both is the cell itself.
    Cell(Option<End>, Option<End>),
}

impl Spec {
    fn uses_cell(&self) -> bool {
        match self {
            Self::Tract(ends) => ends.iter().flatten().any(|e| !e.absolute),
            Self::Cell(None, None) => true,
            Self::Cell(row, col) => [row, col].into_iter().flatten().any(|e| !e.absolute),
        }
    }
}

/// One piece of the formula tree.
enum Piece {
    Static(&'static str),
    Text(String),
    Node(usize),
}

/// A node of the formula tree.
enum Expr {
    /// A reference, written as `prefix::A1` or `#REF!`.
    Reference(Option<Prefix>, Spec),
    /// Two references joined by a colon. When both resolve, they share the
    /// first one's prefix (`Table::A1:B2`); otherwise each is written alone.
    Range(Option<Prefix>, Spec, Option<Prefix>, Spec),
    Seq(Vec<Piece>),
}

/// A formula compiled for one table, ready to be written for any cell.
pub(crate) struct Compiled {
    exprs: Vec<Expr>,
    /// What is left on the stack, written last first.
    roots: Vec<usize>,
    uses_cell: bool,
}

impl Compiled {
    /// The work of writing the formula out once, in nodes.
    pub(crate) fn size(&self) -> u64 {
        self.exprs.len() as u64
    }

    /// True when the text depends on the cell the formula is in.
    pub(crate) fn uses_cell(&self) -> bool {
        self.uses_cell
    }

    /// The formula text for the cell at `row` and `column`.
    pub(crate) fn text(&self, row: u32, column: u32) -> Result<String, Fail> {
        let base = (i64::from(row), i64::from(column));
        let mut out = String::new();
        for &root in self.roots.iter().rev() {
            self.render(root, base, &mut out)?;
        }
        Ok(out)
    }

    /// Writes the tree under `root` without recursion.
    fn render(&self, root: usize, base: (i64, i64), out: &mut String) -> Result<(), Fail> {
        let mut todo: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some((node, next)) = todo.pop() {
            match &self.exprs[node] {
                Expr::Reference(prefix, spec) => write_reference(prefix, spec, base, out),
                Expr::Range(first_prefix, first, last_prefix, last) => {
                    match (resolve(first, base), resolve(last, base)) {
                        (Some(a), Some(b)) => {
                            if let Some(p) = first_prefix {
                                p.write(out);
                            }
                            out.push_str(&a);
                            out.push(':');
                            out.push_str(&b);
                        }
                        _ => {
                            write_reference(first_prefix, first, base, out);
                            out.push(':');
                            write_reference(last_prefix, last, base, out);
                        }
                    }
                }
                Expr::Seq(pieces) => {
                    let mut i = next;
                    while let Some(piece) = pieces.get(i) {
                        i += 1;
                        match piece {
                            Piece::Static(s) => out.push_str(s),
                            Piece::Text(s) => out.push_str(s),
                            Piece::Node(child) => {
                                todo.push((node, i));
                                todo.push((*child, 0));
                                break;
                            }
                        }
                    }
                }
            }
            if out.len() > MAX_TEXT {
                return Err(format!("the formula text is longer than {MAX_TEXT} bytes"));
            }
        }
        Ok(())
    }
}

fn write_reference(prefix: &Option<Prefix>, spec: &Spec, base: (i64, i64), out: &mut String) {
    match resolve(spec, base) {
        Some(body) => {
            if let Some(p) = prefix {
                p.write(out);
            }
            out.push_str(&body);
        }
        None => out.push_str("#REF!"),
    }
}

struct Builder {
    exprs: Vec<Expr>,
    /// The fixed part of each node's text length (references count 0).
    lengths: Vec<usize>,
    stack: Vec<usize>,
    uses_cell: bool,
}

impl Builder {
    fn push(&mut self, expr: Expr) -> Result<(), Fail> {
        let len = match &expr {
            Expr::Reference(prefix, _) => prefix.as_ref().map_or(0, Prefix::len),
            Expr::Range(prefix, ..) => prefix.as_ref().map_or(0, Prefix::len) + 1,
            Expr::Seq(pieces) => pieces
                .iter()
                .map(|p| match p {
                    Piece::Static(s) => s.len(),
                    Piece::Text(s) => s.len(),
                    Piece::Node(n) => self.lengths[*n],
                })
                .sum(),
        };
        if len > MAX_TEXT {
            return Err(format!("the formula text is longer than {MAX_TEXT} bytes"));
        }
        self.exprs.push(expr);
        self.lengths.push(len);
        self.stack.push(self.exprs.len() - 1);
        Ok(())
    }

    fn text(&mut self, text: String) -> Result<(), Fail> {
        self.push(Expr::Seq(vec![Piece::Text(text)]))
    }

    fn pop(&mut self) -> Result<usize, Fail> {
        self.stack
            .pop()
            .ok_or_else(|| "an operator has fewer operands than it needs".to_owned())
    }

    /// Pops `n` operands, first operand first.
    fn pop_n(&mut self, n: usize) -> Result<Vec<usize>, Fail> {
        if n > self.stack.len() {
            return Err("a function has fewer arguments than it needs".into());
        }
        Ok(self.stack.split_off(self.stack.len() - n))
    }

    fn binary(&mut self, op: &'static str) -> Result<(), Fail> {
        let right = self.pop()?;
        let left = self.pop()?;
        self.push(Expr::Seq(vec![
            Piece::Node(left),
            Piece::Static(op),
            Piece::Node(right),
        ]))
    }

    fn joined(
        &mut self,
        open: &'static str,
        items: &[usize],
        sep: &'static str,
        close: &'static str,
    ) -> Result<(), Fail> {
        let mut pieces = Vec::with_capacity(items.len() * 2 + 2);
        pieces.push(Piece::Static(open));
        for (i, &item) in items.iter().enumerate() {
            if i > 0 {
                pieces.push(Piece::Static(sep));
            }
            pieces.push(Piece::Node(item));
        }
        pieces.push(Piece::Static(close));
        self.push(Expr::Seq(pieces))
    }
}

/// Compiles a stored formula for the table at index `table`, or says why it
/// cannot be turned into text.
pub(crate) fn compile(
    formula: &FormulaArchive,
    table: usize,
    tables: &Tables,
    max_nodes: usize,
) -> Result<Compiled, Fail> {
    let nodes = &formula.ast_node_array.ast_node;
    if nodes.len() > max_nodes {
        return Err(format!("the formula has more than {max_nodes} parts"));
    }
    let mut b = Builder {
        exprs: Vec::with_capacity(nodes.len()),
        lengths: Vec::with_capacity(nodes.len()),
        stack: Vec::new(),
        uses_cell: false,
    };
    for node in nodes {
        apply(&mut b, node, table, tables)?;
    }
    if b.stack.is_empty() {
        return Err("the formula is empty".into());
    }
    // numbers-parser joins whatever is left, last first.
    let total: usize = b.stack.iter().map(|&n| b.lengths[n]).sum();
    if total > MAX_TEXT {
        return Err(format!("the formula text is longer than {MAX_TEXT} bytes"));
    }
    Ok(Compiled {
        exprs: b.exprs,
        roots: b.stack,
        uses_cell: b.uses_cell,
    })
}

fn apply(
    b: &mut Builder,
    node: &AstNodeArchive,
    table: usize,
    tables: &Tables,
) -> Result<(), Fail> {
    use AstNodeType as T;
    let Ok(kind) = AstNodeType::try_from(node.ast_node_type) else {
        return Err(format!(
            "formula node type {} is unknown",
            node.ast_node_type
        ));
    };
    match kind {
        T::AdditionNode => b.binary("+"),
        T::SubtractionNode => b.binary("-"),
        T::MultiplicationNode => b.binary("×"),
        T::DivisionNode => b.binary("÷"),
        T::PowerNode => b.binary("^"),
        T::ConcatenationNode => b.binary("&"),
        T::GreaterThanNode => b.binary(">"),
        T::GreaterThanOrEqualToNode => b.binary("≥"),
        T::LessThanNode => b.binary("<"),
        T::LessThanOrEqualToNode => b.binary("≤"),
        T::EqualToNode => b.binary("="),
        T::NotEqualToNode => b.binary("≠"),
        T::NegationNode => {
            let arg = b.pop()?;
            b.push(Expr::Seq(vec![Piece::Static("-"), Piece::Node(arg)]))
        }
        T::PercentNode => {
            let arg = b.pop()?;
            b.push(Expr::Seq(vec![Piece::Node(arg), Piece::Static("%")]))
        }
        T::FunctionNode => {
            let index = node.ast_function_node_index.unwrap_or(0);
            let name = function_name(index)
                .ok_or_else(|| format!("function number {index} is unknown"))?;
            let count = node.ast_function_node_num_args.unwrap_or(0) as usize;
            call(b, name.to_owned(), count)
        }
        T::UnknownFunctionNode => {
            let name = node
                .ast_unknown_function_node_string
                .clone()
                .unwrap_or_default();
            let count = node.ast_unknown_function_node_num_args.unwrap_or(0) as usize;
            call(b, name, count)
        }
        T::NumberNode => b.text(number_text(node)),
        T::BooleanNode | T::TokenNode => {
            let value = node
                .ast_token_node_boolean
                .or(node.ast_boolean_node_boolean)
                .unwrap_or(false);
            b.push(Expr::Seq(vec![Piece::Static(if value {
                "TRUE"
            } else {
                "FALSE"
            })]))
        }
        T::StringNode => {
            // Numbers does not escape quotes in the tree; it shows them doubled.
            let s = node.ast_string_node_string.as_deref().unwrap_or("");
            b.text(format!("\"{}\"", s.replace('"', "\"\"")))
        }
        T::DateNode => {
            let seconds = node.ast_date_node_date_num.unwrap_or(0.0);
            match Date::from_seconds_since_2001(seconds).parts() {
                Some(p) => b.text(format!("DATE({},{},{})", p.year, p.month, p.day)),
                None => Err("a date in the formula is out of range".into()),
            }
        }
        T::EmptyArgumentNode => b.push(Expr::Seq(Vec::new())),
        T::ArrayNode => {
            let cols = node.ast_array_node_num_col.unwrap_or(0) as usize;
            let rows = node.ast_array_node_num_row.unwrap_or(0) as usize;
            let items = b.pop_n(cols.checked_mul(rows).ok_or("an array is too large")?)?;
            let mut pieces = vec![Piece::Static("{")];
            for (i, item) in items.into_iter().enumerate() {
                if i > 0 {
                    pieces.push(Piece::Static(if i % cols.max(1) == 0 { ";" } else { "," }));
                }
                pieces.push(Piece::Node(item));
            }
            pieces.push(Piece::Static("}"));
            b.push(Expr::Seq(pieces))
        }
        T::ListNode => {
            let count = node.ast_list_node_num_args.unwrap_or(0) as usize;
            let items = b.pop_n(count)?;
            b.joined("(", &items, ",", ")")
        }
        T::ColonNode | T::ColonNodeWithUids => {
            let end = b.pop()?;
            let start = b.pop()?;
            let joined = match (&b.exprs[start], &b.exprs[end]) {
                (
                    Expr::Reference(Some(first_prefix), first),
                    Expr::Reference(last_prefix, last),
                ) => Some(Expr::Range(
                    Some(first_prefix.clone()),
                    *first,
                    last_prefix.clone(),
                    *last,
                )),
                _ => None,
            };
            match joined {
                Some(expr) => b.push(expr),
                None => b.push(Expr::Seq(vec![
                    Piece::Node(start),
                    Piece::Static(":"),
                    Piece::Node(end),
                ])),
            }
        }
        T::CellReferenceNode | T::ColonTractNode => {
            let (prefix, spec) = reference(node, table, tables)?;
            b.uses_cell |= spec.uses_cell();
            b.push(Expr::Reference(prefix, spec))
        }
        T::ReferenceErrorNode | T::ReferenceErrorWithUids => {
            b.push(Expr::Seq(vec![Piece::Static("#REF!")]))
        }
        T::PlusSignNode
        | T::AppendWhitespaceNode
        | T::PrependWhitespaceNode
        | T::BeginThunkNode
        | T::EndThunkNode => Ok(()),
        other => Err(format!(
            "formula node {} is not supported yet",
            other.as_str_name()
        )),
    }
}

/// `NAME(arg,arg)` from the last `count` operands.
fn call(b: &mut Builder, name: String, count: usize) -> Result<(), Fail> {
    let args = b.pop_n(count)?;
    let mut pieces = Vec::with_capacity(args.len() * 2 + 2);
    pieces.push(Piece::Text(name));
    pieces.push(Piece::Static("("));
    for (i, arg) in args.into_iter().enumerate() {
        if i > 0 {
            pieces.push(Piece::Static(","));
        }
        pieces.push(Piece::Node(arg));
    }
    pieces.push(Piece::Static(")"));
    b.push(Expr::Seq(pieces))
}

/// A number literal. Numbers keeps the exact decimal; use it when present.
fn number_text(node: &AstNodeArchive) -> String {
    if let (Some(low), Some(high)) = (
        node.ast_number_node_decimal_low,
        node.ast_number_node_decimal_high,
    ) && let Some(text) = bid128_text(low, high)
    {
        return text;
    }
    let value = node.ast_number_node_number.unwrap_or(0.0);
    // Rust prints the shortest text that reads back as the same f64,
    // without an exponent, as numbers-parser does.
    format!("{value}")
}

/// Formats an IEEE 754 decimal128 (binary integer encoding) exactly, or
/// `None` for the forms Numbers does not use for literals.
fn bid128_text(low: u64, high: u64) -> Option<String> {
    if high >> 61 & 3 == 3 {
        return None; // infinity, NaN or the large-coefficient form
    }
    let negative = high >> 63 == 1;
    let exponent = ((high >> 49) & 0x3fff) as i64 - 6176;
    let coefficient = u128::from(high & ((1 << 49) - 1)) << 64 | u128::from(low);
    if coefficient >= 10u128.pow(34) || !(-60..=60).contains(&exponent) {
        return None;
    }
    let digits = coefficient.to_string();
    let mut text = if exponent >= 0 {
        if coefficient == 0 {
            "0".to_owned()
        } else {
            digits + &"0".repeat(exponent as usize)
        }
    } else {
        let shift = (-exponent) as usize;
        let padded = format!("{digits:0>width$}", width = shift + 1);
        let (int, frac) = padded.split_at(padded.len() - shift);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            int.to_owned()
        } else {
            format!("{int}.{frac}")
        }
    };
    if negative {
        text.insert(0, '-');
    }
    Some(text)
}

/// A column number as letters: 0 is A, 26 is AA.
pub(crate) fn column_name(mut col: u32) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    letters.reverse();
    String::from_utf8(letters).unwrap_or_default()
}

/// A resolved end of a reference.
struct At {
    value: i64,
    absolute: bool,
}

fn cell_text(row: &At, col: &At) -> String {
    let mut s = String::new();
    if col.absolute {
        s.push('$');
    }
    s.push_str(&column_name(col.value as u32));
    if row.absolute {
        s.push('$');
    }
    let _ = write!(s, "{}", row.value + 1);
    s
}

fn row_text(row: &At) -> String {
    format!("{}{}", if row.absolute { "$" } else { "" }, row.value + 1)
}

fn col_text(col: &At) -> String {
    format!(
        "{}{}",
        if col.absolute { "$" } else { "" },
        column_name(col.value as u32)
    )
}

/// The text of a reference for the cell at `base` (row, column), or `None`
/// when it points outside the sheet (`#REF!`).
fn resolve(spec: &Spec, base: (i64, i64)) -> Option<String> {
    let at = |end: Option<End>, base: i64| {
        end.map(|e| At {
            value: e.at(base),
            absolute: e.absolute,
        })
    };
    let outside = |e: &Option<At>| {
        e.as_ref()
            .is_some_and(|e| e.value < 0 || e.value > i64::from(u32::MAX))
    };
    match *spec {
        Spec::Tract([row_start, row_end, col_start, col_end]) => {
            let ends = [
                at(row_start, base.0),
                at(row_end, base.0),
                at(col_start, base.1),
                at(col_end, base.1),
            ];
            if ends.iter().any(outside) {
                return None;
            }
            let [rs, re, cs, ce] = ends;
            Some(match (rs, re, cs, ce) {
                (Some(rs), re, None, _) => match re {
                    Some(re) => format!("{}:{}", row_text(&rs), row_text(&re)),
                    None => format!("{}:{}", row_text(&rs), row_text(&rs)),
                },
                (None, _, Some(cs), ce) => match ce {
                    Some(ce) => format!("{}:{}", col_text(&cs), col_text(&ce)),
                    None => col_text(&cs),
                },
                (Some(rs), Some(re), Some(cs), Some(ce)) => {
                    format!("{}:{}", cell_text(&rs, &cs), cell_text(&re, &ce))
                }
                (Some(rs), _, Some(cs), _) => cell_text(&rs, &cs),
                // Ruled out when the reference was compiled.
                (None, _, None, _) => return None,
            })
        }
        Spec::Cell(row, col) => {
            let (row, col) = (at(row, base.0), at(col, base.1));
            if outside(&row) || outside(&col) {
                return None;
            }
            Some(match (row, col) {
                (Some(r), None) => format!("{}:{}", row_text(&r), row_text(&r)),
                (None, Some(c)) => col_text(&c),
                (Some(r), Some(c)) => cell_text(&r, &c),
                (None, None) => cell_text(
                    &At {
                        value: base.0,
                        absolute: false,
                    },
                    &At {
                        value: base.1,
                        absolute: false,
                    },
                ),
            })
        }
    }
}

/// A cell, range, row or column reference, with the cell it is relative
/// to left open.
fn reference(
    node: &AstNodeArchive,
    table: usize,
    tables: &Tables,
) -> Result<(Option<Prefix>, Spec), Fail> {
    let target = node
        .ast_cross_table_reference_extra_info
        .as_ref()
        .and_then(|info| {
            let id = &info.table_id;
            let uid = u128::from(id.uuid_w3.unwrap_or(0)) << 96
                | u128::from(id.uuid_w2.unwrap_or(0)) << 64
                | u128::from(id.uuid_w1.unwrap_or(0)) << 32
                | u128::from(id.uuid_w0.unwrap_or(0));
            tables.by_uid.get(&uid).copied()
        })
        // An unknown table is shown as this table, as numbers-parser does.
        .unwrap_or(table);

    let spec = if let Some(tract) = &node.ast_colon_tract {
        let sticky = node.ast_sticky_bits.unwrap_or_default();
        let bad = || "a range reference is incomplete".to_owned();
        let end = |absolute: bool,
                   abs: Option<u32>,
                   rel: Option<i32>,
                   all: u32|
         -> Result<Option<End>, Fail> {
            if absolute {
                let v = abs.ok_or_else(bad)?;
                return Ok((v != all).then_some(End {
                    value: i64::from(v),
                    absolute,
                }));
            }
            match rel {
                Some(r) => Ok(Some(End {
                    value: i64::from(r),
                    absolute,
                })),
                None if abs == Some(all) => Ok(None),
                None => Err(bad()),
            }
        };
        let abs_row = tract.absolute_row.first();
        let rel_row = tract.relative_row.first();
        let abs_col = tract.absolute_column.first();
        let rel_col = tract.relative_column.first();
        let ends = [
            end(
                sticky.begin_row_is_absolute,
                abs_row.map(|r| r.range_begin),
                rel_row.map(|r| r.range_begin),
                ALL_ROWS,
            )?,
            end(
                sticky.end_row_is_absolute,
                abs_row.map(|r| r.range_end.unwrap_or(r.range_begin)),
                rel_row.map(|r| r.range_end.unwrap_or(r.range_begin)),
                ALL_ROWS,
            )?,
            end(
                sticky.begin_column_is_absolute,
                abs_col.map(|c| c.range_begin),
                rel_col.map(|c| c.range_begin),
                ALL_COLUMNS,
            )?,
            end(
                sticky.end_column_is_absolute,
                abs_col.map(|c| c.range_end.unwrap_or(c.range_begin)),
                rel_col.map(|c| c.range_end.unwrap_or(c.range_begin)),
                ALL_COLUMNS,
            )?,
        ];
        if ends[0].is_none() && ends[2].is_none() {
            return Err(bad());
        }
        Spec::Tract(ends)
    } else {
        Spec::Cell(
            node.ast_row.map(|r| End {
                value: i64::from(r.row),
                absolute: r.absolute.unwrap_or(false),
            }),
            node.ast_column.map(|c| End {
                value: i64::from(c.column),
                absolute: c.absolute.unwrap_or(false),
            }),
        )
    };

    let prefix = (target != table).then(|| {
        let to = &tables.tables[target];
        let from_sheet = tables.tables[table].sheet;
        let unique = tables.name_count.get(&to.name) == Some(&1);
        Prefix {
            sheet: (to.sheet != from_sheet && !unique).then(|| Arc::clone(&to.sheet_name)),
            name: Arc::clone(&to.name),
        }
    });
    Ok((prefix, spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(52), "BA");
        assert_eq!(column_name(103), "CZ");
        assert_eq!(column_name(u32::MAX), "MWLQKWV");
    }

    #[test]
    fn decimals() {
        let high = |exp: i64| ((exp + 6176) as u64) << 49;
        assert_eq!(bid128_text(5, high(0)).unwrap(), "5");
        assert_eq!(bid128_text(1234, high(13)).unwrap(), "12340000000000000");
        assert_eq!(bid128_text(123_456, high(-3)).unwrap(), "123.456");
        assert_eq!(bid128_text(1000, high(-3)).unwrap(), "1");
        assert_eq!(bid128_text(5, high(-3)).unwrap(), "0.005");
        assert_eq!(bid128_text(5, high(0) | 1 << 63).unwrap(), "-5");
        assert_eq!(bid128_text(5, 3 << 61), None);
    }
}
