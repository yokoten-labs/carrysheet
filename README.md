# Carrysheet

A small, fast, native spreadsheet, written in Rust.
Simplified Chinese name: 随身表格. Traditional Chinese: 隨身試算表.

The first release will be `carrysheet-cli`: a command-line tool with an MCP server, so people and AI agents can open, read, edit and save spreadsheets. It will run on Windows, Linux and macOS. A desktop app comes later.

> **Early preview.** Carrysheet is not released yet. There are no binaries to download. File formats, commands and APIs may change at any time. Please try it, read the code and report what you find, but don't trust it with files you can't replace.

## What works today

The file-format libraries in `crates/` work and are tested. Each one treats every file as hostile: no crashes, limits on size and memory, and an error that says what went wrong.

- **CSV and TSV** (`carrysheet-csv`): read and write, with the delimiter and encoding detected (UTF-8, UTF-16, GBK, Big5, Shift_JIS, Windows-1252 and more). Values stay exactly as written.
- **Old Excel files** (`carrysheet-biff`): read `.xls` from Excel 2.x to Excel 2003 (BIFF2 to BIFF8), and Excel 4.0 `.xlw` workbooks. Records it doesn't understand are kept as bytes. Writing `.xls` is not done yet.
- **Apple Numbers** (`carrysheet-numbers`): read `.numbers` files: sheets, tables, cell values, formulas as text and merged cells. Read only.
- **Password-protected files** (`carrysheet-crypto`): detect them, decrypt them with the password, and encrypt them again. Covers `.xlsx`/`.xlsm`/`.xlsb` (agile and standard encryption) and `.xls` (RC4 and XOR). Files locked with Rights Management are detected and explained.
- **Macros** (`carrysheet-vba`): list the VBA modules in a file, show their source and flag code that may be risky. Macros are never run, and their bytes are kept untouched.
- **The workbook model and the command table** (`carrysheet-model`, `carrysheet-commands`): one list of commands that drives both the command line and the MCP tools, with JSON schemas that Claude, Gemini and OpenAI accept.

`carrysheet-cli` runs, but it is a shell for now. `commands`, `feedback` and `mcp serve` work. The file commands (`overview`, `read`, `edit`, `convert` and the rest) check their input first: the arguments, and that the file exists. Then they answer "not implemented yet" until the calculation engine is in.

Not there yet: reading and writing `.xlsx`, writing `.xls`, formulas that calculate, and the desktop app.

## When is it ready?

Carrysheet 1.0 is ready when you can open a real `.xlsx` file, edit it, and save it with nothing lost. Until then, it is a preview.

## Build from source

You need [Rust](https://rustup.rs) 1.90 or later and Node.js (for the check script).

```sh
cargo build -p carrysheet-cli
```

The program is then in `target/debug/`. While you work, check your change with:

```sh
node scripts/check.mjs --quick
```

It formats, lints and tests the crates you changed and the crates that use them, and checks the dependency policy. The full run, `node scripts/check.mjs` with no options, is what CI runs on every pull request. It needs Linux, cargo-deny, the wasm target and the mingw-w64 Windows targets, so you usually leave it to CI. `node scripts/check.mjs --list` shows the steps.

## Contributing and security

- How to contribute: [CONTRIBUTING.md](CONTRIBUTING.md). The rules for code, tests and commits are in [AGENTS.md](AGENTS.md).
- Found a security problem? Please report it privately: see [SECURITY.md](SECURITY.md).

## License

The libraries in `crates/` are MIT OR Apache-2.0, at your choice. 
The programs in `apps/`, including `carrysheet-cli`, are GPL-3.0-or-later. Files you make with Carrysheet are yours. 
The name and logo are not licensed; forks need their own name. See [LICENSING.md](LICENSING.md) and [TRADEMARKS.md](TRADEMARKS.md).
