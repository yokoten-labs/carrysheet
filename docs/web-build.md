# Building for the browser

The web converter and the web trial run our libraries in the browser as
WebAssembly (target `wasm32-unknown-unknown`).

## What builds

Every library in `crates/` builds: carrysheet-model, carrysheet-csv,
carrysheet-commands, carrysheet-cfb, carrysheet-biff, carrysheet-vba,
carrysheet-crypto and carrysheet-numbers. carrysheet-cli does
not, and does not need to: it is a terminal program with an MCP server.

    rustup target add wasm32-unknown-unknown   # once
    cargo wasm-check

`cargo wasm-check` is an alias in `.cargo/config.toml`. It builds the whole
workspace except the programs. A new library is checked with no change; a new
program must be added to the alias's `--exclude` list.

`cargo build --workspace --target wasm32-unknown-unknown` fails: rmcp, used
only by carrysheet-cli, turns on uuid's random UUIDs, and uuid has no source
of random numbers there. The libraries need no uuid: carrysheet-cfb reads
and writes class IDs as plain bytes and never makes a random one.

## Known limits

- **A panic stops the module.** On this target a panic aborts. The readers
  return errors on damaged files instead of panicking (compound files go
  through carrysheet-cfb, which needs no `catch_unwind`), but run parsing in
  a Web Worker anyway and treat a dead worker as a bug to report.
- **No files.** `std::fs` calls return an error, so `Root::new` in
  carrysheet-commands fails. Pass bytes in and get bytes out
  (`carrysheet_biff::read`, `carrysheet_csv::read`, `read_xls_vba`).
- **Run it through wasm-bindgen.** carrysheet-crypto asks the browser for
  random bytes (getrandom's `wasm_js` backend) when it encrypts, so a
  `.wasm` that includes it imports wasm-bindgen functions. Process it with
  wasm-bindgen (or wasm-pack) before a page loads it. carrysheet-cfb never
  asks for the time: new storages get zero times.
- **One thread.** Nothing in the libraries starts a thread.
