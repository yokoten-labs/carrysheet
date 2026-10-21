//! Detection: a bounded sample of rows votes on the decimal mark, the digit
//! grouping and the date order, column by column; then every cell is
//! classified under its column's decision.
//!
//! A column decides from its own values. Only when its own values are all
//! ambiguous does it borrow the decision of the other columns of the same
//! kind, and only when they cast enough votes; every value read that way
//! gets a note.

use std::cmp::Reverse;

use super::date::{looks_like_excel_date, looks_like_gene, parse_date};
use super::number::{GroupAccept, MAX_VALUE_BYTES, NumberParse, parse_number};
use super::report::{
    Analysis, ColumnKind, ColumnReport, Decision, Note, NoteKind, Notes, printable,
};
use super::value::{Number, Time, Value};
use super::{Convention, ConventionChoice, DateOrder, DecimalMark, Grouping, Header, normalize};
use crate::encoding::Confidence;
use crate::read::Detected;

/// Detection looks at no more than this many rows after the header...
pub(crate) const SAMPLE_ROWS: usize = 1_000;
/// ...this many bytes of field text...
pub(crate) const SAMPLE_BYTES: usize = 1 << 20;
/// ...and this many cells. Whatever comes first ends the sample.
pub(crate) const SAMPLE_CELLS: usize = 200_000;
/// Columns past this one (Excel's last) are not typed; their cells stay text.
pub(crate) const MAX_COLUMNS: usize = 1 << 14;
/// Header names in the report are cut to this many bytes.
const HEADER_NAME_BYTES: usize = 64;
/// A column whose own values are all ambiguous borrows the decision of the
/// other columns only when they cast at least this many votes.
pub(crate) const MIN_LEND_VOTES: usize = 10;

/// Where a decision came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The caller chose it.
    Chosen,
    /// The column's own values settled it, this surely.
    Votes(Confidence),
    /// The column could not settle it; the other columns did.
    Borrowed,
    /// The first value that settles it came after the sample.
    Late,
}

impl Source {
    /// The note for values whose reading depends on this decision.
    fn note(self, decimal: bool) -> Option<NoteKind> {
        match (self, decimal) {
            (Source::Borrowed, true) => Some(NoteKind::BorrowedDecimal),
            (Source::Borrowed, false) => Some(NoteKind::BorrowedDateOrder),
            (Source::Late, _) => Some(NoteKind::SettledLate),
            _ => None,
        }
    }
}

/// A column's decision about the decimal mark or the date order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark<T> {
    Fixed(T, Source),
    /// Nothing settled it yet; only values that read the same either way
    /// are typed, and the first value that settles it fixes it.
    Open,
    /// The values disagree; only values that read the same either way are
    /// typed.
    Mixed,
}

/// How to classify the cells of one column.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnPlan {
    /// `None` types every value it can, as [`Value::parse`] does.
    pub kind: Option<ColumnKind>,
    pub decimal: Mark<DecimalMark>,
    pub grouping: GroupAccept,
    pub date_order: Mark<DateOrder>,
    pub pivot: u16,
    /// Notes about the column as a whole, such as a conflict with the
    /// other columns.
    pub column_notes: Vec<Note>,
}

impl ColumnPlan {
    pub(crate) fn fixed(c: &Convention) -> Self {
        Self {
            kind: None,
            decimal: Mark::Fixed(c.decimal, Source::Chosen),
            grouping: GroupAccept {
                only: Some(c.grouping),
                space_integers: c.grouping.same_family(Grouping::Space),
                nbsp_integers: c.decimal == DecimalMark::Comma,
            },
            date_order: Mark::Fixed(c.date_order, Source::Chosen),
            pivot: c.two_digit_pivot,
            column_notes: Vec::new(),
        }
    }
}

/// What one cell is under its column's plan.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Class {
    Empty,
    Bool(bool),
    Number(Number),
    /// A number whose reading depends on a borrowed or late decision.
    NumberVia(Number, NoteKind),
    /// A date or a date with time; `true` when it used a two-digit year.
    Date(Value, bool),
    /// A date whose reading depends on a borrowed or late decision.
    DateVia(Value, NoteKind),
    Time(Time),
    LeadingZero,
    TooManyDigits,
    PhoneLike,
    TooSmall,
    /// A whole number grouped with spaces that are not accepted.
    SpaceGrouped(Spaced),
    AmbiguousNumber,
    OtherDecimal,
    AmbiguousDate,
    OtherDateOrder,
    ExcelDate,
    Gene,
    Text,
}

impl Class {
    /// The column kind this cell counts toward.
    fn kind(&self) -> Option<ColumnKind> {
        Some(match self {
            Class::Number(_)
            | Class::NumberVia(..)
            | Class::AmbiguousNumber
            | Class::OtherDecimal => ColumnKind::Number,
            Class::Date(..) | Class::DateVia(..) | Class::AmbiguousDate | Class::OtherDateOrder => {
                ColumnKind::Date
            }
            Class::Time(_) => ColumnKind::Time,
            Class::Bool(_) => ColumnKind::Bool,
            _ => return None,
        })
    }

    /// Counts toward a kind but stays text.
    fn unsettled(&self) -> bool {
        matches!(
            self,
            Class::AmbiguousNumber
                | Class::OtherDecimal
                | Class::AmbiguousDate
                | Class::OtherDateOrder
        )
    }

    /// The typed value, whatever the column.
    pub(crate) fn into_any_value(self) -> Option<Value> {
        match self {
            Class::Empty => Some(Value::Empty),
            Class::Bool(b) => Some(Value::Bool(b)),
            Class::Number(n) | Class::NumberVia(n, _) => Some(Value::Number(n)),
            Class::Date(v, _) | Class::DateVia(v, _) => Some(v),
            Class::Time(t) => Some(Value::Time(t)),
            _ => None,
        }
    }

    /// The typed value in a column of `kind`, or `None` for text.
    fn into_value(self, kind: ColumnKind) -> Option<Value> {
        if self == Class::Empty {
            return Some(Value::Empty);
        }
        if self.kind() == Some(kind) {
            self.into_any_value()
        } else {
            None
        }
    }

