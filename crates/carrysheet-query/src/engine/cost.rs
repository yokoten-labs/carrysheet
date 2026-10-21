//! Bounds on the work SQLite can do between two time checks.
//!
//! SQLite runs the progress handler only when its program jumps (to the next
//! row, say). Everything between two jumps runs without a check: all of one
//! row's expressions, however many calls and comparisons they hold. SQLite
//! copies a subquery's or WITH table's expression into every place that uses
//! it, so the SQL text says little about this; the compiled program says it
//! all. After preparing a query we list its program (`EXPLAIN`), give each
//! instruction the longest it can take on values up to 256 KiB (see
//! [`Kind::micros`]), and refuse the query when the sum is over
//! [`BUDGET_MICROS`].
//!
//! This is an early, cheap refusal, not the time limit: a worker process
//! that is killed at the deadline bounds the time (`crate::process`). So
//! the list is default-deny: an instruction not known to take constant time
//! costs as much as a function call. LIKE, GLOB, `instr`, `replace` and
//! `trim` with a character set check the time themselves
//! ([`super::functions`]), so they count as cheap.

use rusqlite::{Connection, Result};

/// Most instructions in one compiled query (`SQLITE_LIMIT_VDBE_OP`). Building
/// a longer program fails while it is being built, before it runs.
pub(crate) const MAX_INSTRUCTIONS: usize = 50_000;

/// Most arguments in one call of `max`, `min` or `json_extract`, which
/// do a full pass over a value per argument. The program does not show how
/// many arguments a call has, so [`super::scan`] counts them in the text.
pub(crate) const MAX_ARGUMENTS: usize = 32;

/// The most work, in microseconds on the development machine, that the
/// instructions of one query may add up to: one row of a query that can
/// never finish within the default time limit is refused before it runs.
pub(crate) const BUDGET_MICROS: u64 = 1_000_000;

/// Values up to this many bytes, written into the program as constants, are
/// cheap to compare and copy.
const SMALL_CONSTANT: usize = 1024;

/// What an instruction does, for its cost.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub(crate) enum Kind {
    /// A call of a function that works through its arguments once.
    Call,
    /// Arithmetic, CAST, type conversions and truth tests: each may read a
    /// whole text value as a number.
    Arithmetic,
    /// `max`, `min` or `json_extract`: one pass per argument.
    ManyArguments,
    /// `||`.
    Concat,
    /// `=`, `<`, ... with the default (byte) collation.
    Compare,
    /// `=`, `<`, ... with NOCASE or another collation.
    CollatedCompare,
    /// Looking up or adding a large key in a temporary index (IN lists,
    /// DISTINCT, GROUP BY): up to 18 comparisons.
    Lookup,
    /// The same with NOCASE or another collation.
    CollatedLookup,
    /// Copying a value or building a record.
    Copy,
    /// Reading a JSON document for `json_each`.
    JsonTable,
    /// An instruction not known to take constant time.
    Unknown,
    /// Constant-time instructions (jumps, constants, cursor moves) and our
    /// stoppable functions.
    Other,
}

const KINDS: [Kind; 12] = [
    Kind::Call,
    Kind::Arithmetic,
    Kind::ManyArguments,
    Kind::Concat,
    Kind::Compare,
    Kind::CollatedCompare,
    Kind::Lookup,
    Kind::CollatedLookup,
    Kind::Copy,
    Kind::JsonTable,
    Kind::Unknown,
    Kind::Other,
];

impl Kind {
    /// The longest one instruction of this kind took on 256 KiB values
    /// (SQLite built with -O2, this machine), rounded up. Measured worst
    /// cases: `substr(t, -5)` 220 µs, `upper(t)` 130 µs, `json_extract` 115 µs
    /// per path, a NOCASE comparison 121 µs, `||` 125 µs, a byte comparison
    /// 20 µs, `+` on a 262,000-digit text 420 µs. Calls, arithmetic and
    /// unknown instructions all count 450 µs.
    pub(crate) fn micros(self) -> u64 {
        match self {
            Kind::Call | Kind::Arithmetic | Kind::Unknown => 450,
            Kind::ManyArguments => 125 * MAX_ARGUMENTS as u64,
            Kind::Concat => 150,
            Kind::Compare | Kind::Copy => 25,
            Kind::CollatedCompare => 150,
            Kind::Lookup => 18 * 25,
            Kind::CollatedLookup => 18 * 150,
            Kind::JsonTable => 3_000,
            Kind::Other => 2,
        }
    }

