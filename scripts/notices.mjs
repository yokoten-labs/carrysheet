#!/usr/bin/env node
// Write THIRD-PARTY-NOTICES.txt for the release archives (issue 95): the
// license text of every crate the carrysheet-cli binary ships, the NOTICE
// files of those crates, the vendored GridCraft kernel's license and NOTICE,
// and the notices of our own shipped crates (such as Apache POI's, quoted in
// carrysheet-biff's NOTICE). No dependencies.
//
// Usage: node scripts/notices.mjs <about.json> <out.txt>
//   <about.json>  the output of
//                 cargo about generate --format json --locked --fail \
//                   -m apps/carrysheet-cli/Cargo.toml -o <about.json>
//                 (cargo-about reads about.toml for the accepted licenses)
//   <out.txt>     the notice file to write
//
// What ships: `cargo tree -p carrysheet-cli -e normal,no-proc-macro` for each
// release target, so no dev-dependencies, build-script dependencies or proc
// macros. Fails (exit 1) and writes nothing when a shipped crate has no
// license text read from its own files.
// Exit codes: 0 written, 1 something missing or cargo failed, 2 usage.
// Self-test: node --test scripts/notices.test.mjs

import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PACKAGE = "carrysheet-cli";

// The release targets; keep in step with about.toml and release.yml.
export const TARGETS = [
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
];

// Crates whose license cargo-about can only give as the standard text,
// because the published crate has no license file. Allowed only where the
// standard text is the whole license (no copyright line to fill in), and
// each with the reason.
export const STANDARD_TEXT_OK = {
  rmcp: "Apache-2.0, whose text names no copyright holder; the crate ships no LICENSE file",
};

// Files next to a crate that carry notices to pass on.
const NOTICE_NAME = /^(NOTICE|NOTICE\.txt|NOTICE\.md|THIRD-PARTY\.md)$/i;
// License files at the root of a vendored project.
const LICENSE_NAME = /^(LICENSE|LICENSE-[A-Z0-9.-]+|COPYING)(\.txt|\.md)?$/i;

// `cargo tree --prefix none --format {p}` lines -> "name@version" keys.
export function parseTree(text) {
  const out = new Set();
  for (const line of text.split(/\r?\n/)) {
    const m = line.trim().match(/^(\S+) v(\S+)/);
    if (m) out.add(`${m[1]}@${m[2]}`);
  }
  return out;
}

// Sort shipped crates into our own (workspace members), vendored (a path
// outside the workspace members) and registry crates.
export function classify(shipped, metadata) {
  const members = new Set(metadata.workspace_members);
  const byKey = new Map(metadata.packages.map((p) => [`${p.name}@${p.version}`, p]));
  const out = { own: [], vendored: [], registry: [], unknown: [] };
  for (const key of [...shipped].sort()) {
    const p = byKey.get(key);
    if (!p) out.unknown.push(key);
    else if (members.has(p.id)) out.own.push(p);
    else if (p.source === null) out.vendored.push(p);
    else out.registry.push(p);
  }
  return out;
}

// cargo-about's licenses, each kept with the shipped registry crates that
// use it. Returns { sections, missing } where `missing` lists shipped crates
// with no license text read from their own files.
export function registryLicenses(about, registry, standardOk = STANDARD_TEXT_OK) {
  const keys = new Set(registry.map((p) => `${p.name}@${p.version}`));
  const covered = new Set();
  const sections = [];
  for (const l of about.licenses ?? []) {
    const users = (l.used_by ?? []).map((u) => `${u.crate.name}@${u.crate.version}`).filter((k) => keys.has(k));
    if (users.length === 0) continue;
    const own = Boolean(l.source_path);
    const okUsers = users.filter((k) => own || standardOk[k.slice(0, k.lastIndexOf("@"))]);
    if (okUsers.length === 0) continue;
    okUsers.forEach((k) => covered.add(k));
    sections.push({ id: l.id, users: [...new Set(okUsers)].sort(), text: l.text });
  }
  const missing = [...keys].filter((k) => !covered.has(k)).sort();
  return { sections, missing };
}

const rule = (title, ch = "=") => `${title}\n${ch.repeat(Math.min(title.length, 78))}`;
const clean = (text) => text.replace(/\r\n/g, "\n").replace(/\s+$/, "");

// The notice file's text.
export function render({ version, own, vendored, registry, sections }) {
  const out = [];
  out.push(rule(`Carrysheet ${version}: third-party notices`));
  out.push(`carrysheet-cli is licensed GPL-3.0-or-later (LICENSE-GPL). Carrysheet's own
libraries in it are MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE). LICENSING.md
explains which applies where.

The program also contains the third-party code listed here. Their licenses
ask that their copyright and license notices travel with the program, so
this file holds them all. It was generated at release time from Cargo.lock
by scripts/notices.mjs, and lists only what the program ships: no
dev-dependencies, build tools or procedural macros.`);

  out.push(rule("Contents", "-"));
  out.push([
    "1. Notices from Carrysheet's own crates",
    "2. Vendored code",
    `3. Crates from crates.io (${registry.length}), then their license texts`,
  ].join("\n"));

  out.push(rule("1. Notices from Carrysheet's own crates"));
  if (own.length === 0) out.push("None of the shipped crates carries a notice of its own.");
  for (const n of own) out.push(rule(`${n.crate}: ${n.file}`, "-"), clean(n.text));

  out.push(rule("2. Vendored code"));
  for (const v of vendored) {
    out.push(rule(`${v.title} (${v.dir}): ${v.crates.join(", ")}`, "-"));
    for (const f of v.files) out.push(`--- ${f.file} ---`, clean(f.text));
  }

  out.push(rule(`3. Crates from crates.io (${registry.length})`));
  out.push(registry.map((p) => `${p.name} ${p.version}  ${p.license ?? ""}`.trimEnd()).join("\n"));
  for (const n of registry.flatMap((p) => p.notices ?? [])) out.push(rule(`${n.crate}: ${n.file}`, "-"), clean(n.text));
  for (const s of sections) out.push(rule(`${s.id}, used by ${s.users.join(", ")}`, "-"), clean(s.text));
  return `${out.join("\n\n")}\n`;
}