    /// The note this cell adds in a column of `kind`.
    fn note(&self, kind: ColumnKind, pivot: u16) -> Option<NoteKind> {
        let special = match self {
            Class::LeadingZero => Some(NoteKind::LeadingZeros),
            Class::TooManyDigits => Some(NoteKind::TooManyDigits),
            Class::PhoneLike => Some(NoteKind::PhoneNumbers),
            Class::TooSmall => Some(NoteKind::TooSmall),
            Class::SpaceGrouped(Spaced::Maybe) => Some(NoteKind::SpaceGrouped),
            Class::ExcelDate => Some(NoteKind::LooksLikeDate),
            Class::Gene => Some(NoteKind::GeneName),
            Class::AmbiguousNumber => Some(NoteKind::AmbiguousDecimal),
            Class::OtherDecimal => Some(NoteKind::OtherDecimal),
            Class::AmbiguousDate => Some(NoteKind::AmbiguousDate),
            Class::OtherDateOrder => Some(NoteKind::OtherDateOrder),
            _ => None,
        };
        if special.is_some() {
            return special;
        }
        match (kind, self) {
            (ColumnKind::Text | ColumnKind::Empty, _) | (_, Class::Empty) => None,
            _ if self.kind() != Some(kind) => Some(NoteKind::OtherKind(kind)),
            (_, Class::NumberVia(_, note) | Class::DateVia(_, note)) => Some(*note),
            (ColumnKind::Number, Class::SpaceGrouped(Spaced::Unsettled)) => {
                Some(NoteKind::BeforeSettled)
            }
            (ColumnKind::Date, Class::Date(_, true)) => Some(NoteKind::TwoDigitYears {
                window_start: pivot,
            }),
            _ => None,
        }
    }
}

/// Classifies one cell. `normalized` is `normalize(text)`. An open decision
/// in a typed column is fixed by the first value that settles it.
pub(crate) fn classify(text: &str, normalized: Option<&str>, plan: &mut ColumnPlan) -> Class {
    if text.is_empty() {
        return Class::Empty;
    }
    let Some(n) = normalized.filter(|n| !n.is_empty()) else {
        return Class::Text;
    };
    if n.eq_ignore_ascii_case("true") {
        return Class::Bool(true);
    }
    if n.eq_ignore_ascii_case("false") {
        return Class::Bool(false);
    }
    if let Some(class) = classify_number(n, plan) {
        return class;
    }
    if let Some(class) = classify_date(n, plan) {
        return class;
    }
    if looks_like_gene(n) {
        return Class::Gene;
    }
    if looks_like_excel_date(n) {
        return Class::ExcelDate;
    }
    Class::Text
}

fn classify_number(n: &str, plan: &mut ColumnPlan) -> Option<Class> {
    let accept = plan.grouping;
    if let Mark::Fixed(d, source) = plan.decimal {
        return match parse_number(n, d, accept) {
            NumberParse::Number(number, _) => Some(match source.note(true) {
                Some(note) if depends_on_mark(n, d, &number, accept) => {
                    Class::NumberVia(number, note)
                }
                _ => number_class(n, number),
            }),
            // Only text with a `.` or `,` can read with the other mark.
            NumberParse::Not if !n.contains(['.', ',']) => None,
            NumberParse::Not => {
                let here = parse_number(n, d, GroupAccept::ANY).number();
                let there = parse_number(n, other_mark(d), GroupAccept::ANY).number();
                (here.is_none() && there.is_some()).then_some(Class::OtherDecimal)
            }
            other => text_class(other, accept, false),
        };
    }
    let point = parse_number(n, DecimalMark::Point, accept);
    let comma = parse_number(n, DecimalMark::Comma, accept);
    match (point, comma) {
        (NumberParse::Number(a, _), NumberParse::Number(b, _)) => Some(if a.value == b.value {
            number_class(n, a)
        } else {
            Class::AmbiguousNumber
        }),
        (NumberParse::Number(a, _), _) => Some(settle_decimal(plan, DecimalMark::Point, n, a)),
        (_, NumberParse::Number(b, _)) => Some(settle_decimal(plan, DecimalMark::Comma, n, b)),
        (NumberParse::Not, other) | (other, _) => text_class(other, accept, true),
    }
}

/// Why a space-grouped whole number stays text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Spaced {
    /// The decimal comma applies, so it may well be a number (a note says
    /// so).
    Maybe,
    /// The decimal mark is not settled yet; a later value may settle it.
    Unsettled,
    /// The decimal point applies: spaces here are sizes or codes.
    Plain,
}

/// The class of a value that stays text for a reason, or `None`. `open`:
/// the column's decimal mark is not settled.
fn text_class(parse: NumberParse, accept: GroupAccept, open: bool) -> Option<Class> {
    match parse {
        NumberParse::SpaceGrouped => Some(Class::SpaceGrouped(if accept.nbsp_integers {
            Spaced::Maybe
        } else if open {
            Spaced::Unsettled
        } else {
            Spaced::Plain
        })),
        NumberParse::LeadingZero => Some(Class::LeadingZero),
        NumberParse::TooManyDigits => Some(Class::TooManyDigits),
        NumberParse::PhoneLike => Some(Class::PhoneLike),
        NumberParse::TooSmall => Some(Class::TooSmall),
        NumberParse::Number(..) | NumberParse::Not => None,
    }
}

/// A number, with a note when it may be a code: `1E5` reads as 100000, as
/// Excel reads it, but is often a part or batch number.
fn number_class(n: &str, number: Number) -> Class {
    if looks_like_code(n) {
        Class::NumberVia(number, NoteKind::LikeCode)
    } else {
        Class::Number(number)
    }
}

/// Digits, `E`, digits: `1E5`, `12E4`.
fn looks_like_code(n: &str) -> bool {
    n.split_once(['e', 'E']).is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
    })
}

/// The value reads differently, or not at all, with the other mark.
fn depends_on_mark(n: &str, d: DecimalMark, number: &Number, accept: GroupAccept) -> bool {
    parse_number(n, other_mark(d), accept)
        .number()
        .is_none_or(|(other, _)| other.value != number.value)
}

/// Only a column of that kind (or a single value) settles an open decision.
fn may_settle(plan: &ColumnPlan, kind: ColumnKind) -> bool {
    plan.kind.is_none_or(|k| k == kind)
}

fn settle_decimal(plan: &mut ColumnPlan, mark: DecimalMark, n: &str, number: Number) -> Class {
    match plan.decimal {
        Mark::Open if may_settle(plan, ColumnKind::Number) && plan.kind.is_some() => {
            plan.decimal = Mark::Fixed(mark, Source::Late);
            plan.grouping.nbsp_integers |= mark == DecimalMark::Comma;
            Class::NumberVia(number, NoteKind::SettledLate)
        }
        Mark::Open => number_class(n, number),
        _ => Class::OtherDecimal,
    }
}

pub(crate) fn other_mark(d: DecimalMark) -> DecimalMark {
    match d {
        DecimalMark::Point => DecimalMark::Comma,
        DecimalMark::Comma => DecimalMark::Point,
    }
}

