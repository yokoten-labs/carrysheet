# Excel ground truth

Our formula engine has to give the same answers as Excel, including Excel's odd ones. Only real Excel can say what those answers are. These scripts build small test workbooks in Excel, let Excel calculate them, and save what Excel computed as JSON. The kernel tests compare our engine with that JSON.

GitHub's runners have no Excel, so this runs on a Windows PC with Excel installed, by hand, once per Excel update.

## Run it

You need:

- Windows 10 or 11 with desktop Excel. Microsoft 365 or Excel 2021 and later run every case; older Excel skips the dynamic-array cases and says so.
- Windows PowerShell 5.1 (built into Windows) or PowerShell 7.
- No unsaved work in Excel. The script stops if an open Excel has unsaved changes.

From the repository root, in a PowerShell or Command Prompt window:

```
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\excel-ground-truth\Invoke-GroundTruth.ps1
```

To see what it would do without starting Excel or writing anything, add `-WhatIf`. To run one topic, add `-Topic dates/leap-1900`.

When it finishes:

1. Read the summary. Lines that say "case note expects ..., Excel gave ..." are not failures: Excel is the truth, and those lines show a case note that is wrong or an Excel result worth a closer look.
2. Look at the changes with `git diff tools/excel-ground-truth/expected`.
3. Commit the `expected` folder.

### How long it takes

About 2 to 5 minutes for the roughly 400 cases on a normal PC. This is an estimate; it has not been timed yet. Excel's own start-up adds a few seconds.

## What it changes on your machine

- It writes only two folders, both inside this tool's folder: `out/workbooks/` (the test workbooks, ignored by git) and `expected/` (the results, which you commit). Nothing else is written.
- It starts its own hidden copy of Excel and closes it at the end, also when something fails. If that copy does not close within 15 seconds, the script ends that one process, never any other Excel.
- It does not change Excel's options: no separators, fonts, add-ins or Trust Center settings. Calculation mode and iteration are set only inside its own Excel session. Macros are forced off. Files are opened read-only and kept out of Excel's recent-files list.
- `-ExecutionPolicy Bypass` applies only to that one PowerShell window's command; it does not change the policy on your machine.
- Excel may still write its usual temporary and log files, as it does whenever it starts.

It refuses to start, and changes nothing, when:

- an open Excel has unsaved workbooks, is busy (a cell in edit mode or a dialog open), or runs more than once;
- a case file has a mistake (it lists every problem);
- the workbook or output folder is a drive root, your profile, Desktop or Documents, or already holds files of other kinds.

## The files

| File | What it does |
|---|---|
| `Invoke-GroundTruth.ps1` | The one command: runs the next two in order. |
| `Build-Workbooks.ps1` | Reads `cases/`, builds one workbook per case file with Excel (`.xlsx`, plus `.xls` where the case file asks), writes `out/workbooks/manifest.json`. |
| `Export-GroundTruth.ps1` | Opens each workbook read-only, runs a full recalculation (`CalculateFullRebuild`), writes `expected/<topic>.json`. |
| `GroundTruth.psm1` | Shared code: reading cases, writing JSON, starting and stopping Excel safely, removing private names and paths. |
| `cases/` | The test cases, one JSON file per topic. |

Each script also takes folder parameters (`-CasesPath`, `-WorkbookPath`, `-OutputPath`) and `-WhatIf`. Run `Get-Help .\tools\excel-ground-truth\Build-Workbooks.ps1 -Detailed` for details.

## Case files

One file per topic under `cases/<group>/<topic>.json`. The topic name is the path without `.json`, for example `dates/leap-1900`. Each case becomes its own worksheet (`case001`, `case002`, ...), so cell addresses in one case never collide with another.

```json
{
  "schema": 1,
  "title": "The 1900 leap-year bug",
  "formats": ["xlsx", "xls"],
  "settings": { "date1904": false, "iteration": false, "maxIterations": 100, "maxChange": 0.001 },
  "requires": [],
  "inputs": { "A1": 10 },
  "cases": [
    {
      "id": "leap1900.typed-feb-29",
      "note": "Why this is a trap, in one or two sentences.",
      "inputs": { "A1": { "typed": "1900-02-29" } },
      "formulas": { "B1": "=A1", "B2": { "f": "=A1", "format": "yyyy-mm-dd" } },
      "expect": { "B1": 60 },
      "volatile": false,
      "requires": ["dynamic-arrays"],
      "columnWidths": { "B": 3 }
    }
  ]
}
```

