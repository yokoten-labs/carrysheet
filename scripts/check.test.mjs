// Self-test for scripts/check.mjs: argument parsing, changed-crate detection,
// scrubbing, output limits, results and the summary. No network, no builds.
// Run: node --test scripts/check.test.mjs

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  BUSY_EXIT, LOCK_TIMEOUT_EXIT, LineTail, STEPS, cacheLine, changedPackages, cleanLine, commandFor, dependents,
  formatTime, headCommit, machineConfig, packageName, parseArgs, quickBase, refusal, run, scrub, scrubHome, selectSteps,
  splitNul, stepResult, stripAnsi, summarize, treeState,
} from "./check.mjs";
import { flockArgs } from "./output.mjs";

const scripts = dirname(fileURLToPath(import.meta.url));
const unix = process.platform !== "win32";
const hasFlock = unix && spawnSync("flock", ["--version"], { stdio: "ignore" }).status === 0;
// A scratch folder, removed after the test: under target/check-test-tmp/ in
// the repository (git-ignored), or under TMPDIR when that is set.
function scratch(t) {
  const base = process.env.TMPDIR ? tmpdir() : join(scripts, "..", "target", "check-test-tmp");
  mkdirSync(base, { recursive: true });
  const dir = mkdtempSync(join(base, "carrysheet-check-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

const node = process.execPath;
const ctx = { home: "/home/alice", names: ["alice"] };

test("parseArgs: defaults and flags", () => {
  assert.deepEqual(parseArgs([], {}), { quick: false, from: null, strict: true, list: false, json: false }, "a full run is strict");
  assert.deepEqual(parseArgs(["--quick", "--json", "--list"], {}), { quick: true, from: null, strict: false, list: true, json: true });
  assert.equal(parseArgs(["--from", "wasm"], {}).strict, false);
  assert.equal(parseArgs(["--from", "wasm"], {}).from, "wasm");
  assert.equal(parseArgs(["--from=test"], {}).from, "test");
  assert.equal(parseArgs(["--quick", "--from", "deny"], {}).from, "deny");
  assert.equal(parseArgs(["--strict"], {}).strict, true);
  assert.equal(parseArgs(["--quick"], { CI: "1" }).strict, true, "CI=1 turns on --strict");
  assert.equal(parseArgs(["--quick"], { CI: "true" }).strict, true);
  assert.equal(parseArgs(["--quick"], { CI: "0" }).strict, false);
});

test("parseArgs: usage errors", () => {
  assert.ok(parseArgs(["--bogus"]).error);
  assert.ok(parseArgs(["fmt"]).error);
  assert.ok(parseArgs(["--from"]).error);
  assert.ok(parseArgs(["--from", "--quick"]).error);
  assert.ok(parseArgs(["--from", "nope"]).error);
  assert.ok(parseArgs(["--quick", "--from", "wasm"]).error, "--quick has no wasm step");
});

test("selectSteps: order, --quick and --from", () => {
  assert.deepEqual(selectSteps(parseArgs([])).map((s) => s.name), STEPS.map((s) => s.name));
  assert.deepEqual(selectSteps(parseArgs(["--quick"])).map((s) => s.name), ["fmt", "clippy", "test", "deny"]);
  assert.deepEqual(selectSteps(parseArgs(["--from", "windows-x86"])).map((s) => s.name), ["windows-x86", "release"]);
  assert.deepEqual(selectSteps(parseArgs(["--quick", "--from", "test"])).map((s) => s.name), ["test", "deny"]);
});

const packages = new Map([
  ["crates/carrysheet-model", "carrysheet-model"],
  ["crates/carrysheet-csv", "carrysheet-csv"],
  ["apps/carrysheet-cli", "carrysheet-cli"],
]);

test("changedPackages: maps files to crates", () => {
  assert.deepEqual(
    changedPackages(["crates/carrysheet-csv/src/lib.rs", "apps/carrysheet-cli/src/main.rs", "crates/carrysheet-csv/tests/a.rs"], packages),
    { all: false, packages: ["carrysheet-cli", "carrysheet-csv"] },
  );
  assert.deepEqual(changedPackages(["crates\\carrysheet-model\\src\\lib.rs"], packages), { all: false, packages: ["carrysheet-model"] });
});

test("changedPackages: docs and scripts change no crate", () => {
  assert.deepEqual(changedPackages(["docs/web-build.md", "scripts/check.mjs", "AGENTS.md"], packages), { all: false, packages: [] });
  assert.deepEqual(changedPackages([], packages), { all: false, packages: [] });
  assert.deepEqual(changedPackages(["crates/gone/src/lib.rs"], packages), { all: false, packages: [] }, "deleted crate");
});

test("changedPackages: workspace files check everything", () => {
  for (const p of ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml", "clippy.toml"]) {
    assert.equal(changedPackages(["crates/carrysheet-csv/src/lib.rs", p], packages).all, true, p);
  }
  assert.equal(changedPackages(["crates/carrysheet-csv/Cargo.toml"], packages).all, false);
});

test("splitNul: quoted, non-ASCII and non-UTF-8 names map to their crate", () => {
  const buf = Buffer.concat([
    Buffer.from('crates/carrysheet-csv/tests/data/"quoted" name.csv\0'),
    Buffer.from("crates/carrysheet-model/src/\u00e4\u00f6\u65e5.rs\0"),
    Buffer.from("apps/carrysheet-cli/tests/line\nbreak.rs\0"),
    Buffer.from("crates/carrysheet-model/"), Buffer.from([0xff, 0xfe]), Buffer.from(".bin\0"),
  ]);
  const paths = splitNul(buf);
  assert.equal(paths.length, 4);
  assert.equal(paths[0], 'crates/carrysheet-csv/tests/data/"quoted" name.csv');
  assert.equal(paths[1], "crates/carrysheet-model/src/\u00e4\u00f6\u65e5.rs");
  assert.ok(paths[3].startsWith("crates/carrysheet-model/"));
  assert.deepEqual(changedPackages(paths, packages), {
    all: false, packages: ["carrysheet-cli", "carrysheet-csv", "carrysheet-model"],
  });
  assert.deepEqual(splitNul(Buffer.alloc(0)), []);
  assert.deepEqual(splitNul(Buffer.from("a\0b")), ["a", "b"], "no trailing NUL");
});

test("dependents: crates that use a changed crate, directly or not", () => {
  const metadata = {
    packages: [
      { name: "carrysheet-model", dependencies: [{ name: "serde", source: "registry+https://github.com/rust-lang/crates.io-index" }] },
      { name: "carrysheet-csv", dependencies: [{ name: "carrysheet-model", path: "x" }] },
      { name: "carrysheet-cli", dependencies: [{ name: "carrysheet-csv", path: "y", kind: "dev" }] },
      { name: "other", dependencies: [] },
    ],
  };
  assert.deepEqual(dependents(["carrysheet-model"], metadata), ["carrysheet-cli", "carrysheet-csv", "carrysheet-model"]);
  assert.deepEqual(dependents(["carrysheet-cli"], metadata), ["carrysheet-cli"]);
});

test("packageName: reads [package] name, not [[bin]] name", () => {
  const manifest = '[[bin]]\nname = "other"\n\n[package]\nname = "carrysheet-cli"\nversion = "0.1.0"\n\n[[bin]]\nname = "x"\n';
  assert.equal(packageName(manifest), "carrysheet-cli");
  assert.equal(packageName('[workspace]\nmembers = []\n'), null);
});

test("scrubHome: hides the home folder", () => {
  assert.equal(scrubHome("error at /home/alice/src/x.rs:1", "/home/alice"), "error at ~/src/x.rs:1");
  assert.equal(scrubHome("/home/alice and /home/alice/b", "/home/alice"), "~ and ~/b");
  assert.equal(scrubHome("C:\\Users\\alice\\x and C:/Users/alice/y", "C:\\Users\\alice"), "~\\x and ~/y");
  assert.equal(scrubHome("/home/bob/x", "/home/alice"), "/home/bob/x");
  assert.equal(scrubHome("/usr/bin", "/"), "/usr/bin", "a root home is left alone");
});

test("scrub: the user name as a whole word, and Windows user folders", () => {
  assert.equal(scrub("/home/alice/x by Alice, alice@host", ctx), "~/x by <user>, <user>@host");
  assert.equal(scrub("malice and alicex and alice_1", ctx), "malice and alicex and alice_1", "whole words only");
  assert.equal(scrub("at /mnt/c/Users/Bob Smith/src/a.rs", ctx), "at /mnt/c/Users/<user>/src/a.rs");
  assert.equal(scrub("at C:\\Users\\bob\\src and D:/users/bob/x", ctx), "at C:\\Users\\<user>\\src and D:/users/<user>/x");
  assert.equal(scrub('panicked at "C:\\\\Users\\\\bob\\\\x.rs"', ctx), 'panicked at "C:\\\\Users\\\\<user>\\\\x.rs"');
  assert.equal(scrub("C:\\Users\\Public\\x", ctx), "C:\\Users\\Public\\x", "shared folders stay");
  assert.equal(scrub("user <user>", { home: "/home/user", names: ["user"] }), "<user> <user>", "a name like the mark");
});

test("cleanLine: a cut line drops a partial home folder or name at the end", () => {
  assert.equal(cleanLine("path /home/al", true, 9999, ctx), "path  [... line cut, 9999 characters]");
  assert.equal(cleanLine("by ali", true, 9999, ctx), "by  [... line cut, 9999 characters]");
  assert.equal(cleanLine("short", false, 5, ctx), "short");
  assert.equal(cleanLine("\x1b[31mred\x1b[m\x07", false, 9, ctx), "red");
});

test("LineTail: bounded lines, bytes and line length", () => {
  const t = new LineTail({ lines: 3, ctx });
  const s = t.stream();
  s.write(Buffer.from("one\ntwo\r\nthr"));
  s.write(Buffer.from("ee\nfour\nfive"));
  s.end();
  assert.deepEqual(t.lines, ["three", "four", "five"]);

  const big = new LineTail({ ctx });
  const sink = big.stream();
  for (let i = 0; i < 100; i++) sink.write(Buffer.from(`${"y".repeat(450)}\n`));
  sink.end();
  assert.ok(big.lines.reduce((n, l) => n + l.length + 1, 0) <= 16 * 1024);

  const split = new LineTail({ ctx });
  const u = split.stream();
  const bytes = Buffer.from("\u65e5\n");
  u.write(bytes.subarray(0, 1));
  u.write(bytes.subarray(1));
  u.end();
  assert.deepEqual(split.lines, ["\u65e5"], "a character split across chunks");
});

test("run: one 10 MB line comes out short", async () => {
  const r = await run([node, ["-e", "process.stdout.write('x'.repeat(10 * 1024 * 1024) + '\\nend\\n')"]], { ctx });
  assert.equal(r.code, 0);
  assert.equal(r.tail.length, 2);
  assert.ok(r.tail[0].length < 600, `line is ${r.tail[0].length} characters`);
  assert.match(r.tail[0], /line cut, 10485760 characters/);
  assert.equal(r.tail[1], "end");
});

test("run: scrubs the output", async () => {
  const r = await run([node, ["-e", "console.error('at /home/alice/x and C:\\\\Users\\\\bob\\\\y by alice')"]], { ctx });
  assert.deepEqual(r.tail, ["at ~/x and C:\\Users\\<user>\\y by <user>"]);
});

test("run: a command killed by a signal fails", async () => {
  const r = await run([node, ["-e", "process.kill(process.pid, 'SIGKILL')"]], { ctx });
  assert.equal(r.signal, "SIGKILL");
  const res = stepResult("test", r, 1);
  assert.equal(res.status, "failed");
  assert.match(res.note, /killed by SIGKILL/);
});

test("run: a command that doesn't exist fails", async () => {
  const r = await run(["carrysheet-no-such-command", []], { ctx });
  assert.equal(r.error, "ENOENT");
  assert.match(r.tail.at(-1), /Could not start carrysheet-no-such-command: ENOENT/);
  const res = stepResult("fmt", r, 0);
  assert.equal(res.status, "failed");
  assert.match(res.note, /could not start/);
});

test("commandFor: heavy outside, build lock inside", () => {
  const m = { heavy: "/opt/heavy", gentle: true, flock: true, nice: true, unix: true, lock: "/l/build.lock", lockWait: 60 };
  assert.deepEqual(commandFor("cargo", ["fmt", "--all"], m), ["/opt/heavy", [
    "env", "LC_ALL=C", "flock", "--verbose", "-w", "60", "-E", String(LOCK_TIMEOUT_EXIT), "/l/build.lock", "env", "-u", "LC_ALL",
    "nice", "-n", "10", "env", "CARGO_BUILD_JOBS=1", "cargo", "fmt", "--all"]]);
  const de = commandFor("cargo", ["fmt"], { ...m, env: { LC_ALL: "de_DE.UTF-8" } })[1];
  assert.deepEqual(de.slice(0, 12), ["env", "LC_ALL=C", "flock", "--verbose", "-w", "60", "-E", "73", "/l/build.lock", "env", "LC_ALL=de_DE.UTF-8", "nice"],
    "flock in English, the step in the caller's locale");
  assert.deepEqual(commandFor("node", ["release-check.mjs", "t"], m), ["/opt/heavy", [node, "release-check.mjs", "t"]]);
  assert.equal(commandFor("light", ["deny", "--version"], m)[0], "env", "probes skip the wrapper");
  const plain = { heavy: null, gentle: true, flock: true, nice: true, unix: true, lock: "/l", lockWait: 5 };
  assert.equal(commandFor("cargo", ["fmt"], plain)[0], "env", "no wrapper unless CARRYSHEET_HEAVY");
  assert.deepEqual(commandFor("node", ["x"], plain), [node, ["x"]]);
  assert.deepEqual(commandFor("cargo", ["fmt"], { heavy: null, flock: false, nice: false, unix: false }), ["cargo", ["fmt"]]);
  const [, plainArgs] = commandFor("cargo", ["fmt"], plain);
  assert.deepEqual(plainArgs.slice(-7), ["nice", "-n", "10", "env", "CARGO_BUILD_JOBS=1", "cargo", "fmt"], "gentle without the wrapper too");
  const [, ciArgs] = commandFor("cargo", ["fmt"], { ...plain, gentle: false });
  assert.ok(!ciArgs.includes("nice") && !ciArgs.includes("CARGO_BUILD_JOBS=1"), "CI: no nice and no job limit");
  assert.deepEqual(ciArgs.slice(-2), ["cargo", "fmt"]);
});

test("machineConfig: one build job and nice unless CI is on", () => {
  const probe = () => true;
  for (const environ of [{ PATH: "/usr/bin" }, { PATH: "/usr/bin", CARRYSHEET_HEAVY: "/opt/heavy" }, { PATH: "/usr/bin", CI: "0", CARGO_BUILD_JOBS: "8" }]) {
    const m = machineConfig(environ, probe);
    assert.equal(m.gentle, true, JSON.stringify(environ));
    assert.equal(m.env.CARGO_BUILD_JOBS, "1", "a bare run on the shared machine stays gentle");
    assert.equal(m.nice, process.platform !== "win32");
  }
  for (const flag of ["1", "true", "TRUE"]) {
    const ci = machineConfig({ PATH: "/usr/bin", CI: flag }, probe);
    assert.equal(ci.gentle, false);
    assert.equal(ci.env.CARGO_BUILD_JOBS, undefined, "CI uses every core");
    assert.equal(ci.nice, false);
  }
  assert.equal(machineConfig({ PATH: "/usr/bin", CI: "1", CARGO_BUILD_JOBS: "3" }, probe).env.CARGO_BUILD_JOBS, "3", "CI keeps its own setting");
});

test("stepResult: exit 75 is busy only when the wrapper says it ran nothing", async () => {
  const refused = "console.error('heavy: 500 MB available after 900s, want 900; nothing was run: cargo fmt')";
  const r = await run([node, ["-e", `${refused}; process.exit(${BUSY_EXIT})`]], { ctx });
  const busy = stepResult("clippy", r, 1, { heavy: "/opt/heavy" });
  assert.equal(busy.status, "busy");
  assert.equal(stepResult("clippy", r, 1, {}).status, "failed", "75 without a wrapper is a failure");
  const own = await run([node, ["-e", `console.error('error: real step'); process.exit(${BUSY_EXIT})`]], { ctx });
  assert.equal(stepResult("clippy", own, 1, { heavy: "/opt/heavy" }).status, "failed", "the step's own exit 75");
  // The heavy wrapper's chatter while it waits does not make a step busy.
  const waited = { code: BUSY_EXIT, tail: ["heavy: another job has the machine (load 3); waiting for it (at most 1800s)"] };
  assert.equal(stepResult("clippy", waited, 1, { heavy: "/opt/heavy" }).status, "failed");
  const s = summarize([{ name: "fmt", status: "ok", seconds: 1 }, busy], parseArgs([], {}), same("abc"), same(""));
  assert.equal(s.result, "busy");
  assert.equal(s.ok, false);
  assert.match(s.summary, /BUSY at clippy/);
  assert.ok(s.report.some((l) => l.startsWith("Why: the machine was busy")));
  assert.ok(s.report.some((l) => l.startsWith("Fix: retry later: node scripts/check.mjs --from clippy")));
});

const same = (v) => ({ start: v, end: v });
const ok = (name) => ({ name, status: "ok", seconds: 1 });
const skipped = (name) => ({ name, status: "skipped", seconds: 0, note: "not installed" });

test("summarize: passed, with the skip count", () => {
  const s = summarize([ok("fmt"), skipped("deny")], parseArgs([], {}), same("0123456789abcdef"), same(""), 1500);
  assert.equal(s.result, "passed");
  assert.equal(s.json.skipped, 1);
  assert.equal(s.json.dirty, false);
  assert.equal(s.summary, "check.mjs: passed (1 ok, 1 skipped, 0 failed) on 0123456789ab, clean tree, 1.5s");
});

test("summarize: all skipped is not passed", () => {
  const s = summarize([skipped("test"), skipped("deny")], parseArgs(["--quick", "--from", "test"], {}), same("abc"), same(" M x"));
  assert.equal(s.result, "nothing checked");
  assert.equal(s.ok, false);
  assert.equal(s.json.skipped, 2);
  assert.match(s.summary, /^check.mjs --quick --from test: NOTHING CHECKED/);
  assert.equal(summarize([], parseArgs([], {}), same("abc"), same("")).ok, false);
});

test("summarize: --from and --quick together resume with both", () => {
  const failed = { name: "test", status: "failed", seconds: 2, exitCode: 101, tail: ["boom"] };
  const s = summarize([failed], parseArgs(["--quick", "--from", "test"], {}), same("abc"), same(""));
  assert.equal(s.result, "failed");
  assert.match(s.summary, /^check.mjs --quick --from test: FAILED at test/);
  assert.ok(s.report.includes("  boom"));
  assert.ok(s.report.some((l) => l.endsWith("node scripts/check.mjs --quick --from test")));
});

test("summarize: failed tests and panics before the last lines are shown", async () => {
  // A harness = false test on stderr, a libtest panic on stdout, then more
  // than the tail holds on each.
  const script = [
    "console.error('test a_killed_worker_comes_back ... FAILED');",
    "console.error('no memory cap');",
    "console.log(\"thread 'x::y' panicked at src/x.rs:1:1:\");",
    "console.log('boom in /home/alice/secret.xlsx');",
    "for (let i = 0; i < 100; i++) { console.log('ok ' + i); console.error('fine ' + i); }",
    "process.exit(1);",
  ].join("\n");
  const ran = await run([node, ["-e", script]], { ctx });
  const failed = stepResult("test", ran, 1);
  const s = summarize([failed], parseArgs([], {}), same("abc"), same(""));
  const at = s.report.indexOf("Failed tests and panics in test, from before the last lines:");
  assert.ok(at >= 0, s.report.join("\n"));
  const shown = s.report.slice(at + 1, s.report.indexOf("", at + 1));
  for (const line of [
    "  test a_killed_worker_comes_back ... FAILED",
    "  no memory cap",
    "  thread 'x::y' panicked at src/x.rs:1:1:",
    "  boom in ~/secret.xlsx",
  ]) {
    assert.ok(shown.includes(line), `${line} in:\n${shown.join("\n")}`);
  }
  // Each stream keeps 3 lines after its failure line, and nothing past that.
  assert.ok(shown.length <= 8, shown.join("\n"));
  assert.ok(!shown.includes("  ok 99") && !shown.includes("  fine 99"));
  // Nothing scrolled out: no extra section.
  const quiet = summarize([stepResult("test", { code: 1, tail: ["test t ... FAILED"], kept: ["test t ... FAILED"] }, 1)], parseArgs([], {}), same("abc"), same(""));
  assert.ok(!quiet.report.some((l) => l.startsWith("Failed tests and panics")));
});

test("summarize: a tree that changes during the run fails", () => {
  const steps = [ok("fmt"), ok("clippy")];
  const head = summarize(steps, parseArgs([], {}), { start: "aaa", end: "bbb" }, same(""));
  assert.equal(head.result, "failed");
  assert.match(head.summary, /FAILED: tree changed during the run/);
  assert.equal(head.json.treeChanged, true);
  const edit = summarize(steps, parseArgs(["--quick"], {}), same("aaa"), { start: "", end: " M crates/x/src/lib.rs" });
  assert.equal(edit.result, "failed");
  assert.equal(summarize(steps, parseArgs([], {}), same("aaa"), same("")).result, "passed");
});

test("cacheLine: sccache hit rate between two readings", () => {
  const stats = (hits, misses) => ({ stats: { cache_hits: { counts: { Rust: hits } }, cache_misses: { counts: { Rust: misses, "C/C++": 1 } } } });
  assert.match(cacheLine(stats(10, 20), stats(40, 30)), /^sccache: 75% hits \(30 hits, 10 misses/);
  assert.equal(cacheLine(stats(10, 20), stats(10, 20)), null, "no compiles");
  assert.equal(cacheLine(stats(10, 20), stats(1, 1)), null, "server restarted");
  assert.equal(cacheLine(null, stats(1, 1)), null, "no sccache");
});

test("formatTime", () => {
  assert.equal(formatTime(1234), "1.2s");
  assert.equal(formatTime(125_000), "2m05s");
});

test("stripAnsi: removes colour codes", () => {
  assert.equal(stripAnsi("\x1b[31m-pub fn x\x1b(B\x1b[m"), "-pub fn x");
  assert.equal(stripAnsi("plain"), "plain");
});

test("stepResult: exit 73 is busy only when flock timed out", () => {
  const timedOut = { code: LOCK_TIMEOUT_EXIT, tail: ["flock: timeout while waiting to get lock"] };
  assert.equal(stepResult("test", timedOut, 1, { flock: true }).status, "busy");
  assert.equal(stepResult("test", { code: LOCK_TIMEOUT_EXIT, tail: ["error: something"] }, 1, { flock: true }).status, "failed");
  assert.equal(stepResult("test", { code: 1, tail: ["flock: timeout while waiting to get lock"] }, 1, {}).status, "failed");
});

test("run: flock's lock-timeout message makes a real 73 busy, and its chatter is dropped", { skip: !hasFlock }, async (t) => {
  const dir = scratch(t);
  const lock = join(dir, "build.lock");
  const m = { heavy: null, flock: true, nice: false, unix: true, lock, lockWait: 1 };
  // The command itself exits 73: flock ran it, so it is a failure.
  // A locale that would translate flock's messages; flock still runs with LC_ALL=C.
  const env = { ...process.env, LC_ALL: "de_DE.UTF-8", LANG: "de_DE.UTF-8", LANGUAGE: "de" };
  const [first, ...rest] = flockArgs(m, env);
  const own = await run([first, [...rest, node, "-e", "process.exit(73)"]], { ctx, env });
  assert.equal(own.code, LOCK_TIMEOUT_EXIT);
  assert.ok(!own.tail.some((l) => l.startsWith("flock:")), own.tail.join("\n"));
  assert.equal(stepResult("test", own, 1, m).status, "failed");
  // Someone else holds the lock: flock gives up, and says so.
  const holder = spawn("flock", [lock, "sleep", "3"], { stdio: "ignore" });
  t.after(() => holder.kill());
  await new Promise((r) => setTimeout(r, 500));
  const waited = await run([first, [...rest, "true"]], { ctx, env });
  assert.ok(waited.tail.includes("flock: timeout while waiting to get lock"), waited.tail.join("\n"));
  assert.equal(stepResult("test", waited, 1, m).status, "busy");
});

test("summarize: git that can't report HEAD or the tree counts as a change", () => {
  const steps = [ok("fmt")];
  for (const [commit, tree] of [
    [same(null), same("")], [same("a"), same(null)], [{ start: "a", end: null }, same("")], [same("a"), { start: null, end: "" }],
  ]) {
    const s = summarize(steps, parseArgs(["--quick"], {}), commit, tree);
    assert.equal(s.result, "failed", JSON.stringify({ commit, tree }));
    assert.equal(s.ok, false);
    assert.ok(s.report.some((l) => l.includes("git could not report")));
  }
});

test("refusal: a full run needs a known, clean tree", () => {
  const full = parseArgs([], {});
  assert.equal(refusal(full, "abc", ""), null);
  assert.match(refusal(full, "abc", "dirty 123").what, /dirty tree/);
  assert.match(refusal(full, null, "").what, /git could not report/);
  assert.match(refusal(full, "abc", null).what, /git could not report/);
  for (const r of [refusal(full, "abc", "dirty 1"), refusal(full, null, null)]) assert.ok(r.why && r.fix, "three parts");
  assert.equal(refusal(parseArgs(["--quick"], {}), null, null), null, "--quick is not refused");
  assert.equal(refusal(parseArgs(["--from", "wasm"], {}), "abc", "dirty 1"), null);
});

// A small git repository with one commit.
function repo(t) {
  const dir = scratch(t);
  const git = (...args) => {
    const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", ...args], { cwd: dir, encoding: "utf8" });
    assert.equal(r.status, 0, r.stderr);
  };
  git("init", "-q");
  writeFileSync(join(dir, "a.txt"), "a\n");
  git("add", "a.txt");
  git("commit", "-q", "-m", "one");
  return { dir, git };
}

test("quickBase: origin/main when it exists, else origin/foundation", (t) => {
  const { dir, git } = repo(t);
  assert.equal(quickBase(dir), "origin/foundation", "neither: the old base (no merge base, whole workspace)");
  git("update-ref", "refs/remotes/origin/foundation", "HEAD");
  assert.equal(quickBase(dir), "origin/foundation");
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  assert.equal(quickBase(dir), "origin/main", "the same commit: main");
  git("commit", "-q", "--allow-empty", "-m", "two");
  git("update-ref", "refs/remotes/origin/foundation", "HEAD");
  assert.equal(quickBase(dir), "origin/foundation", "an old main that foundation contains: foundation");
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  git("commit", "-q", "--allow-empty", "-m", "three");
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  assert.equal(quickBase(dir), "origin/main", "main ahead of foundation: main");
  git("update-ref", "-d", "refs/remotes/origin/foundation");
  assert.equal(quickBase(dir), "origin/main", "only main (the public repository)");
});

test("treeState: clean, dirty, and git that can't tell", (t) => {
  const { dir } = repo(t);
  assert.equal(treeState(dir), "");
  assert.match(headCommit(dir), /^[0-9a-f]{40}$/);
  writeFileSync(join(dir, "a.txt"), "b\n");
  const one = treeState(dir);
  assert.match(one, /^dirty [0-9a-f]{64}$/);
  writeFileSync(join(dir, "new.txt"), "n\n");
  assert.notEqual(treeState(dir), one, "a new file changes the state");
  const broken = { ...process.env, GIT_DIR: join(dir, "nonexistent") };
  assert.equal(treeState(dir, broken), null);
  assert.equal(headCommit(dir, broken), null);
  assert.equal(refusal(parseArgs([], {}), headCommit(dir, broken), treeState(dir, broken)).what.includes("git could not report"), true);
});

test("treeState: untracked files count even with status.showUntrackedFiles=no", (t) => {
  const { dir, git } = repo(t);
  git("config", "status.showUntrackedFiles", "no");
  mkdirSync(join(dir, "tests"));
  writeFileSync(join(dir, "tests", "extra.rs"), "#[test] fn x() {}\n");
  assert.match(treeState(dir), /^dirty /);
});

test("treeState: 5,000 untracked files (over 1 MB of status) is dirty, not unknown", (t) => {
  const { dir } = repo(t);
  const deep = join(dir, "u".repeat(100), "v".repeat(100));
  mkdirSync(deep, { recursive: true });
  for (let i = 0; i < 5000; i++) writeFileSync(join(deep, `file-${String(i).padStart(5, "0")}.txt`), "");
  const state = treeState(dir);
  assert.match(state, /^dirty /);
  assert.match(refusal(parseArgs([], {}), headCommit(dir), state).what, /dirty tree/);
});

test("release-check: a 300 MB line from cargo stays bounded", { skip: !unix, timeout: 120_000 }, async (t) => {
  const dir = scratch(t);
  mkdirSync(join(dir, "cargo", "bin"), { recursive: true });
  const cargo = join(dir, "cargo", "bin", "cargo");
  writeFileSync(cargo, "#!/bin/sh\nhead -c 300000000 /dev/zero | tr '\\0' x\necho\necho build done\n");
  chmodSync(cargo, 0o755);
  const env = {
    ...process.env, CARGO_HOME: join(dir, "cargo"), CARGO_TARGET_DIR: join(dir, "target"),
    CARRYSHEET_BUILD_LOCK: join(dir, "build.lock"), CARRYSHEET_LOCK_WAIT: "5",
  };
  // 48 MB of heap: holding the line whole (300 million characters) would crash.
  const r = await run([node, ["--max-old-space-size=48", join(scripts, "release-check.mjs"), "x86_64-unknown-linux-gnu"]], { env, ctx });
  assert.equal(r.code, 1, r.tail.join("\n"));
  assert.ok(r.tail.some((l) => /x+ \[\.\.\. line cut, 300000000 characters\]$/.test(l)), r.tail.join("\n"));
  assert.ok(r.tail.every((l) => l.length < 600));
  assert.ok(r.tail.includes("build done"));
  assert.ok(r.tail.some((l) => l.startsWith("No binary at")), "went on past the build");
});

test("release-check: nice and one build job unless CI is on", { skip: !hasFlock }, async (t) => {
  const own = Number(spawnSync("nice", [], { encoding: "utf8" }).stdout.trim());
  const dir = scratch(t);
  mkdirSync(join(dir, "cargo", "bin"), { recursive: true });
  const cargo = join(dir, "cargo", "bin", "cargo");
  writeFileSync(cargo, "#!/bin/sh\necho \"niceness $(nice) jobs [${CARGO_BUILD_JOBS}]\"\n");
  chmodSync(cargo, 0o755);
  const base = { ...process.env, CARGO_HOME: join(dir, "cargo"), CARGO_TARGET_DIR: join(dir, "target"),
    CARRYSHEET_BUILD_LOCK: join(dir, "build.lock"), CARRYSHEET_LOCK_WAIT: "5" };
  delete base.CARGO_BUILD_JOBS;
  delete base.CI;
  const go = (env) => run([node, [join(scripts, "release-check.mjs"), "x86_64-unknown-linux-gnu"]], { env, ctx });
  const line = (r) => r.tail.find((l) => l.startsWith("niceness ")) ?? r.tail.join("\n");
  const shared = line(await go(base));
  assert.equal(shared, `niceness ${Math.min(own + 10, 19)} jobs [1]`, "by default: nice, one job");
  const ci = line(await go({ ...base, CI: "1" }));
  assert.equal(ci, `niceness ${own} jobs []`, "CI: normal priority, every core");
});

test("refusal: a full run refuses CARRYSHEET_BUILD_LOCK", () => {
  const r = refusal(parseArgs([], {}), "abc", "", { customLock: true });
  assert.match(r.what, /CARRYSHEET_BUILD_LOCK/);
  assert.ok(r.why && r.fix, "three parts");
  assert.equal(refusal(parseArgs(["--quick"], {}), "abc", "", { customLock: true }), null, "partial runs may use it");
  assert.equal(refusal(parseArgs([], {}), "abc", "", { customLock: false }), null);
});

test("summarize: the resume hint keeps --strict", () => {
  const failed = { name: "deny", status: "failed", seconds: 0, exitCode: null, note: "--strict turns a skip into a failure: x", tail: [] };
  const partial = summarize([failed], parseArgs(["--quick", "--strict"], {}), same("abc"), same(""));
  assert.ok(partial.report.some((l) => l.endsWith("node scripts/check.mjs --quick --from deny --strict")), partial.report.join("\n"));
  const full = summarize([failed], parseArgs([], {}), same("abc"), same(""));
  assert.ok(full.report.some((l) => l.endsWith("node scripts/check.mjs --from deny --strict")), "a full run is strict, so its resume is too");
});

// check.mjs reads exit 75 as "busy" only next to heavy's "nothing was run"
// line. If this machine has ~/bin/heavy, make sure every `exit 75` in it still
// prints that phrase, so a change of wording fails here instead of quietly
// turning BUSY into FAILED. Read only: running it would log and start sccache.
const heavyPath = process.env.HOME ? join(process.env.HOME, "bin", "heavy") : null;
test("~/bin/heavy still says \"nothing was run\" before exit 75", { skip: !heavyPath || !existsSync(heavyPath) }, () => {
  const lines = readFileSync(heavyPath, "utf8").split("\n");
  const exits = lines.flatMap((l, i) => (/\bexit 75\b/.test(l) && !l.trim().startsWith("#") ? [i] : []));
  assert.ok(exits.length > 0, "heavy has an exit 75");
  for (const i of exits) {
    const before = lines.slice(Math.max(0, i - 4), i).join("\n");
    assert.match(before, /echo "heavy: .*nothing was run/, `exit 75 on line ${i + 1} without the "nothing was run" line`);
  }
});

test("release-check: a float ending in @ before a string is not an email address", (t) => {
  const dir = scratch(t);
  const release = join(dir, "target", "x86_64-unknown-linux-gnu", "release");
  mkdirSync(release, { recursive: true });
  const binary = join(release, "carrysheet-cli");
  const check = (bytes) => {
    writeFileSync(binary, bytes);
    return spawnSync(node, [join(scripts, "release-check.mjs"), "x86_64-unknown-linux-gnu", "--no-build"], {
      encoding: "utf8", env: { ...process.env, CARGO_TARGET_DIR: join(dir, "target") },
    });
  };
  // 24.0 as a little-endian f64 ends in "8@", and the next string follows at once.
  const float = Buffer.alloc(8);
  float.writeDoubleLE(24);
  const near = check(Buffer.concat([Buffer.alloc(16), float, Buffer.from("parsed.formula_reference"), Buffer.alloc(16)]));
  assert.equal(near.status, 0, near.stdout + near.stderr);
  assert.match(near.stdout, /Privacy check: clean/);

  const real = check(Buffer.concat([Buffer.alloc(16), Buffer.from("jo@example.org"), Buffer.alloc(16)]));
  assert.equal(real.status, 1, real.stdout + real.stderr);
  assert.match(real.stdout, /1 x email address/);

  const ours = check(Buffer.concat([Buffer.alloc(16), Buffer.from("support@carrysheet.com"), Buffer.alloc(16)]));
  assert.equal(ours.status, 0, ours.stdout + ours.stderr);
});
