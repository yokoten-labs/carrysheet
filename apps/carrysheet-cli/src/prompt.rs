//! Confirmations. Never asks without a terminal: a missing answer is no.

use std::io::{BufRead, IsTerminal, Write};

/// How the program was started, for deciding whether it may ask questions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Context {
    /// `--json`: an agent is reading, so never ask.
    pub json: bool,
    /// `--yes`: every confirmation is answered yes.
    pub yes: bool,
    /// stdin and stderr are both terminals, so a person can answer.
    pub interactive: bool,
    /// `--max-seconds`: the time one command may take; `None` is the
    /// default for agents (`limits::MAX_CALL_SECONDS`).
    pub max_seconds: Option<f64>,
}

impl Context {
    pub fn detect(json: bool, yes: bool) -> Self {
        Context {
            json,
            yes,
            interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            max_seconds: None,
        }
    }
}

/// Asks a yes/no question on stderr when a person can answer. `--yes` answers
/// yes; `--json` or no terminal answers no without asking.
pub fn confirm(question: &str, ctx: &Context) -> bool {
    if ctx.yes {
        return true;
    }
    if ctx.json || !ctx.interactive {
        return false;
    }
    let stdin = std::io::stdin();
    ask(question, &mut stdin.lock(), &mut std::io::stderr())
}

/// Writes `question [y/N] ` and reads one line. Only `y` or `yes` (any case) is yes.
pub fn ask(question: &str, input: &mut impl BufRead, out: &mut impl Write) -> bool {
    if write!(out, "{question} [y/N] ")
        .and_then(|()| out.flush())
        .is_err()
    {
        return false;
    }
    let mut line = String::new();
    match std::io::Read::take(&mut *input, 1024).read_line(&mut line) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(json: bool, yes: bool, interactive: bool) -> Context {
        Context {
            json,
            yes,
            interactive,
            max_seconds: None,
        }
    }

    #[test]
    fn yes_flag_answers_yes() {
        assert!(confirm("Overwrite?", &ctx(false, true, false)));
        assert!(confirm("Overwrite?", &ctx(true, true, false)));
    }

    #[test]
    fn no_terminal_or_json_answers_no_without_asking() {
        assert!(!confirm("Overwrite?", &ctx(false, false, false)));
        assert!(!confirm("Overwrite?", &ctx(true, false, true)));
    }

    #[test]
    fn ask_reads_one_answer() {
        for (input, expected) in [
            ("y\n", true),
            ("YES\n", true),
            (" yes \r\n", true),
            ("n\n", false),
            ("\n", false),
            ("", false),
            ("yep\n", false),
        ] {
            let mut out = Vec::new();
            assert_eq!(
                ask("Go?", &mut input.as_bytes(), &mut out),
                expected,
                "{input:?}"
            );
            assert_eq!(String::from_utf8(out).unwrap(), "Go? [y/N] ");
        }
    }
}
