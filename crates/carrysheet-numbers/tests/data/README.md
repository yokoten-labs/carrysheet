# Test documents

All documents come from numbers-parser's `tests/data/`
(<https://github.com/masaccio/numbers-parser>, commit
`a6affe5599c8373f575192ac5777d0c6426da696`), MIT License, Copyright 2021
Jon Connell (full text in `../../THIRD-PARTY.md`).

They were made smaller: previews and images were removed and the zip was
written again. Except for `cells/test-1.numbers`, which keeps every
`Index/` part, the style sheet, view state and package metadata archives
were removed too; this reader does not use them. The values the tests
expect come from numbers-parser's own tests, named in each test.

| File | Saved by Numbers (file format) | Used for |
|---|---|---|
| `cells/test-1.numbers` | 11.0 (11.0.3) | sheets, tables, text, empty cells |
| `cells/test-4.numbers` | 11.2 (11.2.9) | number, date, duration, bool values |
| `cells/test-empty-rows.numbers` | 11.1 (11.1.2) | empty rows |
| `formulas/test-10.numbers` | 14.3 (14.2.2) | formula text |
| `formulas/issue-42.numbers` | 14.3 (14.2.2) | `#REF!`, error cells |
| `formulas/test-issue-253.numbers` | 15.4 (26.4.0) | large number literals |
| `merges/test-9.numbers` | 11.1 (11.1.2) | merged ranges |
| `merges/issue-18.numbers` | unknown (empty metadata) | merge region map, missing version |
| `packages/test-5.zip` | 11.0 (11.0.3) | package folder with an `Index/` folder, zipped for the repository |
| `packages/test-7.zip` | 11.0 (11.0.3) | package folder with `Index.zip`, zipped for the repository |
| `unsupported/pre-bnc.numbers` | 13.1 (13.1.2) | older cell storage, reported as unsupported |
| `unsupported/encrypted.numbers` | (26.3.1) | password protection, reported |

The package folders are kept zipped because a folder of many small files
would not fit the repository's layout rules; the tests unpack them into a
temporary folder.