- `formats`: `xlsx` (default), and `xls` for cases that also make sense in the old BIFF8 format.
- `settings` (optional): the 1904 date system and iterative calculation, per workbook.
- `inputs` at the top level are put on every sheet of the file; a case's own `inputs` replace them cell by cell.
- An input is a number, `true`/`false`, text (always stored as text, so `"1"` stays text), `null` (blank), `{"value": ..., "format": "0.00"}`, or `{"typed": "..."}`. `typed` is entered exactly as if typed into the cell in US English, so Excel parses it: `"1900-02-29"` becomes a date, `"12345678901234567"` loses its last two digits, `"#N/A"` becomes an error value. Use `typed` for numbers whose exact bits matter, because JSON numbers go through PowerShell's parser first.
- A formula is a string (entered with `Formula2`, the dynamic-array way, falling back to `Formula` on older Excel) or `{"f": "...", "set": "formula" | "formula2" | "array", "format": "..."}`. `formula` enters it the pre-2019 way (implicit intersection); `array` enters a legacy Ctrl+Shift+Enter array formula, and its address may be a range like `C1:C3`.
- `expect` (optional) is what the case author believes Excel returns. It is not the truth: the export only prints where Excel disagrees. Errors are written as text, like `"#N/A"`.
- `volatile`: the value changes on every calculation (NOW, RAND, OFFSET, INDIRECT, CELL, INFO). The value is still recorded, but tests must not compare it.
- `requires: ["dynamic-arrays"]`: skipped, with a reason, on Excel without dynamic arrays.

## Output format

`expected/<topic>.json`, one per case file. UTF-8 without BOM, LF line ends, two-space indent, keys sorted by code unit. No timestamps, so a re-run with the same Excel gives the same bytes. User names, the computer name and home paths are replaced with `<user>`, `<computer>` and `~`.

Described as JSON Schema (draft 2020-12):

```json
{
  "type": "object",
  "required": ["schema", "topic", "title", "excel", "workbooks"],
  "properties": {
    "schema": { "const": 1 },
    "topic": { "type": "string", "description": "Matches the case file path, e.g. dates/leap-1900." },
    "title": { "type": "string" },
    "excel": {
      "type": "object",
      "description": "The Excel that did the export.",
      "properties": {
        "version": { "type": "string", "description": "Application.Version, e.g. 16.0." },
        "build": { "type": "string", "description": "Application.Build." },
        "fileVersion": { "type": ["string", "null"], "description": "EXCEL.EXE version, e.g. 16.0.17928.20156." },
        "calculationVersion": { "type": ["integer", "null"] },
        "operatingSystem": { "type": ["string", "null"] },
        "dynamicArrays": { "type": "boolean" },
        "decimalSeparator": { "type": ["string", "null"] },
        "thousandsSeparator": { "type": ["string", "null"] },
        "useSystemSeparators": { "type": ["boolean", "null"] },
        "international": {
          "type": "object",
          "description": "Application.International values: countryCode, countrySetting, dateOrder (0 MDY, 1 DMY, 2 YMD), dateSeparator, decimalSeparator, fourDigitYears, listSeparator, thousandsSeparator, timeSeparator, twentyFourHourClock."
        },
        "languages": {
          "type": "object",
          "description": "Office language ids (LCIDs): install, ui, exeMode. 2052 is Chinese (PRC), 1033 English (US)."
        },
        "windowsCulture": { "type": "string" },
        "windowsUiCulture": { "type": "string" },
        "powershell": { "type": "string" },
        "workbooksBuiltWith": { "type": ["string", "null"], "description": "fileVersion of the Excel that built the workbooks." }
      }
    },
    "workbooks": {
      "type": "object",
      "description": "One entry per file format that was built.",
      "propertyNames": { "enum": ["xlsx", "xls"] },
      "additionalProperties": {
        "type": "object",
        "required": ["file", "date1904", "settings", "cases"],
        "properties": {
          "file": { "type": "string", "description": "Path below out/workbooks, e.g. dates/leap-1900.xls." },
          "date1904": { "type": "boolean", "description": "Read back from the workbook." },
          "font": { "type": "string", "description": "Default font and size, which affect column widths and so the displayed text." },
          "settings": {
            "type": "object",
            "properties": {
              "date1904": { "type": "boolean" },
              "iteration": { "type": "boolean" },
              "maxIterations": { "type": "integer" },
              "maxChange": { "type": "number" }
            }
          },
          "cases": {
            "type": "object",
            "description": "Keyed by case id.",
            "additionalProperties": {
              "type": "object",
              "required": ["sheet", "status", "volatile"],
              "properties": {
                "sheet": { "type": "string", "description": "case001, case002, ..." },
                "status": { "enum": ["built", "skipped", "error"] },
                "reason": { "type": "string", "description": "Why it was skipped, or Excel's message when it refused a formula." },
                "volatile": { "type": "boolean" },
                "cells": {
                  "type": "object",
                  "description": "Keyed by A1 address. Every formula cell of the case, and every cell a dynamic array spilled into.",
                  "additionalProperties": { "$ref": "#/$defs/cell" }
                }
              }
            }
          }
        }
      }
    }
  },
  "$defs": {
    "cell": {
      "type": "object",
      "required": ["type", "value", "text", "numberFormat"],
      "properties": {
        "type": { "enum": ["number", "string", "boolean", "error", "empty", "unknown"] },
        "value": {
          "description": "Range.Value2. number: the exact double (written so it parses back to the same bits). string: the text. boolean: true/false. error: the error name such as #N/A. empty: null.",
          "type": ["number", "string", "boolean", "null"]
        },
        "errorCode": { "type": "integer", "description": "For errors: Excel's code, e.g. 2007 for #DIV/0!, 2042 for #N/A." },
        "text": { "type": "string", "description": "Range.Text: what the cell displays (depends on number format, column width, font and locale)." },
        "numberFormat": { "type": "string", "description": "Range.NumberFormat, in US English codes." },
        "formula": { "type": "string", "description": "The formula as Excel stores it (Formula2 when available), e.g. an =A1:A5 entered the old way reads back as =@A1:A5." },
        "set": { "enum": ["formula2", "formula", "array"], "description": "How the formula was entered. Only on cells listed in the case." },
        "spill": { "type": "string", "description": "On a spilling formula: the spill range, e.g. A1:A3." },
        "spillTruncated": { "type": "boolean", "description": "Only the first 1000 spilled cells were read." },
        "spilledFrom": { "type": "string", "description": "On a cell filled by a spill: the address of the formula." },
        "arrayRange": { "type": "string", "description": "On a cell of a legacy array formula: its whole range." }
      }
    }
  }
}
```

