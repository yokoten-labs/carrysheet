# 0010 A 5,000-line cap: one library, one program, GridCraft reads the formats

Status: accepted; decisions 2, 10 and 11 amended by record 0011
Date: 2026-11-27 (UTC)

The owner asked the lead to decide on his behalf ("auto dev", 2026-11-27); the lead took the recommended answer to each open question: the cap counts physical lines (`wc -l`, blank and comment lines included); removed code stays in `main`'s git history and the CHANGELOG names the last commit that had it; the `macros` command is removed now, and `overview` still reports that a file has macros, keeps them on save and never runs them; `tools/excel-ground-truth` leaves with the cut and may return under `tests/` when Excel comparisons start; v0.1.0 ships once the count is under 5,000 (after step 8).

This record replaces record 0009 where the two conflict. Decision 9 lists the parts. Record 0009 lands on `main` in the same change, as it was accepted.

## Context

The owner set a limit on 2026-11-27: the repository holds at most 5,000 lines of code. He chose what counts: our own source, not tests and not `vendor/`. He left the choice of what to drop or move to the lead.

Everything below was measured or read on `main` at `fd8d930` on 2026-11-27. Nothing was built or run. Every "after" number is an estimate.

### Sizes today

Counted with the rule of decision 1 (`git ls-files`, physical lines):

| Part | Lines |
|---|---|
| All counted code | 73,110 |
| Six readers no program links (biff, csv, crypto, numbers, connections, query), `tools/`, one generator script | 45,044 |
| The `macros` command, MCP over HTTP, and the vba, cfb and zip crates | 12,562 |
| The rest: what a user runs today (command line, commands, kernel join, model, scripts) | 15,504 |
| of which test modules inside `src/` | 3,923 |
| of which code only tests call (schema lint, memory engine, model `Workbook`) | 1,047 |
| The rest without those two | 10,534 |

Not counted: test files, 25,291 lines; `vendor/gridcraft`, 88,841 lines of Rust.

So the limit asks for two things. Deleting unused and optional code gives 73,110 to about 10,500. The last step, 10,500 to 5,000, is a rewrite of code that works today.

### Facts the decision rests on

- The program opens every file through `gridcraft_engine::io::open_bytes` (`crates/carrysheet-kernel/src/book.rs`). GridCraft reads xlsx, xlsm, xlsb, ods, xls (BIFF8) and csv, and detects encrypted files. Record 0009 context point 1 has the details.
- `carrysheet-cli` depends on commands, kernel, model, vba and zip, and on query behind a feature that is off. The vba, cfb and zip crates serve only the `macros` command.
- `check`, `sanitize`, `mcp install` and `mcp doctor` are in the command table and answer "not implemented yet".
- GridCraft has `CellRef::parse` and `RangeRef::parse` (`base/core/src/addr.rs`) and a csv option `parse_values` (`xlsx/src/lib.rs`). `WEBSERVICE` returns `#VALUE!` (`base/functions/src/info.rs`).
- `schemars` 1.2.3 is in `Cargo.lock` through `rmcp`. `encoding_rs` 0.8.42 and `chardetng` 0.1.17 are there through our csv reader. `cap-std` is not there.
- On branch `refactor-0009`, our GridCraft patches are 7 patch files with 981 added lines.
- The owner's other project counts its cap with `wc -l`, blank lines included, tests left out. It has a target below the cap. When it is over, a feature leaves the core; the cap does not rise.

### Work in flight

PR 107 and PR 109 merged on 2026-11-27. Open: PR 105 (release, draft), PR 106 (kernel unpack, draft), PR 110 (vba bomb), two dependency update PRs. Local, not merged: `refactor-0009` and its three step-1 branches.

## Decision

### 1. The cap and how it is counted

Our own source is at most **5,000 lines**. The planning target is **4,500**, so fixes have room.

- **Counted:** every tracked file with a code extension (`rs`, `mjs`, `js`, `cjs`, `ts`, `py`, `sh`, `ps1`, `psm1`), outside `vendor/`. Every line counts, blank and comment lines too.
- **Not counted:** a path with a folder named `tests` or `benches`, and `*.test.mjs`. Nothing else is left out.
- A test module written inside a source file counts. Unit tests live in `src/tests/`.
- Generated code that is committed counts. Config and docs (TOML, YAML, Markdown, JSON) do not.
- A crate of ours that the program links counts wherever it lives. A second repository does not make the product smaller.
- `vendor/` holds only upstream code plus our patch files. A patch changes GridCraft's behaviour; it never carries a Carrysheet feature.

