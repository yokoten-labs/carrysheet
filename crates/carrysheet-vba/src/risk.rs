//! Risk flags: code that runs by itself, reaches outside the workbook, or
//! hides what it does. Flags are reports for a person to read; nothing here
//! runs or changes a macro.
//!
//! All rules are in [`RULES`], one table. Matching ignores case. Comments
//! and `Attribute` lines are skipped; string literals are searched only by
//! rules that look for names that live in strings (ProgIDs such as
//! `WScript.Shell`, DLL aliases).

use crate::{Module, ModuleKind, RiskFlag};

/// What a flagged line or module may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RiskKind {
    /// Runs without the user starting it: on open, close, save, activate.
    AutoRun,
    /// Starts programs or COM objects.
    Execute,
    /// Writes, copies or deletes files.
    FileSystem,
    /// Downloads or talks to the network.
    Network,
    /// Calls Windows DLL functions directly.
    Win32Api,
    /// Reads environment variables (user name, paths).
    Environment,
    /// Calls code by a name built at run time.
    DynamicCall,
    /// Hides strings or code from a reader.
    Obfuscation,
    /// The module hides from the editor, or its source and compiled code
    /// disagree (VBA stomping).
    Concealment,
}

impl RiskKind {
    /// A short lowercase name, such as `auto-run`.
    pub fn as_str(self) -> &'static str {
        match self {
            RiskKind::AutoRun => "auto-run",
            RiskKind::Execute => "execute",
            RiskKind::FileSystem => "file-system",
            RiskKind::Network => "network",
            RiskKind::Win32Api => "win32-api",
            RiskKind::Environment => "environment",
            RiskKind::DynamicCall => "dynamic-call",
            RiskKind::Obfuscation => "obfuscation",
            RiskKind::Concealment => "concealment",
        }
    }
}

