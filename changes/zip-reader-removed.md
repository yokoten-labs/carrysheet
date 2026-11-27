Removed

The crate `carrysheet-zip`, the hardened zip reader, and its `zip-inflaters` CI step. No code used it any more: GridCraft and the save-loss report both read packages with the `zip` crate, and both cap what a part may unpack (issue 80). `carrysheet-cli` behaves as before. The crate stays in the history of `main`: the last commit that holds it is `895dee5` (`crates/carrysheet-zip`), as does every commit before this change (record 0010: decision 4 lists the zip crate among what leaves with `macros`, and decision 2 keeps two crates, `carrysheet` and `carrysheet-cli`).