Text values can contain a lone UTF-16 surrogate (for example `LEFT` of an emoji). It is written as a `\udXXX` escape, which some strict JSON readers reject; read those files with a parser that accepts it, or treat the cell as "text, not compared".

## From results to tests

The kernel tests read a case file and its `expected` file side by side:

1. Build the sheet from the case: the inputs, then the formulas (with `set` deciding how a formula is entered).
2. Calculate with our engine.
3. Compare each cell in `expected`:
   - `type` must match.
   - Numbers: compare the exact double. If a case is known to depend on Excel's 15-digit display rules, say so in the test rather than loosening every comparison.
   - `text` (the displayed value) depends on the locale, the default font and column width; compare it only when the test runs with the same locale as `excel.international`, or skip it.
   - `volatile` cases: compare `type` only.
   - Cases with `status` other than `built`: skip.
   - Iterative cases (`settings.iteration: true`) that do not converge depend on how often Excel recalculated (once when the workbook was built and once in the export); compare those loosely or by type.
4. When Excel gets a big update, run the tool again. The git diff of `expected` shows exactly which behaviours changed.

The workbooks in `out/workbooks` hold Excel's own cached values. They are also useful as small file-format fixtures; copy the ones you need into a crate's `tests/data/` on purpose rather than committing the whole folder.

## Things that depend on your machine

Recorded in `excel`, so a test can tell whether it applies:

- The decimal and thousands separators, and the Windows date order (`"1,000"+0`, `DATEVALUE("1/2/2024")`, the built-in short date format).
- The Office editing language: `LENB`, `FINDB`, `LEFTB`, `ASC` and `DBCS` count or convert double-byte characters only under Chinese, Japanese or Korean.
- The ANSI code page: `CODE("中")` and `CHAR` use it.
- Day and month names, AM/PM, `TRUE`/`FALSE` and some error names in `text` follow the Excel language.

Formulas themselves are always entered in US English (`Formula`/`Formula2`, comma separators), so the same case files work in every locale.

## Troubleshooting

- "Excel is running ... unsaved changes": save or close your workbooks, then run again.
- "Excel is running N times": close Excel; check Task Manager for a hidden `EXCEL.EXE` left from a crash and end it.
- A case ends with "Excel refused the case": Excel rejected one of its formulas. The message is in `reason`; fix the case file.
- Many `#NAME?` results: that Excel is older than the function (for example `XLOOKUP`, `TEXTSPLIT`, `VSTACK`). That is still a true result for that Excel version; the version is recorded.
