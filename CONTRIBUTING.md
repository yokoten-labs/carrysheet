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

## AI-assisted changes

AI-assisted changes are welcome. Review what the tool wrote before you send it, and say which tool you used with a trailer at the end of each commit message:

```
Co-Authored-By: <agent and model> <noreply address>
```

For example: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. This is our record of where code came from.