    /// What people call it, for the error message.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Kind::Call => "function calls",
            Kind::Arithmetic => "arithmetic operations, CASTs or type conversions",
            Kind::ManyArguments => "calls of max(), min() or json_extract()",
            Kind::Concat => "|| joins",
            Kind::Compare => "comparisons",
            Kind::CollatedCompare => "comparisons with COLLATE NOCASE (or another collation)",
            Kind::Lookup => "values in IN lists or lookups for DISTINCT and GROUP BY",
            Kind::CollatedLookup => {
                "values in IN lists or lookups for DISTINCT and GROUP BY with COLLATE NOCASE"
            }
            Kind::Copy => "copied values",
            Kind::JsonTable => "json_each() tables",
            Kind::Unknown => "other instructions",
            Kind::Other => "instructions",
        }
    }

    /// How many of these alone fit in the budget.
    pub(crate) fn cap(self) -> u64 {
        BUDGET_MICROS / self.micros()
    }
}

/// How many instructions of each kind a compiled query has.
#[derive(Debug, Default, Clone)]
pub(crate) struct Cost {
    counts: [u64; KINDS.len()],
}

impl Cost {
    fn add(&mut self, kind: Kind) {
        self.counts[kind as usize] += 1;
    }

    pub(crate) fn count(&self, kind: Kind) -> u64 {
        self.counts[kind as usize]
    }

    /// The longest the query's instructions can take together, in µs.
    pub(crate) fn micros(&self) -> u64 {
        KINDS
            .iter()
            .map(|&k| self.count(k).saturating_mul(k.micros()))
            .fold(0, u64::saturating_add)
    }

    /// The kind that adds the most.
    pub(crate) fn largest(&self) -> Kind {
        KINDS
            .iter()
            .copied()
            .max_by_key(|&k| self.count(k).saturating_mul(k.micros()))
            .unwrap_or(Kind::Other)
    }

    pub(crate) fn too_much(&self) -> bool {
        self.micros() > BUDGET_MICROS
    }
}

/// One line of `EXPLAIN`.
struct Op {
    opcode: String,
    p1: i64,
    p2: i64,
    p3: i64,
    p4: Option<String>,
}

/// Lists the program SQLite compiles `sql` to and adds up its cost. `sql`
/// must be one statement that is not itself an `EXPLAIN`.
pub(crate) fn measure(conn: &Connection, sql: &str) -> Result<Cost> {
    let mut stmt = conn.prepare(&format!("EXPLAIN {sql}"))?;
    let mut rows = stmt.query([])?;
    let mut ops = Vec::new();
    while let Some(row) = rows.next()? {
        ops.push(Op {
            opcode: row.get(1)?,
            p1: row.get(2)?,
            p2: row.get(3)?,
            p3: row.get(4)?,
            p4: row.get::<_, Option<String>>(5).unwrap_or(None),
        });
    }
    Ok(cost_of(&ops))
}

fn cost_of(ops: &[Op]) -> Cost {
    // Cursors over a temporary index whose key has a collation other than
    // the default, from the instruction that opens them.
    let collated: std::collections::HashSet<i64> = ops
        .iter()
        .filter(|op| op.opcode.starts_with("Open") || op.opcode == "SorterOpen")
        .filter(|op| op.p4.as_deref().is_some_and(key_info_is_collated))
        .map(|op| op.p1)
        .collect();
    let mut cost = Cost::default();
    for (i, op) in ops.iter().enumerate() {
        let kind = match op.opcode.as_str() {
            "Function" | "PureFunc" => function_kind(op.p4.as_deref().unwrap_or("")),
            "AggStep" | "AggStep1" | "AggValue" | "AggInverse" | "AggFinal" => Kind::Call,
            "Concat" => Kind::Concat,
            "Eq" | "Ne" | "Lt" | "Le" | "Gt" | "Ge" => {
                if op.p4.as_deref().is_some_and(collation_is_custom) {
                    Kind::CollatedCompare
                } else {
                    Kind::Compare
                }
            }
            "Compare" => {
                if op.p4.as_deref().is_some_and(key_info_is_collated) {
                    Kind::CollatedLookup
                } else {
                    Kind::Lookup
                }
            }
            "MakeRecord" if constant_key(ops, i + 1) => Kind::Other,
            "MakeRecord" | "Copy" | "Column" | "RowData" | "SorterData" | "VColumn" | "Insert"
            | "RowCell" => Kind::Copy,
            "IdxInsert" if constant_key(ops, i) => Kind::Other,
            "IdxInsert" | "SorterInsert" | "Found" | "NotFound" | "NoConflict" | "IfNoHope"
            | "SeekGE" | "SeekGT" | "SeekLE" | "SeekLT" | "IdxGE" | "IdxGT" | "IdxLE" | "IdxLT"
            | "IdxDelete" | "SorterCompare" | "IFindKey" | "Filter" | "FilterAdd" => {
                if collated.contains(&op.p1) {
                    Kind::CollatedLookup
                } else {
                    Kind::Lookup
                }
            }
            "VFilter" => Kind::JsonTable,
            "Add" | "Subtract" | "Multiply" | "Divide" | "Remainder" | "BitAnd" | "BitOr"
            | "ShiftLeft" | "ShiftRight" | "BitNot" | "AddImm" | "MustBeInt" | "Cast"
            | "Affinity" | "RealAffinity" | "IsTrue" | "Not" | "And" | "Or" | "If" | "IfNot" => {
                Kind::Arithmetic
            }
            op if CONSTANT_TIME.contains(&op) => Kind::Other,
            _ => Kind::Unknown,
        };
        cost.add(kind);
    }
    cost
}

