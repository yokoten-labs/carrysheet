// Self-test for scripts/vendor.mjs: the folder map, left_out, patches and every kind of
// difference, on a small made-up upstream. No network.
// Run: node --test scripts/tests/vendor.test.mjs

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { listFiles, verify } from "../vendor.mjs";

const script = join(dirname(fileURLToPath(import.meta.url)), "..", "vendor.mjs");

const PATCH = `diff --git a/crates/base/core/src/lib.rs b/crates/base/core/src/lib.rs
--- a/crates/base/core/src/lib.rs
+++ b/crates/base/core/src/lib.rs
@@ -1,2 +1,3 @@
 pub mod addr;
+pub mod limits;
 pub mod value;
diff --git a/crates/base/core/src/limits.rs b/crates/base/core/src/limits.rs
new file mode 100644
--- /dev/null
+++ b/crates/base/core/src/limits.rs
@@ -0,0 +1 @@
+pub const MAX: u32 = 1;
`;

function write(root, files) {
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
}

// A made-up upstream and a vendored copy that matches it: one mapped crate, one file, one patch.
function setUp(t) {
  const root = mkdtempSync(join(tmpdir(), "carrysheet-vendor-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const upstream = join(root, "upstream"), vendor = join(root, "vendor");
  write(upstream, {
    "crates/core/Cargo.toml": "[package]\n",
    "crates/core/src/lib.rs": "pub mod addr;\npub mod value;\n",
    "crates/core/src/old.rs": "// dropped\n",
    "crates/core/tests/big.rs": "// not copied\n",
    "NOTICE": "upstream notice\n",
    "README.md": "not copied\n",
  });
  const manifest = {
    repository: "https://example.invalid/gridcraft", commit: "823c56582d00be91e31b4f7f3680496cf89e84fb",
    copy: ["src", "Cargo.toml"], folders: { "crates/core": "crates/base/core" }, files: { NOTICE: "NOTICE" },
    left_out: ["crates/base/core/src/old.rs"], ours: ["README.md", "upstream.json", "patches"], patches: ["patches/core.patch"],
  };
  write(vendor, {
    "crates/base/core/Cargo.toml": "[package]\n",
    "crates/base/core/src/lib.rs": "pub mod addr;\npub mod limits;\npub mod value;\n",
    "crates/base/core/src/limits.rs": "pub const MAX: u32 = 1;\n",
    "NOTICE": "upstream notice\n",
    "README.md": "ours\n",
    "upstream.json": JSON.stringify(manifest),
    "patches/core.patch": PATCH,
  });
  return { upstream, vendor };
}

test("a folder that is upstream plus the patches passes, and the added lines are counted", (t) => {
  const { upstream, vendor } = setUp(t);
  assert.equal(verify(vendor, upstream), "vendor: vendor/gridcraft is GridCraft 823c565 plus 1 patch files (2 added lines); 4 files match.");
});

test("an edit outside the patches fails in three parts", (t) => {
  const { upstream, vendor } = setUp(t);
  write(vendor, { "crates/base/core/src/limits.rs": "pub const MAX: u32 = 2;\n" });
  assert.throws(() => verify(vendor, upstream), (error) => error.isVendor
    && /1 file\(s\) in vendor\/gridcraft differ/.test(error.message) && /limits\.rs \(content\)/.test(error.message)
    && /\n {2}Why: .*823c565/.test(error.message) && /\n {2}Fix: /.test(error.message));
});

test("an added file, a missing file and a left_out file that came back each fail", (t) => {
  const { upstream, vendor } = setUp(t);
  write(vendor, { "crates/base/core/src/extra.rs": "\n", "crates/base/core/src/old.rs": "// dropped\n" });
  rmSync(join(vendor, "NOTICE"));
  assert.throws(() => verify(vendor, upstream), (error) => /3 file\(s\)/.test(error.message)
    && /NOTICE \(missing here\)/.test(error.message) && /extra\.rs \(not upstream and not in a patch\)/.test(error.message)
    && /old\.rs \(not upstream and not in a patch\)/.test(error.message));
});

test("a patch file that upstream.json does not list fails", (t) => {
  const { upstream, vendor } = setUp(t);
  write(vendor, { "patches/hidden.patch": "" });
  assert.throws(() => verify(vendor, upstream), /does not list: hidden\.patch/);
});

test("a patch that does not apply fails and names the patch", (t) => {
  const { upstream, vendor } = setUp(t);
  write(upstream, { "crates/core/src/lib.rs": "pub mod other;\n" });
  assert.throws(() => verify(vendor, upstream), (error) => /patches\/core\.patch does not apply/.test(error.message) && /Fix: /.test(error.message));
});

test("a symlink in the folder fails even when its target matches", { skip: process.platform === "win32" }, (t) => {
  const { upstream, vendor } = setUp(t);
  rmSync(join(vendor, "NOTICE"));
  write(vendor, { "notice.txt": "upstream notice\n" });
  symlinkSync("notice.txt", join(vendor, "NOTICE"));
  assert.throws(() => verify(vendor, upstream), (error) => /NOTICE \(a symlink\)/.test(error.message));
});

test("GIT_DIR from the caller (as in a hook) does not reach git", (t) => {
  const { upstream, vendor } = setUp(t);
  t.after(() => { delete process.env.GIT_DIR; delete process.env.GIT_WORK_TREE; });
  process.env.GIT_DIR = join(vendor, "README.md"); // Not a repository: git stops if it reads this.
  process.env.GIT_WORK_TREE = upstream;
  assert.match(verify(vendor, upstream), /4 files match\.$/);
});

test("listFiles skips our own paths and lists files only", (t) => {
  const { vendor } = setUp(t);
  assert.deepEqual(listFiles(vendor, ["patches", "README.md", "upstream.json"]),
    ["NOTICE", "crates/base/core/Cargo.toml", "crates/base/core/src/lib.rs", "crates/base/core/src/limits.rs"]);
});

test("any command but verify is a usage error", () => {
  const result = spawnSync(process.execPath, [script, "diff"], { encoding: "utf8" });
  assert.equal(result.status, 2);
  assert.match(result.stderr, /no such command\.\n {2}Why: .*\n {2}Fix: /);
});

test("the script runs when reached through a symlinked folder", { skip: process.platform === "win32" }, (t) => {
  const root = mkdtempSync(join(tmpdir(), "carrysheet-vendor-link-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  symlinkSync(dirname(script), join(root, "scripts"));
  assert.equal(spawnSync(process.execPath, [join(root, "scripts", "vendor.mjs"), "bogus"]).status, 2);
});
