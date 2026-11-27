# Licensing

Carrysheet uses two licenses in one repository, for its two crates: the library `carrysheet` and the program `carrysheet-cli`. Each folder says which one applies.

| Folder | What's in it | License |
|---|---|---|
| `crates/` | The library `carrysheet`: the commands, on the vendored GridCraft kernel | MIT OR Apache-2.0, at your choice |
| `apps/` | Finished programs: `carrysheet-cli` and its MCP server today; later the desktop app, the web trial page, and all Pro features | GPL-3.0-or-later |
| `vendor/` | Code copied from other projects, such as the GridCraft kernel (record 0003) | Its own license, kept next to it with its NOTICE. GridCraft: MIT OR Apache-2.0 |
| everything else | Docs, research, scripts, tests | MIT OR Apache-2.0, at your choice |

Every crate also states its license in the `license` field of its `Cargo.toml`. If a file says otherwise in its header, the header wins. Each crate carries copies of its license files in its own folder (`LICENSE-MIT` and `LICENSE-APACHE` in `crates/carrysheet`, `LICENSE-GPL` in `apps/carrysheet-cli`), so the text travels with the package.

Third-party test data keeps its own license, noted in a `NOTICE` file next to it: for example, Apache POI samples are under Apache-2.0 and calamine samples under MIT (`crates/carrysheet/tests/data/formats/biff/NOTICE`).

**Why two licenses.** We want the libraries to be easy to use anywhere, including in closed-source products. The finished programs, Pro features included, stay open: anyone who ships a changed version must share its source under the GPL.

**Pro features** are in this repository like everything else. A paid license key unlocks them in the official builds. You may build and use the code yourself under the GPL.

**Your files and results are yours.** These licenses cover Carrysheet itself, not what you make with it. Spreadsheets, exports and answers that Carrysheet produces for you belong to you, and the GPL does not reach them.

**Contributions** are accepted under the license of the folder you change (inbound = outbound). There is no contributor agreement to sign.

**The name and logo** are not covered by any of these licenses. See [TRADEMARKS.md](TRADEMARKS.md).

**In the release archives.** Each archive of `carrysheet-cli` holds `THIRD-PARTY-NOTICES.txt`, written at release time by [cargo-about](https://github.com/EmbarkStudios/cargo-about) with `about.toml`. It has the license text of every crate the program ships. The release fails when a shipped crate's license is not accepted or cannot be found. The archive also holds `LICENSE-GPL` (the program's license), `LICENSE-MIT`, `LICENSE-APACHE`, this file, and GridCraft's `NOTICE`.

License texts: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-GPL](LICENSE-GPL).
