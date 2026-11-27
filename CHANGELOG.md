# Changelog

Changes people and scripts notice, newest first. Commands, arguments and
error codes are the public interface (`carrysheet-cli commands --json`).

## Unreleased

### Removed

- **Everything outside the 5,000-line cap** (record 0010 and record 0011):
  the `macros` command, MCP over HTTP, `feedback`, our own readers that no
  program linked (old `.xls`, Numbers, csv, encryption, data connections,
  SQL) and `tools/`; thirteen crates became two. GridCraft now reads every
  format. The removed code stays in the history of `main`: `50bd07b` is the
  last commit on `main` before the first deletion merged, and holds all of
  it. Code rewritten or removed later in the cut, such as `eval`, is named
  in its own `changes/` file.

### Security

- **zip 9.0.5.** Fixes RUSTSEC-2026-0548 (local header names that differ from the
  central directory in the streaming reader). Carrysheet only reads archives
  through the central directory and never extracts or streams, so it is not
  affected and needs no new check. Also updated for the same cargo-deny run:
  calamine 0.38 (dev only, drops zip 8; RUSTSEC-2026-0436), rmcp 3.9.0
  (RUSTSEC-2026-0457, 0461 and 0462; 0461 is the unbounded tool schema cache in
  the Streamable HTTP server that `mcp serve --http` uses), rustix 1.1.6
  (RUSTSEC-2026-0486) and ctutils 0.4.4 (RUSTSEC-2026-0385). After record 0010
  step 1, calamine and ctutils are no longer in `Cargo.lock`.

### Breaking changes for scripts

- **Saving content a file would lose needs `confirm_loss`.** `edit` and
  `convert` refuse to save when the written file would lack something the
  source held (macros, charts, comments, content the save check could not
  search...). Pass `confirm_loss: true` (`--confirm-loss` on the command
  line) after reading `save.lost` from a `dry_run`. `confirm` no longer
  covers this: it only agrees to deleting sheets, rows or columns, or to
  overwriting a file. `--yes` gives both.
- **CSV output escapes formula-like text by default.** Text starting with
  `=`, `+`, `-` or `@` is written with a leading `'`, so spreadsheet apps
  do not run it. Pass `csv_raw: true` (`--csv-raw`) for the old output.
- **One command may take 10 seconds by default.** The limit covers the
  whole command: opening, calculating and saving. A command that runs out
  fails with `limit_exceeded` and writes nothing. On the command line,
  `--max-seconds N` allows up to 3,600 seconds; MCP and agent calls keep
  the 10-second limit.
- **More formula functions are refused** in `edit`, because they reach
  outside the file: `TRANSLATE`, `DETECTLANGUAGE`, `IMPORTRANGE`,
  `IMPORTHTML`, `IMPORTFEED`, `GOOGLEFINANCE` and the `CUBE...` functions,
  also when written next to a range (`A1:WEBSERVICE(...)`).
