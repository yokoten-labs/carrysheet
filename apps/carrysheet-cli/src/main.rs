//! `carrysheet-cli`: Carrysheet on the command line, made from `carrysheet::args`.
//!
//! Exit codes: 0 done, 1 failed, 2 usage error, 3 nothing to do.

mod cli;
mod files;
mod mcp;
mod output;
mod run;
#[cfg(test)]
mod tests;

use std::ffi::OsString;

use carrysheet::commands::{CommandError, ErrorCode, ExitCode, validate_args};
use clap::{ArgMatches, FromArgMatches};
use serde_json::Value;

/// `--json`, or a `--json=` form clap refuses, before any `--`: a clap error is JSON too.
fn wants_json(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.as_os_str() != "--")
        .filter_map(|a| a.to_str())
        .any(|a| a == "--json" || a.starts_with("--json="))
}

fn main() -> std::process::ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let matches = match cli::build().try_get_matches_from(&args) {
        Ok(m) => m,
        Err(e) => return output::handle_clap_error(&e, wants_json(&args)).into(),
    };
    // In `mcp serve` stdout carries JSON-RPC only, also after the session ends.
    let serving = cli::chosen(&matches).is_some_and(|(c, _)| c.name == "mcp_serve");
    let json = matches.get_flag("json") && !serving;
    let code = match run_command(&matches) {
        Ok(Some(done)) => {
            output::print_done(&done, json);
            ExitCode::Done.code()
        }
        // `mcp serve` ran until the client closed stdin; it prints no result.
        Ok(None) => ExitCode::Done.code(),
        Err(err) => {
            output::print_error(&err, json);
            err.exit_code().code()
        }
    };
    code.into()
}

/// Runs the chosen command with its checked arguments.
fn run_command(matches: &ArgMatches) -> Result<Option<run::Done>, CommandError> {
    let global = cli::Global::from_arg_matches(matches).expect("clap parsed the global flags");
    let ctx = run::Context {
        yes: global.yes,
        max_seconds: cli::max_seconds(&global)?,
        root: None,
    };
    // clap requires a subcommand, and each one comes from the command table.
    let (command, sub) = cli::chosen(matches).expect("clap chose a command");
    let args = validate_args(command, &Value::Object(cli::args_json(command, sub)?))?;
    if command.name == "mcp_serve" {
        return mcp::serve(&args).map(|()| None);
    }
    run::run(command, &args, &ctx).map(Some).map_err(with_yes)
}

/// Nobody is asked: a refusal for want of a confirmation also names `--yes`.
fn with_yes(mut err: CommandError) -> CommandError {
    if err.code == ErrorCode::ConfirmationRequired {
        err.fix
            .push_str(" Or add --yes, which confirms everything.");
    }
    err
}
