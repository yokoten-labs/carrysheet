# Contributing

Thanks for helping. Carrysheet is an early preview, so things move fast. A short issue before a big change saves us both time.

## The rules

All the rules are in [AGENTS.md](AGENTS.md). They apply to people and AI agents alike. The most important ones:

- **Clean room.** Never copy or closely follow code from GPL, LGPL, AGPL or MPL projects (LibreOffice, Gnumeric and others). Use Microsoft's published specs and permissively licensed code instead.
- **Checks.** Run `node scripts/check.mjs --quick` while you work. It checks the crates you changed.
- **Commits.** One logical change per commit, with a message in English that says what changed and why.

## Pull requests

- CI runs every check on each pull request. A pull request is merged only when CI is green.
- Contributions are accepted under the license of the folder you change. See [LICENSING.md](LICENSING.md).
- Security problems go through private reporting, not pull requests or issues. See [SECURITY.md](SECURITY.md).

## Versions and releases

- **One version.** Every crate takes the version in the root `Cargo.toml` (`version.workspace = true`), and depends on the others through `[workspace.dependencies]`. `node scripts/release-meta.mjs` checks this (check.mjs runs it first).
- **SemVer, 0.x for now.** We stay on 0.x until the README's 1.0 bar is met: a real `.xlsx` opens, is edited and saves with nothing lost. On 0.x, a minor bump (0.1 to 0.2) means a breaking change, and a patch bump (0.1.0 to 0.1.1) is compatible. The public interface is the commands, arguments, JSON output and error codes.
- **Changelog.** Add what people or scripts notice to the `## Unreleased` section of [CHANGELOG.md](CHANGELOG.md), with the PR number. It says how a release renames that section.
- **Releases come from tags.** Pushing tag `vX.Y.Z` runs `.github/workflows/release.yml`: it checks the tag against the workspace version, builds and checks the archives, writes checksums and the third-party notices, and creates a draft release for the owner to publish. To try it without publishing, run it by hand: `gh workflow run release.yml --ref <branch>`.
- **A crate that can be published** (no `publish = false`) keeps copies of its license files next to its `Cargo.toml`, since `cargo package` packs only the crate's folder. Copies, not symbolic links: on Windows a link checks out as a short text file.

## AI-assisted changes

AI-assisted changes are welcome. Review what the tool wrote before you send it, and say which tool you used with a trailer at the end of each commit message:

```
Co-Authored-By: <agent and model> <noreply address>
```

For example: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. This is our record of where code came from.
