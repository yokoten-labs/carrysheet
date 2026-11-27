Changed

The command line follows more of clig.dev (#150). Help for `read`, `convert` and `edit` has an example, and global flags sit under "Global options". The root help links to the source and the issues page. A usage error with no tip says to run `carrysheet-cli --help`. The text form no longer prints empty `key:` lines for null fields; `--json` is unchanged. Panics in any command are logged with hidden characters escaped. The README has install steps and a quick start.
