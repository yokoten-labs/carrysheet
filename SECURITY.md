# Security policy

Carrysheet opens files that may come from anyone, and lets AI agents work on
them. We take reports about its security seriously.

## Reporting a vulnerability

Please report privately, not in a public issue, pull request or discussion.

Use GitHub's private vulnerability reporting: open the repository's
**Security** tab and choose **Report a vulnerability**. Only the maintainers
can see the report.

Never report a security problem in a public issue, pull request or
discussion. Do not share a file that triggers it in public either. Attach it
to the private report.

## What to include

- What the problem is and what an attacker could do with it.
- What happens, and what you expected instead.
- The version (`carrysheet-cli --version`), or the commit you tested. Add
  your system (Windows, macOS or Linux, and the processor), and your Rust
  version if you built it yourself.
- How to reproduce it: the command or MCP tool call, and the smallest file
  that shows it. If the file holds anything private, remove it first, or
  describe how to build the file.
- Whether the problem is already known to others or public anywhere.

## What to expect

Carrysheet has one maintainer, so please be patient.

- **Acknowledgement:** our target is within 7 days.
- **Assessment:** we tell you whether we can reproduce it and how serious we
  think it is, and keep you updated while we work on a fix.
- **Fix and disclosure:** we agree on a disclosure date with you. We publish
  a GitHub security advisory with the fix, and credit you there unless you
  ask us not to.

## Supported versions

Before 1.0, only the latest release gets security fixes. Please check that
the problem still happens on the latest release before reporting.

While there is no release yet, fixes land on `main`. There are no older
versions to patch.

## Scope

In scope: the code in this repository, which is the `carrysheet-cli`
program, its MCP server, and the libraries in `crates/` and `apps/`. The
[threat model](docs/security/threat-model.md) says what we defend against,
for example hostile files, hostile text aimed at agents, and MCP clients, and
what we do not.

A hostile file is in scope when it makes Carrysheet:

- crash or panic;
- hang, or work far longer than the file's size explains;
- use memory without bound;
- run a macro or a formula that should not run (macros must never run);
- read or write outside the folder it was given (a path escape);
- lose or change data when it saves a file.

The MCP server (`carrysheet-cli mcp serve`) is in scope when a client or a
file can get around its root folder or read-only mode, run commands, or reach
files it should not.

Out of scope:

- The cases the threat model lists under "What we do not defend against",
  such as a malicious user with the same account on the same computer.
- Problems only in a dependency, unless Carrysheet uses it in an unsafe way.
  Please report those to the dependency's project as well.
- Files that are only slow because they are really big.
- Reports from automated scanners without a way to reproduce them.

## Safe harbor

We will not take legal action against you, or ask anyone else to, for
security research done in good faith under this policy. Good faith means
that you:

- test only on your own computer and with files you own or made;
- do not access, change or delete other people's data;
- do not disrupt services or other users;
- give us reasonable time to fix the problem before you disclose it.

If you are unsure whether something is allowed, ask in your private report
first.
