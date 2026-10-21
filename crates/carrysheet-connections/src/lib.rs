//! Find and describe where a workbook's data comes from: data connections
//! (ODBC, OLE DB, DAO, web, text, ADO, OLAP, the data model), query tables,
//! external links, Power Query and the data model, without showing secrets.
//!
//! The caller opens the package and hands its parts over through
//! [`PartSource`]; this crate reads no files and has no zip dependency for
//! the package itself.
//!
//! ```
//! use carrysheet_connections::{inspect, Auth, Limits, MemoryParts};
//!
//! let mut parts = MemoryParts::new();
//! parts.insert(
//!     "xl/connections.xml",
//!     r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
//!          <connection id="1" name="Sales" type="5" refreshOnLoad="1">
//!            <dbPr connection="Provider=SQLOLEDB;Data Source=sql01,1433;User ID=app;Password=hunter2" command="Sales" commandType="3"/>
//!          </connection>
//!        </connections>"#,
//! );
//! let report = inspect(&parts, &Limits::default());
//! let sales = &report.connections[0];
//! assert_eq!(sales.target.server.as_deref(), Some("sql01"));
//! assert_eq!(sales.target.port, Some(1433));
//! assert_eq!(sales.target.table.as_deref(), Some("Sales"));
//! assert_eq!(sales.auth, [Auth::User, Auth::Password]);
//! assert_eq!(sales.hidden[0].what, "connection string");
//! assert!(sales.refresh.on_load);
//! ```
//!
//! # What the report shows, and why
//!
//! Earlier versions returned the raw texts with secrets masked by patterns.
//! Masking free text is a deny-list: every round of review found a shape it
//! missed (a quote in a password, an encoded URL, an argument list), and
//! every new pattern hid hosts and paths people needed. So the default
//! report is an allow-list:
//!
//! - **Recognized fields only.** Provider or driver, server host and port
//!   (also from an Oracle connect descriptor or Easy Connect string),
//!   database, DSN, the kinds of sign-in present ([`Auth`]: password
//!   present, token present, ... never values), names, and for Power Query
//!   each query's name and the library functions it calls with the server,
//!   database, URL or path its literal arguments name. A value is shown only
//!   when a parser recognized it as that field; anything else is left out
//!   and, for M, counted (`other_calls`). Hosts are never masked: they are
//!   what people want to know.
//! - **Paths are parsed too.** A file path is read into server, share,
//!   drive, folder and file name ([`FilePath`]); text that is not a path is
//!   not shown. A URL is split per RFC 3986 before anything is decoded and
//!   shown as scheme, host, port and path: user information, query and
//!   fragment are always dropped, and a URL with an `@` after its authority
//!   is not shown at all (a password holding an unencoded `/`, `?` or `#`
//!   would leave its tail in the path). A path that may carry a credential
//!   itself (a share link, a webhook address, `;name=value` parameters, a
//!   `:` in a URL path, a part that looks like a token) is hidden whole,
//!   with the reason, in `hidden_path`; the host stays. DDE topics follow
//!   the same rules.
//! - **Raw texts are hidden** and listed in `hidden` with what they are,
//!   their size and the one option that shows them: connection strings, full
//!   URLs, descriptions, POST data, non-SQL commands and M formulas.
//! - **SQL command text is the one exception, by an allow-list.** It is
//!   shown whole only when it is a plain read: one `SELECT` or `WITH` query
//!   with no comments, no `EXEC`, `CALL`, `sp_` or `xp_`, no writes, no
//!   `OPENROWSET`-style connections, no string concatenation (other than
//!   with spaces or punctuation), and no backslash, dollar or q quoting.
//!   Credentials are not matched by shape: a statement that names a secret
//!   column anywhere and holds any literal anywhere is hidden, and so is one
//!   with a literal that names or looks like a secret. Everything else is
//!   hidden whole, never in part, and the note says why. A partial mask
//!   shows the text around a secret the patterns did not recognize, and
//!   each check for credential shapes was a deny-list that a review got
//!   past; showing only what is known to be plain turns each miss into a
//!   harmless false positive.
//! - **Nothing is cut.** A text longer than its limit is hidden whole, even
//!   with [`Options::include_raw`]: a cut can end inside a secret and leave
//!   its start where masking no longer recognizes it. Names are the only
//!   text cut to [`Limits::max_name_chars`], with a note of how much.
//!
//! [`Options::include_raw`] adds the raw texts, masked by
//! [`mask_connection_string`] and [`mask_secrets`]. That masking is best
//! effort and the report says so in [`Report::warnings`]; an MCP server
//! leaves the option off unless the person asks.
//!
//! `tests/masking.rs` keeps the review's planted-secret fuzzer: it requires
//! that no six characters of a secret in a row reach the default report, in
//! any of its shapes and encodings. The raw path is fuzzed too, but only held
//! to its current count, since pattern masking cannot promise zero.

mod limits;
mod mashup;
mod mask;
mod package;
mod parts;
mod report;

pub use limits::{Limits, Options};
pub use mask::{MASK, mask_connection_string, mask_secrets};
pub use package::{MemoryParts, PartError, PartSource};
pub use report::{
    Auth, Call, CommandType, Connection, ConnectionKind, Credentials, DataModel, ExternalLink,
    FilePath, Hidden, LinkKind, LinkLocation, PowerQuery, Problem, Query, QueryTableSettings,
    RawItem, RawText, Refresh, Report, Target, Use, UseKind,
};

/// Reads everything the package says about its data sources, as recognized
/// fields only (no raw text). Never fails: what cannot be read is listed in
/// [`Report::problems`] and the rest is still reported.
pub fn inspect(source: &dyn PartSource, limits: &Limits) -> Report {
    inspect_with(source, limits, &Options::default())
}

/// [`inspect`] with options, such as [`Options::include_raw`].
pub fn inspect_with(source: &dyn PartSource, limits: &Limits, options: &Options) -> Report {
    let mut package = package::Package::new(source, limits, options.clone());
    parts::inspect(&mut package)
}
