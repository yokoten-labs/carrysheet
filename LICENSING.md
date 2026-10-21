# Licensing

Carrysheet uses two licenses in one repository. Each folder says which one applies.

| Folder | What's in it | License |
|---|---|---|
| `crates/` | Libraries: reading and writing spreadsheet files (old formats included), formulas, the command layer | MIT OR Apache-2.0, at your choice |
| `apps/` | Finished programs: `carrysheet-cli` and its MCP server, the desktop app, the web trial page, and all Pro features | GPL-3.0-or-later |
| `tools/` | Developer tools: `fuzzlite` (the fuzzer for the file readers) and `bench` | MIT OR Apache-2.0, at your choice |
| `vendor/` | Code copied from other projects, such as the GridCraft kernel (record 0003) | Its own license, kept next to it with its NOTICE. GridCraft: MIT OR Apache-2.0 |
| everything else | Docs, research, scripts, tests | MIT OR Apache-2.0, at your choice |

Every crate also states its license in the `license` field of its `Cargo.toml`. If a file says otherwise in its header, the header wins. A crate that can be published to crates.io carries copies of its license files in its own folder (`LICENSE-MIT` and `LICENSE-APACHE`, or `LICENSE-GPL`), so the text travels with the package; `scripts/release-meta.mjs` checks this in CI.

**Why two licenses.** We want the libraries to be easy to use anywhere, including in closed-source products. The finished programs, Pro features included, stay open: anyone who ships a changed version must share its source under the GPL.

**Pro features** are in this repository like everything else. A paid license key unlocks them in the official builds. You may build and use the code yourself under the GPL.

**Your files and results are yours.** These licenses cover Carrysheet itself, not what you make with it. Spreadsheets, exports and answers that Carrysheet produces for you belong to you, and the GPL does not reach them.

**Contributions** are accepted under the license of the folder you change (inbound = outbound). There is no contributor agreement to sign.

**The name and logo** are not covered by any of these licenses. See [TRADEMARKS.md](TRADEMARKS.md).

## Third-party material

Material from other projects keeps its own license. Each notice file says what was taken, from where, and under which license:

| Notice | What it covers |
|---|---|
| [`crates/carrysheet-biff/NOTICE`](crates/carrysheet-biff/NOTICE) | The formula function table generated from Apache POI (Apache-2.0), with POI's NOTICE text |
| [`crates/carrysheet-biff/tests/data/NOTICE`](crates/carrysheet-biff/tests/data/NOTICE) | Test files from Apache POI (Apache-2.0) and calamine (MIT, with its `calamine/LICENSE-MIT`) |
| [`crates/carrysheet-cfb/tests/data/README`](crates/carrysheet-cfb/tests/data/README) | Test files we made with the `cfb` crate (MIT) |
| [`crates/carrysheet-crypto/tests/data/NOTICE`](crates/carrysheet-crypto/tests/data/NOTICE) | Test files from Apache POI (Apache-2.0) |
| [`crates/carrysheet-kernel/tests/data/NOTICE`](crates/carrysheet-kernel/tests/data/NOTICE) | Test files from Apache POI (Apache-2.0) |
| [`crates/carrysheet-numbers/THIRD-PARTY.md`](crates/carrysheet-numbers/THIRD-PARTY.md) | Protobuf definitions, the function table and test files from numbers-parser (MIT) |
| [`crates/carrysheet-vba/tests/data/NOTICE`](crates/carrysheet-vba/tests/data/NOTICE) | Test files from Apache POI (Apache-2.0) |
| [`vendor/gridcraft/NOTICE`](vendor/gridcraft/NOTICE) | The GridCraft kernel (MIT OR Apache-2.0), with its `LICENSE-MIT` and `LICENSE-APACHE` |

**In the release archives.** Each archive of `carrysheet-cli` holds `THIRD-PARTY-NOTICES.txt`, written at release time by `scripts/notices.mjs` (with [cargo-about](https://github.com/EmbarkStudios/cargo-about), configured in `about.toml`). It has the license text of every crate the program ships, their NOTICE files, the GridCraft notice and licenses, and the notices of our own crates in the program. The release fails when a shipped crate's license text is missing. The archive also holds `LICENSE-GPL` (the program's license), `LICENSE-MIT`, `LICENSE-APACHE` and this file.

License texts: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-GPL](LICENSE-GPL).
