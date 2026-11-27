# 0011 Closing the 5,000-line cap: format, cut eval, three small cuts

Status: accepted
Date: 2026-11-28 (UTC)

The owner chose "Format + cut eval" on 2026-11-28, from the options below, and asked for auto dev and auto merge. This record amends [record 0010](0010-five-thousand-lines.md), decisions 2, 10 and 11. Everything else in record 0010 stands.

## Context

Step 6b (branch `7b-output-mcp` at `5987655`) ended at **5,467** counted lines (`node scripts/lines.mjs`). The cap is 5,000. Record 0010, decision 11 gave the order of cuts when the count is over: header rows in `overview` (done in step 6b), counts inside sheets in the save-loss report (done in step 5), insert and delete of rows and columns, then `eval`.

The lead measured the options before asking:

| Option | Measured or estimated |
|---|---|
| `use_small_heuristics = "Max"` in `rustfmt.toml`, width unchanged | -249, measured |
| Sweep names kept only for the parallel compile of wave 7b | about -48 |
| Cut `eval` | about -120 |
| `-edited-N` output names, `save.parts` counts, the `recalculated` list | about -14, -20 and -45 |
| Insert and delete of rows and columns (decision 11, point 3) | about -220 |

The owner's options were: format only; format and the sweep; format and cut `eval`; or rows and columns as decision 11 says. He chose format and cut `eval`. The sweep and the three small cuts came with it, so that the count lands under 5,000 with room for fixes.

## Decision

### 1. What leaves

| Change | Lines, measured |
|---|---|
| `rustfmt.toml`: `use_small_heuristics = "Max"`, then `cargo fmt --all`. One commit, no logic change | 5,467 to 5,218 (-249) |
| `range::validate_sheet_name` (a range check; `changes/sheet-name-check.md` has the visible change), `range::Shape`, `CommandError::not_implemented` and `ErrorCode::NotImplemented` (nothing returned it) | to 5,163 (-55) |
| `eval`: the command, its arguments, its view code and its MCP tool | to 5,042 (-121) |
| `edit` without `output` writes only `name-edited.xlsx`; the `-edited-2` to `-edited-99` search leaves | to 5,020 (-22) |
| The `parts` counts in the save report (identical, rewritten, dropped, added) | to 4,993 (-27) |
| The `recalculated` and `recalculated_total` lists in the `edit` answer | to 4,944 (-49) |
| `.ods` opens (point 4) | 4,944 (0) |

What stays: insert and delete of rows and columns, paging across wide ranges, `--ops @file` and `--max-seconds`.

What a user loses, and what replaces it:

- `eval`: `edit` with `dry_run` shows what a formula computes. Set the formula in a free cell; the change list gives its value, and nothing is written. The cut-text hint of `read` now says so.
- A sheet name that breaks Excel's rules inside a range is found by lookup, so the error is `no_such_sheet` (exit 1) instead of `invalid_reference` (exit 2).
- A taken `name-edited.xlsx` is refused with `file_exists`; the caller passes `output`.
- `save.parts` and `recalculated`: `lost`, `dropped_parts`, `notes` and `changes` still say what a save loses and what an edit wrote.

Removed code stays in the history of `main` (record 0010, decision 5). `4defaae` is the newest `main` commit that holds all of it. Each `changes/` file names it.

### 2. Budgets per file (amends decision 2)

Record 0010's budget assumed `cap-std` and an `eval` in `view.rs`. These are the budgets of wave 7b's plan, with the count after this record:

| File | Wave 7b budget | After this record |
|---|---|---|
| `carrysheet/src/lib.rs` | 242 | 239 |
| `args.rs` | 598 | 579 |
| `error.rs` | 188 | 158 |
| `text.rs` | 100 | 95 |
| `open.rs` | 318 | 301 |
| `range.rs` | 145 | 96 |
| `view.rs` | 715 | 572 |
| `edit.rs` | 765 | 691 |
| `save.rs` | 515 | 449 |
| `save/loss.rs` | 270 | 231 |
| `commands/limits.rs` | 40 | 40 |
| `carrysheet-cli/src/main.rs` | 77 | 71 |
| `cli.rs` | 137 | 134 |
| `run/mod.rs` | 132 | 124 |
| `output.rs` | 158 | 156 |
| `files.rs` | 570 | 508 |
| `mcp.rs` | 345 | 322 |

The budget is a plan, not a result. The count decides.

### 3. Order (amends decisions 10 and 11)

- Decision 10: step 6 became three steps. Step 6 slimmed `view`, `edit` and the program; step 6b (wave 7b) slimmed every file to the budgets above; step 6c is this record. The switch of the CI ceiling to 5,000 moves to its own step after 6c. Steps 7 and 8 follow as planned.
- Decision 11: `eval` leaves before insert and delete of rows and columns. Rows and columns stay. If the count is over again, they are the next cut, and a new record says so.

### 4. `.ods`

Decision 3 lists `.ods` among the formats that open, and the README and `docs/agents.md` say the same. The program refused the extension: `check_opens` did not list it. GridCraft picks its reader by the file's content, so an .ods file with another name already reached the .ods reader. The code was the smaller fix: one word in `OPENS`, 0 counted lines. A test builds a small .ods and reads it. The record is unchanged.

## Options not taken

- **Rows and columns, as decision 11 says.** About -220 lines, but it removes a feature agents use to restructure a sheet. `eval` has a replacement in `edit` with `dry_run`; rows and columns have none.
- **Format only.** 5,218 is still over the cap.
- **A wider `max_width` in `rustfmt.toml`.** It saves more lines, but long lines are harder to read and review, and record 0010 rewards short lines, not dense ones.
- **Change the record for `.ods`.** It would take a format out of decision 3, the README and `docs/agents.md` that GridCraft already reads by content.

## Consequences

- Easier: the count is 4,944, under the cap, with 56 lines of room. The planning target of 4,500 is still out of reach; above it, `lines.mjs` prints a note.
- Easier: one format change, in its own commit, keeps later diffs short. `cargo fmt --all --check` holds it.
- Harder: an agent that called `eval` gets `unknown command` and must use `edit` with `dry_run`. `changes/eval-removed.md` says how.
- Harder: `use_small_heuristics = "Max"` puts more on one line. A reviewer reads denser `if` and struct expressions.
- To check:
  - the CI ceiling (`--max` in the `lines` job) against the count after this step;
  - that no client relied on `save.parts` or `recalculated`; both are gone from the JSON.
