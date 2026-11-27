Changed

A range whose sheet name breaks Excel's rules, such as `'a:b'!A1` or `'History'!A1`, now fails with `no_such_sheet` instead of `invalid_reference`, because no sheet has that name. The exit code is now 1, where it was 2. The error code `not_implemented` is gone; no command returned it. The removed check stays in the history of `main`: `4defaae` has it (`crates/carrysheet/src/range.rs`).