### 2. What the repository holds

Two crates and two scripts.

| Path | License | Holds |
|---|---|---|
| `crates/carrysheet` | MIT OR Apache-2.0 | The product as a library: bytes in, JSON out. No file, network or terminal calls. Builds for wasm32 |
| `apps/carrysheet-cli` | GPL-3.0-or-later | Arguments, files and `--root`, output for people, the MCP server over stdio |
| `scripts/lines.mjs`, `scripts/vendor.mjs` | MIT OR Apache-2.0 | The cap check; the check that `vendor/` is upstream plus the patch files |

Budget, estimated from today's files:

| File | Holds | Today | Budget |
|---|---|---|---|
| `carrysheet/src/lib.rs` | run one command, the deadline, a caught panic | 615 | 250 |
| `args.rs` | one struct per command: flags, MCP schemas, help | 1,083 | 420 |
| `error.rs` | three-part errors, codes | 237 | 170 |
| `text.rs` | safe text from files (issue 67) | 153 on `core-crate` | 130 |
| `open.rs` | bytes to GridCraft's workbook, limits, the csv front end | 208 + part of 483 | 330 |
| `view.rs` | `overview`, `read`, `eval` | 799 + part of 483 | 800 |
| `edit.rs` | `edit` | 1,083 | 750 |
| `save.rs` | `convert`, csv escaping, the save-loss report | 1,390 | 600 |
| `carrysheet-cli/src/main.rs`, `cli.rs` | arguments, dispatch, exit codes | 746 | 230 |
| `output.rs` | JSON, and one text form for people | 283 | 120 |
| `files.rs` | `--root`, bounded reads, atomic writes, `~` | 628 | 320 |
| `mcp.rs` | MCP over stdio | 954 | 380 |
| `scripts/` | the two scripts | 1,074 | 160 |
| **Total** | | **about 10,500**, with `carrysheet-model` 955 | **4,660** |

The budget is a plan, not a result. Expect 15% either way. The count decides, not this table.

### 3. What stays

- `overview`, `read` (pages, cursor, long text cut and reported), `eval`, `edit` (with `dry_run`, all or nothing), `convert` to xlsx and csv, and `commands`.
- Opening xlsx, xlsm, xltx, xltm, xlsb, ods, xls (BIFF8) and csv through GridCraft.
- Record 0006's csv promise, which today's program breaks. A small front end detects the encoding and keeps `007` as text.
- The guard layer: limits and the 10-second deadline, safe text, paging, `--root`, the refusal of formulas that reach outside the file, csv formula escaping, `confirm_loss`, three-part errors, exit codes 0/1/2/3.
- MCP over stdio with `--root` and `--read-only`. `--json`, `--dry-run` and `--yes` on the command line.
- Every test of kept behaviour. The hostile and mutation files of the removed readers become tests of the open path.

### 4. What leaves, and what a user loses

| Leaves | Lines | A user loses |
|---|---|---|
| biff, csv, crypto, numbers, connections, query | 36,366 | Nothing: no program links them. The plans for BIFF2 to BIFF7, Numbers, SQL and the connection list end here |
| `macros` with vba, cfb, zip | 9,903 | The module list, source view and risk flags. `overview` and the save report still say that macros exist, are kept, and never run |
| MCP over HTTP | 2,659 | Clients that speak only HTTP. Stdio clients are not affected |
| `feedback`, the y/N question, `--log-file`, the four placeholders | about 600 | The drafted bug report and the log file. Logs go to stderr. People pass `--yes` |
| `tools/bench`, `tools/fuzzlite` | 7,145 | Nothing. They test the removed readers |
| `tools/excel-ground-truth` (PowerShell) | 1,455 | Nothing today: no expected values are committed yet. It returns under a `tests` folder when that work starts, if the owner agrees it is test tooling |
| `check.mjs`, `output.mjs`, `release-check.mjs`, the generator | 1,152 | Nothing. CI runs cargo and the privacy scan as plain steps |
| The `Engine` interface, the memory engine, `carrysheet-model` | about 1,800 | Nothing. Commands call GridCraft; tests run on the real kernel |

