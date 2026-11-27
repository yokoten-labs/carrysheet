Removed

The save report of `edit` and `convert` no longer has a `parts` field with counts of identical, rewritten, dropped and added package parts (record 0011). What a save loses is still reported: `lost` by feature, `dropped_parts` by name, and `notes` for parts Excel rebuilds. The counts stay in the history of `main`: `4defaae` has them (`crates/carrysheet/src/save.rs`, `crates/carrysheet/src/save/loss.rs`).
