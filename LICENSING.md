# Licensing

Carrysheet uses two licenses in one repository. Each folder says which one applies.

| Folder | What's in it | License |
|---|---|---|
| `crates/` | Libraries: reading and writing spreadsheet files (old formats included), formulas, the command layer | MIT OR Apache-2.0, at your choice |
| `apps/` | Finished programs: `carrysheet-cli` and its MCP server, the desktop app, the web trial page, and all Pro features | GPL-3.0-or-later |
| `vendor/` | Code copied from other projects, such as the GridCraft kernel (record 0003) | Its own license, kept next to it with its NOTICE. GridCraft: MIT OR Apache-2.0 |
| everything else | Docs, research, scripts, tests | MIT OR Apache-2.0, at your choice |

Every crate also states its license in the `license` field of its `Cargo.toml`. If a file says otherwise in its header, the header wins.

Third-party test data keeps its own license, noted in a `NOTICE` file next to it: for example, Apache POI samples are under Apache-2.0 and calamine samples under MIT (`crates/carrysheet-biff/tests/data/NOTICE`).

**Why two licenses.** We want the libraries to be easy to use anywhere, including in closed-source products. The finished programs, Pro features included, stay open: anyone who ships a changed version must share its source under the GPL.

**Pro features** are in this repository like everything else. A paid license key unlocks them in the official builds. You may build and use the code yourself under the GPL.

**Your files and results are yours.** These licenses cover Carrysheet itself, not what you make with it. Spreadsheets, exports and answers that Carrysheet produces for you belong to you, and the GPL does not reach them.

**Contributions** are accepted under the license of the folder you change (inbound = outbound). There is no contributor agreement to sign.

**The name and logo** are not covered by any of these licenses. See [TRADEMARKS.md](TRADEMARKS.md).

License texts: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-GPL](LICENSE-GPL).