fn classify_date(n: &str, plan: &mut ColumnPlan) -> Option<Class> {
    let parse = parse_date(n, plan.pivot)?;
    // `10.20.30` with dots and a two-digit year reads as a date only when
    // the column's own dates (or the caller) settled the order.
    let dotted_ok = matches!(
        plan.date_order,
        Mark::Fixed(_, Source::Chosen | Source::Votes(_))
    );
    let usable = |i: usize| {
        parse.by_order.get(i).is_some_and(Option::is_some)
            && (dotted_ok
                || parse.separator != Some('.')
                || !parse.two_digit.get(i).copied().unwrap_or(false))
    };
    let as_class = |order: usize| -> Option<Class> {
        let value = parse.by_order.get(order)?.clone()?;
        let two = parse.two_digit.get(order).copied().unwrap_or(false);
        Some(match value {
            Value::Time(t) => Class::Time(t),
            other => Class::Date(other, two),
        })
    };
    let mut orders = [0usize; 3];
    let mut count = 0;
    for i in 0..3 {
        if usable(i)
            && let Some(slot) = orders.get_mut(count)
        {
            *slot = i;
            count += 1;
        }
    }
    let valid = orders.get(..count).unwrap_or_default();
    let first = *valid.first()?;
    // Neutral: two or more orders read it, all the same way.
    let same = valid.len() >= 2
        && valid
            .iter()
            .all(|&i| parse.by_order.get(i) == parse.by_order.get(first));
    match plan.date_order {
        Mark::Fixed(order, source) => {
            if valid.contains(&order.index()) {
                let class = as_class(order.index())?;
                return Some(match (source.note(false), class) {
                    (Some(note), Class::Date(v, _)) if !same => Class::DateVia(v, note),
                    (_, class) => class,
                });
            }
            Some(if same {
                as_class(first)?
            } else {
                Class::OtherDateOrder
            })
        }
        _ if same => as_class(first),
        Mark::Open => match valid[..] {
            [only] if may_settle(plan, ColumnKind::Date) && plan.kind.is_some() => {
                let order = *DateOrder::ALL.get(only)?;
                plan.date_order = Mark::Fixed(order, Source::Late);
                Some(match as_class(only)? {
                    Class::Date(v, _) => Class::DateVia(v, NoteKind::SettledLate),
                    other => other,
                })
            }
            [only] => as_class(only),
            _ => Some(Class::AmbiguousDate),
        },
        Mark::Mixed => Some(if valid.len() == 1 {
            Class::OtherDateOrder
        } else {
            Class::AmbiguousDate
        }),
    }
}

/// What the sample says about one column.
#[derive(Clone, Debug, Default)]
struct Votes {
    /// Values that read only with `.` / only with `,`.
    decimal: [usize; 2],
    /// The first row of such a value.
    decimal_first: [usize; 2],
    /// Values that read differently with `.` and `,`, like `1,234`.
    decimal_ambiguous: usize,
    /// Grouping seen, per decimal mark the value was read with.
    grouping: [[usize; 7]; 2],
    /// Space-grouped values with a fraction, like `1 234,5`.
    space_fractions: usize,
    /// Dates that read only in DMY / MDY / YMD.
    order: [usize; 3],
    /// The first row of such a date.
    order_first: [usize; 3],
    /// Dates that read differently in two orders, like `03/04/2026`.
    order_ambiguous: usize,
    /// Date separators seen: `/`, `-`, `.`.
    separators: [usize; 3],
}

impl Votes {
    fn add(&mut self, n: &str, accept: GroupAccept, pivot: u16, row: usize) {
        let point = parse_number(n, DecimalMark::Point, accept).number();
        let comma = parse_number(n, DecimalMark::Comma, accept).number();
        let mut seen = |side: usize, number: &Number, g: Grouping| {
            if let Some(counts) = self.grouping.get_mut(side) {
                bump(counts, g.index());
            }
            if g.same_family(Grouping::Space) && number.format.decimals > 0 {
                self.space_fractions += 1;
            }
        };
        match (point, comma) {
            (Some((a, ga)), Some((b, gb))) => {
                if a.value == b.value {
                    seen(0, &a, ga);
                    seen(1, &b, gb);
                } else {
                    self.decimal_ambiguous += 1;
                }
            }
            (Some((a, g)), None) => {
                seen(0, &a, g);
                vote(&mut self.decimal, &mut self.decimal_first, 0, row);
            }
            (None, Some((b, g))) => {
                seen(1, &b, g);
                vote(&mut self.decimal, &mut self.decimal_first, 1, row);
            }
            (None, None) => {}
        }
        let Some(parse) = parse_date(n, pivot) else {
            return;
        };
        // Two-digit years with dots (`10.20.30`) are too often versions or
        // codes to vote.
        let dotted = parse.separator == Some('.');
        let valid: Vec<usize> = (0..3)
            .filter(|&i| {
                parse.by_order.get(i).is_some_and(Option::is_some)
                    && !(dotted && parse.two_digit.get(i).copied().unwrap_or(false))
            })
            .collect();
        let Some(&first) = valid.first() else {
            return;
        };
        if let Some(i) = parse
            .separator
            .and_then(|s| ['/', '-', '.'].iter().position(|&c| c == s))
        {
            bump(&mut self.separators, i);
        }
        if valid.len() >= 2
            && valid
                .iter()
                .all(|&i| parse.by_order.get(i) == parse.by_order.get(first))
        {
            return;
        }
        if let [only] = valid[..] {
            vote(&mut self.order, &mut self.order_first, only, row);
        } else {
            self.order_ambiguous += 1;
        }
    }
}

fn vote(counts: &mut [usize], first: &mut [usize], i: usize, row: usize) {
    if let (Some(c), Some(f)) = (counts.get_mut(i), first.get_mut(i)) {
        if *c == 0 {
            *f = row;
        }
        *c += 1;
    }
}

fn bump(counts: &mut [usize], i: usize) {
    if let Some(c) = counts.get_mut(i) {
        *c += 1;
    }
}

/// How the votes come out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    NoVotes,
    /// The winner has at least three quarters of the votes.
    Decided(usize, Confidence),
    Mixed,
}

fn verdict(votes: &[usize]) -> Verdict {
    let total: usize = votes.iter().sum();
    let Some((best, &top)) = votes
        .iter()
        .enumerate()
        .max_by_key(|(i, v)| (**v, Reverse(*i)))
    else {
        return Verdict::NoVotes;
    };
    if total == 0 {
        Verdict::NoVotes
    } else if top.saturating_mul(4) >= total.saturating_mul(3) {
        let confidence = if top == total {
            Confidence::High
        } else {
            Confidence::Medium
        };
        Verdict::Decided(best, confidence)
    } else {
        Verdict::Mixed
    }
}

/// How the sampled cells of a column classify under its plan.
#[derive(Clone, Debug, Default)]
struct Fits {
    nonempty: usize,
    by_kind: [usize; 4],
    leading_zero: usize,
    too_many_digits: usize,
    too_small: usize,
    phone: usize,
    /// Space-grouped whole numbers that may be numbers (with a note).
    space_grouped: usize,
    /// Every space-grouped whole number left as text.
    spaced_any: usize,
    number_ambiguous: usize,
    date_ambiguous: usize,
    /// Values that count toward a kind but stay text: ambiguous, or in the
    /// other convention.
    unsettled: [usize; 4],
}

