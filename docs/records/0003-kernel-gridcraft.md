# 0003 Kernel: GridCraft, pinned and vendored

Status: accepted
Date: 2026-10-20 (UTC)

## Context

The kernel bench (PR 36, `research/kernel-bench/RESULTS.md` on branch kernel-bench) compared IronCalc 0.9.0 and GridCraft 0.8.0 (git 823c565). It ran natively on Windows. Issue 37 asked the owner four questions. The owner asked for the lead's proposal and accepted it on 2026-10-20.

| | IronCalc | GridCraft |
|---|---|---|
| Broken files (PLAN hard gate) | 20 panics, 2 hangs | 0 |
| Real files (Enron, 2,000 xlsx) | 56 panics | 0 |
| Formulas right, all POI files / real files | 72.99% / 72.15% | 98.86% / 95.41% |
| Saving keeps macros, comments, charts, images, pivots | none kept | mostly kept (charts 20/27, tables 27/41) |
| Edit one cell in 500k formulas | 27.7 s | 0.06 ms |
| Memory on formula-heavy files | 1× | 7–10× |
| Memory after edits (Fan, 5 edits) | flat | 1.2 GB → 3.3 GB (undo history) |

## Decision

1. **A caught panic is a failure.** A hang is too. On wasm32 a panic aborts, so `catch_unwind` cannot help in the browser, and state after a panic may be half-changed. We still guard with `catch_unwind` as a last line, but never count it as passing.
2. **Formula accuracy is measured over all files**, with "files it opened" as a second number. A file that does not open is a failure for the user.
3. **"Never lose anything silently" is a hard gate.** Known features (macros, comments, charts, images, pivot tables, data validation, external links) must be kept on save, or the user is told before saving what will be lost.
4. **GridCraft's extra memory is accepted, with conditions** (issue 46):
   - undo history has a cap, by memory or by steps (for example 256 MB);
   - one-shot command-line calls keep no undo history;
   - on 64-bit, the F file (500k formulas) stays under 1 GB; 32-bit gets smaller default limits.
5. **The kernel is GridCraft**, as agreed earlier: pinned to one commit and copied into this repository with its license and NOTICE. We do not follow its daily releases. General fixes go back upstream; it welcomes AI-written pull requests. The `.xls` writer stays our own library.

## Options not taken

- **IronCalc.** It fails the hard gate (panics on broken and real files), drops charts, comments, images and macros on save, and recalculates the whole workbook on every edit.
- **Count a caught panic as passing.** It does not work in the browser, and it hides real defects.
- **Depend on GridCraft by git and follow it.** The project is 15 days old and its API changes daily.

## Consequences

- Easier: the core path (open xlsx, edit, calculate, save) can start now (issue 45). Incremental recalculation suits the later review window (record 0001).
- Harder: we maintain a vendored copy. Its upstream is mostly one person plus AI; if it stops, we carry it.
- To check: GridCraft may have been tuned on the POI files; the Enron results support it, but keep testing on real files. Charts (7 of 27) and tables (14 of 41) not fully kept must be fixed or warned about before CLI v1.
