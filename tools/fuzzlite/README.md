# fuzzlite

A mutation fuzzer for Carrysheet's file readers that runs on stable Rust
with no extra tools (no nightly, no `cargo install`). It looks for inputs
that make a reader panic, abort, hang, run out of memory, or do far more
work than the input size explains, and hands back a minimised file.

## Run it

```sh
cargo build --profile fuzz -p fuzzlite
nice -n 10 target/fuzz/fuzzlite list
nice -n 10 target/fuzz/fuzzlite run --target biff --seconds 600 --jobs 1
```

The `fuzz` profile is release with integer overflow checks on, so an
overflow becomes a panic finding. Debug builds are slow enough to look
like blow-ups. The machine is shared: `--jobs` defaults to 1 and is capped
at half the cores, and the default memory limit per case is half the
machine's memory split over the jobs (at most 1 GiB).

Each finding prints as one JSON line on stdout (progress goes to stderr):

```json
{"target":"biff","kind":"timeout","detail":"still running after 5000 ms","family":"biff:continue_chain","ops":["biff:continue_chain","bytes:flip"],"seed_file":"crates/carrysheet-biff/tests/data/SampleSS.xls","rng_seed":42,"case":1234,"original_bytes":30208,"input_bytes":2210,"millis":5000,"peak_mib":12,"file":"~/.cache/carrysheet-fuzz/biff/resource-0123456789abcdef.bin"}
```

`kind` is one of `panic`, `caught_panic` (a panic that a reader's own
`catch_unwind` guard caught, outside the `cfb` crate whose panics the
guards are for), `abort` (including stack overflow), `timeout`,
`memory_limit` and `amplification` (finished, but took over 2 s or 256 MiB
for an input no bigger than `--max-len`, 1 MiB by default). Everything
fuzzlite writes as text (stdout, stderr and the `.json` files) goes through
one writer that shows the home folder as `~`, in its symlinked and real
forms and, on Windows, in any case and slash style. Each finding is written to
`~/.cache/carrysheet-fuzz/<target>/` (or `$XDG_CACHE_HOME`, or
`%LOCALAPPDATA%` on Windows) as `<id>.bin` (minimised), `<id>.orig.bin`
and `<id>.json`, where `<id>` is the group (`panic`, `caught_panic`, `abort`, or
`resource` for time and memory) and a hash of the input. fuzzlite refuses an output folder inside the repository
or any other git work tree.

Check a file again, or a fix:

```sh
target/fuzz/fuzzlite repro --target biff ~/.cache/carrysheet-fuzz/biff/resource-0123456789abcdef.bin
```

`--seed N` repeats a run case for case (with `--jobs 1`). `fuzzlite help`
lists every option. Exit code: 0 nothing found, 1 findings or failure,
2 usage.

## How it works

- **Targets** (`src/targets/`): one module per reader, each a plain
  `fn run(data: &[u8])` that calls the public API with default limits and
  ignores the result. `csv`, `biff` (BIFF2 to BIFF8), `vba` (`from_bin`),
  `vba-xls` (`from_xls`) and `vba-decompress`. The same functions can back
  `cargo fuzz` targets later. To add a reader, copy `targets/csv.rs`, add
  its name to the `targets!` list in `targets/mod.rs`, and add the crate to
  `Cargo.toml`.
- **Seeds**: the committed files in each crate's `tests/data/`, read in
  place, plus `--seeds PATH`. Every seed runs once unmutated first.
- **Mutations** (`src/mutate/`): bit flips, interesting bytes and
  integers, insert, delete, duplicate (many times), copy, truncate,
  splice; and format-aware ones:
  - BIFF: record lengths, CONTINUE chains, SST counts, shared-formula,
    array and table ranges, ptgExp and deep formulas, repeated records and
    BOUNDSHEETs, NAME loops, FILEPASS, nested BOFs. The workbook stream is
    taken out of the compound file, changed, and written back, with
    BOUNDSHEET offsets moved to match.
  - OLE2 (`.xls`, `vbaProject.bin`): FAT chains (cycles, special values),
    header fields, directory sizes, tree links and entries, the mini FAT
    and mini stream, shared sectors.
  - VBA: `dir` stream records (sizes, counts, MODULEOFFSET, code page,
    repeated modules), chunk headers and copy tokens, decompression bombs,
    `PROJECT` lines.
  - CSV: huge quoted fields, many delimiters and rows, BOMs, UTF-16,
    legacy and invalid bytes, `sep=` lines, quote storms.
- **Child processes** (`src/run/child.rs`): each case runs in
  `fuzzlite __child <target>`, input on stdin, with a wall-clock limit
  (`--timeout-ms`, default 5000) and a memory limit (`--memory-mib`,
  default 1024). On Linux the memory limit is RLIMIT_AS, set through
  `/bin/sh -c 'ulimit -v'` so no `unsafe` code or new crate is needed, and
  the parent also kills a child whose resident memory passes it. On other
  systems only the time limit applies. Time and peak memory come from the
  child itself (`/proc/self/status` on Linux). The child also gets the
  time limit and exits by itself (code 124) two seconds after it, so a
  child never outlives a parent that was stopped.
- **Findings** are confirmed by a second run (a loaded machine can make
  one case slow), minimised by delta reduction (`--minimize-runs`,
  `--minimize-seconds`), and kept once per signature: a panic location,
  an abort reason, or, for time and memory, the set of format-aware
  mutations used. Panics and aborts are never capped; time and memory
  findings stop at `--max-resource-findings` (old name `--max-per-kind`),
  and the summary counts repeats, and the signatures and cases dropped at
  the cap, separately.
