Removed

The file readers that no program links: `carrysheet-biff`, `carrysheet-csv`, `carrysheet-crypto`, `carrysheet-numbers`, `carrysheet-connections` and `carrysheet-query`, with `tools/bench`, `tools/fuzzlite`, `tools/excel-ground-truth` and the optional `query` feature of `carrysheet-cli`. Their sample files moved to `crates/carrysheet-kernel/tests/data/formats/`. The code stays in the history of `main`: `364adbb` is the last commit that changed it, and every commit before this change still has it. `carrysheet-cli` behaves as before (record 0010, decision 4, step 1).