/// How a rule matches.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Pattern {
    /// A `Sub` or `Function` with this name.
    Proc(&'static str),
    /// A `Sub` or `Function` whose name ends with this (ActiveX events).
    ProcSuffix(&'static str),
    /// A whole word in code, outside strings. May contain dots.
    Word(&'static str),
    /// A whole word anywhere on the line, strings included.
    Text(&'static str),
    /// These words in this order in code, outside strings.
    Sequence(&'static [&'static str]),
    /// At least `n` uses of these functions on one line.
    Repeated(&'static [&'static str], usize),
    /// A string literal of at least this many characters.
    LongString(usize),
    /// A module-level check, not tied to a line.
    Module(ModuleCheck),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModuleCheck {
    /// In the `dir` stream but not in the `PROJECT` stream.
    Hidden,
    /// Compiled code (p-code) but no source.
    PcodeOnly,
    /// p-code present but the source is broken.
    Unreadable,
    /// Little or no source next to a lot of p-code.
    StompingSuspect,
    /// A line too long to scan whole (a line-level check, kept here
    /// because no text pattern describes it).
    OverlongLine,
    /// The flag list reached its limit and findings were dropped
    /// (emitted by the caller, never matched).
    ListCut,
}

pub(crate) struct Rule {
    pub id: &'static str,
    pub kind: RiskKind,
    pub pattern: Pattern,
    pub why: &'static str,
}

const fn rule(id: &'static str, kind: RiskKind, pattern: Pattern, why: &'static str) -> Rule {
    Rule {
        id,
        kind,
        pattern,
        why,
    }
}

use Pattern::{LongString, Proc, ProcSuffix, Repeated, Sequence, Text, Word};
use RiskKind::{
    AutoRun, Concealment, DynamicCall, Environment, Execute, FileSystem, Network, Obfuscation,
    Win32Api,
};

const OPENS: &str = "runs automatically when the workbook or document opens";
const CLOSES: &str = "runs automatically when the workbook or document closes or is saved";
const ACTIVATES: &str = "runs automatically when a sheet or window is shown or recalculated";
const ACTIVEX: &str = "an ActiveX control or form event that can fire without a click";
const EDITS: &str = "runs automatically whenever a cell is changed or selected";
const FORM: &str = "runs automatically when a form is loaded or shown";

/// Every rule, most specific first: on one line, only the first matching
/// rule of each kind is reported.
#[rustfmt::skip]
pub(crate) const RULES: &[Rule] = &[
    // Auto-run entry points (Excel, Word, PowerPoint names).
    rule("Workbook_Open", AutoRun, Proc("workbook_open"), OPENS),
    rule("Auto_Open", AutoRun, Proc("auto_open"), OPENS),
    rule("AutoOpen", AutoRun, Proc("autoopen"), OPENS),
    rule("Document_Open", AutoRun, Proc("document_open"), OPENS),
    rule("AutoExec", AutoRun, Proc("autoexec"), OPENS),
    rule("AutoNew", AutoRun, Proc("autonew"), OPENS),
    rule("Document_New", AutoRun, Proc("document_new"), OPENS),
    rule("Workbook_BeforeClose", AutoRun, Proc("workbook_beforeclose"), CLOSES),
    rule("Workbook_BeforeSave", AutoRun, Proc("workbook_beforesave"), CLOSES),
    rule("Auto_Close", AutoRun, Proc("auto_close"), CLOSES),
    rule("AutoClose", AutoRun, Proc("autoclose"), CLOSES),
    rule("AutoExit", AutoRun, Proc("autoexit"), CLOSES),
    rule("Document_Close", AutoRun, Proc("document_close"), CLOSES),
    rule("Workbook_AfterSave", AutoRun, Proc("workbook_aftersave"), CLOSES),
    rule("Workbook_Deactivate", AutoRun, Proc("workbook_deactivate"), CLOSES),
    rule("Workbook_BeforePrint", AutoRun, Proc("workbook_beforeprint"), CLOSES),
    rule("Workbook_Activate", AutoRun, Proc("workbook_activate"), ACTIVATES),
    rule("Workbook_WindowActivate", AutoRun, Proc("workbook_windowactivate"), ACTIVATES),
    rule("Workbook_SheetActivate", AutoRun, Proc("workbook_sheetactivate"), ACTIVATES),
    rule("Worksheet_Activate", AutoRun, Proc("worksheet_activate"), ACTIVATES),
    rule("Worksheet_Calculate", AutoRun, Proc("worksheet_calculate"), ACTIVATES),
    rule("Workbook_SheetCalculate", AutoRun, Proc("workbook_sheetcalculate"), ACTIVATES),
    rule("Worksheet_Change", AutoRun, Proc("worksheet_change"), EDITS),
    rule("Worksheet_SelectionChange", AutoRun, Proc("worksheet_selectionchange"), EDITS),
    rule("Workbook_SheetChange", AutoRun, Proc("workbook_sheetchange"), EDITS),
    rule("Workbook_SheetSelectionChange", AutoRun, Proc("workbook_sheetselectionchange"), EDITS),
    rule("Workbook_NewSheet", AutoRun, Proc("workbook_newsheet"), EDITS),
    rule("Worksheet_Deactivate", AutoRun, Proc("worksheet_deactivate"), ACTIVATES),
    rule("Workbook_SheetDeactivate", AutoRun, Proc("workbook_sheetdeactivate"), ACTIVATES),
    rule("UserForm_Initialize", AutoRun, Proc("userform_initialize"), FORM),
    rule("UserForm_Activate", AutoRun, Proc("userform_activate"), FORM),
    rule("Application.OnTime", AutoRun, Word("ontime"), "schedules a macro to run later by itself"),
    rule("Application.OnKey", AutoRun, Word("onkey"), "makes a key press run a macro"),
    rule("_Painted", AutoRun, ProcSuffix("_painted"), ACTIVEX),
    rule("_Painting", AutoRun, ProcSuffix("_painting"), ACTIVEX),
    rule("_GotFocus", AutoRun, ProcSuffix("_gotfocus"), ACTIVEX),
    rule("_Layout", AutoRun, ProcSuffix("_layout"), ACTIVEX),
    rule("_Resize", AutoRun, ProcSuffix("_resize"), ACTIVEX),
    rule("_MouseMove", AutoRun, ProcSuffix("_mousemove"), ACTIVEX),
    rule("_Change", AutoRun, ProcSuffix("_change"), ACTIVEX),
    // Starting programs and objects.
    rule("WScript.Shell", Execute, Text("wscript.shell"), "creates the Windows Script Host shell, which runs any command"),
    rule("Shell.Application", Execute, Text("shell.application"), "creates the Explorer shell object, which can run programs"),
    rule("PowerShell", Execute, Text("powershell"), "mentions PowerShell, often used to run downloaded code"),
    rule("cmd.exe", Execute, Text("cmd.exe"), "mentions the Windows command prompt"),
    rule("ShellExecute", Execute, Text("shellexecute"), "runs a program or opens a file through Windows"),
    rule("Shell", Execute, Word("shell"), "runs a program"),
    rule("MacScript", Execute, Word("macscript"), "runs an AppleScript on a Mac"),
    rule("CreateObject", Execute, Word("createobject"), "creates a COM object, which can reach anything on the computer"),
    rule("GetObject", Execute, Word("getobject"), "attaches to a COM object or file outside the workbook"),
    // Network.
    rule("URLDownloadToFile", Network, Text("urldownloadtofile"), "downloads a file from the internet"),
    rule("URLDownloadToFileA", Network, Text("urldownloadtofilea"), "downloads a file from the internet"),
    rule("ServerXMLHTTP", Network, Text("serverxmlhttp"), "sends web requests"),
    rule("XMLHTTP", Network, Text("xmlhttp"), "sends web requests"),
    rule("WinHttp", Network, Text("winhttp"), "sends web requests"),
    rule("WinHttpRequest", Network, Text("winhttprequest"), "sends web requests"),
    rule("InternetOpen", Network, Text("internetopen"), "opens an internet connection through WinINet"),
    rule("InternetOpenUrl", Network, Text("internetopenurl"), "opens a URL through WinINet"),
    rule("Net.WebClient", Network, Text("net.webclient"), "downloads with the .NET web client"),
    // Files.
    rule("ADODB.Stream", FileSystem, Text("adodb.stream"), "writes binary data, often a downloaded program, to disk"),
    rule("SaveToFile", FileSystem, Word("savetofile"), "saves a stream to a file"),
    rule("FileSystemObject", FileSystem, Text("scripting.filesystemobject"), "reads, writes and deletes files"),
    rule("CreateTextFile", FileSystem, Word("createtextfile"), "creates a file"),
    rule("Kill", FileSystem, Word("kill"), "deletes files"),
    rule("FileCopy", FileSystem, Word("filecopy"), "copies files"),
    rule("Open For Output", FileSystem, Sequence(&["open", "for", "output"]), "writes a file"),
    rule("Open For Append", FileSystem, Sequence(&["open", "for", "append"]), "writes a file"),
    rule("Open For Binary", FileSystem, Sequence(&["open", "for", "binary"]), "reads or writes a file as raw bytes"),
    // Windows API.
    rule("VirtualAlloc", Win32Api, Text("virtualalloc"), "allocates executable memory, a sign of shellcode"),
    rule("RtlMoveMemory", Win32Api, Text("rtlmovememory"), "copies raw memory, a sign of shellcode"),
    rule("CreateThread", Win32Api, Text("createthread"), "starts a native thread, a sign of shellcode"),
    rule("Declare", Win32Api, Word("declare"), "declares a function from a Windows DLL, which bypasses VBA's own limits"),
    // Environment.
    rule("Environ", Environment, Word("environ"), "reads environment variables such as the user name or folders"),
    // Dynamic calls.
    rule("CallByName", DynamicCall, Word("callbyname"), "calls a method by a name built at run time"),
    rule("ExecuteExcel4Macro", DynamicCall, Word("executeexcel4macro"), "runs an old Excel 4 macro, which can call DLLs"),
    // Obfuscation.
    rule("Chr chain", Obfuscation, Repeated(&["chr", "chrw", "chrb"], 4), "builds text from character codes, a way to hide strings"),
    rule("StrReverse", Obfuscation, Word("strreverse"), "reverses a string, a way to hide it"),
    rule("Long string", Obfuscation, LongString(200), "holds a very long string literal, which can hide encoded code"),
    rule("Overlong line", Obfuscation, Pattern::Module(ModuleCheck::OverlongLine), "a statement longer than the VBA editor allows, made by a tool rather than a person; only its start was checked"),
    rule("Flag list cut short", Obfuscation, Pattern::Module(ModuleCheck::ListCut), "the project has more findings than the flag limit, so the rest were not reported; many harmless findings can hide a real one behind them"),
    // Module-level checks.
    rule("Hidden module", Concealment, Pattern::Module(ModuleCheck::Hidden), "the module is in the project but not listed for the editor, so it may not show in the VBA window"),
    rule("P-code only", Concealment, Pattern::Module(ModuleCheck::PcodeOnly), "the module has compiled code but no source; Excel may run code no one can read (VBA stomping)"),
    rule("Unreadable source", Concealment, Pattern::Module(ModuleCheck::Unreadable), "the module's source cannot be read, but compiled code is there; Excel may run code no one can read (VBA stomping)"),
    rule("Stomping suspect", Concealment, Pattern::Module(ModuleCheck::StompingSuspect), "the module has much more compiled code than its source explains; the source may have been replaced (VBA stomping)"),
];

/// p-code bytes of a module with no code, by kind. Measured on Excel
/// files: empty standard modules carry 637 bytes, empty sheet and workbook
/// modules 805 to 1018.
const fn empty_pcode(kind: ModuleKind) -> usize {
    match kind {
        ModuleKind::Standard => 640,
        _ => 1024,
    }
}
/// p-code bytes above the empty baseline that are never suspect on their own.
pub(crate) const STOMPING_SLACK: usize = 384;
/// p-code bytes one character of source text explains. Real code compiles
/// to about 32 to 40 bytes per line; short lines and procedure headers cost
/// more per character (SquareMacro.xls: 119 characters, 1500 bytes).
pub(crate) const STOMPING_BYTES_PER_CHAR: usize = 16;
/// p-code bytes one source line explains at most, however long it is.
pub(crate) const STOMPING_BYTES_PER_LINE: usize = 200;
/// p-code bytes one character of a comment line explains: the compiled
/// code keeps the comment text at about its own length.
pub(crate) const STOMPING_BYTES_PER_COMMENT_CHAR: usize = 2;

/// A source line split into the parts the rules look at.
struct Line {
    /// Code outside strings and comments, ASCII-lowercased.
    code: String,
    /// Code and strings, comments removed, ASCII-lowercased.
    text: String,
    /// Length in characters of each string literal.
    strings: Vec<usize>,
}

fn split_line(line: &str) -> Line {
    let mut code = String::with_capacity(line.len());
    let mut text = String::with_capacity(line.len());
    let mut strings = Vec::new();
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut current = 0usize;
    while let Some(c) = chars.next() {
        if in_string {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current += 1;
                    text.push('"');
                    continue;
                }
                in_string = false;
                strings.push(current);
                text.push('"');
                // Keep words on either side of the string apart.
                code.push(' ');
                continue;
            }
            current += 1;
            text.push(c.to_ascii_lowercase());
        } else {
            match c {
                '"' => {
                    in_string = true;
                    current = 0;
                    text.push('"');
                    code.push(' ');
                }
                '\'' => break,
                _ => {
                    code.push(c.to_ascii_lowercase());
                    text.push(c.to_ascii_lowercase());
                }
            }
        }
    }
    if in_string {
        strings.push(current);
    }
    // `Rem` comments: a statement that starts with the word Rem.
    let trimmed = code.trim_start();
    if trimmed == "rem" || trimmed.starts_with("rem ") || trimmed.starts_with("rem\t") {
        code.clear();
        text.clear();
        strings.clear();
    }
    Line {
        code,
        text,
        strings,
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Counts whole-word occurrences of an ASCII-lowercase needle.
fn count_word(haystack: &str, needle: &str) -> usize {
    let h = haystack.as_bytes();
    let mut count = 0;
    let mut from = 0;
    while let Some(i) = haystack.get(from..).and_then(|s| s.find(needle)) {
        let start = from + i;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_ident(h[start - 1]);
        let after_ok = end >= h.len() || !is_ident(h[end]);
        if before_ok && after_ok {
            count += 1;
        }
        from = end;
    }
    count
}

fn words(code: &str) -> impl Iterator<Item = &str> {
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
}

/// The name of the `Sub` or `Function` this line declares, if any.
fn proc_name(code: &str) -> Option<&str> {
    let mut it = words(code);
    let mut w = it.next()?;
    if matches!(w, "public" | "private" | "friend") {
        w = it.next()?;
    }
    if w == "static" {
        w = it.next()?;
    }
    if matches!(w, "sub" | "function") {
        it.next()
    } else {
        None
    }
}

fn line_matches(pattern: &Pattern, line: &Line) -> bool {
    match *pattern {
        Pattern::Proc(name) => proc_name(&line.code) == Some(name),
        Pattern::ProcSuffix(suffix) => proc_name(&line.code).is_some_and(|n| n.ends_with(suffix)),
        Pattern::Word(w) => count_word(&line.code, w) > 0,
        Pattern::Text(w) => count_word(&line.text, w) > 0,
        Pattern::Sequence(seq) => {
            let mut want = seq.iter();
            let mut next = want.next();
            for w in words(&line.code) {
                if Some(&w) == next {
                    next = want.next();
                    if next.is_none() {
                        return true;
                    }
                }
            }
            false
        }
        Pattern::Repeated(names, n) => {
            names
                .iter()
                .map(|w| count_word(&line.code, w))
                .sum::<usize>()
                >= n
        }
        Pattern::LongString(n) => line.strings.iter().any(|&len| len >= n),
        Pattern::Module(_) => false,
    }
}

/// Facts about a module for the module-level checks.
pub(crate) struct ModuleFacts {
    pub kind: ModuleKind,
    pub hidden: bool,
    pub pcode_bytes: usize,
    /// The source container was there and decompressed.
    pub source_present: bool,
    /// The source could not be read (error).
    pub unreadable: bool,
    /// p-code bytes the source text explains, from [`source_stats`].
    pub allowance: usize,
}

fn module_matches(check: ModuleCheck, f: &ModuleFacts) -> bool {
    match check {
        ModuleCheck::OverlongLine | ModuleCheck::ListCut => false,
        ModuleCheck::Hidden => f.hidden,
        ModuleCheck::PcodeOnly => !f.source_present && !f.unreadable && f.pcode_bytes > 0,
        ModuleCheck::Unreadable => f.unreadable && f.pcode_bytes > 0,
        ModuleCheck::StompingSuspect => {
            let excess = f.pcode_bytes.saturating_sub(empty_pcode(f.kind));
            f.source_present && excess > STOMPING_SLACK && excess > f.allowance
        }
    }
}

/// Line facts about a source text, in one pass.
pub(crate) struct SourceStats {
    /// All lines.
    pub lines: usize,
    /// Lines that are not blank and not `Attribute` lines. Comments count;
    /// a module whose only lines are `Attribute` lines has no source.
    pub text_lines: usize,
    /// See [`source_stats`].
    pub allowance: usize,
}

/// Line facts about a source text. The allowance is how many p-code
/// bytes the source text can explain: for each line that
/// is not blank and not an `Attribute` line, 16 bytes per character of
/// trimmed text, at most 200 per line; a comment line, 2 bytes per
/// character. Padding a decoy with a few short lines or with comments
/// therefore explains little.
///
/// Known limit: a decoy padded with enough short code lines (about 47
/// lines of 13 characters per 10 KB of p-code) still explains the p-code
/// it hides. The strong check is to compare the procedure names in the
/// p-code with the source, but `_VBA_PROJECT` and the p-code layout are
/// not in MS-OVBA, so that is not done.
pub(crate) fn source_stats(source: &str) -> SourceStats {
    let mut stats = SourceStats {
        lines: 0,
        text_lines: 0,
        allowance: 0,
    };
    for line in crate::source_lines(source) {
        stats.lines += 1;
        let t = line.trim();
        if !t.is_empty() && !starts_with_word(t, "attribute") {
            stats.text_lines += 1;
            stats.allowance += if t.starts_with('\'') || starts_with_word(t, "rem") {
                t.len() * STOMPING_BYTES_PER_COMMENT_CHAR
            } else {
                (t.len() * STOMPING_BYTES_PER_CHAR).min(STOMPING_BYTES_PER_LINE)
            };
        }
    }
    stats
}

/// [`source_stats`]'s allowance alone.
#[cfg(test)]
pub(crate) fn pcode_allowance(source: &str) -> usize {
    source_stats(source).allowance
}

fn starts_with_word(line: &str, word: &str) -> bool {
    line.len() >= word.len()
        && line.as_bytes()[..word.len()].eq_ignore_ascii_case(word.as_bytes())
        && line
            .as_bytes()
            .get(word.len())
            .is_none_or(|&b| !is_ident(b))
}

/// Physical lines joined at ` _` line continuations, with the number of
/// the first physical line of each and whether the join stopped at the cap
/// of 64 lines (VBA allows 24), so a hostile module cannot make one huge
/// line. An `Attribute` line is never continued: the line after it is read
/// on its own.
fn logical_lines(source: &str) -> impl Iterator<Item = (u32, String, bool)> + '_ {
    let mut lines = crate::source_lines(source).enumerate().peekable();
    std::iter::from_fn(move || {
        let (first, line) = lines.next()?;
        let mut joined = line.to_string();
        let mut capped = false;
        if !starts_with_word(line.trim_start(), "attribute") {
            let mut parts = 1;
            while continues(&joined) {
                if parts == MAX_JOINED_LINES {
                    capped = true;
                    break;
                }
                let Some((_, next)) = lines.next() else {
                    break;
                };
                // Drop the `_` and keep the space before it.
                joined.truncate(joined.trim_end().len() - 1);
                joined.push_str(next);
                parts += 1;
            }
        }
        Some((first as u32 + 1, joined, capped))
    })
}

/// Physical lines joined into one statement at most.
const MAX_JOINED_LINES: usize = 64;

/// A line ends with a continuation: a space (or tab) then `_`.
fn continues(line: &str) -> bool {
    let t = line.trim_end();
    t == "_" || t.ends_with(" _") || t.ends_with("\t_")
}

/// Characters of one logical line that are scanned. VBA allows 1023
/// characters per line and 24 continuations; anything longer is hand-made.
pub(crate) const MAX_SCANNED_LINE: usize = 32 * 1024;

/// Line-level findings in one source text.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// (index into [`RULES`], line).
    pub hits: Vec<(usize, u32)>,
    /// More findings were there than room for them.
    pub cut: bool,
}

