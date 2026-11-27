Removed

The answer of `edit` no longer has the `recalculated` and `recalculated_total` fields, which listed formula cells whose result changed because of the edit (record 0011). `changes` still lists every cell the operations wrote, with a new formula's value. To see a formula elsewhere after an edit, `read` it, or set it in a free cell with `edit` and `dry_run`. The list stays in the history of `main`: `4defaae` has it (`crates/carrysheet/src/edit.rs`).
