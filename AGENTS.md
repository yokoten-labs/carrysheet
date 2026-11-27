<!-- yokoten kit 0.1.0 -->
# How work is done here

This file is for every agent and every person working in this repo. It starts from
[yokoten](https://github.com/yokoten-labs/yokoten)'s core rules; the project's own section is
at the end.

Every rule names why it exists and the check that holds it. A rule with no check is a
wish: the check is what makes it true on a busy day. The "why" is an **incident** when
one happened, and a **risk** when the rule came first. Incidents are never made up; a
risk becomes an incident only when it happens. When a rule here fails, use the `rca`
skill: find the cause, add or fix the check, then carry the fix to the other projects.

## Roles

- **The owner** decides what is built and when it ships. Only the owner approves pull
  requests, creates release tags, and changes repo settings.
- **The lead** is the session the owner talks to. It plans, splits the work, reviews,
  commits and opens pull requests (as the bot, never as the owner).
- **A worker** is a session the lead starts for one task, in its own worktree, with
  `YOKOTEN_ROLE=worker`. It changes code and tests, and reports back. It never pushes.

## Core rules

| # | Rule | Why | Check |
|---|------|-----|-------|
| 1 | Every piece of work starts as an issue. No todo files. | Incident: follow-ups written only in chat reports were nearly lost; a shared todo file made one PR run CI four times on merge conflicts. | `doctor` reports todo files |
| 2 | Nothing reaches `main` without a pull request and green CI. | Risk: one bad push breaks every branch built on it. | Branch ruleset on the server |
| 3 | Only the owner creates tags and releases. A general "merge", "ship it" or "go ahead" does not approve a release. Only an owner request that names the version or says "release", "publish" or "tag" does. | Risk: when to ship is the owner's call, and a pushed tag can start a release. | Tool guard; tag ruleset on the server |
| 4 | A fix task changes code, never tests (`YOKOTEN_FIX_TASK=1`). | Risk: the fastest way to make a test pass is to change what it asserts. | Tool guard and commit check |
| 5 | A worker does not change lead-only files: these rules, agent config, hooks, CI. | Risk: a pull request that edits CI runs that CI before anyone reads it. | Tool guard and commit check for workers; for the bot, no Workflows permission, or CODEOWNERS review of CI changes with every secret behind a protected environment |
| 6 | Never skip the commit check (`--no-verify`, changing `core.hooksPath`). | Risk: a skipped check makes a broken commit look reviewed. | Tool guard; CI runs the same check |
| 7 | No plain force push. Merge `main` into a branch only on a real conflict; never rebase a shared branch. | Risk: a force push throws away commits someone else pushed. | Tool guard; branch ruleset |
| 8 | Squash merge. One pull request, one purpose; small polish goes in one batch. A pull request has a commit-form title, a `tier:` line, Why / What changed / How tested, a linked issue, a file in `changes/` when behaviour changes, a milestone, a type label, and a tier label that matches the `tier:` line. An issue title is `area: verb phrase` (a task) or `area: symptom` (an incident): at most 60 characters, no full stop, no `#N`. | Incident: the owner was asked to review a +3 −2 pull request on its own and said small changes should not each cost a review. | Repo settings (squash only); `check --pr` and `check --issue` in CI |
| 9 | Review and test the exact commit that merges. The reviewer only reads; the verifier runs it. | Incident: two pull requests, each green on its own, broke `main` together, twice. | `inspect` skill; merge queue where the host has one |
| 10 | No folder holds more than 7 entries (the root is not counted). | Risk: a flat folder grows until nobody can find anything. Set as a standard by the owner. | Commit check (layout) |
| 11 | Nothing in the repo, or in what yokoten prints, points at a person or opens their accounts: no personal email, IP, name, city or private domain, and no secret (token or private key). A hit is reported by place, never by content; the home folder shows as `~`. | Incident: the old check read only the live docs, and server names and addresses were still in 56 tracked files. | Commit check and CI (privacy and secrets); `check --history` before going public |
| 12 | Decisions are written down once and kept. A changed decision gets a new entry that points back. Facts are replaced, not added to. Times are UTC, commit times included. | Risk: a decision that lives only in a chat gets argued again. Incident: every commit recorded the owner's time zone (#40). | `record` skill; commit check (commit time) |

## Writing

Plain English, close to ASD-STE100 (Simplified Technical English), because the readers
include beginners, people reading a second language, and agents. An instruction has at
most 20 words; a description at most 25. One instruction per sentence. One word for one
thing. Active voice. A flow gets a diagram. `check --pr` notes long sentences in changed
Markdown; a note never fails CI.

## Names

A name is what a reader meets first and reads most. Pick names that a new reader, or an
agent, understands without scrolling back. The yokoten guide says why:
[Naming](https://github.com/yokoten-labs/yokoten/blob/main/docs/guide.md#naming).

- Whole words. A short form only when everyone knows it (`id`, `url`, `config`). Never
  delete letters inside a word.
- At most 4 words. There is no character limit, because a limit makes people abbreviate.
- One letter only for a name used within about 10 lines, such as a loop counter.
- A function's name starts with a verb. A true/false value starts with `is`, `has` or
  `can`.
- One idea keeps one name in every file.
- Use the language's own case style, for example `camelCase` in TypeScript and
  `snake_case` in Python.

| Hard to read | Easy to read | Why |
|---|---|---|
| `r`, still used 30 lines later | `result` | By line 30 the reader has forgotten what `r` holds. |
| `cfgMgr` | `configManager` | Letters cut from inside words make the reader guess. |
| `processTheUserInputDataNow` | `parseInput` | Six words, and three of them say nothing. |
| `valid` | `isValid` | `is` says the value is only true or false. |

## Where to look

| For | Read |
|-----|------|
| Why something is the way it is | `docs/records/` |
| Handing a task to a worker | the `dispatch` skill |
| Reviewing a pull request | the `inspect` skill |
| Something went wrong | the `rca` skill |
| What this repo is missing | `yokoten doctor` |

## This project

## Working on Carrysheet

Rules for AI agents (and people) changing this repository. Read all of it before your first change.

### What this is

Carrysheet is a small, fast, native spreadsheet written in Rust. The first release is `carrysheet-cli`, a command-line tool with an MCP server, for Windows, Linux and macOS. A Windows desktop app comes later. Promise: AI-friendly, human-friendly, MCP-friendly.

### Layout and licenses

| Folder | What | License |
|---|---|---|
| `crates/` | Libraries: file formats, the workbook model, the command layer | MIT OR Apache-2.0 |
| `apps/` | Programs: `carrysheet-cli` (with MCP), later the desktop app | GPL-3.0-or-later |
| `research/` | Experiments such as the kernel comparison; its own Cargo workspace | MIT OR Apache-2.0 |
| `tools/` | Developer tools: `fuzzlite` (the fuzzer for the file readers), later `bench` | MIT OR Apache-2.0 |

Every crate sets `license` in its `Cargo.toml` to match its folder. A library must never depend on anything in `apps/`.

### Clean-room rule (hard)

You may read and use: Microsoft's published specs (MS-XLS, MS-XLSB, MS-OFFCRYPTO, ECMA-376), and permissively licensed code (MIT, Apache-2.0, BSD), e.g. Apache POI and calamine.

You must never copy, translate or closely paraphrase code from GPL, LGPL, AGPL or MPL projects (LibreOffice, Gnumeric, OpenOffice, ONLYOFFICE, wine) into this repository, and must not read their source while writing the matching part here. If you know an approach only from such code, say so in the PR instead of writing it.

### Code rules

- Rust edition 2024, stable toolchain, `rust-version = 1.90`. No `unsafe` (workspace lint forbids it).
- CI is the merge gate: it runs the full check (`CI=1 node scripts/check.mjs`) on every PR, and a PR is merged only when CI is green. A local full run is optional. check.mjs runs every check in order (fmt, clippy, tests, cargo-deny, the zip tests on both inflaters, wasm, Windows builds, release privacy check). Only a full run (no `--quick`, no `--from`) on a clean tree is the merge check. A full run is always strict: a skipped step (a missing tool or target) fails it. check.mjs refuses a full run on a dirty tree or when git can't report the tree, and fails if HEAD or the tree changes while it runs. `--list` shows the steps, `--quick` checks the crates you changed and the crates that depend on them while you work, `--from <step>` resumes, and `--strict` (on when `CI=1`) makes a partial run strict too. Its own tests: `node --test scripts/check.test.mjs`.
- Parsers treat every input file as hostile: no panics, no unbounded allocation, limits on sizes and counts. A malformed file returns an error that says what is wrong.
- Never drop data you don't understand when saving: keep it and write it back.
- Errors for people and agents have three parts: what happened, why, and how to fix it.
- Tests use small files committed under the crate's `tests/data/` (made by us or from Apache POI's test data with its license noted). Never commit large corpora.

### Command line and MCP

The CLI and MCP server follow `docs/agents.md`. In short: `--json` everywhere with JSON-only stdout; exit codes 0 done, 1 failed, 2 usage, 3 nothing to do; never prompt without a terminal; `--dry-run` on every command that changes a file; MCP schemas that Claude, Gemini and OpenAI strict mode all accept.

### Working on this machine

- The development machine is shared: build with `CARGO_BUILD_JOBS=1` and `nice -n 10`, under `flock ~/.cache/carrysheet-build.lock` (check.mjs does this for you unless `CI=1`); never run two builds at once.
- On this machine, set `export CARRYSHEET_HEAVY=~/bin/heavy` (the wrapper that waits for free memory). check.mjs then runs each heavy step as `~/bin/heavy flock ~/.cache/carrysheet-build.lock nice -n 10 env CARGO_BUILD_JOBS=1 cargo ...`, and the release step as `~/bin/heavy node scripts/release-check.mjs`, which takes the build lock inside. The order is always the same: the heavy wrapper outside, the build lock inside; never the other way round, or two jobs can each wait for the other. Exit code 75 from the wrapper means the machine was busy and nothing ran (check.mjs reports BUSY: retry later). Without `CARRYSHEET_HEAVY` there is no wrapper, but cargo still runs with one job and `nice` under the build lock; only in CI (`CI=1`) does it use every core at normal priority.
- Builds share one compile cache through sccache, set in the user's global cargo config (`~/.cargo/config.toml`: `rustc-wrapper`, incremental builds off). Each worktree keeps its own `target/`. Never turn on incremental builds, override the wrapper (`RUSTC_WRAPPER`), or vary `RUSTFLAGS` or `CARGO_TARGET_DIR` from run to run in a committed file or script: each makes the cache miss. If you really need incremental builds for a while, set `CARGO_INCREMENTAL=1` in your shell only; don't edit `.cargo/config.toml`, which is committed (a change makes the tree dirty, and the merge run refuses it). When sccache is on `PATH`, check.mjs prints the cache hit rate at the end.
- Rust is in `~/.cargo/bin` (add it to `PATH`). Windows targets build with mingw: `x86_64-pc-windows-gnu`, `i686-pc-windows-gnu`.
- CI runs `CI=1 node scripts/check.mjs` on every PR; merge only when CI is green. On the shared development machine, run `--quick` only.
- Never write the owner's email, Windows user name, or absolute home paths into files, commits, PRs or output. Use `~` or `<user>`.

### Commits and PRs

- Commit with `TZ=UTC git commit`, messages in English, one logical change each, ending with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` (or your own agent and model; this is our AI provenance record).
- PRs are drafts until the owner reviews them. PR descriptions end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)` when written by Claude.
