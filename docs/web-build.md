# Building for the browser

The web converter and the web trial run our libraries in the browser as
WebAssembly (target `wasm32-unknown-unknown`).

## What builds

Every library in `crates/` builds: carrysheet and carrysheet-zip.
carrysheet-cli does not, and does not need to: it is a terminal program with an MCP server.

    rustup target add wasm32-unknown-unknown   # once
    cargo wasm-check

`cargo wasm-check` is an alias in `.cargo/config.toml`. It builds the whole
workspace except the programs. A new library is checked with no change; a new
program must be added to the alias's `--exclude` list.

`cargo build --workspace --target wasm32-unknown-unknown` fails: rmcp, used
only by carrysheet-cli, turns on uuid's random UUIDs, and uuid has no source
of random numbers there. The libraries need no uuid.

## Known limits

- **A panic stops the module.** On this target a panic aborts, so the
  kernel's `catch_unwind` around GridCraft does not help there. Run parsing
  in a Web Worker and treat a dead worker as a bug to report.
- **No files.** `std::fs` calls return an error, so `Root::new` in
  `carrysheet::commands` fails. Pass bytes in and get bytes out
  (`Kernel::open_bytes`).
- **One thread.** Nothing in the libraries starts a thread.
