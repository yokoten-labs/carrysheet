# Carrysheet

A small, fast, native spreadsheet, written in Rust.
Simplified Chinese name: 随身表格. Traditional Chinese: 隨身試算表.

The first release will be `carrysheet-cli`: a command-line tool with an MCP server, so people and AI agents can open, read, edit and save spreadsheets. It will run on Windows, Linux and macOS. A desktop app comes later.

> **Early preview.** Carrysheet is not released yet. There are no binaries to download. File formats, commands and APIs may change at any time. Please try it, read the code and report what you find, but don't trust it with files you can't replace.

## What works today

The libraries in `crates/` work and are tested. Each one treats every file as hostile: no crashes, limits on size and memory, and an error that says what went wrong.

- **Workbooks** (`carrysheet-kernel`, on the vendored GridCraft kernel): open `.xlsx`, `.xlsm`, `.xltx`, `.xltm`, `.xlsb`, `.ods`, `.xls` (BIFF8) and `.csv`, calculate formulas, and save as `.xlsx` or `.csv`. A save that would lose content needs `confirm_loss`.
- **Macros**: `overview` says when an .xlsx, .xlsm or .xlsb file has macros (a VBA project or Excel 4 macro sheets), and says it did not check for .xls and .ods files. Carrysheet never runs them, and a save keeps them where the format can hold them; a save that would drop them needs `confirm_loss`.
- **The workbook model and the command table** (`carrysheet-model`, `carrysheet-commands`): one list of commands that drives both the command line and the MCP tools, with JSON schemas that Claude, Gemini and OpenAI accept.

`carrysheet-cli` runs `overview`, `read`, `eval`, `edit`, `convert`, `commands` and `mcp serve` (MCP over stdio). It never asks a question: pass `--yes` to confirm deleting, overwriting or a lossy save.

Not there yet: writing `.xls`, `.xlsb` or `.ods`, and the desktop app.

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

The libraries in `crates/` are MIT OR Apache-2.0, at your choice. The programs in `apps/`, including `carrysheet-cli`, are GPL-3.0-or-later. Files you make with Carrysheet are yours. The name and logo are not licensed; forks need their own name. See [LICENSING.md](LICENSING.md) and [TRADEMARKS.md](TRADEMARKS.md).