const TYPED_KINDS: [ColumnKind; 4] = [
    ColumnKind::Number,
    ColumnKind::Date,
    ColumnKind::Time,
    ColumnKind::Bool,
];

impl Fits {
    fn add(&mut self, class: &Class) {
        if *class == Class::Empty {
            return;
        }
        self.nonempty += 1;
        match class {
            Class::LeadingZero => self.leading_zero += 1,
            Class::TooManyDigits => self.too_many_digits += 1,
            Class::TooSmall => self.too_small += 1,
            Class::PhoneLike => self.phone += 1,
            Class::SpaceGrouped(spaced) => {
                self.spaced_any += 1;
                if *spaced == Spaced::Maybe {
                    self.space_grouped += 1;
                }
            }
            Class::AmbiguousNumber => self.number_ambiguous += 1,
            Class::AmbiguousDate => self.date_ambiguous += 1,
            _ => {}
        }
        if let Some(i) = class
            .kind()
            .and_then(|k| TYPED_KINDS.iter().position(|&t| t == k))
        {
            let counts = if class.unsettled() {
                &mut self.unsettled
            } else {
                &mut self.by_kind
            };
            bump(counts, i);
        }
    }

    fn loses_information(&self) -> bool {
        self.leading_zero + self.too_many_digits + self.too_small + self.phone > 0
    }

    /// The kind with the most values, counting those that stay text because
    /// their convention is unclear, and how many are typed and in all.
    fn best(&self) -> Option<(ColumnKind, usize, usize)> {
        (0..TYPED_KINDS.len())
            .map(|i| {
                let typed = self.by_kind.get(i).copied().unwrap_or(0);
                let total = typed + self.unsettled.get(i).copied().unwrap_or(0);
                (i, typed, total)
            })
            .max_by_key(|&(i, _, total)| (total, Reverse(i)))
            .and_then(|(i, typed, total)| Some((*TYPED_KINDS.get(i)?, typed, total)))
    }

    /// Typed when one kind covers more than half of the values (counting
    /// those that stay text because their convention is unclear), most of
    /// those are typed, and nothing would lose information; text otherwise.
    /// Also returns how many values are typed.
    fn kind(&self) -> (ColumnKind, usize) {
        if self.nonempty == 0 {
            return (ColumnKind::Empty, 0);
        }
        if self.loses_information() {
            return (ColumnKind::Text, 0);
        }
        match self.best() {
            Some((kind, typed, total)) if typed * 2 > total && total * 2 > self.nonempty => {
                (kind, typed)
            }
            _ => (ColumnKind::Text, 0),
        }
    }

    /// The kind the column would have if every unclear value were settled:
    /// only such columns vote for the whole file.
    fn loose_kind(&self) -> ColumnKind {
        if self.nonempty == 0 || self.loses_information() {
            return ColumnKind::Text;
        }
        // Space-grouped whole numbers count as numbers here, so that a
        // column of them can take the decimal comma's no-break-space rule.
        let numbers = self.by_kind.first().copied().unwrap_or(0)
            + self.unsettled.first().copied().unwrap_or(0)
            + self.spaced_any;
        match self.best() {
            _ if numbers * 2 > self.nonempty => ColumnKind::Number,
            Some((kind, _, total)) if total * 2 > self.nonempty => kind,
            _ => ColumnKind::Text,
        }
    }

    /// The kind most values would have if their convention were clear.
    fn unsettled_kind(&self) -> Option<ColumnKind> {
        let (i, &count) = self
            .unsettled
            .iter()
            .enumerate()
            .max_by_key(|(i, v)| (**v, Reverse(*i)))?;
        if count * 2 > self.nonempty {
            TYPED_KINDS.get(i).copied()
        } else {
            None
        }
    }
}

/// The votes of the columns of one kind, for the whole file.
#[derive(Clone, Copy, Debug)]
struct FileVotes<const N: usize> {
    votes: [usize; N],
    ambiguous: usize,
    verdict: Verdict,
}

impl<const N: usize> FileVotes<N> {
    fn new(votes: [usize; N], ambiguous: usize) -> Self {
        Self {
            votes,
            ambiguous,
            verdict: verdict(&votes),
        }
    }

    fn total(&self) -> usize {
        self.votes.iter().sum()
    }

    /// The decision a column with only ambiguous values may borrow.
    fn lendable(&self) -> Option<usize> {
        match self.verdict {
            Verdict::Decided(i, _) if self.total() >= MIN_LEND_VOTES => Some(i),
            _ => None,
        }
    }
}

/// The plans for every column, from a sample starting at row `start`.
struct Planned {
    plans: Vec<ColumnPlan>,
    votes: Vec<Votes>,
    fits: Vec<Fits>,
    decimal: FileVotes<2>,
    order: FileVotes<3>,
    /// Rows `start..end` are the sample.
    end: usize,
    bytes: usize,
}

fn cells<'a, R: AsRef<[S]>, S: AsRef<str> + 'a>(row: &'a R) -> impl Iterator<Item = &'a str> + 'a {
    row.as_ref().iter().map(AsRef::as_ref)
}

fn sample_end<R: AsRef<[S]>, S: AsRef<str>>(rows: &[R], start: usize) -> (usize, usize) {
    let mut bytes = 0usize;
    let mut count = 0usize;
    let mut end = start;
    for row in rows.iter().skip(start).take(SAMPLE_ROWS) {
        let row = row.as_ref();
        let row_cells = row.len().min(MAX_COLUMNS);
        // A cell longer than the parsers look at counts only that much.
        let row_bytes = row
            .iter()
            .take(MAX_COLUMNS)
            .map(|s| s.as_ref().len().min(MAX_VALUE_BYTES))
            .fold(0usize, usize::saturating_add);
        // The row that would pass a budget ends the sample.
        // The first row is always read, so the sample is never empty.
        if end > start
            && (bytes.saturating_add(row_bytes) > SAMPLE_BYTES
                || count.saturating_add(row_cells) > SAMPLE_CELLS)
        {
            break;
        }
        count = count.saturating_add(row_cells);
        bytes = bytes.saturating_add(row_bytes);
        end += 1;
    }
    (end, bytes)
}

/// The column's own decision: chosen, or from its own votes.
fn own_mark<T: Copy>(chosen: Option<T>, votes: &[usize], all: &[T]) -> Mark<T> {
    if let Some(t) = chosen {
        return Mark::Fixed(t, Source::Chosen);
    }
    match verdict(votes) {
        Verdict::Decided(i, c) => all
            .get(i)
            .map_or(Mark::Open, |&t| Mark::Fixed(t, Source::Votes(c))),
        Verdict::Mixed => Mark::Mixed,
        Verdict::NoVotes => Mark::Open,
    }
}

