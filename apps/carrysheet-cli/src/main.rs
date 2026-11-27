//! `carrysheet-cli`: Carrysheet on the command line, generated from the
//! command table in `carrysheet::commands`.
//!
//! Exit codes: 0 done, 1 failed, 2 usage error, 3 nothing to do.

mod cli;
mod mcp;
mod output;
mod run;
#[cfg(test)]
mod tests;

use std::ffi::OsString;

use carrysheet::commands::validate_args;
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
    let mut ctx = run::Context {
        yes: matches.get_flag("yes"),
        max_seconds: None,
    };
    if let Some(&seconds) = matches.get_one::<f64>("max-seconds") {
        let max = carrysheet::commands::limits::MAX_CLI_SECONDS;
        if !(seconds > 0.0 && seconds <= max) {
            let err = carrysheet::commands::CommandError::invalid_argument(
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
        let err = carrysheet::commands::CommandError::new(
            carrysheet::commands::ErrorCode::Internal,
            "no command was chosen",
            "the command line parsed but named no command",
            "Report this at https://github.com/yokoten-labs/carrysheet/issues.",
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
            Ok(()) => carrysheet::commands::ExitCode::Done.code(),
            Err(err) => {
                output::print_error(&err, json);
                err.exit_code().code()
            }
        };
    }
    let result = args.and_then(|args| run::run(command, &args, &ctx));
    match result.map_err(with_yes) {
        Ok(done) => {
            output::print_done(&done, json);
            carrysheet::commands::ExitCode::Done.code()
        }
        Err(err) => {
            output::print_error(&err, json);
            err.exit_code().code()
        }
    }
}

/// Nobody is asked to confirm: a refusal for want of a confirmation also
/// names `--yes`, which gives every confirmation at once.
fn with_yes(mut err: carrysheet::commands::CommandError) -> carrysheet::commands::CommandError {
    if err.code == carrysheet::commands::ErrorCode::ConfirmationRequired {
        err.fix
            .push_str(" Or add --yes, which confirms everything.");
    }
    err
}

fn main() -> std::process::ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    std::process::ExitCode::from(real_main(args))
}
