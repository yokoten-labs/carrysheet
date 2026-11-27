# Contributing

Thanks for helping. Carrysheet is an early preview, so things move fast. A short issue before a big change saves us both time.

## What is here

Two crates: the library `carrysheet` in `crates/carrysheet` (MIT OR Apache-2.0) and the program `carrysheet-cli` in `apps/carrysheet-cli` (GPL-3.0-or-later), on the vendored GridCraft kernel in `vendor/gridcraft/`. Our own source is capped at 5,000 lines ([record 0010](docs/records/0010-five-thousand-lines.md)); a change that adds code may need to remove some.

## The rules

All the rules are in [AGENTS.md](AGENTS.md). They apply to people and AI agents alike. The most important ones:

- **Clean room.** Never copy or closely follow code from GPL, LGPL, AGPL or MPL projects (LibreOffice, Gnumeric and others). Use Microsoft's published specs and permissively licensed code instead.
- **Checks.** Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` while you work, and `node scripts/lines.mjs` for the line cap.
- **Commits.** One logical change per commit, with a message in English that says what changed and why.

## Pull requests

- CI runs every check on each pull request. A pull request is merged only when CI is green.
- Contributions are accepted under the license of the folder you change. See [LICENSING.md](LICENSING.md).
- Security problems go through private reporting, not pull requests or issues. See [SECURITY.md](SECURITY.md).

## Versions and releases

- **One version.** Both crates take the version in the root `Cargo.toml` (`version.workspace = true`). The program depends on the library through `[workspace.dependencies]`, with the same version.
- **SemVer, 0.x for now.** We stay on 0.x until a real `.xlsx` opens, is edited and saves with nothing lost. On 0.x, a minor bump (0.1 to 0.2) breaks something, and a patch bump (0.1.0 to 0.1.1) does not. The public interface is the commands, arguments, JSON output and error codes.
- **Changelog.** A pull request that changes what people or scripts notice adds a file in [`changes/`](changes/README.md). Before a release, `yokoten changelog --write` moves the files into `CHANGELOG.md`. Then rename `## Unreleased` to the version and date, such as `## v0.1.0 - 2026-12-01`, and start a new empty `## Unreleased` above it.
- **Releases come from tags.** Only the owner creates a tag. Tag `vX.Y.Z` on `main` runs the release workflow. It checks the tag against the workspace version (`cargo metadata`), builds for Windows, Linux and macOS with remapped paths, and runs the privacy scan. It writes the third-party notices with `cargo about` (`about.toml`), and attaches archives with `SHA256SUMS` to a draft release. The owner publishes the draft.
- **A published crate carries its license files** next to its `Cargo.toml`, because `cargo package` packs only the crate's folder. They are copies, not links: on Windows a link checks out as a short text file. Check with `cargo package --locked --list -p carrysheet`.

## AI-assisted changes

AI-assisted changes are welcome. Review what the tool wrote before you send it, and say which tool you used with a trailer at the end of each commit message:

```
Co-Authored-By: <agent and model> <noreply address>
```

For example: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. This is our record of where code came from.
