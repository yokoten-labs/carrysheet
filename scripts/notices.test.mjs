// Self-tests for notices.mjs: node --test scripts/notices.test.mjs

import assert from "node:assert/strict";
import test from "node:test";
import { classify, parseTree, registryLicenses, render } from "./notices.mjs";

test("parseTree: names and versions, markers ignored", () => {
  const text = [
    "carrysheet-cli v0.1.0 (/x/apps/carrysheet-cli)",
    "serde v1.0.229",
    "serde v1.0.229 (*)",
    "",
    "gridcraft-core v0.8.0 (/x/vendor/gridcraft/crates/base/core) (*)",
  ].join("\n");
  assert.deepEqual([...parseTree(text)].sort(), ["carrysheet-cli@0.1.0", "gridcraft-core@0.8.0", "serde@1.0.229"]);
});

const metadata = {
  workspace_members: ["path+file:///x/apps/cli#carrysheet-cli@0.1.0"],
  packages: [
    { id: "path+file:///x/apps/cli#carrysheet-cli@0.1.0", name: "carrysheet-cli", version: "0.1.0", source: null },
    { id: "path+file:///x/vendor/g#gridcraft-core@0.8.0", name: "gridcraft-core", version: "0.8.0", source: null },
    { id: "registry+x#serde@1.0.229", name: "serde", version: "1.0.229", source: "registry+x" },
    { id: "registry+x#rmcp@3.5.1", name: "rmcp", version: "3.5.1", source: "registry+x" },
  ],
};

test("classify: own, vendored, registry, unknown", () => {
  const c = classify(new Set(["carrysheet-cli@0.1.0", "gridcraft-core@0.8.0", "serde@1.0.229", "ghost@1.0.0"]), metadata);
  assert.deepEqual(c.own.map((p) => p.name), ["carrysheet-cli"]);
  assert.deepEqual(c.vendored.map((p) => p.name), ["gridcraft-core"]);
  assert.deepEqual(c.registry.map((p) => p.name), ["serde"]);
  assert.deepEqual(c.unknown, ["ghost@1.0.0"]);
});

const lic = (id, source_path, users) => ({
  id, source_path, text: `${id} text`, used_by: users.map(([name, version]) => ({ crate: { name, version } })),
});

test("registryLicenses: keeps shipped users, reports crates with only the standard text", () => {
  const registry = metadata.packages.filter((p) => p.source !== null);
  const about = { licenses: [
    lic("MIT", "/r/serde/LICENSE-MIT", [["serde", "1.0.229"], ["not-shipped", "1.0.0"]]),
    lic("Apache-2.0", null, [["rmcp", "3.5.1"]]),
  ] };
  const ok = registryLicenses(about, registry);
  assert.deepEqual(ok.missing, []);
  assert.deepEqual(ok.sections.map((s) => [s.id, s.users]), [["MIT", ["serde@1.0.229"]], ["Apache-2.0", ["rmcp@3.5.1"]]]);

  const strict = registryLicenses(about, registry, {});
  assert.deepEqual(strict.missing, ["rmcp@3.5.1"], "the standard text alone is not enough");
  const none = registryLicenses({ licenses: [] }, registry);
  assert.deepEqual(none.missing, ["rmcp@3.5.1", "serde@1.0.229"]);
});

test("render: every part, with the notices and license texts", () => {
  const text = render({
    version: "0.1.0",
    own: [{ crate: "carrysheet-biff", file: "crates/carrysheet-biff/NOTICE", text: "Apache POI\r\nCopyright\n\n" }],
    vendored: [{ title: "gridcraft", dir: "vendor/gridcraft", crates: ["gridcraft-core"],
      files: [{ file: "NOTICE", text: "GridCraft notice" }, { file: "LICENSE-MIT", text: "GridCraft MIT" }] }],
    registry: [{ name: "serde", version: "1.0.229", license: "MIT OR Apache-2.0",
      notices: [{ crate: "serde 1.0.229", file: "NOTICE", text: "serde notice" }] }],
    sections: [{ id: "MIT", users: ["serde@1.0.229"], text: "MIT text" }],
  });
  for (const s of ["Carrysheet 0.1.0: third-party notices", "Apache POI\nCopyright", "vendor/gridcraft",
    "GridCraft notice", "GridCraft MIT", "serde 1.0.229  MIT OR Apache-2.0", "serde notice",
    "MIT, used by serde@1.0.229", "MIT text"]) {
    assert.ok(text.includes(s), `missing: ${s}`);
  }
  assert.ok(!text.includes("\r"), "line ends normalized");
});
