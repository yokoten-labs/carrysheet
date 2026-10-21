# Security

Carrysheet opens files that other people made. A spreadsheet can be built to attack the program that opens it, so we take reports about hostile files seriously.

## How to report

Report a security problem privately, through GitHub:

1. Open the **Security** tab of this repository.
2. Click **Report a vulnerability**.
3. Fill in the form.

Only the maintainers see the report. Please **never** report a security problem in a public issue, discussion or pull request, and don't share a file that triggers it in public.

## What is in scope

- **Hostile files.** Any file that makes Carrysheet:
  - crash or panic,
  - hang, or work far longer than the file's size explains,
  - use memory without bound,
  - run a macro or a formula that should not run (macros must never run),
  - read or write outside the folder it was given (path escapes),
  - lose or change data when it saves a file.
- **The MCP server** (`carrysheet-cli mcp serve`): a way for a client or a file to get around its root folder or read-only mode, run commands, or reach files it should not.
- The libraries in `crates/` and the programs in `apps/`.

Out of scope: problems in a dependency that don't affect Carrysheet (report those upstream), and files that are only slow because they are really big.

## What to include

- What happens, and what you expected instead.
- How to make it happen: the command or code you ran, and the commit you tested.
- The file, if one is needed. Make it as small as you can. Attach it to the private report, not anywhere public.
- Your system (Windows, Linux or macOS) and Rust version, if you built it yourself.

## What happens next

We confirm the report, fix it, and tell you when the fix is in. We credit you in the fix unless you'd rather we didn't.

Carrysheet has no release yet. Fixes land on `main`, and there are no older versions to patch.
