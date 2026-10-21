//! What [`crate::inspect`] returns. Plain data, ready for JSON.
//!
//! By default the report holds only fields a parser recognized: provider,
//! server, port, database, file path, URL reduced to scheme, host and path,
//! the kind of sign-in, names, and the functions Power Query calls. No raw
//! connection string, command text, M formula or full URL is in it, so a
//! secret cannot ride along in text nobody understood; what was left out is
//! listed in `hidden`, with its size and the option that shows it. Raw text,
//! masked as well as patterns can do it, is added only on request
//! ([`crate::Options::include_raw`]), under `raw` and `formula`, with a
//! warning in [`Report::warnings`].

use serde::Serialize;

/// Everything a workbook says about where its data comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Data connections from `xl/connections.xml`, in file order.
    pub connections: Vec<Connection>,
    /// Links to other workbooks, DDE servers and OLE objects, in the order
    /// formulas number them (`[1]`, `[2]`, ...).
    pub external_links: Vec<ExternalLink>,
    /// The Power Query package (DataMashup), when the workbook has one.
    pub power_query: Option<PowerQuery>,
    /// The embedded data model (Power Pivot), when the workbook has one.
    pub data_model: Option<DataModel>,
    /// What could not be read, and why. Empty for a clean workbook.
    pub problems: Vec<Problem>,
    /// Said once when raw text was asked for: its masking is best effort.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Report {
    /// True when the workbook has no connections, links, Power Query or data
    /// model.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
            && self.external_links.is_empty()
            && self.power_query.is_none()
            && self.data_model.is_none()
    }
}

/// One data connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connection {
    /// The connection's `id`; query tables and pivot caches refer to it.
    pub id: u32,
    pub name: String,
    pub kind: ConnectionKind,
    /// The raw `type` number from the file (1 ODBC, 2 DAO, 3 file database,
    /// 4 web query, 5 OLE DB, 6 text, 7 ADO, 8 DSP; others are extensions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_number: Option<u32>,
    /// Where the data comes from, read from the connection string and the
    /// web or text settings. A table or cube command fills `table`.
    pub target: Target,
    /// How the connection string signs in, by the kinds of properties it
    /// has: never their values.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub auth: Vec<Auth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_type: Option<CommandType>,
    /// The SQL command, shown whole when nothing in it looks like a
    /// credential; otherwise it is hidden whole and listed in `hidden`.
    /// Never partly masked here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_text: Option<String>,
    /// Each text left out of this connection: what it is, its size, and how
    /// to see it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<Hidden>,
    /// The raw texts, masked as well as patterns can do it; only with
    /// [`crate::Options::include_raw`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawText>,
    /// The `.odc` connection file this connection was made from: its path
    /// (`file`) or URL (`url`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_file: Option<Target>,
    pub refresh: Refresh,
    /// Query tables, tables and pivot caches that take data from it.
    pub used_by: Vec<Use>,
    /// Anything else worth knowing, as short sentences.
    pub notes: Vec<String>,
}

/// The kind of sign-in a connection string holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// Windows sign-in (`Integrated Security`, `Trusted_Connection`).
    Integrated,
    /// A user name is present.
    User,
    /// A password is present.
    Password,
    /// A token is present.
    Token,
    /// An account, access or API key is present.
    Key,
    /// Another secret property is present.
    OtherSecret,
}

/// A text the report leaves out by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hidden {
    /// What it is: `connection string`, `SQL command`, `M formula`, ...
    pub what: String,
    /// Its length in characters.
    pub characters: usize,
    /// One sentence for people: why it is hidden and the flag that shows it.
    pub note: String,
}

/// A file path, read into its parts. Only a path whose every part parsed is
/// shown; anything else is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilePath {
    /// The whole path, as written.
    pub path: String,
    /// The server of a network path (`\\server\share\...`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The share of a network path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<String>,
    /// The drive letter of a local path, such as `C:`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive: Option<String>,
    /// The folder the file is in, after the drive or share.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The last part of the path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

impl FilePath {
    pub(crate) fn text_len(&self) -> usize {
        self.path.len()
            + text_len([
                &self.host,
                &self.share,
                &self.drive,
                &self.folder,
                &self.file_name,
            ])
    }
}

