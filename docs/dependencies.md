# Dependencies and release builds

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
| `cfb` 0.15.0 (MIT) | `carrysheet-vba` tests (`tests/keep.rs`) | An independent compound file reader, to check that what `carrysheet-cfb` writes opens in someone else's reader. Issue 42 replaced `cfb` with `carrysheet-cfb` in every library; this is the one place it stays, on purpose. |

## Policy check: cargo-deny

`deny.toml` sets which licenses, sources and versions we accept, and why.
Run `cargo deny check` before a release and after any `Cargo.lock` change
(install once: `cargo install --locked cargo-deny`).

## Release build check

`node scripts/release-check.mjs <target>` builds `carrysheet-cli` for that
target with build-machine paths remapped (release builds only), then checks
the binary for the home folder, user name, email addresses, `/home/` and
`C:\Users\`, and prints its size. Findings show only kind and offset. Run it
for every target before publishing a build.

## SBOM at release time

Make a CycloneDX bill of materials for each release and attach it to the
release, not the repository:

    cargo cyclonedx --format json --all-features --target <target>

(install once: `cargo install --locked cargo-cyclonedx`). It writes a
`*.cdx.json` next to each crate's `Cargo.toml`; move them into the release
folder.
