# Carrysheet

A small, fast, native spreadsheet, written in Rust.
Simplified Chinese name: 随身表格. Traditional Chinese: 隨身試算表.

The first release will be `carrysheet-cli`: a command-line tool with an MCP server, so people and AI agents can open, read, edit and save spreadsheets. It will run on Windows, Linux and macOS. A desktop app comes later.

> **Early preview.** Carrysheet is not released yet. There are no binaries to download. File formats, commands and APIs may change at any time. Please try it, read the code and report what you find, but don't trust it with files you can't replace.

## What works today

The libraries in `crates/` work and are tested. Each one treats every file as hostile: no crashes, limits on size and memory, and an error that says what went wrong.

- **Workbooks** (`carrysheet`, on the vendored GridCraft kernel): open `.xlsx`, `.xlsm`, `.xltx`, `.xltm`, `.xlsb`, `.ods`, `.xls` (BIFF8) and `.csv`, calculate formulas, and save as `.xlsx` or `.csv`. csv opens in its own encoding (UTF-8, UTF-16, GBK, Big5, Shift_JIS and others), and a field becomes a number only when nothing changes, so `007` and `SEPT1` stay as written. A save that would lose content needs `confirm_loss`.
- **Macros**: `overview` says when an .xlsx, .xlsm or .xlsb file has macros (a VBA project or Excel 4 macro sheets), and says it did not check for .xls and .ods files. Carrysheet never runs them, and a save keeps them where the format can hold them; a save that would drop them needs `confirm_loss`.
- **The commands** (`carrysheet::args`): one struct per command that drives both the command line and the MCP tools, with JSON schemas that Claude, Gemini and OpenAI accept.

`carrysheet-cli` runs `overview`, `read`, `eval`, `edit`, `convert`, `commands` and `mcp serve` (MCP over stdio). It never asks a question: pass `--yes` to confirm deleting, overwriting or a lossy save.

Not there yet: writing `.xls`, `.xlsb` or `.ods`, and the desktop app.

## When is it ready?

Carrysheet 1.0 is ready when you can open a real `.xlsx` file, edit it, and save it with nothing lost. Until then, it is a preview.

## Build from source

You need [Rust](https://rustup.rs) 1.90 or later (`rust-toolchain.toml` picks the exact version) and Node.js (for the line-cap check).

```sh
cargo build -p carrysheet-cli
```

The program is then in `target/debug/`. While you work, check your change with:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node scripts/lines.mjs --max <ceiling>
```

The line-cap ceiling of the current step is the `--max` of the `lines` job in `.github/workflows/ci.yml`.

CI runs the same commands on Linux, Windows and macOS, plus `cargo deny check`, the wasm and Windows builds, a build on the minimum Rust version, and a scan of the release binary for build-machine paths. The workflow is `.github/workflows/ci.yml`.

## Contributing and security

- How to contribute: [CONTRIBUTING.md](CONTRIBUTING.md). The rules for code, tests and commits are in [AGENTS.md](AGENTS.md).
- Found a security problem? Please report it privately: see [SECURITY.md](SECURITY.md).

## License

The libraries in `crates/` are MIT OR Apache-2.0, at your choice. The programs in `apps/`, including `carrysheet-cli`, are GPL-3.0-or-later. Files you make with Carrysheet are yours. The name and logo are not licensed; forks need their own name. See [LICENSING.md](LICENSING.md) and [TRADEMARKS.md](TRADEMARKS.md).