/// Scans one source text for line-level findings, keeping at most `room`
/// of them: the room left in the project's flag list. With no room left
/// it stops at the first finding and only reports the cut. `events` is
/// whether control and form events (`TextBox1_Change`) can fire in the
/// module: they only do in document, class and form modules; in a
/// standard module the same name is an ordinary procedure, so those rules
/// are not tried there and cannot fill the room.
pub(crate) fn scan_source(source: &str, room: usize, events: bool) -> Scan {
    let mut scan = Scan::default();
    let hits = &mut scan.hits;
    let mut seen_kinds: Vec<RiskKind> = Vec::new();
    for (number, mut raw, capped) in logical_lines(source) {
        if starts_with_word(raw.trim_start(), "attribute") {
            continue;
        }
        let overlong = capped || raw.len() > MAX_SCANNED_LINE;
        if raw.len() > MAX_SCANNED_LINE {
            let mut cut = MAX_SCANNED_LINE;
            while !raw.is_char_boundary(cut) {
                cut -= 1;
            }
            raw.truncate(cut);
        }
        let line = split_line(&raw);
        if line.text.is_empty() && !overlong {
            continue;
        }
        seen_kinds.clear();
        for (i, r) in RULES.iter().enumerate() {
            // An overlong line is always reported: other flags on the line
            // do not stand in for the part that was not checked.
            let (matched, exclusive) = match r.pattern {
                Pattern::Module(ModuleCheck::OverlongLine) => (overlong, false),
                Pattern::ProcSuffix(_) if !events => (false, true),
                _ => (line_matches(&r.pattern, &line), true),
            };
            if !matched || (exclusive && seen_kinds.contains(&r.kind)) {
                continue;
            }
            if hits.len() >= room {
                scan.cut = true;
                return scan;
            }
            if exclusive {
                seen_kinds.push(r.kind);
            }
            hits.push((i, number));
        }
    }
    scan
}