function cargo(args) {
  const r = spawnSync("cargo", args, { cwd: root, encoding: "utf8", maxBuffer: 256 << 20 });
  if (r.status !== 0) {
    console.error(`cargo ${args.join(" ")} failed (exit ${r.status}).`);
    console.error((r.stderr ?? "").split("\n").slice(-20).join("\n"));
    process.exit(1);
  }
  return r.stdout;
}

// A path for output: relative to the repository, or only the file name
// outside it, so no home folder is printed.
const shown = (p) => {
  const r = relative(root, p);
  return r.startsWith("..") ? basename(p) : r.split(sep).join("/");
};

// Notice files directly in `dir`, as { file, text } with repo-relative names.
function noticesIn(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir).filter((f) => NOTICE_NAME.test(f)).sort()
    .map((f) => ({ file: f, path: join(dir, f), text: readFileSync(join(dir, f), "utf8") }));
}

// The folder a vendored crate came with: the nearest one above it, inside
// the repository, that holds license files.
function vendorRoot(crateDir) {
  for (let d = crateDir; d.startsWith(root + sep); d = dirname(d)) {
    if (readdirSync(d).some((f) => LICENSE_NAME.test(f))) return d;
  }
  return null;
}

function main(argv) {
  if (argv.length !== 2) {
    console.error("Usage: node scripts/notices.mjs <about.json> <out.txt>");
    process.exit(2);
  }
  const [aboutPath, outPath] = argv;
  const about = JSON.parse(readFileSync(aboutPath, "utf8"));
  const metadata = JSON.parse(cargo(["metadata", "--format-version", "1", "--locked"]));
  const shipped = new Set();
  for (const target of TARGETS) {
    const tree = cargo(["tree", "--locked", "-p", PACKAGE, "-e", "normal,no-proc-macro", "--target", target,
      "--prefix", "none", "--format", "{p}"]);
    for (const k of parseTree(tree)) shipped.add(k);
  }
  const parts = classify(shipped, metadata);
  const problems = [];
  if (parts.unknown.length) problems.push(`not in cargo metadata: ${parts.unknown.join(", ")}`);

  const own = parts.own.flatMap((p) => noticesIn(dirname(p.manifest_path))
    .map((n) => ({ crate: p.name, file: shown(n.path), text: n.text })));

  const groups = new Map();
  for (const p of parts.vendored) {
    const dir = vendorRoot(dirname(p.manifest_path));
    if (!dir) { problems.push(`${p.name} ${p.version} (vendored): no license file in its folder or above it`); continue; }
    if (!groups.has(dir)) groups.set(dir, []);
    groups.get(dir).push(p.name);
  }
  const vendored = [...groups].map(([dir, crates]) => ({
    dir: shown(dir),
    title: dir.split(sep).pop(),
    crates: crates.sort(),
    files: readdirSync(dir).filter((f) => LICENSE_NAME.test(f) || NOTICE_NAME.test(f)).sort()
      .map((f) => ({ file: f, text: readFileSync(join(dir, f), "utf8") })),
  }));

  const { sections, missing } = registryLicenses(about, parts.registry);
  for (const k of missing) problems.push(`${k}: cargo-about found no license file in the crate`);
  const registry = parts.registry.map((p) => ({
    name: p.name, version: p.version, license: p.license,
    notices: noticesIn(dirname(p.manifest_path)).map((n) => ({ crate: `${p.name} ${p.version}`, file: n.file, text: n.text })),
  }));

  if (problems.length) {
    console.error(`No notice file written: ${problems.length} shipped crate(s) without a license text.`);
    for (const p of problems) console.error(`  ${p}`);
    console.error("Why: their licenses ask that the text, with its copyright line, ships with the program.");
    console.error("Fix: add a [<crate>.clarify] entry with the license file and its checksum to about.toml");
    console.error("(see cargo-about's docs), or, for a license with no copyright line, STANDARD_TEXT_OK in this script.");
    process.exit(1);
  }
  const version = metadata.packages.find((p) => p.name === PACKAGE)?.version ?? "?";
  writeFileSync(outPath, render({ version, own, vendored, registry, sections }));
  console.log(`Wrote ${shown(resolve(outPath))}: ${registry.length} crates from crates.io, `
    + `${vendored.reduce((n, v) => n + v.crates.length, 0)} vendored, ${own.length} notice(s) of our own crates.`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) main(process.argv.slice(2));
