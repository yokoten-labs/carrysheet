# Change files

Each pull request that changes what users notice adds one file here, named
`changes/<issue>-<slug>.md`. Two pull requests then never conflict on `CHANGELOG.md`.

Line 1 is the section: Added, Changed, Deprecated, Removed, Fixed, Security or Internal.
Line 2 is blank. From line 3, write the entry as it should read in the changelog:

```markdown
Added

A dark mode for the settings page (#42).
```

`node .yokoten/bin/yokoten.ts changelog` shows what the files add. Before a release,
`node .yokoten/bin/yokoten.ts changelog --write` moves them into `CHANGELOG.md` and deletes them.
