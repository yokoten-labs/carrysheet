#!/usr/bin/env node
// Usage: node scripts/vendor.mjs verify. Checks that vendor/gridcraft is upstream GridCraft at the
// commit in upstream.json plus the patch files there, byte for byte (record 0009 decision 5.1,
// kept by record 0010). Exit codes: 0 the same, 1 a difference or a failed step, 2 usage.

import { execFileSync } from "node:child_process";
import { cpSync, lstatSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, sep } from "node:path";
import { fileURLToPath } from "node:url";

// An error in three parts: what happened, why, and how to fix it.
export const vendorError = (what, why, fix) => Object.assign(new Error(`vendor: ${what}\n  Why: ${why}\n  Fix: ${fix}`), { isVendor: true });

function git(args, cwd, failure) { // The ceiling keeps git from using a repository above cwd.
  const env = { ...process.env, GIT_CEILING_DIRECTORIES: dirname(cwd), GIT_TERMINAL_PROMPT: "0" };
  try {
    return execFileSync("git", ["-c", "core.autocrlf=false", ...args], { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
  } catch (error) {
    throw vendorError(failure.what, String(error.stderr ?? "").trim().split("\n")[0] || `git ${args[0]} failed.`, failure.fix);
  }
}

// Every file below root, sorted, with "/" separators, without the skipped paths. Symlinks are not followed.
export function listFiles(root, skip = []) {
  const isSkipped = (rel) => skip.some((path) => rel === path || rel.startsWith(`${path}/`));
  const all = readdirSync(root, { recursive: true }).map((path) => path.split(sep).join("/"));
  return all.filter((rel) => !isSkipped(rel) && !lstatSync(join(root, rel)).isDirectory()).sort();
}

// Write upstream's files through the folder map into target, drop left_out, apply the patches.
export function buildExpected(upstream, vendor, manifest, target) {
  const copies = Object.entries(manifest.folders).flatMap(([from, to]) => manifest.copy.map((entry) => [join(from, entry), join(to, entry)]));
  for (const [from, to] of copies) cpSync(join(upstream, from), join(target, to), { recursive: true });
  for (const [from, to] of Object.entries(manifest.files)) cpSync(join(upstream, from), join(target, to));
  for (const path of manifest.left_out) rmSync(join(target, path), { recursive: true });
  const unlisted = readdirSync(join(vendor, "patches")).filter((name) => !manifest.patches.includes(`patches/${name}`));
  if (unlisted.length) throw vendorError(`files in patches/ that upstream.json does not list: ${unlisted.join(", ")}.`, "verify applies only the listed patches, so it never checks an unlisted file.",
      'add each one to "patches" in vendor/gridcraft/upstream.json, or delete it.');
  for (const patch of manifest.patches) {
    const failure = { what: `${patch} does not apply to upstream ${manifest.commit.slice(0, 7)}.`, fix: "make it again (see vendor/gridcraft/README.md)." };
    git(["apply", "--whitespace=nowarn", join(vendor, patch)], target, failure);
  }
}

// The paths that differ between the expected tree and the vendored folder, each with its reason.
export function compareTrees(expected, vendor, ours) {
  const want = listFiles(expected), have = listFiles(vendor, ours), differences = [];
  for (const path of want) if (!have.includes(path)) differences.push(`${path} (missing here)`);
  for (const path of have) {
    if (!want.includes(path)) differences.push(`${path} (not upstream and not in a patch)`);
    else if ([expected, vendor].some((root) => lstatSync(join(root, path)).isSymbolicLink())) differences.push(`${path} (a symlink)`);
    else if (!readFileSync(join(expected, path)).equals(readFileSync(join(vendor, path)))) differences.push(`${path} (content)`);
  }
  return differences;
}

// Check vendor against upstream; upstream is a local checkout, or undefined to fetch it.
export function verify(vendor, upstream) {
  const manifest = JSON.parse(readFileSync(join(vendor, "upstream.json"), "utf8"));
  const short = manifest.commit.slice(0, 7);
  const work = mkdtempSync(join(tmpdir(), "carrysheet-vendor-"));
  try {
    if (!upstream) {
      const failure = { what: `could not fetch ${manifest.repository} at ${short}.`, fix: "check the network and the commit in upstream.json, then run again." };
      upstream = join(work, "upstream");
      git(["init", "-q", upstream], work, failure);
      git(["fetch", "-q", "--depth", "1", manifest.repository, manifest.commit], upstream, failure);
      git(["-c", "advice.detachedHead=false", "checkout", "-q", "FETCH_HEAD"], upstream, failure);
    }
    const expected = join(work, "expected");
    buildExpected(upstream, vendor, manifest, expected);
    const differences = compareTrees(expected, vendor, manifest.ours);
    if (differences.length) {
      const why = `the folder must be GridCraft ${short} with the patch files in upstream.json applied (record 0009 decision 5.1).`;
      throw vendorError(`${differences.length} file(s) in vendor/gridcraft differ from upstream plus the patches:\n    ${differences.join("\n    ")}`, why,
        "put each change in a patch file in vendor/gridcraft/patches/ (see the README there), or undo it.");
    }
    const added = manifest.patches.flatMap((patch) => readFileSync(join(vendor, patch), "utf8").split("\n").filter((line) => /^\+(?!\+\+ )/.test(line))).length;
    return `vendor: vendor/gridcraft is GridCraft ${short} plus ${manifest.patches.length} patch files (${added} added lines); ${listFiles(expected).length} files match.`;
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const vendor = join(dirname(fileURLToPath(import.meta.url)), "..", "vendor", "gridcraft");
  if (process.argv.slice(2).join(" ") !== "verify") {
    console.error(vendorError("no such command.", "verify is the only command.", "run: node scripts/vendor.mjs verify").message);
    process.exit(2);
  }
  try {
    console.log(verify(vendor));
  } catch (error) {
    console.error(error.isVendor ? error.message : `vendor: ${error.message}`);
    process.exit(1);
  }
}
