//! Errors with three parts: what happened, why, and how to fix it.

use std::fmt;

/// Everything that can go wrong while reading or writing CSV.
///
/// Rows, columns and lines are counted from 1, as a spreadsheet shows them.
/// Each error explains itself in three parts: [`CsvError::what`],
/// [`CsvError::why`] and [`CsvError::fix`]. `Display` joins all three.
/// Errors never quote cell contents, so they are safe to log.
#[derive(Debug)]
#[non_exhaustive]
pub enum CsvError {
    /// The input is bigger than [`Limits::max_total_bytes`](crate::Limits::max_total_bytes).
    InputTooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// The input has more rows than [`Limits::max_rows`](crate::Limits::max_rows).
    TooManyRows {
        /// The limit.
        limit: usize,
        /// The line where the first row over the limit starts.
        line: u64,
    },
    /// A row has more cells than [`Limits::max_columns`](crate::Limits::max_columns).
    TooManyColumns {
        /// The limit.
        limit: usize,
        /// The row that is too wide.
        row: usize,
        /// The line where that row starts.
        line: u64,
    },
    /// The whole input has more cells than [`Limits::max_cells`](crate::Limits::max_cells).
    TooManyCells {
        /// The limit.
        limit: usize,
        /// The row where the limit was reached.
        row: usize,
        /// The line where that row starts.
        line: u64,
    },
    /// One cell holds more text than [`Limits::max_field_bytes`](crate::Limits::max_field_bytes).
    FieldTooLarge {
        /// The limit in bytes of UTF-8 text.
        limit: usize,
        /// The row of the cell.
        row: usize,
        /// The column of the cell.
        column: usize,
        /// The line where that row starts.
        line: u64,
    },
    /// A cell opens a quote that never closes.
    UnclosedQuote {
        /// The row of the cell.
        row: usize,
        /// The line where the quote opens.
        line: u64,
    },
    /// The bytes are not valid text in the chosen encoding.
    MalformedText {
        /// The encoding name, such as `Shift_JIS`.
        encoding: &'static str,
        /// The byte offset of the first bad byte, counted from 0.
        offset: usize,
    },
    /// The delimiter cannot separate cells.
    InvalidDelimiter {
        /// The delimiter that was asked for.
        delimiter: char,
    },
    /// A character has no code in the output encoding.
    Unencodable {
        /// The character.
        character: char,
        /// The output encoding name.
        encoding: &'static str,
        /// The row of the cell.
        row: usize,
        /// The column of the cell.
        column: usize,
    },
    /// A byte order mark was asked for an encoding that has none.
    BomNotAvailable {
        /// The output encoding name.
        encoding: &'static str,
    },
    /// The encoding can be read but not written.
    CannotWriteEncoding {
        /// The encoding name.
        encoding: &'static str,
    },
    /// A regional convention cannot work.
    InvalidConvention {
        /// What is wrong with it.
        problem: &'static str,
    },
    /// Reading from or writing to the stream failed.
    Io(std::io::Error),
}

impl CsvError {
    /// What happened, in one sentence.
    pub fn what(&self) -> String {
        match self {
            Self::InputTooLarge { limit } => {
                format!("The file is larger than the limit of {limit} bytes.")
            }
            Self::TooManyRows { limit, line } => {
                format!(
                    "The file has more than {limit} rows (row {} starts on line {line}).",
                    limit.saturating_add(1)
                )
            }
            Self::TooManyColumns { limit, row, line } => {
                format!("Row {row} (line {line}) has more than {limit} cells.")
            }
            Self::TooManyCells { limit, row, line } => {
                format!(
                    "The file has more than {limit} cells in total (reached at row {row}, line {line})."
                )
            }
            Self::FieldTooLarge {
                limit,
                row,
                column,
                line,
            } => {
                format!(
                    "The cell at row {row}, column {column} (line {line}) holds more than {limit} bytes of text."
                )
            }
            Self::UnclosedQuote { row, line } => {
                format!("A quoted cell in row {row} opens on line {line} and never closes.")
            }
            Self::MalformedText { encoding, offset } => {
                format!("The bytes at offset {offset} are not valid {encoding} text.")
            }
            Self::InvalidDelimiter { delimiter } => {
                format!(
                    "The delimiter {} cannot separate cells.",
                    describe_char(*delimiter)
                )
            }
            Self::Unencodable {
                character,
                encoding,
                row,
                column,
            } => {
                format!(
                    "The cell at row {row}, column {column} contains {}, which {encoding} cannot store.",
                    describe_char(*character)
                )
            }
            Self::BomNotAvailable { encoding } => {
                format!("A byte order mark was asked for, but {encoding} has none.")
            }
            Self::CannotWriteEncoding { encoding } => {
                format!("Text cannot be written in the {encoding} encoding.")
            }
            Self::InvalidConvention { problem } => {
                format!("The regional convention cannot work: {problem}.")
            }
            Self::Io(e) => format!("Reading or writing the data failed: {e}."),
        }
    }

