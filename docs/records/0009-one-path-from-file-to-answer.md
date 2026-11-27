# 0009 One path from file to answer: architecture and tech stack

Status: accepted; replaced in part by record 0010
Date: 2026-11-26 (UTC)

The owner accepted this record on 2026-11-26, with three choices: the `macros` code moves from `apps/` to `crates/` under MIT OR Apache-2.0; open work lands in the order of context point 6; v0.1.0 ships after step 3 of the order, and the kernel cut and the budget go to v0.2.0.

## Context

The owner asked for a refactor of the tech stack and of the architecture before more features are added. This record says what changes, what stays, and in which order.

Everything below was checked on `main` at `f30ac0e` on 2026-11-26 by reading the code, the manifests, `Cargo.lock`, the open issues and the open pull requests. Nothing was built or run for this record, so effects on behaviour are marked "read in the code".

### Sizes

- Our Rust code in `crates/`, `apps/` and `tools/`: 95,185 lines, tests included.
- The vendored kernel in `vendor/gridcraft`: 88,841 lines in 10 crates.
- `Cargo.lock`: 217 packages.

### 1. The program does not use most of our readers

`carrysheet-cli` opens every file through one call, `gridcraft_engine::io::open_bytes` (`crates/carrysheet-kernel/src/book.rs`). GridCraft then reads xlsx, xlsb, ods, xls and csv with its own code, and detects encrypted files with its own compound-file and crypto code.

Five of our libraries are linked into no program. Only `tools/bench` and `tools/fuzzlite` use some of them:

| Library | Lines in `src/` | Used by a program? |
|---|---|---|
| carrysheet-biff | 5,797 | no |
| carrysheet-csv | 5,681 | no |
| carrysheet-crypto | 3,697 | no |
| carrysheet-numbers | 6,427 | no |
| carrysheet-connections | 7,702 | no |
| carrysheet-query | 6,723 | only as a worker entry behind the `query` feature, which is off; the command table has no `query` |

What a user gets today instead (read in the code):

- **csv:** GridCraft's `read_csv` runs with `parse_values: true`, so `007` becomes the number 7. Bytes that are not UTF-8 are read as Windows-1252, so a GBK, Big5 or Shift_JIS file comes out as wrong characters. Record 0006 says csv import never converts in a way that loses data, and `carrysheet-csv` was written to keep that promise (`read_values`, encoding detection). It is not on the path.
- **xls:** GridCraft's reader takes BIFF8 only and refuses older files. `carrysheet-biff` reads BIFF2 to BIFF8.
- **Encrypted files:** found by GridCraft's own compound-file code and refused. `carrysheet-crypto`, which tells agile, standard, RC4, XOR and Rights Management apart and can decrypt, is not asked.

### 2. Command logic lives in the GPL program

`apps/carrysheet-cli/src/run/` holds the whole `macros` command and its package reading: `macros.rs` 896 lines, `package.rs` 911, `xml.rs` 510, `text.rs` 133. That is 2,450 lines a desktop app or a web page cannot reuse without copying. The workbook commands are already in a library (`carrysheet-commands/src/engine/run.rs`), but the place where the real kernel is joined to them is the program (`run/mod.rs`), because `carrysheet-kernel` depends on `carrysheet-commands` and not the other way round.

### 3. The same job is written several times