fn classify_sample<R: AsRef<[S]>, S: AsRef<str>>(
    sample: &[R],
    plans: &mut [ColumnPlan],
) -> Vec<Fits> {
    let mut fits = vec![Fits::default(); plans.len()];
    for row in sample {
        for ((text, plan), fit) in cells(row).zip(plans.iter_mut()).zip(fits.iter_mut()) {
            fit.add(&classify(text, normalize(text).as_deref(), plan));
        }
    }
    fits
}

fn plan_all<R: AsRef<[S]>, S: AsRef<str>>(
    rows: &[R],
    start: usize,
    choice: &ConventionChoice,
) -> Planned {
    let (end, bytes) = sample_end(rows, start);
    let sample = rows.get(start..end).unwrap_or_default();
    let width = sample
        .iter()
        .map(|r| r.as_ref().len())
        .max()
        .unwrap_or(0)
        .min(MAX_COLUMNS);

    let chosen_space = choice
        .grouping
        .is_some_and(|g| g.same_family(Grouping::Space));
    let chosen_comma = choice.decimal == Some(DecimalMark::Comma);
    let accept = GroupAccept {
        only: choice.grouping,
        space_integers: chosen_space,
        nbsp_integers: false,
    };
    let mut votes = vec![Votes::default(); width];
    for (k, row) in sample.iter().enumerate() {
        for (text, v) in cells(row).zip(votes.iter_mut()) {
            if let Some(n) = normalize(text).filter(|n| !n.is_empty()) {
                v.add(&n, accept, choice.two_digit_pivot, start + k + 1);
            }
        }
    }

    // First pass: each column on its own, to learn what kind it is.
    let own: Vec<(Mark<DecimalMark>, Mark<DateOrder>)> = votes
        .iter()
        .map(|v| {
            (
                own_mark(choice.decimal, &v.decimal, &DecimalMark::ALL),
                own_mark(choice.date_order, &v.order, &DateOrder::ALL),
            )
        })
        .collect();
    let mut plans: Vec<ColumnPlan> = own
        .iter()
        .zip(&votes)
        .map(|(&(decimal, date_order), v)| ColumnPlan {
            kind: Some(ColumnKind::Text),
            decimal,
            grouping: GroupAccept {
                space_integers: chosen_space || v.space_fractions > 0,
                nbsp_integers: matches!(decimal, Mark::Fixed(DecimalMark::Comma, _)),
                ..accept
            },
            date_order,
            pivot: choice.two_digit_pivot,
            column_notes: Vec::new(),
        })
        .collect();
    let loose: Vec<ColumnKind> = classify_sample(sample, &mut plans)
        .iter()
        .map(Fits::loose_kind)
        .collect();

    // The file's votes come only from columns of that kind.
    let mut decimal_votes = [0usize; 2];
    let mut decimal_ambiguous = 0;
    let mut order_votes = [0usize; 3];
    let mut order_ambiguous = 0;
    for (v, kind) in votes.iter().zip(&loose) {
        if *kind == ColumnKind::Number {
            add_into(&mut decimal_votes, &v.decimal);
            decimal_ambiguous += v.decimal_ambiguous;
        }
        if *kind == ColumnKind::Date {
            add_into(&mut order_votes, &v.order);
            order_ambiguous += v.order_ambiguous;
        }
    }
    let file_decimal = FileVotes::new(decimal_votes, decimal_ambiguous);
    let file_order = FileVotes::new(order_votes, order_ambiguous);
    // Whole numbers grouped with spaces (`1 234`) are numbers only when the
    // caller chose a space grouping or a number column groups fractions
    // with spaces (`1 234,5`). A decimal comma alone is not enough: German
    // files group with `.`, and `640 480` is a size.
    // Integers grouped with no-break spaces are numbers once the decimal
    // comma is decided: those spaces almost never group anything else.
    let comma_decided = chosen_comma || matches!(file_decimal.verdict, Verdict::Decided(1, _));
    let space_integers = chosen_space
        || votes
            .iter()
            .zip(&loose)
            .any(|(v, k)| *k == ColumnKind::Number && v.space_fractions > 0);

    // Second pass: borrow for columns whose values are all ambiguous, and
    // flag columns that disagree with the file.
    for ((plan, v), kind) in plans.iter_mut().zip(&votes).zip(&loose) {
        plan.grouping.space_integers |= space_integers;
        if *kind == ColumnKind::Number {
            plan.decimal = settle_with_file(
                plan.decimal,
                v.decimal_ambiguous,
                &file_decimal,
                &DecimalMark::ALL,
            );
            if let Some(note) = conflict(
                &mut plan.decimal,
                &v.decimal,
                &v.decimal_first,
                &file_decimal,
                |d: DecimalMark| NoteKind::DecimalConflict(d),
            ) {
                plan.column_notes.push(note);
            }
        }
        if *kind == ColumnKind::Date {
            plan.date_order = settle_with_file(
                plan.date_order,
                v.order_ambiguous,
                &file_order,
                &DateOrder::ALL,
            );
            if let Some(note) = conflict(
                &mut plan.date_order,
                &v.order,
                &v.order_first,
                &file_order,
                |o: DateOrder| NoteKind::DateOrderConflict(o),
            ) {
                plan.column_notes.push(note);
            }
        }
    }
    // Integers grouped with no-break spaces are numbers in number columns
    // where the decimal comma applies: its own, or the file's when it has
    // none. Not in text columns, not where a point is decided.
    for (plan, kind) in plans.iter_mut().zip(&loose) {
        plan.grouping.nbsp_integers = *kind == ColumnKind::Number
            && match plan.decimal {
                Mark::Fixed(d, _) => d == DecimalMark::Comma,
                Mark::Open => comma_decided,
                Mark::Mixed => false,
            };
    }
    let fits = classify_sample(sample, &mut plans);
    for (plan, fit) in plans.iter_mut().zip(&fits) {
        plan.kind = Some(fit.kind().0);
    }
    Planned {
        plans,
        votes,
        fits,
        decimal: file_decimal,
        order: file_order,
        end,
        bytes,
    }
}

fn add_into<const N: usize>(total: &mut [usize; N], votes: &[usize; N]) {
    for (t, x) in total.iter_mut().zip(votes) {
        *t += x;
    }
}

/// An open column with ambiguous values borrows a well-supported file
/// decision.
fn settle_with_file<T: Copy, const N: usize>(
    mark: Mark<T>,
    ambiguous: usize,
    file: &FileVotes<N>,
    all: &[T],
) -> Mark<T> {
    match (mark, file.lendable()) {
        (Mark::Open, Some(i)) if ambiguous > 0 => all
            .get(i)
            .map_or(Mark::Open, |&t| Mark::Fixed(t, Source::Borrowed)),
        _ => mark,
    }
}