/// Instructions whose time does not grow with the size of a value: jumps,
/// constants, register moves that share a value instead of copying it, and
/// cursor moves. (Sorting grows with the number of rows; the deadline bounds
/// it, not this list.) Everything not listed here or above counts as
/// [`Kind::Unknown`].
const CONSTANT_TIME: &[&str] = &[
    "Abortable",
    "AutoCommit",
    "BeginSubrtn",
    "Blob",
    "Close",
    "ClrSubtype",
    "CollSeq",
    "ColumnsUsed",
    "Count",
    "CursorHint",
    "CursorLock",
    "CursorUnlock",
    "DecrJumpZero",
    "DeferredSeek",
    "Delete",
    "ElseEq",
    "EndCoroutine",
    "Explain",
    "FinishSeek",
    "GetSubtype",
    "Gosub",
    "Goto",
    "Halt",
    "HaltIfNull",
    "IdxRowid",
    "IfEmpty",
    "IfNotOpen",
    "IfNotZero",
    "IfNullRow",
    "IfPos",
    "IfSizeBetween",
    "Init",
    "InitCoroutine",
    "Int64",
    "IntCopy",
    "Integer",
    "IsNull",
    "IsType",
    "Jump",
    "Last",
    "MemMax",
    "Move",
    "NewRowid",
    "Next",
    "Noop",
    "NotExists",
    "NotNull",
    "Null",
    "NullRow",
    "Offset",
    "OffsetLimit",
    "Once",
    "OpenAutoindex",
    "OpenDup",
    "OpenEphemeral",
    "OpenPseudo",
    "OpenRead",
    "Permutation",
    "Prev",
    "ReadCookie",
    "Real",
    "ReleaseReg",
    "ReopenIdx",
    "ResetCount",
    "ResetSorter",
    "ResultRow",
    "Return",
    "Rewind",
    "Rowid",
    "SCopy",
    "SeekEnd",
    "SeekHit",
    "SeekRowid",
    "SeekScan",
    "Sequence",
    "SequenceTest",
    "SetSubtype",
    "SoftNull",
    "Sort",
    "SorterNext",
    "SorterOpen",
    "SorterSort",
    "String",
    "String8",
    "TableLock",
    "Trace",
    "Transaction",
    "TypeCheck",
    "Variable",
    "VInitIn",
    "VNext",
    "VOpen",
    "Yield",
    "ZeroOrNull",
];

/// True when `ops[i]` adds one small constant to a temporary index: the
/// three instructions SQLite writes for each value of a constant IN list,
/// `String8`/`Integer` into r, `MakeRecord r 1 k`, `IdxInsert c k`.
fn constant_key(ops: &[Op], i: usize) -> bool {
    let (Some(insert), Some(record), Some(value)) = (
        ops.get(i),
        i.checked_sub(1).and_then(|j| ops.get(j)),
        i.checked_sub(2).and_then(|j| ops.get(j)),
    ) else {
        return false;
    };
    let small = match value.opcode.as_str() {
        "Integer" | "Int64" | "Real" => true,
        "String8" | "String" | "Blob" => {
            value.p4.as_ref().is_some_and(|s| s.len() <= SMALL_CONSTANT)
        }
        _ => false,
    };
    insert.opcode == "IdxInsert"
        && record.opcode == "MakeRecord"
        && small
        && record.p1 == value.p2
        && record.p2 == 1
        && insert.p2 == record.p3
}

