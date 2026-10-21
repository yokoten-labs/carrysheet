# Changelog

Changes people and scripts notice, newest first. Commands, arguments and
error codes are the public interface (`carrysheet-cli commands --json`).

How it works:

- A pull request that changes what people or scripts notice adds a line
  under **Unreleased**, ending with its number, e.g. `(PR 123)`.
- To release, rename **Unreleased** to the version and date, such as
  `## v0.1.0 - 2026-11-15`, set the same version in `Cargo.toml`
  (`[workspace.package]` and `[workspace.dependencies]`), and start a new
  empty `## Unreleased` above it. After that commit is on main, tag it
  `v0.1.0`: the release workflow takes that section as the release notes.
- `node scripts/release-meta.mjs notes 0.1.0` prints what the notes will
  say. CI fails when the `## Unreleased` heading is missing.

## Unreleased

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
