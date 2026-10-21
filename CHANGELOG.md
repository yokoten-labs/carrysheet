# Changelog

Changes people and scripts notice, newest first. Commands, arguments and
error codes are the public interface (`carrysheet-cli commands --json`).

## Unreleased

### Added

- **`check` shows what a save would keep, change or lose, before
  anything is written.** Give a workbook and, optionally, the format to
  save as (`xlsx`, `xlsm` or `csv`; default: the file's own). The report
  is the same one `convert` and `edit` check before saving. It exits 4
  when something would be lost, so a script can stop before a lossy save;
  0 when nothing known is lost. Also an MCP tool, read-only.

### Breaking changes for scripts

- **`sanitize`, `mcp install` and `mcp doctor` are hidden** from
  `commands`, `--help` and the MCP tool list until they work. They were
  listed before but only answered `not_implemented`.

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
