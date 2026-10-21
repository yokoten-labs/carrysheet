# bench

Times Carrysheet's readers and writers on fixed files, and fails when a
change makes one more than 10% slower or more than 10% hungrier for memory
than the numbers committed in `baseline.json` (issue 58).

```sh
cargo build --release -p carrysheet-bench
target/release/bench list          # the cases, and what is not measured yet
target/release/bench run           # time every case
target/release/bench compare       # fail (exit 1) on a slowdown or memory growth
target/release/bench update        # rewrite baseline.json
```

Always build with `--release`: a debug build is ten times slower and says
nothing. On the shared development machine, wrap every run the way you wrap
a build: `~/bin/heavy nice -n 10 target/release/bench compare`.

Options: `--json` (JSON on stdout, nothing else), `--runs N` (timed runs per
case, default 5), `--filter TEXT` (cases whose name contains TEXT),
`--baseline PATH`, `--inputs DIR`. Exit codes: 0 done, 1 failed, 2 usage.

## What it measures

A case is a file and an operation, named `file/operation`:
`csv-100mb/open`, `csv-100mb/save`, `xls-1m-cells/read-range`. For each it
reports:

- **time**: the median of N timed runs (after one untimed warm-up), with
  `±` half the spread between the fastest and slowest run. A fast
  operation repeats inside each run (`inner`) so a run lasts long enough to
  time, and the time shown is per operation.
- **peak memory**: each case runs in its own child process (this same
  binary, `bench child <case>`), which reads its high-water mark (`VmHWM`)
  from `/proc/self/status` after its last run. The peak includes the input
  file's bytes and the program itself (about 3 MiB). On Windows and macOS
  the number would need `unsafe` code or a dependency, so it is reported as
  unavailable (`n/a`, `null` in JSON) and never compared.
- **check**: a number from the operation's result (cells read, bytes
  written). If it changes, the operation now computes something else; the
  line says `result changed`.

`--json` gives each case with `file` and `operation` as separate fields, so
issue 72 (parallel reading, writing and MCP) can put its numbers next to
these single-thread ones, file by file. Everything here runs on one thread.

`bench list` ends with the operations that have no case yet for each file.
Today that is every save except CSV (no .xls or Numbers writer yet) and
every recalculation (the kernel, issue 45, is not merged).

## Inputs

- **Committed samples**: the crates' own `tests/data/` files, read as one
  set per format (`csv-samples`, `xls-samples`, `numbers-samples`,
  `vba-samples`, `crypto-samples`).
- **Generated files**, made from a fixed seed the first time they are
  needed and kept in `target/bench-inputs/` (never committed):
  - `csv-100mb`: 100 MB of typical data, about 880,000 rows by 8 columns;
  - `csv-wide`: 256 rows by 16,384 columns;
  - `csv-gbk-20mb`: 20 MB of Chinese text in GBK;
  - `xls-1m-cells`: a BIFF8 sheet of 65,536 rows by 16 columns (1,048,576
    cells, 262,144 formulas), written record by record by the bench, since
    the biff crate only reads;
  - `xls-1m-cells-rc4`: the same workbook stream encrypted with RC4
    CryptoAPI. Encryption draws a fresh salt, so this file differs on
    every machine, and its hash is not compared.
- **Hostile probes** from the reviews, rebuilt in code (the review folders
  are scratch): doubled quotes and line breaks in every field, 9 million
  empty cells, 3 million unknown `.xls` records, and the VBA source that
  expands 24 KB to 16 MiB (issue 8).

The file names carry the seed and a generator version (`-s58-v1`). A
changed generator must bump `VERSION` in `src/generate.rs`, or `compare`
fails with `INPUT CHANGED` because the input's hash no longer matches.

## How compare decides

1. Each case runs once (N timed runs in one child process).
2. **Time**: the median is scaled by the machine's speed, then compared
   with the baseline's median. The machine's speed is a fixed reference
   workload (formatting numbers, allocating small strings, hashing), timed
   in its own process just before and just after the case. If the machine
   is 30% slower now than when the baseline was taken (another job is
   running, the laptop is warm), every case is about 30% slower too, and
   the scaling takes that out. The `machine` column shows that factor.
3. **Memory**: peak memory against the baseline's. A difference under
   256 KiB is allocator noise and never fails.
4. A case more than 10% over on either runs 3 more times, each in a new
   process. It fails only if the best of the 4 is still more than 10% over.
5. The summary lists every failing case and by how much. A case in the
   baseline that no longer exists also fails; a new case without a
   baseline is only noted.

The baseline itself pools 3 measurements per case (15 timed runs in 3
processes), so it records a typical time, not one process's luck.

## When it fails

If the change is a mistake, fix it. If it is expected (a new feature that
costs time, a safety limit that costs memory), run `bench update`, commit
`baseline.json` with the change, and say in the commit message which cases
moved, by how much, and why that is accepted. `update` prints that
reminder. `baseline.json` has one case per line, so the diff shows what
moved.

## Noise: the machine is shared

The baseline was measured on the shared development machine (WSL, 8
virtual CPUs of a hybrid laptop CPU, 4 GB): other agents' builds and tests
run there at the same time, the CPU slows when warm, and its fast and slow
cores give the same code different times. Run to run, a case moves by 5%
to 20% with no change at all, and the whole machine can be 30% to 50%
slower for minutes. The reference scaling and the reruns take most of that
out, but not all of it: treat a failure of 10% to 15% on one or two
unrelated cases as a reason to run `compare` again, not as proof. Changes
of 25% or more, or a failure in every case of one area, are real.

- Run it under `~/bin/heavy` so it does not overlap a build.
- A baseline only means something on the machine that measured it.
  `compare` warns when the machine (CPU model, CPU count, memory) differs.
- Peak memory is far steadier than time; a memory failure is almost always
  real.
- Generated inputs fit the machine: the largest case (`csv-100mb/save`)
  peaks at about 650 MiB, under the 1 GB budget per case.

## In CI, once the repository is public

The repository is private, so there is no workflow yet (no GitHub Actions
while private). When it is public, the plan is:

- A job on a **dedicated runner** (one machine with nothing else on it, the
  same one every time), not GitHub's shared runners, whose times vary too
  much for a 10% line.
- `bench update` once on that runner, committed as its baseline (the
  `machine` field records where it came from).
- On every pull request: `cargo build --release -p carrysheet-bench` and
  `target/release/bench compare --json`. Exit 1 fails the check; the JSON
  (`cases[].time_change`, `memory_change`, `machine_change`, `slower`,
  `hungrier`) goes into the job summary.
- A pull request that changes `baseline.json` must say why in its
  description, as the commit message does.

Locally, `node scripts/check.mjs --bench` will run `compare` (issue 57
adds the flag).

## Adding cases

A case is a `Case` in `src/cases/`: a name (`file/operation`), an
`Operation`, an input (committed folders, a generated file, or none), how
many times the operation repeats per timed run, and a `setup` function that
gets the input's bytes, does the untimed part (such as opening the file
before timing its save) and returns the timed operation.

To add an area, such as the kernel's open, read a range, edit one cell plus
recalculate and save: add `src/cases/kernel.rs` with a `cases()` function
like the others, list it in `all()` in `src/cases/mod.rs`, and run
`bench update --filter kernel` to add its numbers to the baseline without
touching the others.
