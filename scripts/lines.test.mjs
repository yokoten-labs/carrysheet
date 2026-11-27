// Self-test for scripts/lines.mjs, the cap check of record 0010.
// Run: node --test scripts/lines.test.mjs

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { CAP, PATCH_MAX, checkTree, findRustHits } from "./lines.mjs";

const scriptPath = join(dirname(fileURLToPath(import.meta.url)), "lines.mjs");
const lineText = (n) => "x\n".repeat(n);

// Runs checkTree on an in-memory tree: { path: text }.
function checkFiles(tree, max) {
  return checkTree(Object.keys(tree), (path) => tree[path], max);
}

test("lines.mjs stays at most 80 lines", () => {
  assert.ok(readFileSync(scriptPath, "utf8").split("\n").length - 1 <= 80);
});

test("counts physical lines of code files, blank lines too", () => {
  const result = checkFiles({ "a.rs": "fn a() {}\n\n// c\n", "b.mjs": "x\ny", "c.py": "", "d/e.sh": lineText(4) });
  assert.equal(result.code, 0);
  assert.match(result.out[0], /^lines: 9 counted, cap 5,000, room left 4,991$/);
});

test("skips vendor, tests, benches and *.test.mjs", () => {
  const result = checkFiles({
    "vendor/x/src/a.rs": lineText(9000), "crates/a/tests/t.rs": lineText(9000), "crates/a/benches/b.rs": lineText(9000),
    "scripts/x.test.mjs": lineText(9000), "tests/top.rs": lineText(9000), "vendor/x/odd.weird": "", "crates/a/src/lib.rs": lineText(3),
  });
  assert.equal(result.code, 0);
  assert.match(result.out[0], /^lines: 3 counted/);
});

test("a folder named like tests but not tests is counted", () => {
  assert.match(checkFiles({ "crates/a/src/tests_util.rs": lineText(2), "contests/a.rs": lineText(1) }).out[0], /^lines: 3 /);
});

test("data files pass; an unknown extension fails with three parts", () => {
  const data = ["Cargo.toml", "a/b.yml", "README.md", "Cargo.lock", "x.json", "n.txt", "LICENSE-MIT", "a/NOTICE", ".gitignore", ".github/CODEOWNERS"];
  assert.equal(checkFiles(Object.fromEntries(data.map((path) => [path, "x\n"]))).code, 0);
  const result = checkFiles({ "src/main.go": "package main\n", Makefile: "all:\n" });
  assert.equal(result.code, 1);
  const errors = result.out.filter((line) => line.startsWith("error:"));
  assert.equal(errors.length, 2);
  for (const error of errors) assert.match(error, /not on the data list\.\n {2}Why: .*\n {2}Fix: /);
});

test("above the cap: fails with what, why, fix and a folder table, biggest first", () => {
  const result = checkFiles({ "crates/a/src/lib.rs": lineText(3000), "apps/b/src/main.rs": lineText(2500), "build.rs": lineText(1) });
  assert.equal(result.code, 1);
  assert.match(result.out[0], /room left -501$/);
  const error = result.out.find((line) => line.startsWith("error:"));
  assert.match(error, /5,501 lines of code, above the cap of 5,000 by 501\.\n {2}Why: .*\n {2}Fix: /);
  assert.match(error, /3,000 {2}crates\/a\n +2,500 {2}apps\/b\n +1 {2}\.$/);
});

test("exactly the cap passes; above the target prints a note", () => {
  const result = checkFiles({ "a.rs": lineText(CAP) });
  assert.equal(result.code, 0);
  assert.ok(result.out.some((line) => line.startsWith("note: above the 4,500-line target")));
  assert.ok(!checkFiles({ "a.rs": lineText(4500) }).out.some((line) => line.startsWith("note:")));
});

test("--max replaces the cap", () => {
  assert.equal(checkFiles({ "a.rs": lineText(6000) }, 7000).code, 0);
  assert.equal(checkFiles({ "a.rs": lineText(6000) }, 5999).code, 1);
});

test("Rust rules: #[path, include!( and an unguarded mod tests;", () => {
  const text = '#[path = "x.rs"]\nmod x;\ninclude!("y.rs");\nmod tests;\n#[cfg(test)]\n\nmod tests;\ninclude_str!("z");\n';
  assert.deepEqual(findRustHits("a.rs", text), ['a.rs:1: #[path = "x.rs"]', 'a.rs:3: include!("y.rs");', "a.rs:4: mod tests;"]);
  assert.deepEqual(findRustHits("a.rs", "#[cfg(test)]\npub(crate) mod tests;\n"), []);
  assert.equal(findRustHits("a.rs", "pub mod tests;\n").length, 1);
});

test("Rust rules fail without --max and are a note with it", () => {
  const tree = { "src/lib.rs": "mod tests;\n" };
  const strict = checkFiles(tree);
  assert.equal(strict.code, 1);
  assert.match(strict.out.find((line) => line.startsWith("error:")), /src\/lib\.rs:1: mod tests;\n {2}Why: .*\n {2}Fix: /);
  const loose = checkFiles(tree, 100);
  assert.equal(loose.code, 0);
  assert.ok(loose.out.some((line) => line.startsWith("note: 1 test-path escape")));
});

test("patch files: counts added lines and prints the number", () => {
  const patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1,2 @@\n-old\n+new\n+more\n ctx\n";
  const result = checkFiles({ "vendor/gridcraft/patches/0001-a.patch": patch, "vendor/gridcraft/patches/0002-b.patch": "+one\n" });
  assert.ok(result.out.includes("patches: 3 added lines in vendor/gridcraft/patches"));
  assert.equal(result.code, 0, `PATCH_MAX is ${PATCH_MAX}: 0 means off`);
  assert.ok(!checkFiles({ "a.rs": "x\n" }).out.some((line) => line.startsWith("patches:")));
});

test("the command line: a git tree, exit codes and usage", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "carrysheet-lines-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const git = (...args) => spawnSync("git", ["-C", dir, ...args], { encoding: "utf8" });
  git("init", "-q");
  mkdirSync(join(dir, "src"));
  writeFileSync(join(dir, "src", "lib.rs"), lineText(10));
  writeFileSync(join(dir, "untracked.rs"), lineText(9000));
  git("add", "src/lib.rs");
  const run = (...args) => spawnSync(process.execPath, [scriptPath, ...args], { cwd: join(dir, "src"), encoding: "utf8" });
  const ok = run();
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /^lines: 10 counted, cap 5,000, room left 4,990\n$/);
  const over = run("--max", "5");
  assert.equal(over.status, 1);
  assert.match(over.stderr, /error: 10 lines of code, above the cap of 5 by 5\./);
  for (const args of [["--max"], ["--max", "0"], ["--max", "x"], ["--other"]]) {
    const usage = run(...args);
    assert.equal(usage.status, 2, args.join(" "));
    assert.match(usage.stderr, /^error: unknown arguments\.\n {2}Why: .*\n {2}Fix: /);
  }
});
