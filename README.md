# Carrysheet

A small, fast, native spreadsheet, written in Rust.
Simplified Chinese name: 随身表格. Traditional Chinese: 隨身試算表.

The first release will be `carrysheet-cli`: a command-line tool with an MCP server, so people and AI agents can open, read, edit and save spreadsheets. It will run on Windows, Linux and macOS. A desktop app comes later.

> **Early preview.** File formats, commands and APIs may change at any time. Please try it, read the code and report what you find, but don't trust it with files you can't replace.

## Install

Download `carrysheet-cli-<version>-<target>` from the [GitHub Releases](https://github.com/yokoten-labs/carrysheet/releases) page: a `.zip` for Windows, a `.tar.gz` for Linux and macOS. Check it against `SHA256SUMS` from the same release, unpack it, and put `carrysheet-cli` on your `PATH`.

The program is not signed yet. On macOS, run `xattr -d com.apple.quarantine carrysheet-cli` once, or macOS refuses to start it. On Windows, run the examples below in PowerShell: `cmd.exe` does not read the single quotes.

## Quick start

```sh
carrysheet-cli overview book.xlsx
carrysheet-cli read book.xlsx 'Sheet1!A1:D20'
carrysheet-cli edit book.xlsx --ops '[{"op":"set_value","target":"A1","value":3}]' --dry-run
```

`edit` writes a new file beside the original; `--dry-run` only lists the changes. Run `carrysheet-cli --help` for every command, and add `--json` for output that scripts and agents can parse.

## What it does

Carrysheet is two crates: the library `carrysheet` (`crates/`), which takes bytes and gives back bytes, and the program `carrysheet-cli` (`apps/`), which reads and writes the files. Both stand on the vendored [GridCraft](vendor/gridcraft) kernel, the only code that reads file formats. Every file is treated as hostile: no crashes, limits on size, memory and time (10 seconds per command by default), and an error that says what happened, why, and how to fix it.

- **Opens** `.xlsx`, `.xlsm`, `.xltx`, `.xltm`, `.xlsb`, `.ods`, `.xls` (BIFF8) and `.csv`. csv opens in its own encoding (UTF-8, UTF-16, GBK, Big5, Shift_JIS and others), and a field becomes a number only when nothing changes, so `007` and `SEPT1` stay as written.
- **Saves** as `.xlsx` or `.csv`. A save that would lose content needs `confirm_loss`. csv output escapes text that a spreadsheet app would run as a formula.
- **Five commands**, on the command line and (all but `commands`) as MCP tools:
  - `overview`: sheets, used ranges, counts, defined names, and whether the file has macros.
  - `read`: a range as a paged table of values or formulas.
  - `edit`: set or clear values and formulas, insert or delete rows and columns, add, rename or delete sheets; all or nothing, with `dry_run`.
  - `convert`: save as `.xlsx` or `.csv`, to a new file.
  - `commands`: every command and its JSON schema, for agents.
- **MCP over stdio**: `carrysheet-cli mcp serve`, confined to `--root`, with `--read-only` to hide the tools that write. The schemas pass the rules of Claude, Gemini and OpenAI strict mode.
- **Macros**: `overview` says when an .xlsx, .xlsm or .xlsb file has macros (a VBA project or Excel 4 macro sheets), and says it did not check for .xls and .ods files. Carrysheet never runs them, and a save keeps them where the format can hold them; a save that would drop them needs `confirm_loss`.

The program never asks a question: pass `--yes` to confirm deleting, overwriting or a lossy save.

## What it does not do

- Write `.xls`, `.xlsb` or `.ods`. There is no desktop app yet.
- `eval`: use `edit` with `dry_run` and set the formula in a free cell; the change list shows its value.
- List or show macro source, MCP over HTTP, the `feedback` command, and our own readers for old `.xls` (BIFF2 to BIFF7), Numbers files, data connections and SQL.

These left so that our own source stays at most 5,000 lines, which one person or agent can read in full. [Record 0010](docs/records/0010-five-thousand-lines.md) sets the cap and says what left and why; [record 0011](docs/records/0011-closing-the-cap.md) adds `eval`. The removed code stays in the history of `main` (see [CHANGELOG.md](CHANGELOG.md)). A feature comes back only if it fits under the cap, or as its own program.

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
node scripts/lines.mjs
```

`node scripts/lines.mjs` checks the 5,000-line cap. It takes no options.

CI runs the same commands on Linux, Windows and macOS, plus `cargo deny check`, the wasm and Windows builds, a build on the minimum Rust version, and a scan of the release binary for build-machine paths. The workflow is `.github/workflows/ci.yml`.

## Contributing and security

- How to contribute: [CONTRIBUTING.md](CONTRIBUTING.md). The rules for code, tests and commits are in [AGENTS.md](AGENTS.md).
- Found a security problem? Please report it privately: see [SECURITY.md](SECURITY.md).

## License

The libraries in `crates/` are MIT OR Apache-2.0, at your choice. The programs in `apps/`, including `carrysheet-cli`, are GPL-3.0-or-later. Files you make with Carrysheet are yours. The name and logo are not licensed; forks need their own name. See [LICENSING.md](LICENSING.md) and [TRADEMARKS.md](TRADEMARKS.md).