/// Raw texts of a connection, for callers that asked for them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RawText {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_string: Option<String>,
    /// SQL, table or cube name, or web POST data, masked. A text longer than
    /// [`crate::Limits::max_command_chars`] is never cut: it stays hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_text: Option<String>,
    /// The web query URL as written, masked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The source file as written, masked, when it is not a path or URL the
    /// report could read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    /// The connection file as written, masked, when it is not a path or URL
    /// the report could read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_file: Option<String>,
}

/// One raw text of an external link, for callers that asked for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RawItem {
    /// What it is: `target`, `DDE topic`, `DDE service`, ...
    pub what: String,
    /// The text as written, masked as well as patterns can do it.
    pub text: String,
}

/// What kind of source a connection reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Odbc,
    Dao,
    FileDatabase,
    WebQuery,
    OleDb,
    TextFile,
    Ado,
    Dsp,
    /// An OLAP cube (OLE DB with OLAP settings or an MSOLAP provider).
    Olap,
    /// The workbook's own data model (Power Pivot).
    DataModel,
    /// A Power Query query loaded through the Mashup OLE DB provider.
    PowerQuery,
    /// A table or range in this workbook added to the data model.
    Worksheet,
    Other,
}

/// Where a connection's data lives. Fields the file does not say are absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Target {
    /// The server host, as `host\instance` when an instance is named.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FilePath>,
    /// Scheme, host, port and path only: never user information, query or
    /// fragment. A path that may carry a credential (a share link, a
    /// webhook, `;name=value` parameters, a token) is left out too, and
    /// `hidden_path` says so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A URL path or file path left out because it may carry a credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_path: Option<Hidden>,
    /// The OLE DB provider or the ODBC driver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The ODBC data source name (`DSN=`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_source_name: Option<String>,
    /// The Power Query query a Mashup connection loads (`Location=`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// The table or range a worksheet connection reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
}

/// What the command text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandType {
    Cube,
    Sql,
    Table,
    Default,
    WebList,
    /// Data sent with a web query's POST.
    Post,
    Unknown,
}

/// When and how a connection refreshes. Absent attributes take the defaults
/// ECMA-376 gives them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refresh {
    /// Refreshes when the workbook opens.
    pub on_load: bool,
    /// Refreshes in the background.
    pub background: bool,
    /// Refreshes every this many minutes; absent when it does not repeat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u32>,
    /// Keeps the external data in the file.
    pub save_data: bool,
    /// Saves the password with the connection (the password itself is
    /// never shown).
    pub save_password: bool,
    pub credentials: Credentials,
    /// Keeps the connection open after a refresh.
    pub keep_alive: bool,
}

impl Default for Refresh {
    fn default() -> Self {
        Self {
            on_load: false,
            background: false,
            interval_minutes: None,
            save_data: false,
            save_password: false,
            credentials: Credentials::Integrated,
            keep_alive: false,
        }
    }
}

/// How a connection signs in to its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Credentials {
    /// Windows sign-in of the person refreshing.
    Integrated,
    /// No sign-in.
    None,
    /// A user name and password stored with the connection or elsewhere.
    Stored,
    /// Asks for them on refresh.
    Prompt,
}

/// Something in the workbook that takes data from a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Use {
    pub kind: UseKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// The cells it fills, such as `Sheet1!$A$1:$D$20`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The package part that holds it.
    pub part: String,
    /// The query table's own refresh settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_table: Option<QueryTableSettings>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UseKind {
    QueryTable,
    Table,
    PivotCache,
}

/// Settings stored on a query table (ECMA-376 defaults when absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryTableSettings {
    pub refresh_on_load: bool,
    pub background_refresh: bool,
    pub refresh_disabled: bool,
    pub remove_data_on_save: bool,
}

/// A link to data outside the workbook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExternalLink {
    /// The number formulas use for this link: `[1]Sheet1!A1`.
    pub index: u32,
    pub kind: LinkKind,
    /// The other file: its path (`file`) or URL (`url`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// How the path is stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<LinkLocation>,
    /// Sheet names cached from the other workbook.
    pub sheet_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dde_service: Option<String>,
    /// The DDE topic when it parses as a name, a path or `[book]sheet`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dde_topic: Option<String>,
    /// The OLE program, such as `Word.Document.8`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
    /// DDE or OLE items the workbook reads, when they parse as names.
    pub items: Vec<String>,
    pub part: String,
    /// Each text left out of this link: what it is, its size, and how to
    /// see it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<Hidden>,
    /// The link's raw target and DDE topic, masked as well as patterns can
    /// do it; only with [`crate::Options::include_raw`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub raw: Vec<RawItem>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Workbook,
    Dde,
    Ole,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkLocation {
    /// A normal path or URL.
    Path,
    /// Excel could not find the file when it saved.
    PathMissing,
    /// In Excel's startup folder.
    Startup,
    /// In Excel's alternate startup folder.
    AlternateStartup,
    /// In Excel's library folder.
    Library,
    Other,
}