/// The kind of a function call from its `EXPLAIN` text, `name(nArg)`.
fn function_kind(p4: &str) -> Kind {
    let (name, args) = match p4.split_once('(') {
        Some((name, rest)) => (name, rest.trim_end_matches(')')),
        None => (p4, ""),
    };
    let name = name.to_ascii_lowercase();
    let args: i64 = args.parse().unwrap_or(-1);
    match name.as_str() {
        // Ours: they stop at the time limit themselves.
        "like" | "glob" | "instr" | "replace" => Kind::Other,
        "trim" | "ltrim" | "rtrim" if args == 2 => Kind::Other,
        // Scalar max and min (variadic), and json_extract with any number
        // of paths.
        "max" | "min" | "json_extract" => Kind::ManyArguments,
        _ => Kind::Call,
    }
}

/// True for an `EXPLAIN` collation such as `NOCASE-8`, false for the byte
/// collations BINARY and RTRIM.
fn collation_is_custom(p4: &str) -> bool {
    let name = p4.split('-').next().unwrap_or("");
    !(name.eq_ignore_ascii_case("BINARY") || name.eq_ignore_ascii_case("RTRIM"))
}

/// True for an `EXPLAIN` key description such as `k(2,NOCASE,B)` with a
/// collation other than BINARY (`B`) and RTRIM.
fn key_info_is_collated(p4: &str) -> bool {
    let Some(inner) = p4.strip_prefix("k(").and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    inner.split(',').skip(1).any(|part| {
        let name = part.trim_start_matches('-');
        !(name.is_empty()
            || name == "B"
            || name.eq_ignore_ascii_case("BINARY")
            || name.eq_ignore_ascii_case("RTRIM"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(sql: &str) -> Cost {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (a, b);").unwrap();
        measure(&conn, sql).unwrap()
    }

    #[test]
    fn kinds_come_from_the_compiled_program() {
        let c = cost(
            "SELECT upper(a), a || b, a = b, a = b COLLATE NOCASE, max(a, b), a LIKE 'x%' FROM t",
        );
        assert_eq!(c.count(Kind::Call), 1, "{c:?}");
        assert_eq!(c.count(Kind::Concat), 1, "{c:?}");
        assert_eq!(c.count(Kind::Compare), 1, "{c:?}");
        assert_eq!(c.count(Kind::CollatedCompare), 1, "{c:?}");
        assert_eq!(c.count(Kind::ManyArguments), 1, "{c:?}");
        assert!(!c.too_much());
    }

    #[test]
    fn arithmetic_and_unknown_instructions_are_not_free() {
        let c = cost("SELECT a + b, CAST(a AS REAL), -a FROM t");
        assert!(c.count(Kind::Arithmetic) >= 2, "{c:?}");
        let sums = vec!["(a+a+a+a+a+a+a+a+a+a)"; 400].join(", ");
        let c = cost(&format!("SELECT {sums} FROM t"));
        assert!(c.too_much(), "{c:?}");
        assert_eq!(c.largest(), Kind::Arithmetic);
    }

    #[test]
    fn copies_made_by_subqueries_count() {
        // The subquery's column, two calls, is copied into all 2,000 places.
        let refs = vec!["k"; 2000].join(", ");
        let c = cost(&format!(
            "SELECT {refs} FROM (SELECT upper(lower(a)) AS k FROM t)"
        ));
        assert_eq!(c.count(Kind::Call), 4000, "{c:?}");
        assert!(c.too_much());
        assert_eq!(c.largest(), Kind::Call);
    }

    #[test]
    fn constant_in_lists_are_cheap_but_collated_lists_are_not() {
        let list = (0..1000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let c = cost(&format!("SELECT a IN ({list}) FROM t"));
        assert!(!c.too_much(), "{c:?}");
        let c = cost("SELECT count(DISTINCT a COLLATE NOCASE) FROM t");
        assert!(c.count(Kind::CollatedLookup) >= 1, "{c:?}");
    }

    #[test]
    fn collation_names() {
        assert!(collation_is_custom("NOCASE-8"));
        assert!(!collation_is_custom("BINARY-8"));
        assert!(!collation_is_custom("RTRIM-8"));
        assert!(key_info_is_collated("k(2,NOCASE,B)"));
        assert!(key_info_is_collated("k(1,-NOCASE)"));
        assert!(!key_info_is_collated("k(2,B,-B)"));
        assert!(!key_info_is_collated("k(1,RTRIM)"));
    }
}