    /// Why it is a problem.
    pub fn why(&self) -> String {
        match self {
            Self::InputTooLarge { .. }
            | Self::TooManyRows { .. }
            | Self::TooManyColumns { .. }
            | Self::TooManyCells { .. }
            | Self::FieldTooLarge { .. } => {
                "Limits stop a damaged or hostile file from using up all memory.".to_owned()
            }
            Self::UnclosedQuote { .. } => {
                "Without the closing quote, the rest of the file would become one cell.".to_owned()
            }
            Self::MalformedText { .. } => {
                "The file is probably in a different encoding, or it is damaged.".to_owned()
            }
            Self::InvalidDelimiter { .. } => {
                "A delimiter must be one ASCII character other than a double quote, CR or LF."
                    .to_owned()
            }
            Self::Unencodable { .. } => {
                "Writing it anyway would replace it and lose data.".to_owned()
            }
            Self::BomNotAvailable { .. } => {
                "Only UTF-8 and UTF-16 have a byte order mark.".to_owned()
            }
            Self::CannotWriteEncoding { .. } => {
                "This encoding exists only to read broken legacy data.".to_owned()
            }
            Self::InvalidConvention { .. } => {
                "Numbers and dates written that way could not be read back.".to_owned()
            }
            Self::Io(_) => "The operating system reported an error.".to_owned(),
        }
    }

    /// How to fix it.
    pub fn fix(&self) -> String {
        match self {
            Self::InputTooLarge { .. } => "Split the file, or raise `max_total_bytes`.".to_owned(),
            Self::TooManyRows { .. } => "Split the file, or raise `max_rows`.".to_owned(),
            Self::TooManyColumns { .. } => {
                "Check that the delimiter is right, or raise `max_columns`.".to_owned()
            }
            Self::TooManyCells { .. } => "Split the file, or raise `max_cells`.".to_owned(),
            Self::FieldTooLarge { .. } => {
                "Check for a missing closing quote, or raise `max_field_bytes`.".to_owned()
            }
            Self::UnclosedQuote { .. } => {
                "Add the closing double quote, or check that the delimiter is right.".to_owned()
            }
            Self::MalformedText { .. } => {
                "Choose the right encoding, or read with `Malformed::Replace` to replace bad bytes with U+FFFD."
                    .to_owned()
            }
            Self::InvalidDelimiter { .. } => "Use a comma, semicolon, tab or pipe.".to_owned(),
            Self::Unencodable { .. } => {
                "Save as UTF-8 (or UTF-16), or change the cell.".to_owned()
            }
            Self::BomNotAvailable { .. } => "Turn the byte order mark off.".to_owned(),
            Self::CannotWriteEncoding { .. } => "Save as UTF-8 instead.".to_owned(),
            Self::InvalidConvention { .. } => {
                "Choose different characters, or start from a preset such as `Convention::GERMANY`.".to_owned()
            }
            Self::Io(_) => {
                "Check the path, the permissions and the free disk space, then try again.".to_owned()
            }
        }
    }
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.what(), self.why(), self.fix())
    }
}

impl std::error::Error for CsvError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CsvError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Names a character so it shows up even when it is invisible.
fn describe_char(c: char) -> String {
    let code = format!("U+{:04X}", u32::from(c));
    if c.is_control() || c.is_whitespace() {
        code
    } else {
        format!("\"{c}\" ({code})")
    }
}
