// Self-tests for release-meta.mjs: node --test scripts/release-meta.test.mjs

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  changelogSection, licenseIds, memberFacts, problems, tagVersion, workspaceFacts,
} from "./release-meta.mjs";

const script = join(dirname(fileURLToPath(import.meta.url)), "release-meta.mjs");

const ROOT = `[workspace]
members = ["crates/*"]

[workspace.package]
version = "0.3.1" # one version
edition = "2024"

[workspace.dependencies]
carrysheet-a = { path = "crates/a", version = "0.3.1" }
carrysheet-b = { path = "crates/b", version = "0.3.0" }
serde = { version = "1" }
`;

const member = (extra = "") => `[package]
name = "carrysheet-a"
version.workspace = true
license = "MIT OR Apache-2.0"
${extra}
[dependencies]
version = "9"
`;

// A fake repository: path -> text.
function repo(files) {
  return {
    read: (p) => { if (!(p in files)) throw new Error(`no ${p}`); return files[p]; },
    exists: (p) => p in files,
    list: () => Object.keys(files).filter((p) => /^crates\/[^/]+\/Cargo\.toml$/.test(p)).map((p) => dirname(p)).sort(),
    notices: () => Object.keys(files).filter((p) => /\/(NOTICE|THIRD-PARTY\.md)$/.test(p)).sort(),
  };
}

const good = () => ({
  "Cargo.toml": ROOT.replace('version = "0.3.0"', 'version = "0.3.1"'),
  "CHANGELOG.md": "# Changelog\n\n## Unreleased\n\n## v0.3.0 - 2026-01-01\n\n- x\n",
  "LICENSE-MIT": "mit\n",
  "LICENSE-APACHE": "apache\n",
  "crates/a/Cargo.toml": member(),
  "crates/a/LICENSE-MIT": "mit\r\n",
  "crates/a/LICENSE-APACHE": "apache\n",
  "crates/b/Cargo.toml": member("publish = false").replace("carrysheet-a", "carrysheet-b"),
  "crates/b/NOTICE": "b notice",
  "LICENSING.md": "| `crates/b/NOTICE` | b |\n",
});

test("workspaceFacts: version and path dependencies", () => {
  const ws = workspaceFacts(ROOT);
  assert.equal(ws.version, "0.3.1");
  assert.deepEqual(ws.deps, [{ name: "carrysheet-a", version: "0.3.1" }, { name: "carrysheet-b", version: "0.3.0" }]);
});

test("memberFacts: inherited version, publish, license; other tables ignored", () => {
  assert.deepEqual(memberFacts(member()), {
    name: "carrysheet-a", ownVersion: null, inheritsVersion: true, publishable: true, license: "MIT OR Apache-2.0",
  });
  const own = memberFacts(member().replace("version.workspace = true", 'version = "0.1.0"'));
  assert.equal(own.ownVersion, "0.1.0");
  assert.equal(own.inheritsVersion, false);
  assert.equal(memberFacts(member("publish = false")).publishable, false);
  assert.equal(memberFacts(member().replace("version.workspace = true", "version = { workspace = true }")).inheritsVersion, true);
});

test("licenseIds", () => {
  assert.deepEqual(licenseIds("MIT OR Apache-2.0"), ["MIT", "Apache-2.0"]);
  assert.deepEqual(licenseIds("GPL-3.0-or-later"), ["GPL-3.0-or-later"]);
  assert.deepEqual(licenseIds(null), []);
});

test("changelogSection: heading forms and boundaries", () => {
  const text = "# C\n\n## Unreleased\n\n- new\n\n## v0.2.0 - 2026-11-15\n\n- two\n\n## [0.1.0]\n\n- one\n";
  assert.equal(changelogSection(text, "Unreleased"), "- new");
  assert.equal(changelogSection(text, "unreleased"), "- new");
  assert.equal(changelogSection(text, "0.2.0"), "- two");
  assert.equal(changelogSection(text, "0.1.0"), "- one");
  assert.equal(changelogSection(text, "0.0.1"), null);
  assert.equal(changelogSection("## Unreleased\n", "Unreleased"), "");
});

test("tagVersion", () => {
  assert.equal(tagVersion("v0.1.0"), "0.1.0");
  assert.equal(tagVersion("refs/tags/v1.20.3"), "1.20.3");
  assert.equal(tagVersion("0.1.0"), null);
  assert.equal(tagVersion("v0.1"), null);
  assert.equal(tagVersion("v0.1.0-rc.1"), null);
});

test("problems: a clean repository has none", () => {
  assert.deepEqual(problems("", repo(good())), []);
});

test("problems: own version, dependency version, missing and changed license, no Unreleased", () => {
  const files = good();
  files["Cargo.toml"] = ROOT;
  files["crates/b/Cargo.toml"] = files["crates/b/Cargo.toml"].replace("version.workspace = true", 'version = "0.3.0"');
  delete files["crates/a/LICENSE-APACHE"];
  files["crates/a/LICENSE-MIT"] = "../../LICENSE-MIT";
  files["CHANGELOG.md"] = "# Changelog\n\n## v0.3.0\n";
  files["vendor/x/THIRD-PARTY.md"] = "x";
  const what = problems("", repo(files)).map((p) => p.what);
  assert.equal(what.length, 6, what.join("\n"));
  assert.match(what[0], /carrysheet-b asks for version 0\.3\.0/);
  assert.match(what[1], /LICENSE-MIT differs/);
  assert.match(what[2], /LICENSE-APACHE is missing/);
  assert.match(what[3], /crates\/b\/Cargo\.toml \(carrysheet-b\) sets its own version/);
  assert.match(what[4], /LICENSING\.md does not name vendor\/x\/THIRD-PARTY\.md/);
  assert.match(what[5], /no "## Unreleased"/);
});

test("problems: a private crate needs no license files; an unknown license is reported", () => {
  const files = good();
  delete files["crates/a/LICENSE-MIT"];
  delete files["crates/a/LICENSE-APACHE"];
  files["crates/a/Cargo.toml"] = member("publish = false");
  assert.deepEqual(problems("", repo(files)), []);
  files["crates/a/Cargo.toml"] = member().replace("MIT OR Apache-2.0", "BSD-3-Clause");
  assert.match(problems("", repo(files))[0].what, /no file for/);
});

test("the repository itself passes; usage errors exit 2", () => {
  const r = spawnSync(process.execPath, [script, "check"], { encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(spawnSync(process.execPath, [script, "frobnicate"]).status, 2);
  assert.equal(spawnSync(process.execPath, [script, "tag"]).status, 2);
  assert.equal(spawnSync(process.execPath, [script, "tag", "v999.0.0"]).status, 1);
});