/// Appends a module's flags: module-level checks, then its source's line
/// findings from [`scan_source`]. Stops at `max_flags`, and returns
/// whether any flag was left out because of it.
pub(crate) fn module_flags(
    index: usize,
    module: &Module,
    facts: &ModuleFacts,
    hits: &[(usize, u32)],
    out: &mut Vec<RiskFlag>,
    max_flags: usize,
) -> bool {
    let shown = crate::error::printable(&module.name);
    let flag = |r: &Rule, line| RiskFlag {
        kind: r.kind,
        rule: r.id,
        module: shown.clone(),
        module_index: index,
        line,
        why: r.why,
    };
    for r in RULES {
        if let Pattern::Module(check) = r.pattern
            && module_matches(check, facts)
        {
            if out.len() >= max_flags {
                return true;
            }
            out.push(flag(r, None));
        }
    }
    for &(i, line) in hits {
        if out.len() >= max_flags {
            return true;
        }
        out.push(flag(&RULES[i], Some(line)));
    }
    false
}

/// The flag that says the list was cut short at `module`. It is added
/// once per project, past `max_flags`, so a cut list is never silent.
pub(crate) fn cut_flag(index: usize, module: &Module) -> RiskFlag {
    let r = RULES
        .iter()
        .find(|r| matches!(r.pattern, Pattern::Module(ModuleCheck::ListCut)))
        .unwrap_or(&RULES[0]);
    RiskFlag {
        kind: r.kind,
        rule: r.id,
        module: crate::error::printable(&module.name),
        module_index: index,
        line: None,
        why: r.why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags_for(src: &str) -> Vec<(&'static str, Option<u32>)> {
        flags_in(ModuleKind::Document, src)
    }

    fn flags_in(kind: ModuleKind, src: &str) -> Vec<(&'static str, Option<u32>)> {
        let module = Module::test_module("M", src);
        let facts = ModuleFacts {
            kind,
            hidden: false,
            pcode_bytes: 0,
            source_present: true,
            unreadable: false,
            allowance: pcode_allowance(src),
        };
        let mut out = Vec::new();
        let scan = scan_source(src, 1000, kind != ModuleKind::Standard);
        module_flags(0, &module, &facts, &scan.hits, &mut out, 1000);
        out.into_iter().map(|f| (f.rule, f.line)).collect()
    }

    #[test]
    fn rule_ids_are_unique() {
        for (i, a) in RULES.iter().enumerate() {
            assert!(RULES[i + 1..].iter().all(|b| b.id != a.id), "{}", a.id);
        }
    }

    #[test]
    fn finds_auto_run_and_shell_case_insensitively() {
        let src = "Attribute VB_Name = \"Shell\"\r\nPRIVATE SUB workbook_OPEN()\r\n  x = shell(\"calc\")\r\nEnd Sub\r\n";
        assert_eq!(
            flags_for(src),
            [("Workbook_Open", Some(2)), ("Shell", Some(3))]
        );
    }

    #[test]
    fn comments_do_not_flag_but_strings_do_for_progids() {
        let src = "' Shell and Kill in a comment\r\nRem Kill this\r\nSet o = CreateObject(\"WScript.Shell\") ' Kill\r\nMsgBox \"Kill switch\"\r\n";
        // CreateObject and WScript.Shell share a kind: the more specific wins.
        assert_eq!(flags_for(src), [("WScript.Shell", Some(3))]);
    }

    #[test]
    fn words_must_be_whole() {
        assert!(flags_for("Dim Shelly As Long\r\nSkill = 1\r\nx.Shell_Name = 2\r\n").is_empty());
    }

    #[test]
    fn open_for_output_and_declare() {
        let src = "Private Declare PtrSafe Function URLDownloadToFile Lib \"urlmon\" Alias \"URLDownloadToFileA\" ()\r\nOpen p For Output As #1\r\n";
        assert_eq!(
            flags_for(src),
            [
                ("URLDownloadToFile", Some(1)),
                ("Declare", Some(1)),
                ("Open For Output", Some(2))
            ]
        );
    }

    #[test]
    fn obfuscation_signals() {
        let long = format!("s = \"{}\"\r\n", "A".repeat(250));
        let src = format!(
            "x = Chr(72) & ChrW(105) & Chr$(33) & chrb(1)\r\ny = StrReverse(\"cba\")\r\n{long}"
        );
        assert_eq!(
            flags_for(&src),
            [
                ("Chr chain", Some(1)),
                ("StrReverse", Some(2)),
                ("Long string", Some(3))
            ]
        );
    }

    #[test]
    fn module_checks() {
        use ModuleKind::{Document, Standard};
        let f = |kind, pcode, source_present, unreadable, source: &str| {
            let facts = ModuleFacts {
                kind,
                hidden: false,
                pcode_bytes: pcode,
                source_present,
                unreadable,
                allowance: pcode_allowance(source),
            };
            [
                ModuleCheck::PcodeOnly,
                ModuleCheck::Unreadable,
                ModuleCheck::StompingSuspect,
            ]
            .map(|c| module_matches(c, &facts))
        };
        let none = [false, false, false];
        let stomp = [false, false, true];
        let attr = "Attribute VB_Name = \"M\"\r\n";
        assert_eq!(
            f(Standard, 637, true, false, attr),
            none,
            "empty standard module"
        );
        assert_eq!(
            f(Document, 1018, true, false, attr),
            none,
            "empty sheet module"
        );
        assert_eq!(
            f(Standard, 1186, true, false, attr),
            stomp,
            "decoy over 1186 bytes"
        );
        // SquareMacro.xls: three one-line functions and Option Explicit
        // compile to 2140 bytes; 119 characters of text must explain it.
        let udf = "Option Explicit\r\nFunction SQR(n)\r\n    SQR = n * n\r\nEnd Function\r\nFunction ONE()\r\n    ONE = 1\r\nEnd Function\r\nFunction TWO()\r\n    TWO = 2\r\nEnd Function\r\n";
        assert_eq!(f(Standard, 2140, true, false, udf), none, "SquareMacro");
        let real = "    ActiveCell.FormulaR1C1 = \"This is a macro workbook\"\r\n".repeat(250);
        assert_eq!(
            f(Standard, 9000, true, false, &real),
            none,
            "250 lines explain 9 KB"
        );
        // Padding: short lines explain little.
        assert_eq!(
            f(Standard, 1186, true, false, "Sub a\r\n"),
            stomp,
            "one `Sub a`"
        );
        assert_eq!(
            f(Standard, 1186, true, false, "x\r\nx\r\nx\r\n"),
            stomp,
            "three `x`"
        );
        assert_eq!(
            f(Standard, 10_000, true, false, &"Sub a\r\n".repeat(16)),
            stomp,
            "16 `Sub a`"
        );
        assert_eq!(
            f(Standard, 500, false, false, ""),
            [true, false, false],
            "p-code only"
        );
        assert_eq!(
            f(Standard, 1186, false, true, ""),
            [false, true, false],
            "unreadable"
        );
        assert_eq!(f(Standard, 0, false, true, ""), none, "missing stream");
    }

    #[test]
    fn continued_lines_are_joined() {
        let src = "Private Sub _\r\nWorkbook_Open()\r\nOpen p _\r\n  For Output As #1\r\n' a comment _\r\nKill p\r\nKill q\r\n";
        assert_eq!(
            flags_for(src),
            [
                ("Workbook_Open", Some(1)),
                ("Open For Output", Some(3)),
                ("Kill", Some(7))
            ]
        );
        // The comment continued onto line 6, so `Kill p` there is a comment.
        let lines: Vec<_> = logical_lines(src).map(|(n, _, _)| n).collect();
        assert_eq!(lines, [1, 3, 5, 7]);
    }

    #[test]
    fn events_that_fire_without_a_click() {
        let src = "Private Sub Worksheet_Change(ByVal Target As Range)\r\nEnd Sub\r\nPrivate Sub TextBox1_Change()\r\nEnd Sub\r\nPrivate Sub UserForm_Initialize()\r\n";
        assert_eq!(
            flags_for(src),
            [
                ("Worksheet_Change", Some(1)),
                ("_Change", Some(3)),
                ("UserForm_Initialize", Some(5))
            ]
        );
        // In a standard module `TextBox1_Change` is an ordinary procedure.
        assert_eq!(
            flags_in(ModuleKind::Standard, src),
            [
                ("Worksheet_Change", Some(1)),
                ("UserForm_Initialize", Some(5))
            ]
        );
        let more = "Private Sub Workbook_SheetChange(ByVal Sh As Object, ByVal Target As Range)\r\nApplication.OnTime Now + TimeValue(\"00:00:05\"), \"Payload\"\r\nApplication.OnKey \"~\", \"Payload\"\r\nPrivate Sub Workbook_BeforePrint(Cancel As Boolean)\r\n";
        assert_eq!(
            flags_for(more),
            [
                ("Workbook_SheetChange", Some(1)),
                ("Application.OnTime", Some(2)),
                ("Application.OnKey", Some(3)),
                ("Workbook_BeforePrint", Some(4))
            ]
        );
    }

    #[test]
    fn attribute_lines_are_never_continued() {
        let src = "Attribute VB_Name = \"M\" _\r\nSub AutoOpen(): Shell \"calc\": End Sub\r\n";
        assert_eq!(flags_for(src), [("AutoOpen", Some(2)), ("Shell", Some(2))]);
    }

    #[test]
    fn overlong_is_always_reported() {
        // A long string flags Obfuscation first; Overlong line still shows.
        let src = format!(
            "x = \"{}\" & {}\r\n",
            "a".repeat(300),
            "1 + ".repeat(10_000)
        );
        assert_eq!(
            flags_for(&src),
            [("Long string", Some(1)), ("Overlong line", Some(1))]
        );
        // A statement continued past the 64-line cap.
        let src = format!(
            "Private Sub _\r\n{}Workbook_Open()\r\n",
            " _\r\n".repeat(63)
        );
        assert_eq!(flags_for(&src), [("Overlong line", Some(1))]);
        // 25 lines is within the cap.
        let src = format!(
            "Private Sub _\r\n{}Workbook_Open()\r\n",
            " _\r\n".repeat(23)
        );
        assert_eq!(flags_for(&src), [("Workbook_Open", Some(1))]);
    }
}