The binary then holds no network code. `deny.toml` bans HTTP and TLS crates, and `rmcp` keeps only its stdio transport.

### 5. Where removed code lives

In the history of `main`. `CHANGELOG.md` names the last commit that holds it. No second repository is made now.

### 6. Parts we borrow instead of own

| Job | From | Condition |
|---|---|---|
| File formats, A1 references, formula parsing | vendored GridCraft | Record 0003 stands |
| Flags, schemas, validation | `clap` derive, `serde`, `schemars` | The schema lint passes unchanged, as a test. `tools/list` stays under 10,000 characters |
| csv encodings | `encoding_rs`, `chardetng` | The versions our csv reader used |
| Opening files below `--root` | `cap-std` | Today's root tests pass on Windows, Linux and macOS. If not, our own walk stays |
| Third-party notices | `cargo-about` | A release tool, not a dependency |

A new crate follows `docs/dependencies.md`: at least 7 days old, clean in `cargo deny`. Our text-only path check stays in front of `cap-std`: it refuses UNC, device and other-drive paths before Windows touches them.

A format GridCraft cannot read is offered upstream first (record 0003). We do not hold a second reader here.

### 7. How a feature comes back

A feature that left returns in one of two ways: it fits under the cap, or it is its own program in its own repository. A new record decides which. The cap is not raised to make room.

### 8. How CI holds the cap

- **A job named `lines`** runs `node scripts/lines.mjs` on every pull request and every push to `main`. It needs no Rust, so it answers in seconds. It is a required check.
- The script counts by decision 1. Above 5,000 it fails and prints lines per folder. Above 4,500 it prints a note; the pull request then says why.
- **No new language slips past.** A tracked file outside `vendor/` and outside test paths must have a code extension or be on a short data list. Anything else fails.
- **Test paths stay tests.** A counted Rust file fails the check when it has `#[path`, `include!(`, or a `mod tests;` without `#[cfg(test)]`.
- **Patches cannot hide code.** The `vendor` job proves the folder is upstream plus the patch files. `lines.mjs` counts the added lines in those files and fails above a constant. The constant is the measured number when the patch files land, plus at most 100.
- **The numbers are the owner's.** `scripts/lines.mjs`, `rustfmt.toml` and `deny.toml` go into CODEOWNERS. A change to the cap also needs a new record.
- **During the cut,** CI passes `--max <ceiling>` with the ceiling of the current step (decision 10). The pull request that reaches 5,000 deletes the option.
- `scripts/lines.test.mjs` tests the script. `AGENTS.md` gets the rule and points here.

### 9. Earlier records

Record 0009, by its decision numbers:

| 0009 | Now |
|---|---|
| 1. Seven layers, fences | Replaced: one library, one program. Fences: `gridcraft-*` only in `carrysheet`; `rmcp` and `tokio` only in `carrysheet-cli` |
| 2. Our readers on the open path | Replaced: GridCraft reads every format. The csv promise is kept by the front end |
| 3. The product is a library | Kept. Moving `macros` into libraries is dropped with the command |
| 4. `carrysheet-core`, `Budget`, `carrysheet-opc` | Text safety is kept, inside the library. `Budget` and `carrysheet-opc` are not built. GridCraft's per-call limit is the one clock while GridCraft is the only reader |
| 5. Patch files, the kernel cut, a newer upstream | Kept, unchanged |
| 6. Tech stack | `rusqlite`, `hyper`, our `quick-xml` use and `check.mjs` leave. `rust-toolchain.toml` and one place for versions stay |
| 7. Order | Replaced by decision 10 |

Other records: 0003 point 5 said the `.xls` writer stays our own library; no such library is kept. 0006 said macros are "kept and listed"; they are now kept and reported. 0008 stands; five tools need no toolsets yet. 0005 stands; its release test uses stdio clients.

### 10. Order

Each step is one or more pull requests. `main` stays shippable after each. Ceilings are measured again when a step lands.

