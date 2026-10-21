//! Limits that keep a hostile workbook from using unbounded memory or time.

/// Limits on what [`crate::inspect`] reads and produces. The defaults suit
/// real workbooks with room to spare; anything past a limit is reported as a
/// problem and skipped, never read further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Largest XML part read (connections, query tables, links, workbook,
    /// relationships). Default 4 MiB.
    pub max_xml_part_bytes: u64,
    /// Largest customXml part read when looking for a Power Query
    /// DataMashup (base64 text). Default 64 MiB.
    pub max_mashup_bytes: u64,
    /// Most connections, query tables, external links, sheets, tables,
    /// pivot caches, relationships per part, defined names and Power Query
    /// queries, each counted on its own. Default 10,000.
    pub max_items: usize,
    /// Most part names looked at from the source. Default 100,000.
    pub max_parts: usize,
    /// Deepest XML element nesting. Default 64.
    pub max_depth: usize,
    /// Longest command text kept, in characters; longer text is cut with a
    /// marker. Default 4,000.
    pub max_command_chars: usize,
    /// Longest Power Query formula kept, in characters. Default 16,000.
    pub max_formula_chars: usize,
    /// Longest name, path, URL or other short string kept. Default 1,000.
    pub max_name_chars: usize,
    /// Most entries in the zip inside a DataMashup. Default 1,000.
    pub max_zip_entries: usize,
    /// Largest one entry of that zip may inflate to. Default 16 MiB.
    pub max_zip_entry_bytes: u64,
    /// Largest all entries read from that zip may inflate to together.
    /// Default 64 MiB.
    pub max_zip_total_bytes: u64,
    /// Most items in the whole report: connections, uses, links, sheet
    /// names, link items and queries together. Default 200,000.
    pub max_total_items: usize,
    /// Most bytes of text in the whole report, all strings together.
    /// Default 64 MiB.
    pub max_total_chars: usize,
    /// Most bytes of XML read from all parts together. Default 256 MiB.
    pub max_total_xml_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_xml_part_bytes: 4 * MIB,
            max_mashup_bytes: 64 * MIB,
            max_items: 10_000,
            max_parts: 100_000,
            max_depth: 64,
            max_command_chars: 4_000,
            max_formula_chars: 16_000,
            max_name_chars: 1_000,
            max_zip_entries: 1_000,
            max_zip_entry_bytes: 16 * MIB,
            max_zip_total_bytes: 64 * MIB,
            max_total_items: 200_000,
            max_total_chars: 64 * 1024 * 1024,
            max_total_xml_bytes: 256 * MIB,
        }
    }
}

/// What [`crate::inspect_with`] adds to the report beyond the fields it
/// recognizes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Adds the raw connection strings, command texts, descriptions, web
    /// query URLs and M formulas, cut to the limits and then masked by
    /// [`crate::mask_secrets`] and [`crate::mask_connection_string`].
    /// Masking free text is best effort: it hides the shapes secrets usually
    /// take, not every secret, so the report then carries a warning. Off by
    /// default; an MCP server should leave it off unless the person asks.
    pub include_raw: bool,
}