/// A column decided by its own values against a file that decided the
/// other way (or is mixed): lower its confidence and say so.
fn conflict<T, const N: usize>(
    mark: &mut Mark<T>,
    votes: &[usize; N],
    first: &[usize; N],
    file: &FileVotes<N>,
    note: impl Fn(T) -> NoteKind,
) -> Option<Note>
where
    T: Copy + PartialEq + VoteIndex,
{
    let Mark::Fixed(t, Source::Votes(c)) = *mark else {
        return None;
    };
    let lowered = match file.verdict {
        Verdict::Decided(i, _) if i != t.vote_index() => Confidence::Low,
        Verdict::Mixed => c.min(Confidence::Medium),
        _ => return None,
    };
    *mark = Mark::Fixed(t, Source::Votes(lowered));
    Some(Note {
        kind: note(t),
        count: votes.get(t.vote_index()).copied().unwrap_or(0),
        first_row: first.get(t.vote_index()).copied().unwrap_or(0),
    })
}

/// The position of a decision in its vote counts.
trait VoteIndex {
    fn vote_index(self) -> usize;
}

impl VoteIndex for DecimalMark {
    fn vote_index(self) -> usize {
        self.index()
    }
}

impl VoteIndex for DateOrder {
    fn vote_index(self) -> usize {
        self.index()
    }
}

/// The header row is one where some cell is plain text above a typed column.
fn header_fits<R: AsRef<[S]>, S: AsRef<str>>(first: &R, planned: &Planned) -> bool {
    cells(first).zip(&planned.plans).any(|(text, plan)| {
        let typed = !matches!(plan.kind, Some(ColumnKind::Text | ColumnKind::Empty) | None);
        let mut plan = plan.clone();
        typed && classify(text, normalize(text).as_deref(), &mut plan) == Class::Text
    })
}

/// Decides everything about `rows`; see [`Analysis::detect`].
pub(crate) fn analyze<R: AsRef<[S]>, S: AsRef<str>>(
    rows: &[R],
    detected: &Detected,
    choice: &ConventionChoice,
) -> Analysis {
    // With a header, the plans for the rows after it are the final ones.
    let mut after_first = None;
    let header = match choice.header {
        Header::Yes => !rows.is_empty(),
        Header::No => false,
        Header::Auto => {
            rows.len() >= 2
                && rows.first().is_some_and(|first| {
                    header_fits(first, after_first.insert(plan_all(rows, 1, choice)))
                })
        }
    };
    let start = usize::from(header);
    let planned = match after_first {
        Some(planned) if header => planned,
        _ => plan_all(rows, start, choice),
    };

    let names: Vec<Option<String>> = (0..planned.plans.len())
        .map(|j| {
            header
                .then(|| rows.first().and_then(|r| r.as_ref().get(j)))
                .flatten()
                .map(|s| printable(s.as_ref(), HEADER_NAME_BYTES))
        })
        .collect();
    let columns: Vec<ColumnReport> = planned
        .plans
        .iter()
        .zip(&planned.fits)
        .zip(names)
        .enumerate()
        .map(|(j, ((plan, fit), name))| column_report(j + 1, name, plan, fit))
        .collect();

    let mut analysis = Analysis {
        convention: Convention::default(),
        delimiter: delimiter_decision(rows.get(..planned.end).unwrap_or_default(), detected),
        decimal: Decision::default(),
        grouping: Decision::default(),
        date_order: Decision::default(),
        header,
        sample_rows: planned.end - start,
        sample_bytes: planned.bytes,
        columns,
        notes: Vec::new(),
        plans: planned.plans.clone(),
    };
    file_decisions(&mut analysis, &planned, choice, detected);

    // Notes for the sample, as `convert` would count them for the file.
    let mut plans = planned.plans;
    let mut notes = vec![Notes::default(); plans.len()];
    for (i, row) in rows.iter().enumerate().take(planned.end).skip(start) {
        for ((text, plan), notes) in cells(row).zip(plans.iter_mut()).zip(notes.iter_mut()) {
            let class = classify(text, normalize(text).as_deref(), plan);
            let kind = plan.kind.unwrap_or(ColumnKind::Text);
            if let Some(note) = class.note(kind, plan.pivot) {
                notes.add(note, i + 1);
            }
        }
    }
    for ((column, notes), plan) in analysis.columns.iter_mut().zip(notes).zip(&plans) {
        column.notes = plan.column_notes.clone();
        column.notes.extend(settled_notes(notes, plan));
    }
    analysis
}

fn column_report(
    column: usize,
    header: Option<String>,
    plan: &ColumnPlan,
    fit: &Fits,
) -> ColumnReport {
    let (kind, count) = fit.kind();
    let n = fit.nonempty;
    let mut confidence = Confidence::High;
    let mut decimal = None;
    let mut date_order = None;
    let mut why = match kind {
        ColumnKind::Empty => "No values in the sample.".to_owned(),
        ColumnKind::Text if fit.leading_zero > 0 => {
            "Text: some values have leading zeros that a number would drop.".to_owned()
        }
        ColumnKind::Text if fit.too_many_digits > 0 => {
            "Text: some values have more than 15 digits that a number would round.".to_owned()
        }
        ColumnKind::Text if fit.too_small > 0 => {
            "Text: some values are too small for a number to keep their digits.".to_owned()
        }
        ColumnKind::Text if fit.phone > 0 => {
            "Text: some values look like phone numbers (a plus sign and many digits).".to_owned()
        }
        ColumnKind::Text if fit.unsettled_kind() == Some(ColumnKind::Number) => {
            confidence = Confidence::Medium;
            "Text: these look like numbers, but which decimal mark they use is unclear or mixed."
                .to_owned()
        }
        ColumnKind::Text if fit.unsettled_kind() == Some(ColumnKind::Date) => {
            confidence = Confidence::Medium;
            "Text: these look like dates, but their day and month order is unclear or mixed."
                .to_owned()
        }
        ColumnKind::Text if fit.space_grouped > 0 => {
            confidence = Confidence::Medium;
            "Text: values like 640 480 may be numbers grouped with spaces, which also write sizes and codes; choose space grouping to read them as numbers."
                .to_owned()
        }
        ColumnKind::Text => match fit.by_kind.iter().sum::<usize>() {
            0 => "Text: no value is a number, date, time or boolean.".to_owned(),
            typed => format!(
                "Text: only {typed} of {n} sampled values are numbers, dates, times or booleans."
            ),
        },
        ColumnKind::Number => {
            let (part, c) = mark_why(plan.decimal, fit.number_ambiguous, |d| {
                format!("decimal mark '{}'", d.as_char())
            });
            if let Mark::Fixed(d, _) = plan.decimal {
                decimal = Some(d);
            }
            confidence = c;
            format!("Numbers in {count} of {n} sampled values; {part}")
        }
        ColumnKind::Date => {
            let (part, c) = mark_why(plan.date_order, fit.date_ambiguous, |o| {
                o.describe().to_owned()
            });
            if let Mark::Fixed(o, _) = plan.date_order {
                date_order = Some(o);
            }
            confidence = c;
            format!("Dates in {count} of {n} sampled values; {part}")
        }
        ColumnKind::Time => format!("Times in {count} of {n} sampled values."),
        ColumnKind::Bool => format!("TRUE or FALSE in {count} of {n} sampled values."),
    };
    if !plan.column_notes.is_empty() && matches!(kind, ColumnKind::Number | ColumnKind::Date) {
        why.push_str(" Other columns of this kind disagree; check it.");
    }
    if count > 0 && count.saturating_mul(10) < n.saturating_mul(9) {
        confidence = confidence.min(Confidence::Medium);
    }
    ColumnReport {
        column,
        header,
        kind,
        confidence,
        decimal,
        date_order,
        why,
        notes: Vec::new(),
    }
}

