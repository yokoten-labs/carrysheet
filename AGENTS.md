# Working on Carrysheet

Rules for AI agents (and people) changing this repository. Read all of it before your first change.

## What this is

Carrysheet is a small, fast, native spreadsheet written in Rust. The first release is `carrysheet-cli`, a command-line tool with an MCP server, for Windows, Linux and macOS. A Windows desktop app comes later. Promise: AI-friendly, human-friendly, MCP-friendly.

## Layout and licenses

| Folder | What | License |
|---|---|---|
| `crates/` | Libraries: file formats, the workbook model, the command layer | MIT OR Apache-2.0 |
| `apps/` | Programs: `carrysheet-cli` (with MCP), later the desktop app | GPL-3.0-or-later |
| `research/` | Experiments such as the kernel comparison; its own Cargo workspace | MIT OR Apache-2.0 |
| `tools/` | Developer tools: `fuzzlite` (the fuzzer for the file readers), later `bench` | MIT OR Apache-2.0 |

Every crate sets `license` in its `Cargo.toml` to match its folder. A library must never depend on anything in `apps/`.

## Clean-room rule (hard)

You may read and use: Microsoft's published specs (MS-XLS, MS-XLSB, MS-OFFCRYPTO, ECMA-376), and permissively licensed code (MIT, Apache-2.0, BSD), e.g. Apache POI and calamine.

You must never copy, translate or closely paraphrase code from GPL, LGPL, AGPL or MPL projects (LibreOffice, Gnumeric, OpenOffice, ONLYOFFICE, wine) into this repository, and must not read their source while writing the matching part here. If you know an approach only from such code, say so in the PR instead of writing it.

## Code rules

- Rust edition 2024, stable toolchain, `rust-version = 1.90`. No `unsafe` (workspace lint forbids it).
- CI is the merge gate: it runs the full check (`CI=1 node scripts/check.mjs`) on every PR, and a PR is merged only when CI is green. A local full run is optional. check.mjs runs every check in order (fmt, clippy, tests, cargo-deny, the zip tests on both inflaters, wasm, Windows builds, release privacy check). Only a full run (no `--quick`, no `--from`) on a clean tree is the merge check. A full run is always strict: a skipped step (a missing tool or target) fails it. check.mjs refuses a full run on a dirty tree or when git can't report the tree, and fails if HEAD or the tree changes while it runs. `--list` shows the steps, `--quick` checks the crates you changed and the crates that depend on them while you work, `--from <step>` resumes, and `--strict` (on when `CI=1`) makes a partial run strict too. Its own tests: `node --test scripts/check.test.mjs`.
- Parsers treat every input file as hostile: no panics, no unbounded allocation, limits on sizes and counts. A malformed file returns an error that says what is wrong.
- Never drop data you don't understand when saving: keep it and write it back.
- Errors for people and agents have three parts: what happened, why, and how to fix it.
- Tests use small files committed under the crate's `tests/data/` (made by us or from Apache POI's test data with its license noted). Never commit large corpora.

## Command line and MCP

The CLI and MCP server follow `docs/agents.md` (see PR #1 until it is merged). In short: `--json` everywhere with JSON-only stdout; exit codes 0 done, 1 failed, 2 usage, 3 nothing to do; never prompt without a terminal; `--dry-run` on every command that changes a file; MCP schemas that Claude, Gemini and OpenAI strict mode all accept.

## Working on this machine

- The development machine is shared: build with `CARGO_BUILD_JOBS=1` and `nice -n 10`, under `flock ~/.cache/carrysheet-build.lock` (check.mjs does this for you unless `CI=1`); never run two builds at once.
- On this machine, set `export CARRYSHEET_HEAVY=~/bin/heavy` (the wrapper that waits for free memory). check.mjs then runs each heavy step as `~/bin/heavy flock ~/.cache/carrysheet-build.lock nice -n 10 env CARGO_BUILD_JOBS=1 cargo ...`, and the release step as `~/bin/heavy node scripts/release-check.mjs`, which takes the build lock inside. The order is always the same: the heavy wrapper outside, the build lock inside; never the other way round, or two jobs can each wait for the other. Exit code 75 from the wrapper means the machine was busy and nothing ran (check.mjs reports BUSY: retry later). Without `CARRYSHEET_HEAVY` there is no wrapper, but cargo still runs with one job and `nice` under the build lock; only in CI (`CI=1`) does it use every core at normal priority.
- Builds share one compile cache through sccache, set in the user's global cargo config (`~/.cargo/config.toml`: `rustc-wrapper`, incremental builds off). Each worktree keeps its own `target/`. Never turn on incremental builds, override the wrapper (`RUSTC_WRAPPER`), or vary `RUSTFLAGS` or `CARGO_TARGET_DIR` from run to run in a committed file or script: each makes the cache miss. If you really need incremental builds for a while, set `CARGO_INCREMENTAL=1` in your shell only; don't edit `.cargo/config.toml`, which is committed (a change makes the tree dirty, and the merge run refuses it). When sccache is on `PATH`, check.mjs prints the cache hit rate at the end.
- Rust is in `~/.cargo/bin` (add it to `PATH`). Windows targets build with mingw: `x86_64-pc-windows-gnu`, `i686-pc-windows-gnu`.
- CI runs `CI=1 node scripts/check.mjs` on every PR; merge only when CI is green. On the shared development machine, run `--quick` only.
- Never write the owner's email, Windows user name, or absolute home paths into files, commits, PRs or output. Use `~` or `<user>`.

## Commits and PRs

- Commit with `TZ=UTC git commit`, messages in English, one logical change each, ending with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` (or your own agent and model; this is our AI provenance record).
- PRs are drafts until the owner reviews them. PR descriptions end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)` when written by Claude.
