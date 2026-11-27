Changed

`edit` without `output` writes `name-edited.xlsx` beside the original, as before, but no longer tries `name-edited-2.xlsx` and on up to `-99` when that name is taken. It refuses with `file_exists` instead: pass `output` with another name, or `output` and `overwrite` with `confirm` to replace it. The numbered names stay in the history of `main`: `4defaae` has them (`crates/carrysheet/src/save.rs`).
