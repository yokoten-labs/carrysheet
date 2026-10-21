//! Cell values and error kinds.

use std::fmt;

/// The error values a cell can hold, such as `#DIV/0!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Null,
    Div0,
    Value,
    Ref,
    Name,
    Num,
    NA,
    GettingData,
    Spill,
    Calc,
    Field,
    Blocked,
    Connect,
    Busy,
    /// A circular reference: a formula that reads its own result, directly or
    /// through others. Excel shows 0 and a warning; Carrysheet says what it is.
    Circ,
    Unknown,
}

impl ErrorKind {
    /// Every kind, in a fixed order.
    pub const ALL: [ErrorKind; 16] = [
        ErrorKind::Null,
        ErrorKind::Div0,
        ErrorKind::Value,
        ErrorKind::Ref,
        ErrorKind::Name,
        ErrorKind::Num,
        ErrorKind::NA,
        ErrorKind::GettingData,
        ErrorKind::Spill,
        ErrorKind::Calc,
        ErrorKind::Field,
        ErrorKind::Blocked,
        ErrorKind::Connect,
        ErrorKind::Busy,
        ErrorKind::Circ,
        ErrorKind::Unknown,
    ];

    /// The text Excel shows, such as `#N/A`.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Null => "#NULL!",
            ErrorKind::Div0 => "#DIV/0!",
            ErrorKind::Value => "#VALUE!",
            ErrorKind::Ref => "#REF!",
            ErrorKind::Name => "#NAME?",
            ErrorKind::Num => "#NUM!",
            ErrorKind::NA => "#N/A",
            ErrorKind::GettingData => "#GETTING_DATA",
            ErrorKind::Spill => "#SPILL!",
            ErrorKind::Calc => "#CALC!",
            ErrorKind::Field => "#FIELD!",
            ErrorKind::Blocked => "#BLOCKED!",
            ErrorKind::Connect => "#CONNECT!",
            ErrorKind::Busy => "#BUSY!",
            ErrorKind::Circ => "#CIRC!",
            ErrorKind::Unknown => "#UNKNOWN!",
        }
    }

    /// Reads the text Excel shows, in any case. `None` if it is not an error value.
    pub fn parse(s: &str) -> Option<ErrorKind> {
        ErrorKind::ALL
            .into_iter()
            .find(|k| k.as_str().eq_ignore_ascii_case(s))
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a cell holds: its typed value (for a formula, the last computed result).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CellValue {
    #[default]
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(ErrorKind),
}

impl CellValue {
    /// True for [`CellValue::Empty`].
    pub fn is_empty(&self) -> bool {
        matches!(self, CellValue::Empty)
    }

    /// The type name used in output: `empty`, `number`, `text`, `bool`, `error`.
    pub fn kind(&self) -> &'static str {
        match self {
            CellValue::Empty => "empty",
            CellValue::Number(_) => "number",
            CellValue::Text(_) => "text",
            CellValue::Bool(_) => "bool",
            CellValue::Error(_) => "error",
        }
    }
}

impl fmt::Display for CellValue {
    /// Shows the raw value without number formatting: `TRUE`, `3.5`, `#N/A`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CellValue::Empty => Ok(()),
            CellValue::Number(n) => write!(f, "{n}"),
            CellValue::Text(s) => f.write_str(s),
            CellValue::Bool(true) => f.write_str("TRUE"),
            CellValue::Bool(false) => f.write_str("FALSE"),
            CellValue::Error(e) => write!(f, "{e}"),
        }
    }
}

impl From<f64> for CellValue {
    fn from(n: f64) -> Self {
        CellValue::Number(n)
    }
}

impl From<bool> for CellValue {
    fn from(b: bool) -> Self {
        CellValue::Bool(b)
    }
}

impl From<&str> for CellValue {
    fn from(s: &str) -> Self {
        CellValue::Text(s.to_owned())
    }
}

impl From<String> for CellValue {
    fn from(s: String) -> Self {
        CellValue::Text(s)
    }
}

impl From<ErrorKind> for CellValue {
    fn from(e: ErrorKind) -> Self {
        CellValue::Error(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kinds_round_trip() {
        for k in ErrorKind::ALL {
            assert_eq!(ErrorKind::parse(k.as_str()), Some(k));
            assert_eq!(ErrorKind::parse(&k.as_str().to_lowercase()), Some(k));
        }
        assert_eq!(ErrorKind::parse("#WHAT"), None);
        assert_eq!(ErrorKind::Div0.to_string(), "#DIV/0!");
    }

    #[test]
    fn values_display_and_kind() {
        assert_eq!(CellValue::from(3.5).to_string(), "3.5");
        assert_eq!(CellValue::from(true).to_string(), "TRUE");
        assert_eq!(CellValue::from("hi").to_string(), "hi");
        assert_eq!(CellValue::from(ErrorKind::NA).to_string(), "#N/A");
        assert_eq!(CellValue::Empty.to_string(), "");
        assert_eq!(CellValue::Empty.kind(), "empty");
        assert_eq!(CellValue::from(String::from("x")).kind(), "text");
        assert!(CellValue::default().is_empty());
    }
}
