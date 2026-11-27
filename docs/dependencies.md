# Dependencies and release builds

## What we build on

Two crates of ours (record 0010, decision 6, says why each part is borrowed):

| Crate | Depends on |
|---|---|
| `carrysheet` (MIT OR Apache-2.0) | the vendored GridCraft (`vendor/gridcraft/`), the only reader of file formats; `serde`, `serde_json`; `schemars` for each command's schema; `clap` derive, behind the `cli` feature; `zip`, pinned to GridCraft's version, for the save-loss report; `encoding_rs` and `chardetng` for csv encodings |
| `carrysheet-cli` (GPL-3.0-or-later) | `carrysheet`, `clap`, `serde`, `serde_json`; `rmcp` with only its stdio transport; `tokio` |

No network crate: `deny.toml` bans HTTP and TLS crates, and `rmcp` builds without its HTTP transports. `cap-std` is not used; `apps/carrysheet-cli/src/files.rs` walks paths below `--root` itself.

## Taking in a crate version: wait 7 days

Don't take a crate version that is less than 7 days old, new or as an
update. Most bad releases (broken, yanked, or compromised) are found in the
first days. Example: tokio 1.54.0 landed on a branch one day after release.

Before you commit a changed `Cargo.lock`:

1. See what changes: `cargo update --dry-run` (or `git diff Cargo.lock`).
2. Check each new version's release date:
   `curl -s https://crates.io/api/v1/crates/<name>/<version> | grep -o '"created_at":"[^"]*"'`
   (or the version list on `https://crates.io/crates/<name>/versions`).
3. Too new? Hold it back: `cargo update -p <name> --precise <older-version>`.
   `cargo tree -i <name>` shows which crate pulls it in.

A security fix may skip the wait; say so in the PR.

## Test-only dependencies

A crate in `[dev-dependencies]` is built for tests only: it is not in any
library or program we ship, and no library depends on it. Each one is
listed here with why.

| Crate | Used by | Why |
|---|---|---|
| `flate2` 1.1.10 (MIT OR Apache-2.0) | `carrysheet` tests | Builds deflate streams for the hostile-file tests. The same version GridCraft ships, so it adds nothing to the graph. |

## Policy check: cargo-deny

`deny.toml` sets which licenses, sources and versions we accept, and why.
Run `cargo deny check` before a release and after any `Cargo.lock` change
(install once: `cargo install --locked cargo-deny`).

## Release build check

The `release-scan` job in `.github/workflows/ci.yml` builds `carrysheet-cli` in
release mode with build-machine paths remapped (`--remap-path-prefix`), then
searches the binary for the home folder, `/home/`, the user name and any email
address outside `carrysheet.com`. Any hit fails the job. Run the same steps by hand before publishing a build.

## SBOM at release time

Make a CycloneDX bill of materials for each release and attach it to the
release, not the repository:

    cargo cyclonedx --format json --all-features --target <target>

(install once: `cargo install --locked cargo-cyclonedx`). It writes a
`*.cdx.json` next to each crate's `Cargo.toml`; move them into the release
folder.
