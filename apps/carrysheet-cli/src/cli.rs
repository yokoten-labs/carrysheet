//! The clap command line, generated from the command table.

use std::io::Read;
use std::path::Path;

use carrysheet::commands::table::{self, Arg, ArgType, COMMANDS, Command};
use carrysheet::commands::{CommandError, ErrorCode};
use clap::builder::PossibleValuesParser;
use clap::{ArgAction, ArgMatches};
use serde_json::{Map, Value};

/// The program name, as typed.
pub const BIN: &str = "carrysheet-cli";

/// The largest `@file` an argument may be read from.
const MAX_ARG_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The flag name without `--`: `max_rows` is `max-rows` (see [`Arg::flag`]).
pub fn flag_name(arg: &Arg) -> String {
    arg.flag().split_off(2)
}

fn clap_arg(arg: &'static Arg) -> clap::Arg {
    let mut a = clap::Arg::new(arg.name).help(arg.description);
    if !arg.positional {
        a = a.long(flag_name(arg));
    }
    a = match arg.ty {
        ArgType::Boolean => a.action(ArgAction::SetTrue),
        ArgType::Integer { .. } => a
            .value_name("N")
            .value_parser(clap::value_parser!(i64))
            .allow_negative_numbers(true),
        ArgType::Enum(values) => a.value_parser(PossibleValuesParser::new(values.iter().copied())),
        ArgType::TextList => a.action(ArgAction::Append),
        ArgType::ObjectList(_) => a.value_name("JSON"),
        ArgType::Path => a.value_hint(clap::ValueHint::AnyPath),
        // The table keeps typed values inside JSON lists (see its tests); were
        // one ever a flag, its value would be text.
        ArgType::Text | ArgType::Range | ArgType::Scalar => a,
    };
    if arg.positional {
        a = a.value_name(arg.name.to_ascii_uppercase());
    }
    if arg.required {
        a = a.required(true);
    }
    a
}

fn clap_command(name: &'static str, command: &'static Command) -> clap::Command {
    let mut c = clap::Command::new(name)
        .about(command.summary)
        .long_about(command.description);
    for arg in command.all_args() {
        c = c.arg(clap_arg(arg));
    }
    c
}

/// Builds the whole command line from [`COMMANDS`].
pub fn build() -> clap::Command {
    let mut root = clap::Command::new(BIN)
        .version(env!("CARGO_PKG_VERSION"))
        .about("Carrysheet: read, convert and edit spreadsheets, for people and AI agents")
        .after_help(
            "Agents: run `carrysheet-cli commands --json` for every command and its schema.",
        )
        .max_term_width(80)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            clap::Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Print only JSON on stdout, errors included"),
        )
        .arg(
            clap::Arg::new("yes")
                .long("yes")
                .short('y')
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Confirm deleting, overwriting and saves that lose content (--confirm and --confirm-loss)"),
        )
        .arg(
            clap::Arg::new("max-seconds")
                .long("max-seconds")
                .global(true)
                .value_name("SECONDS")
                .value_parser(clap::value_parser!(f64))
                .help("Most seconds one command may take, opening and saving included (default 10, at most 3600)"),
        );
    let mut groups: Vec<(&str, clap::Command)> = Vec::new();
    for command in COMMANDS {
        match command.cli_path {
            [name] => root = root.subcommand(clap_command(name, command)),
            [group, name] => {
                let sub = clap_command(name, command);
                match groups.iter_mut().find(|(g, _)| g == group) {
                    Some((_, g)) => *g = std::mem::take(g).subcommand(sub),
                    None => {
                        let summary = table::group(group).map_or("", |g| g.summary);
                        let g = clap::Command::new(*group)
                            .about(summary)
                            .subcommand_required(true)
                            .arg_required_else_help(true)
                            .subcommand(sub);
                        groups.push((group, g));
                    }
                }
            }
            _ => unreachable!("the table test allows one or two words"),
        }
    }
    for (_, g) in groups {
        root = root.subcommand(g);
    }
    root
}

/// The table command a parse chose, and the matches holding its arguments.
pub fn chosen(matches: &ArgMatches) -> Option<(&'static Command, &ArgMatches)> {
    let (first, m1) = matches.subcommand()?;
    if let Some(c) = table::command_by_path(&[first]) {
        return Some((c, m1));
    }
    let (second, m2) = m1.subcommand()?;
    table::command_by_path(&[first, second]).map(|c| (c, m2))
}

