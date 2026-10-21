#!/usr/bin/env node
// Versions, the changelog and license files: the cheap release checks that
// need no build (issues 94 and 97). No dependencies; reads files only.
//
// Usage: node scripts/release-meta.mjs [check]
//          every workspace crate takes the workspace version, the
//          workspace's own dependencies ask for that version, CHANGELOG.md
//          has an "## Unreleased" section, and every publishable crate holds
//          the license files its `license` names, identical to the root ones,
//          and LICENSING.md names every NOTICE or THIRD-PARTY file
//        node scripts/release-meta.mjs version
//          print the workspace version
//        node scripts/release-meta.mjs tag <vX.Y.Z | refs/tags/vX.Y.Z>
//          check that the tag names the workspace version
//        node scripts/release-meta.mjs notes <X.Y.Z | Unreleased>
//          print that CHANGELOG.md section (the release notes)
//
// Exit codes: 0 fine, 1 a check failed, 2 usage.
// Self-test: node --test scripts/release-meta.test.mjs

import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const USAGE = "Usage: node scripts/release-meta.mjs [check | version | tag <vX.Y.Z> | notes <X.Y.Z | Unreleased>]";

// The folders whose crates are workspace members (Cargo.toml `members`).
export const MEMBER_GROUPS = ["crates", "apps", "tools"];

// The license file each SPDX identifier needs next to a publishable crate.
export const LICENSE_FILES = {
  "MIT": "LICENSE-MIT",
  "Apache-2.0": "LICENSE-APACHE",
  "GPL-3.0-or-later": "LICENSE-GPL",
};

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

// The tables of a Cargo.toml as { "[name]": [lines] }, comments dropped.
// Enough for the flat keys these checks read; not a TOML parser.
export function tables(manifest) {
  const out = { "": [] };
  let current = "";
  for (const raw of manifest.split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "").trim();
    if (line === "" || line.startsWith("#")) continue;
    if (/^\[\[?[^\]]+\]\]?$/.test(line)) {
      current = line;
      out[current] ??= [];
    } else out[current].push(line);
  }
  return out;
}

// The value of `key = "..."` in a table's lines, or null.
export function stringKey(lines = [], key) {
  for (const line of lines) {
    const m = line.match(new RegExp(`^${key.replace(".", "\\.")}\\s*=\\s*"([^"]*)"$`));
    if (m) return m[1];
  }
  return null;
}

// What one member's manifest says about versions, publishing and license.
export function memberFacts(manifest) {
  const pkg = tables(manifest)["[package]"] ?? [];
  return {
    name: stringKey(pkg, "name"),
    ownVersion: stringKey(pkg, "version"),
    inheritsVersion: pkg.some((l) => /^version\.workspace\s*=\s*true$/.test(l) || /^version\s*=\s*\{\s*workspace\s*=\s*true\s*\}$/.test(l)),
    publishable: !pkg.some((l) => /^publish\s*=\s*false$/.test(l)),
    license: stringKey(pkg, "license"),
  };
}

// The SPDX identifiers in a simple expression ("MIT OR Apache-2.0").
export function licenseIds(expression) {
  return (expression ?? "").split(/\s+(?:OR|AND)\s+|[()]/).map((s) => s.trim()).filter(Boolean);
}

// The root manifest: the workspace version and the workspace's own path
// dependencies with the version they ask for.
export function workspaceFacts(manifest) {
  const t = tables(manifest);
  const deps = [];
  for (const line of t["[workspace.dependencies]"] ?? []) {
    const m = line.match(/^([A-Za-z0-9_-]+)\s*=\s*\{(.*)\}$/);
    if (!m || !/\bpath\s*=/.test(m[2])) continue;
    const v = m[2].match(/\bversion\s*=\s*"([^"]*)"/);
    deps.push({ name: m[1], version: v ? v[1] : null });
  }
  return { version: stringKey(t["[workspace.package]"], "version"), deps };
}