fn mark_why<T: Copy>(
    mark: Mark<T>,
    ambiguous: usize,
    name: impl Fn(T) -> String,
) -> (String, Confidence) {
    match mark {
        Mark::Fixed(t, Source::Chosen) => (format!("{} as chosen.", name(t)), Confidence::Certain),
        Mark::Fixed(t, Source::Votes(c)) => (
            format!("{} from values that read only that way.", name(t)),
            c,
        ),
        Mark::Fixed(t, Source::Borrowed) => (
            format!(
                "{} taken from the other columns, because every value here reads both ways.",
                name(t)
            ),
            Confidence::Medium,
        ),
        Mark::Fixed(t, Source::Late) => (
            format!("{} settled after the sample.", name(t)),
            Confidence::Medium,
        ),
        Mark::Open if ambiguous > 0 => (
            "unclear which way to read some values; they stay text.".to_owned(),
            Confidence::Medium,
        ),
        Mark::Open => (
            "every value reads the same either way.".to_owned(),
            Confidence::High,
        ),
        Mark::Mixed => (
            "values disagree on the convention; only those that read the same either way are typed."
                .to_owned(),
            Confidence::Low,
        ),
    }
}

fn delimiter_decision<R: AsRef<[S]>, S: AsRef<str>>(sample: &[R], detected: &Detected) -> Decision {
    let shown = describe_delimiter(detected.delimiter);
    if !detected.delimiter_detected {
        let why = if detected.sep_line {
            format!("{shown}, named by the file's sep= line.")
        } else {
            format!("{shown}, as chosen.")
        };
        return Decision {
            decided: true,
            confidence: Confidence::Certain,
            why,
        };
    }
    let mut counts: Vec<usize> = sample
        .iter()
        .map(|r| r.as_ref().len())
        .filter(|&n| n > 0)
        .collect();
    counts.sort_unstable();
    let rows = counts.len();
    let (common, hits) = counts
        .chunk_by(|a, b| a == b)
        .map(|c| (c.first().copied().unwrap_or(0), c.len()))
        .max_by_key(|&(value, count)| (count, value))
        .unwrap_or((0, 0));
    if common < 2 {
        return Decision {
            decided: false,
            confidence: Confidence::Low,
            why: "No delimiter splits the rows; read as one column.".to_owned(),
        };
    }
    let mut confidence = if hits * 10 >= rows * 9 {
        Confidence::High
    } else if hits * 10 >= rows * 6 {
        Confidence::Medium
    } else {
        Confidence::Low
    };
    let mut why = format!("{shown} splits {hits} of {rows} sampled rows into {common} cells.");
    if detected.semicolon_for_decimal_commas {
        confidence = confidence.min(Confidence::Medium);
        why.push_str(
            " ',' splits them as well; ';' was chosen because its cells hold decimal-comma numbers like 1,5. Set the delimiter if that is wrong.",
        );
    }
    Decision {
        decided: true,
        confidence,
        why,
    }
}

/// Names a delimiter so that a control character cannot change the report.
pub(crate) fn describe_delimiter(d: char) -> String {
    match d {
        '\t' => "Tab".to_owned(),
        ' ' => "Space".to_owned(),
        c if c.is_control() || !c.is_ascii() => format!("U+{:04X}", u32::from(c)),
        c => format!("'{c}'"),
    }
}

fn file_decisions(
    analysis: &mut Analysis,
    planned: &Planned,
    choice: &ConventionChoice,
    detected: &Detected,
) {
    let mut convention = Convention {
        delimiter: detected.delimiter,
        two_digit_pivot: choice.two_digit_pivot,
        ..Convention::ISO
    };
    let decimal_votes = planned.decimal.votes;
    let order_votes = planned.order.votes;
    let kinds: Vec<Option<ColumnKind>> = planned.plans.iter().map(|p| p.kind).collect();

    // Decimal mark.
    analysis.decimal = match (choice.decimal, planned.decimal.verdict) {
        (Some(d), _) => {
            convention.decimal = d;
            chosen()
        }
        (None, Verdict::Decided(i, confidence)) => {
            convention.decimal = DecimalMark::ALL.get(i).copied().unwrap_or(DecimalMark::Point);
            let mark = convention.decimal;
            let other = other_mark(mark);
            Decision {
                decided: true,
                confidence,
                why: format!(
                    "In number columns, {} values read only with '{}' (like 3{}5), {} only with '{}'.",
                    decimal_votes.get(mark.index()).copied().unwrap_or(0),
                    mark.as_char(),
                    mark.as_char(),
                    decimal_votes.get(other.index()).copied().unwrap_or(0),
                    other.as_char()
                ),
            }
        }
        (None, Verdict::Mixed) => undecided(format!(
            "Number columns disagree: {} values read only with '.', {} only with ','; each column decides for itself.",
            decimal_votes[0], decimal_votes[1]
        )),
        (None, Verdict::NoVotes) if planned.decimal.ambiguous > 0 => undecided(
            "Unclear: values like 1,234 could be 1234 or 1.234, and no value settles it. Set the decimal mark to read them as numbers; until then they stay text."
                .to_owned(),
        ),
        (None, Verdict::NoVotes) => {
            undecided("No number shows its decimal mark; '.' assumed for export.".to_owned())
        }
    };

    // Grouping, from number columns, on the side of the decimal mark.
    let side = convention.decimal.index();
    let mut grouping_counts = [0usize; 7];
    for (v, kind) in planned.votes.iter().zip(&kinds) {
        if *kind == Some(ColumnKind::Number)
            && let Some(g) = v.grouping.get(side)
        {
            add_into(&mut grouping_counts, g);
        }
    }
    if let Some(g) = grouping_counts.get_mut(Grouping::None.index()) {
        *g = 0;
    }
    analysis.grouping = match choice.grouping {
        Some(g) => {
            convention.grouping = g;
            chosen()
        }
        None => {
            let total: usize = grouping_counts.iter().sum();
            let best = grouping_counts
                .iter()
                .enumerate()
                .max_by_key(|(i, v)| (**v, Reverse(*i)));
            match best {
                Some((i, &count)) if count > 0 => {
                    convention.grouping = Grouping::ALL.get(i).copied().unwrap_or(Grouping::None);
                    Decision {
                        decided: true,
                        confidence: if count == total {
                            Confidence::High
                        } else {
                            Confidence::Medium
                        },
                        why: format!(
                            "{} groups digits in {count} of {total} grouped values.",
                            capitalize(convention.grouping.describe())
                        ),
                    }
                }
                _ => undecided("No number groups its digits.".to_owned()),
            }
        }
    };
    if convention.grouping.as_char() == Some(convention.decimal.as_char()) {
        convention.grouping = Grouping::None;
    }

    // Date order.
    analysis.date_order = match (choice.date_order, planned.order.verdict) {
        (Some(o), _) => {
            convention.date_order = o;
            chosen()
        }
        (None, Verdict::Decided(i, confidence)) => {
            convention.date_order = DateOrder::ALL.get(i).copied().unwrap_or(DateOrder::Ymd);
            Decision {
                decided: true,
                confidence,
                why: format!(
                    "In date columns, dates that read only one way: {} DMY, {} MDY, {} YMD.",
                    order_votes[0], order_votes[1], order_votes[2]
                ),
            }
        }
        (None, Verdict::Mixed) => undecided(format!(
            "Date columns disagree: {} dates read only as DMY, {} only as MDY, {} only as YMD; each column decides for itself.",
            order_votes[0], order_votes[1], order_votes[2]
        )),
        (None, Verdict::NoVotes) if planned.order.ambiguous > 0 => undecided(
            "Unclear: dates like 03/04/2026 could be day first or month first, and no date settles it. Set the date order to read them as dates; until then they stay text."
                .to_owned(),
        ),
        (None, Verdict::NoVotes) => undecided(
            "No date shows its order; year first (ISO 8601) assumed for export.".to_owned(),
        ),
    };

    // Date separator, from date columns.
    let mut separators = [0usize; 3];
    for (v, kind) in planned.votes.iter().zip(&kinds) {
        if *kind == Some(ColumnKind::Date) {
            add_into(&mut separators, &v.separators);
        }
    }
    let seen = separators
        .iter()
        .enumerate()
        .max_by_key(|(i, v)| (**v, Reverse(*i)))
        .filter(|(_, v)| **v > 0)
        .and_then(|(i, _)| ['/', '-', '.'].get(i).copied());
    convention.date_separator =
        choice
            .date_separator
            .or(seen)
            .unwrap_or(match convention.date_order {
                DateOrder::Ymd => '-',
                DateOrder::Dmy | DateOrder::Mdy => '/',
            });
    analysis.convention = convention;
}

