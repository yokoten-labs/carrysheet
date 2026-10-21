//! `carrysheet-cli`: Carrysheet on the command line, generated from the
//! command table in `carrysheet-commands`.
//!
//! Exit codes: 0 done, 1 failed, 2 usage error, 3 nothing to do.

mod cli;
mod http;
mod log;
mod mcp;
mod output;
mod prompt;
mod run;

use std::ffi::OsString;

use carrysheet_commands::validate_args;
use serde_json::Value;

/// True when `--json` appears before any `--`, so even a command line clap
/// cannot parse gets its error as JSON. `--json=true` (or any `--json=`
/// form) counts too: clap refuses it, and the refusal comes as JSON.
fn wants_json(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.as_os_str() != "--")
        .filter_map(|a| a.to_str())
        .any(|a| a == "--json" || a.starts_with("--json="))
}

fn real_main(args: Vec<OsString>) -> u8 {
    let json_requested = wants_json(&args);
    let matches = match cli::build().try_get_matches_from(&args) {
        Ok(m) => m,
        Err(e) => return output::handle_clap_error(&e, json_requested),
    };
    let json = matches.get_flag("json");
    let mut ctx = prompt::Context::detect(json, matches.get_flag("yes"));
    if let Some(&seconds) = matches.get_one::<f64>("max-seconds") {
        let max = carrysheet_commands::limits::MAX_CLI_SECONDS;
        if !(seconds > 0.0 && seconds <= max) {
            let err = carrysheet_commands::CommandError::invalid_argument(
                "max-seconds",
                format!("--max-seconds must be more than 0 and at most {max}"),
                "Pass a number of seconds, like --max-seconds 60.",
            );
            output::print_error(&err, json);
            return err.exit_code().code();
        }
        ctx.max_seconds = Some(seconds);
    }

    let Some((command, sub)) = cli::chosen(&matches) else {
        // clap requires a subcommand, so this is a bug if it happens.
        let err = carrysheet_commands::CommandError::new(
            carrysheet_commands::ErrorCode::Internal,
            "no command was chosen",
            "the command line parsed but named no command",
            "Report this with `carrysheet-cli feedback bug`.",
        );
        output::print_error(&err, json);
        return err.exit_code().code();
    };

    let args =
        cli::args_json(command, sub).and_then(|args| validate_args(command, &Value::Object(args)));
    if command.name == "mcp_serve" {
        // Runs until the client closes stdin; it never prints a result.
        return match args
            .and_then(|args| mcp::Options::from_args(&args))
            .and_then(|o| mcp::serve(&o))
        {
            Ok(()) => carrysheet_commands::ExitCode::Done.code(),
            Err(err) => {
                output::print_error(&err, json);
                err.exit_code().code()
            }
        };
    }
    let result = args.and_then(|args| run::run(command, &args, &ctx));
    match result {
        Ok(done) => {
            output::print_done(&done, json);
            carrysheet_commands::ExitCode::Done.code()
        }
        Err(err) => {
            output::print_error(&err, json);
            err.exit_code().code()
        }
    }
}

/// The hidden first argument that runs this program as a query worker
/// (`carrysheet_query::QueryEngine::start`), with the `query` feature. It is
/// read before clap, so it is not in the help.
#[cfg(feature = "query")]
pub(crate) const QUERY_WORKER_ARG: &str = "__query-worker";

fn main() -> std::process::ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    #[cfg(feature = "query")]
    if args.get(1).is_some_and(|a| a == QUERY_WORKER_ARG) {
        return carrysheet_query::serve_worker();
    }
    std::process::ExitCode::from(real_main(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn json_is_detected_before_double_dash_only() {
        assert!(wants_json(&os(&["x", "read", "--json"])));
        assert!(!wants_json(&os(&["x", "read", "--", "--json"])));
        assert!(!wants_json(&os(&["--json"])));
        assert!(wants_json(&os(&["x", "--json=true", "read"])));
        assert!(wants_json(&os(&["x", "read", "--json=false"])));
        assert!(!wants_json(&os(&["x", "read", "--jsonx"])));
    }
}