// The CHANGELOG heading of a section: "## Unreleased", "## v0.1.0",
// "## [0.1.0]" or "## 0.1.0", maybe followed by " - 2026-11-15".
function headingVersion(line) {
  const m = line.match(/^##\s+\[?v?([^\]\s]+)\]?(?:\s+[-–]\s+.*)?\s*$/);
  return m ? m[1] : null;
}

// The body of one CHANGELOG section (heading excluded), or null if absent.
export function changelogSection(text, version) {
  const lines = text.split(/\r?\n/);
  const want = version.toLowerCase();
  const start = lines.findIndex((l) => l.startsWith("## ") && headingVersion(l)?.toLowerCase() === want);
  if (start === -1) return null;
  let end = lines.findIndex((l, i) => i > start && l.startsWith("## "));
  if (end === -1) end = lines.length;
  return lines.slice(start + 1, end).join("\n").trim();
}

// The version a tag names: "v0.1.0" or "refs/tags/v0.1.0" -> "0.1.0".
export function tagVersion(tag) {
  const m = String(tag).match(/^(?:refs\/tags\/)?v(\d+\.\d+\.\d+)$/);
  return m ? m[1] : null;
}

const same = (a, b) => a.replace(/\r\n/g, "\n") === b.replace(/\r\n/g, "\n");

// Every problem in the repository at `dir`, each { what, why, fix }.
// `read` and `list` are injectable for the tests.
export function problems(dir, {
  read = (p) => readFileSync(p, "utf8"), exists = existsSync, list = listMembers, notices = noticeFiles,
} = {}) {
  const out = [];
  const ws = workspaceFacts(read(join(dir, "Cargo.toml")));
  if (!ws.version || !SEMVER.test(ws.version)) {
    out.push({
      what: "Cargo.toml has no workspace version.",
      why: "every crate takes its version from [workspace.package] (issue 97).",
      fix: 'set version = "X.Y.Z" in [workspace.package].',
    });
  }
  for (const dep of ws.deps) {
    if (ws.version && dep.version !== ws.version) {
      out.push({
        what: `[workspace.dependencies] ${dep.name} asks for version ${dep.version ?? "(none)"}, the workspace is ${ws.version}.`,
        why: "the workspace's own crates share one version, and a published crate must ask for the version it ships with.",
        fix: `set version = "${ws.version}" on ${dep.name} in the root Cargo.toml.`,
      });
    }
  }
  for (const rel of list(dir)) {
    const facts = memberFacts(read(join(dir, rel, "Cargo.toml")));
    const name = facts.name ?? rel;
    if (!facts.inheritsVersion || facts.ownVersion !== null) {
      out.push({
        what: `${rel}/Cargo.toml (${name}) sets its own version.`,
        why: "one version for the whole workspace (issue 97).",
        fix: "replace its version line with `version.workspace = true`.",
      });
    }
    if (!facts.publishable) continue;
    const ids = licenseIds(facts.license);
    const unknown = ids.filter((id) => !LICENSE_FILES[id]);
    if (ids.length === 0 || unknown.length > 0) {
      out.push({
        what: `${rel}/Cargo.toml (${name}) has license "${facts.license ?? ""}", which this check has no file for.`,
        why: "a published crate must carry the text of its license (issue 94).",
        fix: "use the folder's license (LICENSING.md), or add the file to LICENSE_FILES in scripts/release-meta.mjs.",
      });
      continue;
    }
    for (const file of new Set(ids.map((id) => LICENSE_FILES[id]))) {
      const copy = join(dir, rel, file);
      if (!exists(copy)) {
        out.push({
          what: `${rel}/${file} is missing; ${name} can be published.`,
          why: "cargo package packs only the crate's folder, so the license text would not reach crates.io (issue 94).",
          fix: `cp ${file} ${rel}/ (or set publish = false in its Cargo.toml).`,
        });
      } else if (!same(read(copy), read(join(dir, file)))) {
        out.push({
          what: `${rel}/${file} differs from the root ${file}.`,
          why: "the copy must be the license text itself (on Windows a symbolic link checks out as a short text file).",
          fix: `cp ${file} ${rel}/${file}`,
        });
      }
    }
  }
  const licensing = exists(join(dir, "LICENSING.md")) ? read(join(dir, "LICENSING.md")) : "";
  for (const rel of notices(dir)) {
    if (!licensing.includes(`\`${rel}\``)) {
      out.push({
        what: `LICENSING.md does not name ${rel}.`,
        why: "LICENSING.md lists every notice of third-party material (issue 94).",
        fix: `add a row for \`${rel}\` to its "Third-party material" table.`,
      });
    }
  }
  const changelog = join(dir, "CHANGELOG.md");
  if (!exists(changelog) || changelogSection(read(changelog), "Unreleased") === null) {
    out.push({
      what: 'CHANGELOG.md has no "## Unreleased" section.',
      why: "changes are written down as they land, and a release renames that section (issue 97).",
      fix: 'add "## Unreleased" above the newest version section.',
    });
  }
  return out;
}

// Member folders ("crates/x") that hold a Cargo.toml.
function listMembers(dir) {
  const out = [];
  for (const group of MEMBER_GROUPS) {
    const g = join(dir, group);
    if (!existsSync(g)) continue;
    for (const e of readdirSync(g, { withFileTypes: true })) {
      if (e.isDirectory() && existsSync(join(g, e.name, "Cargo.toml"))) out.push(`${group}/${e.name}`);
    }
  }
  return out.sort();
}

// Notice files ("crates/x/NOTICE", "vendor/y/THIRD-PARTY.md") in the code
// folders, skipping build output.
export const NOTICE_FILE = /^(NOTICE|THIRD-PARTY)(\.md|\.txt)?$/i;
const SKIP_DIRS = new Set(["target", ".git", "node_modules"]);

function noticeFiles(dir) {
  const out = [];
  const walk = (rel) => {
    for (const e of readdirSync(join(dir, rel), { withFileTypes: true })) {
      const child = `${rel}/${e.name}`;
      if (e.isDirectory() && !SKIP_DIRS.has(e.name)) walk(child);
      else if (e.isFile() && NOTICE_FILE.test(e.name)) out.push(child);
    }
  };
  for (const top of [...MEMBER_GROUPS, "vendor"]) if (existsSync(join(dir, top))) walk(top);
  return out.sort();
}

function fail(lines) {
  for (const l of lines) console.error(l);
  process.exit(1);
}

function main(argv) {
  const [cmd = "check", arg, ...rest] = argv;
  if (rest.length > 0) { console.error(USAGE); process.exit(2); }
  const version = workspaceFacts(readFileSync(join(root, "Cargo.toml"), "utf8")).version;
  switch (cmd) {
    case "check": {
      if (arg !== undefined) { console.error(USAGE); process.exit(2); }
      const found = problems(root);
      if (found.length === 0) {
        console.log(`release-meta: version ${version} everywhere, CHANGELOG has Unreleased, license files and notices in place.`);
        return;
      }
      fail(found.flatMap((p) => [p.what, `  Why: ${p.why}`, `  Fix: ${p.fix}`]));
      return;
    }
    case "version":
      if (arg !== undefined) { console.error(USAGE); process.exit(2); }
      console.log(version);
      return;
    case "tag": {
      if (arg === undefined) { console.error(USAGE); process.exit(2); }
      const v = tagVersion(arg);
      if (v === null) {
        fail([`The tag "${arg}" is not a release tag.`, "  Why: releases come from tags vX.Y.Z.",
          `  Fix: tag the release commit v${version}.`]);
      }
      if (v !== version) {
        fail([`The tag v${v} does not match the workspace version ${version}.`,
          "  Why: the archives would carry a version the binary does not report.",
          `  Fix: set version = "${v}" in [workspace.package] (and [workspace.dependencies]) first, or tag v${version}.`]);
      }
      console.log(`Tag v${v} matches the workspace version.`);
      return;
    }
    case "notes": {
      if (arg === undefined) { console.error(USAGE); process.exit(2); }
      const body = changelogSection(readFileSync(join(root, "CHANGELOG.md"), "utf8"), arg.replace(/^v/, ""));
      if (!body) {
        fail([`CHANGELOG.md has no "## ${arg}" section, or it is empty.`,
          "  Why: the release notes come from it.",
          `  Fix: rename "## Unreleased" to "## v${arg.replace(/^v/, "")} - <date>" and start a new empty "## Unreleased" above it.`]);
      }
      console.log(body);
      return;
    }
    default:
      console.error(USAGE);
      process.exit(2);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) main(process.argv.slice(2));