| Step | What | Count after |
|---|---|---|
| 1 | `lines.mjs`. Delete what no program links. Patch files for the kernel | about 28,200 |
| 2 | Remove `macros`, HTTP, `feedback`, the placeholders. The csv front end. Open-path tests from the removed readers' files | about 15,200 |
| 3 | One library crate, tests out of `src`. CI runs cargo directly; the three scripts go | about 9,900 |
| 4 | Commands call GridCraft directly. File access moves to the program | about 7,300 |
| 5 | Arguments from one struct per command. The slim save-loss report | about 6,000 |
| 6 | Slim `view`, `edit`, and the program. The cap switches to 5,000 | at most 5,000 |
| 7 | Release work, redone small. Then v0.1.0 | at most 5,000 |

Open work: PR 110 merges before step 2, so the fix is in history. PR 106 closes; step 5 rebuilds its kernel rule and tests. PR 105 closes; step 7 rebuilds it. The `refactor-0009` branches are not merged; steps 1, 3 and 6 rebuild what is kept from them.

### 11. If the count is over at step 6

Cut in this order, and say so in `CHANGELOG.md`:

1. Header rows in `overview` (about 60).
2. Counts inside sheets in the save-loss report; the part-level report stays (about 110).
3. Insert and delete of rows and columns in `edit` (about 220).
4. `eval` (about 120).

The csv front end, `confirm_loss`, `--root` and the limits are never cut without a new record.

## Options not taken

- **Move the libraries to a second repository and keep them linked.** By that design's own count, the main repository would hold about 3,700 lines, and no feature would leave. But the program would still be built from about 60,000 lines of ours, and that number grows with record 0009. It also adds a git source to a crates.io-only policy. It meets the words of the limit and not its use.
- **A frozen public repository for the removed readers.** It is honest if nothing depends on it, but it is one more thing to watch for advisories. History costs nothing. The owner can still choose it.
- **Count code lines only, without blank and comment lines.** It gives about 900 lines more room and does not push anyone to delete comments. It needs a comment parser, and it is not how the owner's other cap is counted.
- **Keep our own readers and drop GridCraft's.** One reader alone (csv, 5,681 lines) is over the cap.
- **Keep the command table instead of derived structs.** The table is known to pass the three vendors' rules, but with its schema writer, validator and flag builder it is about 1,300 lines today.
- **Reuse GridCraft's private package reader for the save-loss report.** One zip reader instead of two uses of the same crate, but it ties us to a private module through a patch.
- **A ratchet that forbids any rise per pull request.** Three planned changes add lines (the counter, the patch script, the csv front end). Step ceilings do the same job.
- **The cap check as a Rust test.** It would cost no counted lines, but the development machine has no cargo, and a Node script answers before any build.

## Consequences

- Easier: an agent can read the whole product in one sitting. Every reader of hostile files is GridCraft's, in one place, behind one set of limits.
- Easier: no network code in the binary, 2 crates instead of 15, and fewer dependencies to watch (`Cargo.lock` has 216 packages today).
- Harder: steps 4 to 6 rewrite about 10,500 working lines into about 4,700. The tests are the net; JSON shapes and error codes must not change unless `CHANGELOG.md` says so.
- Harder: we now rest on GridCraft's xls, xlsb and ods readers, which we never reviewed (issue 71). Our fuzzer aimed at our own readers and leaves with them.
- Harder: the physical-line rule rewards short comments. A reason that needs more than a few lines goes into a record or `docs/`.
- Harder: v0.1.0 waits for step 6.
- Lost for now: the macro list, MCP over HTTP, and the plans for SQL, connections, Numbers and old xls.
- To check:
  - `cap-std` on Windows (junctions, reserved names) against today's root tests, before it is adopted;
  - the derived schemas against the lint and the 10,000-character budget, before the table is deleted;
  - `chardetng` on short GBK, Big5 and Shift_JIS files;
  - that `rmcp` with only stdio leaves no HTTP crate in `Cargo.lock`;
  - one stdio-to-HTTP bridge, before the docs name it for HTTP-only clients;
  - the count after every step, against the ceilings above;
  - whether yokoten should carry a code-size check, so the next project does not write its own.