/// Why [`read_bounded`] gave up.
#[derive(Debug)]
pub(crate) enum BoundedReadError {
    Io(std::io::Error),
    /// A folder, device, pipe or socket: it may never end, or block.
    NotAFile,
    TooLarge,
}

impl From<std::io::Error> for BoundedReadError {
    fn from(e: std::io::Error) -> Self {
        BoundedReadError::Io(e)
    }
}

/// Reads a regular file of at most `max` bytes. The size the system
/// reports is not trusted: the read itself stops after `max + 1` bytes.
/// Anything but a regular file is refused before it is opened, because
/// opening a pipe waits for a writer and a device may never end.
pub(crate) fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, BoundedReadError> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(BoundedReadError::NotAFile);
    }
    let file = std::fs::File::open(path)?;
    // The path may have been swapped since the check above.
    if !file.metadata()?.is_file() {
        return Err(BoundedReadError::NotAFile);
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(BoundedReadError::TooLarge);
    }
    Ok(bytes)
}

/// Reads an `@file` argument value, or returns the text as it is.
fn read_at_file(arg: &Arg, text: &str) -> Result<String, CommandError> {
    let Some(file) = text.strip_prefix('@') else {
        return Ok(text.to_owned());
    };
    let path = Path::new(file);
    let shown = carrysheet::commands::paths::display_path(path);
    let flag = format!("--{}", flag_name(arg));
    let what = format!("cannot read {flag} from {shown}");
    let bytes = read_bounded(path, MAX_ARG_FILE_BYTES).map_err(|e| match e {
        BoundedReadError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => CommandError::new(
            ErrorCode::FileNotFound,
            &what,
            e.to_string(),
            format!("Check the file path after @, or pass the JSON itself to {flag}."),
        ),
        BoundedReadError::Io(e) => CommandError::new(
            ErrorCode::Io,
            &what,
            e.to_string(),
            "Check that the file exists and you can read it.",
        ),
        BoundedReadError::NotAFile => CommandError::invalid_argument(
            arg.name,
            format!("{shown} is not a regular file (a folder, device or pipe)"),
            format!("Save the JSON to a file and pass {flag} @file.json, or pass the JSON itself."),
        ),
        BoundedReadError::TooLarge => CommandError::invalid_argument(
            arg.name,
            format!("{shown} is larger than 16 MB"),
            "Split the operations into several files.",
        ),
    })?;
    String::from_utf8(bytes).map_err(|_| {
        CommandError::new(
            ErrorCode::InvalidArgument,
            what,
            "the file is not UTF-8 text",
            "Save the file as UTF-8 and try again.",
        )
    })
}

/// Turns parsed arguments into the JSON object the command layer validates,
/// the same shape an MCP client sends.
pub fn args_json(command: &Command, m: &ArgMatches) -> Result<Map<String, Value>, CommandError> {
    let mut out = Map::new();
    for arg in command.all_args() {
        let value = match arg.ty {
            ArgType::Boolean => {
                if m.get_flag(arg.name) {
                    Value::Bool(true)
                } else {
                    Value::Null
                }
            }
            ArgType::Integer { .. } => m
                .get_one::<i64>(arg.name)
                .map_or(Value::Null, |n| Value::from(*n)),
            ArgType::TextList => m.get_many::<String>(arg.name).map_or(Value::Null, |vs| {
                Value::Array(vs.map(|v| Value::String(v.clone())).collect())
            }),
            ArgType::ObjectList(_) => match m.get_one::<String>(arg.name) {
                None => Value::Null,
                Some(text) => {
                    let text = read_at_file(arg, text)?;
                    serde_json::from_str(&text).map_err(|e| {
                        CommandError::invalid_argument(
                            arg.name,
                            format!("--{} is not valid JSON: {e}", flag_name(arg)),
                            format!(
                                "Pass a JSON list, or @file.json. Example: --{} '[{{\"op\":\"set_value\",\"target\":\"A1\",\"value\":3}}]'",
                                flag_name(arg)
                            ),
                        )
                    })?
                }
            },
            ArgType::Text | ArgType::Path | ArgType::Range | ArgType::Enum(_) | ArgType::Scalar => {
                m.get_one::<String>(arg.name)
                    .map_or(Value::Null, |s| Value::String(s.clone()))
            }
        };
        out.insert(arg.name.to_owned(), value);
    }
    Ok(out)
}