- **Showing text from a file safely:** eight helpers in five libraries and the program (`carrysheet-commands` `clean_from_file`, `carrysheet-connections` `clean` and `safe_text`, `carrysheet-crypto` `clean`, `carrysheet-csv` `printable`, `carrysheet-vba` `printable`, the program's `run/text.rs` and `log.rs`). They do not escape the same characters. `clean_from_file`, which the kernel uses for every message, lets tag characters (U+E0000 to U+E007F) and backslashes through; the vba and csv helpers escape them. Issue 67 names this as a prompt-injection channel.
- **Limits:** ten crates each define their own `Limits` (biff, cfb, connections, crypto, csv, kernel, numbers, query, vba, zip). The 10-second budget of one command is a thread-local inside GridCraft (`gridcraft_core::limits`, our patch). Our own readers cannot see it; the `macros` command keeps a second clock of its own in `package.rs`.
- **Opening a zip package:** the program (`run/package.rs`, on carrysheet-zip), `carrysheet-connections` (`package/`, a `PartSource` trait nobody implements over a real zip), `carrysheet-kernel` (`package.rs`, on the `zip` crate) and GridCraft (`xlsx/src/package.rs`, on the `zip` crate). Issue 40 and PR 106 cover part of this.
- **Workbook models:** GridCraft's, `carrysheet-biff`'s, `carrysheet-numbers`'s, and `carrysheet-model::Workbook`, which only the in-memory test engine uses.

### 4. The vendored kernel cannot be updated and is carried whole

- The kernel crate calls 13 GridCraft commands (`cell.set`, `range.fill`, `range.setValues`, `selection.set`, `edit.clearContents`, `sheet.activate`, `sheet.rename`, and insert or delete for rows, columns and sheets). The engine has 20 command modules (21,567 lines with their `mod.rs`), and they are reached by name, so the compiler cannot drop the unused ones. `image`, `gridcraft-chart` and `gridcraft-pdf` are linked into the program although it draws nothing. Issue 73 measured the cost: the program grew from 2.6 MB to 12.6 MB.
- Our patches are edits in place: 57 places marked `Carrysheet patch` in 18 files, plus the manifest and zip 9 changes that `vendor/gridcraft/README.md` lists in prose. There is no patch file and no script, so nobody can check that the folder is "upstream at 823c565 plus our patches", and nobody can replay the patches on a newer upstream.
- Upstream moved from v0.8.0 (2026-10-18, our copy) to v0.19.1 (2026-11-12).

### 5. Dependencies

- Two versions each of `quick-xml` (0.41, 0.42), `zip` (8.6.0 for a test dependency, 9.0.2) and `miniz_oxide` in `Cargo.lock`.
- Third-party versions are repeated per manifest (`serde_json` in five, `flate2` in five, `zip` pinned in three).
- The Rust version developers must use (1.99.1) is written only in `.github/workflows/ci.yml`.
- `tools/fuzzlite` depends on the `cfb` crate, which issue 42 replaced with `carrysheet-cfb` everywhere else.

### 6. Work in flight

Five branches have waited since 2026-10-21: PR 105 (release, touches every manifest), PR 106 (zip in the kernel and Numbers), PR 107 (agent rules), and the branches `102-check` and `53-vba-bomb`, which have no pull request. A refactor that moves the files under them forces each to be redone.

## Decision

### 1. Layers, checked by the build

A crate depends only on layers below its own.

| Layer | Crates | Holds |
|---|---|---|
| programs | `apps/*` | arguments, terminal output and prompts, MCP transports. No file parsing, no command logic |
| product | `carrysheet` (new) | every command, joined to the real kernel and readers: the product as a library |
| kernel | `carrysheet-kernel`, `vendor/gridcraft` | the one in-memory workbook, calculation, xlsx reading and writing, and the import of every other format into that workbook |
| contract | `carrysheet-commands` | the command table, schemas, validation, errors, the `Engine` interface |
| formats | biff, csv, crypto, numbers, vba, connections, query | bytes in, a typed result out. A format crate never depends on the kernel or on the contract |
| containers | `carrysheet-zip`, `carrysheet-cfb`, `carrysheet-opc` (new) | zip, compound files, and Office packages (content types, relationships, part names) |
| base | `carrysheet-core` (new, no dependencies), `carrysheet-model` | safe text, the work budget; references, ranges and values |

Third-party fences, written as `wrappers` in `deny.toml` so `cargo deny` fails when one is crossed:

- `gridcraft-*`: only `carrysheet-kernel`.
- `rusqlite`: only `carrysheet-query`.
- `rmcp`, `tokio`, `hyper`: only `apps/`.
- `zip`: only `gridcraft-xlsx`, for writing. Every read of a zip goes through `carrysheet-zip`.

### 2. One path from bytes to workbook

The kernel crate owns the path: recognise the format from the bytes, read it with the one reader for that format, and build GridCraft's workbook.

| Format | Today | After |
|---|---|---|
| xlsx, xlsm, xltx, xltm | GridCraft's reader on the `zip` crate | GridCraft's reader, package opened by `carrysheet-zip` |
| csv | GridCraft `read_csv` | `carrysheet-csv` `read_values` |
| xls | GridCraft `read_xls`, BIFF8 only | `carrysheet-biff`, BIFF2 to BIFF8 |
| encrypted | GridCraft detects, we refuse | `carrysheet-crypto` detects, the refusal names the kind. Opening with a password is a later feature that plugs in here |
| xlsb, ods | GridCraft | unchanged: we have no reader of our own |
| numbers | not opened | not in this refactor; it plugs in here later |

A reader replaces GridCraft's only after a test shows it loses nothing GridCraft's kept on the same files. GridCraft's csv reader, xls reader, compound-file sniffer and decryption are then deleted from the vendored copy.

GridCraft's model is the one in-memory workbook (this answers issue 55). `carrysheet-model` is the vocabulary of the command layer: references, ranges, values. Its `Workbook` type stays a test tool.

### 3. The product is a library

A new crate `crates/carrysheet` runs every command with the real kernel and readers. The command line, the MCP server, and later the desktop and web apps call it. The `macros` command and its package reading move out of `apps/carrysheet-cli` into libraries. `text_for` (what a person sees), prompts and the MCP server stay in the program.

### 4. One of each shared part

- **Text from files** (issue 67): `carrysheet_core::text`, with a table generated from Unicode data and committed. Every helper listed in the context is replaced by it.
- **Budget** (issue 56): `carrysheet_core::Budget` holds a command's deadline and work count and returns a three-part error when it runs out. It is passed as an argument, never kept in a global. The kernel hands the same deadline to GridCraft. The readers on the open path and the `macros` path take it first; the others when they join a command.
- **Office packages** (issue 40): `carrysheet-opc`, on `carrysheet-zip`. The `macros` command, the kernel's save report and `carrysheet-connections` use it.

### 5. A kernel we can rebuild, then a smaller one

1. `vendor/gridcraft` records the upstream commit, the folder map, the files left out and our patches as patch files. A script checks that upstream plus the patches gives exactly the folder.
2. Then the cut of issue 73: drawing (`gridcraft-chart`, `gridcraft-pdf`, `image`), the command modules we never call, and the readers of point 2. Content of those features still survives a save through passthrough; the round-trip tests must show no new `lost` item. The program's size is measured before and after.
3. Moving to a newer upstream is a separate decision, made when step 1 shows what replaying the patches costs.

### 6. Tech stack

| Part | Decision |
|---|---|
| Rust 2024, stable, MSRV 1.90, no `unsafe` | Keep. The developer toolchain is pinned in `rust-toolchain.toml`, and CI reads it from there |
| GridCraft, vendored | Keep (record 0003), per point 5 |
| `rmcp`, `tokio`, `hyper` for MCP | Keep, fenced to `apps/`. The SDK follows both MCP lifecycles; writing our own costs more than it saves |
| `clap`, `serde`, `serde_json` | Keep |
| SQLite through `rusqlite` (bundled C) | Keep, fenced to `carrysheet-query`. Agents know its SQL, and the guard work of issue 25 is built on it |
| `quick-xml` | Keep, one version |
| `zip` crate | Writing only, inside `gridcraft-xlsx` |
| `image`, `gridcraft-chart`, `gridcraft-pdf` | Removed with the cut |
| `cfb` crate | Test oracle of `carrysheet-vba` only; `fuzzlite` moves to `carrysheet-cfb` |
| Third-party versions | Written once, in `[workspace.dependencies]` |
| Node scripts (`check.mjs`) | Keep |

### 7. Order

1. Parts that add and move nothing under open work: `carrysheet-core`, the vendoring script, the dependency cleanup.
2. csv on the open path, the shared text function in every library, `carrysheet-opc`.
3. xls and encrypted files on the open path, the `carrysheet` crate.
4. The kernel cut, the budget in the readers.
5. xlsx packages through `carrysheet-zip`, then the docs.

Each step keeps `main` shippable: a step that changes what a user sees says so in `CHANGELOG.md`.

## Options not taken

- **Keep GridCraft's readers and drop ours.** Less code to carry, but xls stays BIFF8 only, csv keeps changing values and misreading East Asian encodings, and the readers we fuzzed stay unused.
- **Replace GridCraft, or write our own engine.** Record 0003 measured the choice; nothing found here changes it.
- **Update GridCraft to v0.19 first.** Without patch files, every patched file would be merged by hand, and the cut would be redone after.
- **Group the crates into folders (issue 38).** It changes nothing a user or the build sees, and it conflicts with every open branch. Not done.
- **Put every command into `carrysheet-commands`.** The kernel depends on that crate for the `Engine` interface, so it cannot depend on the kernel.
- **A registry where formats plug themselves in (issue 63).** Four imports in one `match` do not need one. Look again when a format comes from outside this repository.
- **One `Limits` type for every crate.** The per-format counts (records, sheets, entries) are real differences. Only time and work are shared, so only they move to the budget.
- **Our own JSON-RPC instead of `rmcp`; a Rust SQL engine instead of SQLite; `cargo xtask` instead of Node scripts.** Each removes a dependency and adds code and risk we would own. None fixes a problem found in the context.

## Consequences

- Easier: a reader we write reaches users, because there is one place where formats enter. The desktop and web apps start from a library call, not from a copy of the command line.
- Easier: one fix for text safety and one clock per command.
- Easier: the kernel can take an upstream fix, and a smaller kernel is less code that reads hostile files.
- Harder: three new crates (`carrysheet-core`, `carrysheet-opc`, `carrysheet`), 15 libraries in all.
- Harder: csv and xls results change for users. `007` stays text, East Asian csv files read correctly, old xls files open. Each change needs a changelog line and a test.
- Harder: the open branches of context point 6 are redone on the new layout where they touch moved files, most of all `102-check`.
- To check:
  - the differential tests of point 2 before each GridCraft reader is deleted;
  - the program's size before and after the cut, against the 20 MB limit for an install;