fn chosen() -> Decision {
    Decision {
        decided: true,
        confidence: Confidence::Certain,
        why: "As chosen.".to_owned(),
    }
}

fn undecided(why: String) -> Decision {
    Decision {
        decided: false,
        confidence: Confidence::Low,
        why,
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl Analysis {
    /// Converts rows to typed values under the column decisions, and
    /// recounts the notes for the rows given (detection counted only the
    /// sample). Text that is not of its column's kind stays
    /// [`Value::Text`], unchanged.
    ///
    /// The rows need not be the ones detection saw: detect on a sample,
    /// then convert the whole file. Columns are matched by position. When
    /// [`Analysis::header`] is true, the first row given is the header and
    /// stays text; pass `header: false` to [`Analysis::convert_from`] for
    /// rows without it.
    pub fn convert<I, R, S>(&mut self, rows: I) -> Vec<Vec<Value>>
    where
        I: IntoIterator<Item = R>,
        R: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.convert_from(rows, self.header)
    }

    /// As [`Analysis::convert`], saying whether the first row given is the
    /// header.
    pub fn convert_from<I, R, S>(&mut self, rows: I, header: bool) -> Vec<Vec<Value>>
    where
        I: IntoIterator<Item = R>,
        R: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut plans = self.plans.clone();
        let mut notes = vec![Notes::default(); plans.len()];
        let mut beyond = Notes::default();
        let mut out = Vec::new();
        for (i, row) in rows.into_iter().enumerate() {
            let row_number = i + 1;
            let mut values = Vec::new();
            for (j, text) in row.into_iter().enumerate() {
                let text: String = text.into();
                if header && i == 0 {
                    values.push(text_value(text));
                    continue;
                }
                let Some(plan) = plans.get_mut(j) else {
                    if !text.is_empty() {
                        beyond.add(NoteKind::Unchecked, row_number);
                    }
                    values.push(text_value(text));
                    continue;
                };
                let before = (plan.decimal, plan.date_order);
                let class = classify(&text, normalize(&text).as_deref(), plan);
                if (plan.decimal, plan.date_order) != before
                    && let Some(column) = self.columns.get_mut(j)
                {
                    settled_late(column, plan, row_number);
                }
                let kind = plan.kind.unwrap_or(ColumnKind::Text);
                if let Some(note) = class.note(kind, plan.pivot)
                    && let Some(n) = notes.get_mut(j)
                {
                    n.add(note, row_number);
                }
                values.push(class.into_value(kind).unwrap_or(Value::Text(text)));
            }
            out.push(values);
        }
        for ((column, notes), (plan, after)) in self
            .columns
            .iter_mut()
            .zip(notes)
            .zip(self.plans.iter().zip(&plans))
        {
            column.notes = plan.column_notes.clone();
            column.notes.extend(settled_notes(notes, after));
        }
        self.notes = beyond.into_vec();
        out
    }
}

/// A column's notes once its rows are read: values "before the decimal
/// mark settled" are plainly not numbers when nothing settled it later.
fn settled_notes(mut notes: Notes, plan: &ColumnPlan) -> Vec<Note> {
    if !matches!(plan.decimal, Mark::Fixed(_, Source::Late)) {
        notes.rename(
            NoteKind::BeforeSettled,
            NoteKind::OtherKind(ColumnKind::Number),
        );
    }
    notes.into_vec()
}

fn text_value(text: String) -> Value {
    if text.is_empty() {
        Value::Empty
    } else {
        Value::Text(text)
    }
}

/// A decision settled after the sample: say where, and lower the confidence.
fn settled_late(column: &mut ColumnReport, plan: &ColumnPlan, row: usize) {
    if let Mark::Fixed(d, Source::Late) = plan.decimal
        && column.kind == ColumnKind::Number
        && column.decimal.is_none()
    {
        column.decimal = Some(d);
        column.confidence = column.confidence.min(Confidence::Medium);
        column.why.push_str(&format!(
            " Decimal mark '{}' settled by row {row}, after the sample; earlier unclear values stay text.",
            d.as_char()
        ));
    }
    if let Mark::Fixed(o, Source::Late) = plan.date_order
        && column.kind == ColumnKind::Date
        && column.date_order.is_none()
    {
        column.date_order = Some(o);
        column.confidence = column.confidence.min(Confidence::Medium);
        column.why.push_str(&format!(
            " Order {} settled by row {row}, after the sample; earlier unclear dates stay text.",
            o.describe()
        ));
    }
}