/// The Power Query package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PowerQuery {
    /// The customXml part that holds it.
    pub part: String,
    /// Size of that part in bytes.
    pub size_bytes: u64,
    /// Size of the decoded package in bytes.
    pub package_bytes: u64,
    pub queries: Vec<Query>,
    pub notes: Vec<String>,
}

/// One Power Query query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Query {
    pub name: String,
    /// The library functions it calls, in order, with the server, database,
    /// URL or path a literal argument names.
    pub calls: Vec<Call>,
    /// Calls to functions outside the M standard library, not listed.
    pub other_calls: u32,
    /// The M formula, when it is left out: its size and how to see it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Hidden>,
    /// The M formula, masked; only with [`crate::Options::include_raw`].
    /// A formula longer than [`crate::Limits::max_formula_chars`] is never
    /// cut: it stays hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
}

/// One call in a Power Query formula, such as `Sql.Database("srv", "db")`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Call {
    pub function: String,
    /// What the literal arguments name, for the functions that reach data.
    #[serde(skip_serializing_if = "Target::is_empty")]
    pub target: Target,
}

impl Target {
    /// True when no field is known.
    pub fn is_empty(&self) -> bool {
        *self == Target::default()
    }
}

/// The embedded data model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataModel {
    pub part: String,
    /// Size in bytes, when the source knows it without reading the part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Something that could not be read. Written for people and agents: what
/// happened, why, and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    pub what: String,
    pub why: String,
    pub fix: String,
}

/// Bytes of text in the optional strings, for the report's total budget.
fn text_len<'a>(strings: impl IntoIterator<Item = &'a Option<String>>) -> usize {
    strings.into_iter().flatten().map(String::len).sum()
}

fn hidden_len(hidden: &[Hidden]) -> usize {
    hidden.iter().map(|h| h.what.len() + h.note.len()).sum()
}

impl Target {
    pub(crate) fn text_len(&self) -> usize {
        text_len([
            &self.server,
            &self.database,
            &self.url,
            &self.provider,
            &self.data_source_name,
            &self.query,
            &self.table,
        ]) + self.file.as_ref().map_or(0, FilePath::text_len)
            + self
                .hidden_path
                .as_ref()
                .map_or(0, |h| h.what.len() + h.note.len())
    }
}

fn list_len(strings: &[String]) -> usize {
    strings.iter().map(String::len).sum()
}

impl Connection {
    /// Bytes of text, without what uses it (each use is charged on its own).
    pub(crate) fn text_len(&self) -> usize {
        let t = &self.target;
        let raw = self.raw.as_ref().map_or(0, |r| {
            text_len([
                &r.description,
                &r.connection_string,
                &r.command_text,
                &r.url,
                &r.source_file,
                &r.connection_file,
            ])
        });
        let hidden = hidden_len(&self.hidden);
        raw + hidden
            + text_len([&self.command_text])
            + self.name.len()
            + t.text_len()
            + self.connection_file.as_ref().map_or(0, Target::text_len)
            + list_len(&self.notes)
    }
}

impl Use {
    pub(crate) fn text_len(&self) -> usize {
        self.part.len() + text_len([&self.name, &self.sheet, &self.table, &self.range])
    }
}

impl ExternalLink {
    /// Bytes of text, without the sheet names and items (each is charged as
    /// it is read).
    pub(crate) fn text_len(&self) -> usize {
        self.part.len()
            + self.target.as_ref().map_or(0, Target::text_len)
            + text_len([&self.dde_service, &self.dde_topic, &self.program_id])
            + hidden_len(&self.hidden)
            + self
                .raw
                .iter()
                .map(|r| r.what.len() + r.text.len())
                .sum::<usize>()
            + list_len(&self.notes)
    }
}

impl Query {
    pub(crate) fn text_len(&self) -> usize {
        self.name.len()
            + self.formula.as_ref().map_or(0, String::len)
            + self
                .hidden
                .as_ref()
                .map_or(0, |h| h.what.len() + h.note.len())
            + self
                .calls
                .iter()
                .map(|c| c.function.len() + c.target.text_len())
                .sum::<usize>()
    }
}
