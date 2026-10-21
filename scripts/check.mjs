#!/usr/bin/env node
// Run every check a merge needs, in order, and stop at the first failure:
// format, clippy, tests, cargo-deny, the zip reader on both inflaters, the
// browser build, the Windows builds and the release privacy check. Prints
// one line per step and the commit checked. No dependencies.
//
// Usage: node scripts/check.mjs [--quick] [--from <step>] [--strict] [--list] [--json]
//   --quick         fmt, clippy and tests for the crates changed against
//                   origin/main (origin/foundation when there is no
//                   origin/main) and the crates that depend on them,
//                   and cargo-deny
//   --from <step>   start at this step (resume after a fix); see --list
//   --strict        a skipped step (missing tool or target) fails the run;
//                   on when CI=1 or CI=true, and always on for a full run.
//                   Under --strict, --quick with no crate changed is still a
//                   skip; such a run never counts for a merge.
//   --list          print the steps and exit
//   --json          print only a JSON summary on stdout
//
// Exit codes: 0 passed, 1 a step failed, the machine was busy, nothing was
// checked, or the tree changed during the run; 2 usage.
// Only a full run (no --quick, no --from) on a clean tree counts for a merge;
// a full run on a dirty tree is refused.
//
// Every cargo step takes the build lock (~/.cache/carrysheet-build.lock) and
// runs at low priority (nice -n 10) with one build job, because the
// development machine is shared; only in CI (CI=1 or CI=true) does cargo use
// every core at normal priority.
// CARRYSHEET_HEAVY=<path> wraps each heavy step in that command (outside the
// build lock); its exit code 75 means "machine busy, nothing ran".
// CARRYSHEET_LOCK_WAIT=<seconds> is how long to wait for the build lock (1800);
// CARRYSHEET_BUILD_LOCK=<path> uses another lock file: for tests and partial
// runs only; a full run refuses it.
// If sccache is on PATH, the last line is the compile-cache hit rate.
// Output never shows the home folder or the user name.
// Self-test: node --test scripts/check.test.mjs

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { homedir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { LineTail, buildLock, ciOn, flockArgs, isFlockChatter, lockTimedOut, scrub } from "./output.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const home = homedir();
// The branch --quick compares against: origin/main once it exists (the
// public repository), else origin/foundation.
const BASES = ["origin/main", "origin/foundation"];
export const BUSY_EXIT = 75; // CARRYSHEET_HEAVY: machine busy, nothing ran
// What ~/bin/heavy prints when it gives up; only then is its exit 75 "busy".
const HEAVY_REFUSED = /^heavy: .*nothing was run/;
const USAGE = "Usage: node scripts/check.mjs [--quick] [--from <step>] [--strict] [--list] [--json]";

// The steps, in order. `quick` marks the ones --quick runs.
export const STEPS = [
  { name: "fmt", what: "cargo fmt --all --check", quick: true },
  { name: "clippy", what: "cargo clippy --workspace --all-targets --locked -- -D warnings", quick: true },
  { name: "test", what: "cargo test --workspace --locked", quick: true },
  { name: "deny", what: "cargo deny --locked check", quick: true },
  // The zip reader's tests on each inflater flate2 can link: the CLI ships
  // with miniz_oxide, while the workspace test run unifies to zlib-rs.
  // Each feature makes the tests check that they got the one named.
  { name: "zip-miniz", what: "cargo test -p carrysheet-zip --features test-miniz --locked", quick: false },
  { name: "zip-zlib-rs", what: "cargo test -p carrysheet-zip --features test-zlib-rs --locked", quick: false },
  { name: "wasm", what: "cargo wasm-check (libraries for wasm32-unknown-unknown)", quick: false },
  { name: "windows-x64", what: "cargo build --workspace --locked --target x86_64-pc-windows-gnu", quick: false },
  { name: "windows-x86", what: "cargo build --workspace --locked --target i686-pc-windows-gnu", quick: false },
  { name: "release", what: "node scripts/release-check.mjs x86_64-unknown-linux-gnu", quick: false },
];


// Parse the command line. Returns options, or { error } for a usage error.
export function parseArgs(argv, environ = process.env) {
  const opts = { quick: false, from: null, strict: ciOn(environ), list: false, json: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--quick") opts.quick = true;
    else if (a === "--list") opts.list = true;
    else if (a === "--json") opts.json = true;
    else if (a === "--strict") opts.strict = true;
    else if (a === "--from" || a.startsWith("--from=")) {
      const value = a === "--from" ? argv[++i] : a.slice("--from=".length);
      if (!value || value.startsWith("--")) return { error: "--from needs a step name (see --list)." };
      opts.from = value;
    } else return { error: `Unknown argument: ${a}` };
  }
  if (opts.from !== null) {
    const step = STEPS.find((s) => s.name === opts.from);
    if (!step) return { error: `No step named "${opts.from}". Steps: ${STEPS.map((s) => s.name).join(", ")}.` };
    if (opts.quick && !step.quick) return { error: `--quick does not run "${opts.from}".` };
  }
  // The run that counts for a merge is always strict.
  if (!opts.quick && opts.from === null) opts.strict = true;
  return opts;
}

// The steps a run covers, given the options.
export function selectSteps(opts) {
  const steps = STEPS.filter((s) => !opts.quick || s.quick);
  const start = opts.from ? steps.findIndex((s) => s.name === opts.from) : 0;
  return steps.slice(start);
}

export { LOCK_TIMEOUT_EXIT, LineTail, cleanLine, scrub, scrubHome, stripAnsi, userNames } from "./output.mjs";

// The package name in a Cargo.toml: the `name` key of its [package] table.
export function packageName(manifest) {
  let inPackage = false;
  for (const raw of manifest.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith("[")) inPackage = line === "[package]";
    else if (inPackage) {
      const m = line.match(/^name\s*=\s*"([^"]+)"/);
      if (m) return m[1];
    }
  }
  return null;
}

// Workspace member folders ("crates/x", "apps/y") mapped to package names.
function workspacePackages() {
  const map = new Map();
  for (const group of ["crates", "apps"]) {
    const dir = join(root, group);
    if (!existsSync(dir)) continue;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const manifest = join(dir, entry.name, "Cargo.toml");
      if (!entry.isDirectory() || !existsSync(manifest)) continue;
      const name = packageName(readFileSync(manifest, "utf8"));
      if (name) map.set(`${group}/${entry.name}`, name);
    }
  }
  return map;
}

// Files that change how every crate builds; a change to one checks them all.
const WORKSPACE_FILES = [
  /^Cargo\.(toml|lock)$/, /^\.cargo\//, /^rust-toolchain(\.toml)?$/, /^(\.)?(clippy|rustfmt)\.toml$/,
];

// From changed paths, which packages to check: { all: true } when a
// workspace-wide file changed, else the sorted package names (maybe none).
export function changedPackages(paths, packages) {
  const names = new Set();
  for (const p of paths.map((x) => x.replaceAll("\\", "/"))) {
    if (WORKSPACE_FILES.some((re) => re.test(p))) return { all: true, packages: [] };
    const m = p.match(/^((?:crates|apps)\/[^/]+)\//);
    if (m && packages.has(m[1])) names.add(packages.get(m[1]));
  }
  return { all: false, packages: [...names].sort() };
}

// The packages to check for a change to `names`: those, and every workspace
// package that depends on one of them (directly or not, any dependency kind),
// from `cargo metadata --no-deps` output.
export function dependents(names, metadata) {
  const members = new Set(metadata.packages.map((p) => p.name));
  const users = new Map();
  for (const p of metadata.packages) {
    for (const d of p.dependencies ?? []) {
      if (!members.has(d.name) || d.source) continue;
      if (!users.has(d.name)) users.set(d.name, new Set());
      users.get(d.name).add(p.name);
    }
  }
  const out = new Set(names);
  const queue = [...names];
  while (queue.length > 0) {
    for (const user of users.get(queue.shift()) ?? []) {
      if (!out.has(user)) { out.add(user); queue.push(user); }
    }
  }
  return [...out].sort();
}

// NUL-separated git output (-z) as paths. Paths come out unquoted, whatever
// bytes they hold; bytes that are not UTF-8 become U+FFFD, and the folder
// part, which decides the crate, stays as it is.
export function splitNul(buf) {
  const out = [];
  let start = 0;
  for (let i = buf.indexOf(0); i !== -1; i = buf.indexOf(0, start)) {
    if (i > start) out.push(buf.subarray(start, i).toString("utf8"));
    start = i + 1;
  }
  if (start < buf.length) out.push(buf.subarray(start).toString("utf8"));
  return out;
}

function git(args, cwd = root, env = process.env) {
  const r = spawnSync("git", args, { cwd, env, encoding: "utf8", maxBuffer: 64 << 20 });
  return r.status === 0 ? r.stdout.trim() : null;
}

// The branch --quick compares against: origin/main when git knows it, else
// origin/foundation (when neither is there, the merge base fails and --quick
// checks the whole workspace). An origin/main that origin/foundation already
// contains is an old main from before the public repository, so foundation
// stays the base while it is the newer of the two.
export function quickBase(cwd = root, env = process.env) {
  const [main, foundation] = BASES;
  const has = (b) => git(["rev-parse", "--verify", "--quiet", `${b}^{commit}`], cwd, env) !== null;
  if (!has(main)) return foundation;
  if (has(foundation) && git(["rev-parse", main], cwd, env) !== git(["rev-parse", foundation], cwd, env)
    && git(["merge-base", "--is-ancestor", main, foundation], cwd, env) !== null) return foundation;
  return main;
}

// HEAD, or null if git can't tell.
export function headCommit(cwd = root, env = process.env) {
  return git(["rev-parse", "HEAD"], cwd, env) || null;
}

// The working tree: "" when clean, "dirty <hash of git status>" when not,
// null when git can't tell. Untracked files count whatever the git config
// says (cargo compiles an untracked tests/*.rs too).
export function treeState(cwd = root, env = process.env) {
  // Over 32 MB of status is "unknown", which fails closed.
  const r = spawnSync("git", ["status", "--porcelain", "-z", "--untracked-files=normal"], { cwd, env, maxBuffer: 32 << 20 });
  if (r.error || r.status !== 0) return null;
  return r.stdout.length === 0 ? "" : `dirty ${createHash("sha256").update(r.stdout).digest("hex")}`;
}

// Why a run must not start, as { what, why, fix }, or null. Only a full run
// (no --quick, no --from) is held to this: it is the one that counts for a merge.
// `m` is machineConfig() (only customLock is read).
export function refusal(opts, commit, tree, m = {}) {
  if (opts.quick || opts.from !== null) return null;
  if (m.customLock) {
    return {
      what: "check.mjs: refusing a full run with CARRYSHEET_BUILD_LOCK set.",
      why: "a run that counts for a merge must take the shared build lock, so no other build runs beside it.",
      fix: "unset CARRYSHEET_BUILD_LOCK (it is for tests and partial runs), then run it again.",
    };
  }
  if (commit === null || tree === null) {
    return {
      what: "check.mjs: refusing a full run: git could not report the commit or the working tree.",
      why: "a run that counts for a merge must record one commit and a clean tree.",
      fix: "run it inside the repository with git working (try git status), then run it again.",
    };
  }
  if (tree) {
    return {
      what: "check.mjs: refusing a full run on a dirty tree.",
      why: "only a full run on a clean tree counts for a merge, and uncommitted changes are not in the commit it records.",
      fix: "commit or stash your changes (git status lists them) and run it again; use --quick while you work.",
    };
  }
  return null;
}

function gitPaths(args) {
  const r = spawnSync("git", args, { cwd: root, maxBuffer: 64 << 20 });
  return r.status === 0 ? splitNul(r.stdout) : null;
}

// Paths changed since the merge base with `branch`: commits, staged,
// unstaged and untracked files. null when the base is unknown.
function changedPaths(branch) {
  const base = git(["merge-base", "HEAD", branch]);
  if (!base) return null;
  const diff = gitPaths(["diff", "--name-only", "--no-renames", "-z", base]);
  const untracked = gitPaths(["ls-files", "--others", "--exclude-standard", "-z"]);
  if (diff === null || untracked === null) return null;
  return [...diff, ...untracked];
}

function hasCommand(name, args = ["--version"], env = process.env) {
  return spawnSync(name, args, { stdio: "ignore", env }).status === 0;
}

const expandHome = (p) => (p === "~" ? home : p.startsWith("~/") ? join(home, p.slice(2)) : p);

// How commands run on this machine: lock, niceness, wrapper, environment.
// One build job and low priority (`gentle`) everywhere except in CI.
export function machineConfig(environ = process.env, probe = hasCommand) {
  const unix = process.platform !== "win32";
  const cargoHome = environ.CARGO_HOME || join(home, ".cargo");
  const heavy = environ.CARRYSHEET_HEAVY ? expandHome(environ.CARRYSHEET_HEAVY) : null;
  const env = { ...environ, PATH: [join(cargoHome, "bin"), environ.PATH].join(delimiter) };
  const gentle = !ciOn(environ);
  if (gentle) env.CARGO_BUILD_JOBS = "1";
  return {
    unix,
    env,
    heavy,
    gentle,
    flock: unix && probe("flock"),
    nice: gentle && unix && probe("nice", ["true"]),
    ...buildLock(environ),
    hasCargo: probe("cargo", ["--version"], env),
    sccache: probe("sccache", ["--version"], env),
  };
}

// The command line for one run. kind "cargo": a heavy cargo step,
//   <heavy> flock <lock> nice -n 10 env CARGO_BUILD_JOBS=1 cargo ...
// (in CI, `gentle` false: no nice and no job limit);
// "light": the same without the heavy wrapper (probes, metadata);
// "node": <heavy> node ... (release-check.mjs takes the build lock itself).
// Always heavy outside, build lock inside, so neither waits holding the other.
export function commandFor(kind, args, m) {
  const parts = [];
  if (m.heavy && kind !== "light") parts.push(m.heavy);
  if (kind === "node") parts.push(process.execPath, ...args);
  else {
    if (m.flock) parts.push(...flockArgs(m, m.env ?? {}));
    if (m.gentle && m.nice) parts.push("nice", "-n", "10");
    if (m.gentle && m.unix) parts.push("env", "CARGO_BUILD_JOBS=1");
    parts.push("cargo", ...args);
  }
  return [parts[0], parts.slice(1)];
}

// Say so when another build holds the lock, so a wait doesn't look like a hang.
function announceLock(m) {
  if (!m.flock) return;
  mkdirSync(dirname(m.lock), { recursive: true });
  if (spawnSync("flock", ["-n", m.lock, "true"], { stdio: "ignore" }).status !== 0) {
    process.stderr.write(`check.mjs: waiting for the build lock (${scrub(m.lock)}); another build holds it. `
      + `Giving up after ${m.lockWait}s.\n`);
  }
}

function installedTargets(m) {
  const r = spawnSync("rustup", ["target", "list", "--installed"], { encoding: "utf8", env: m.env });
  return r.status === 0 ? r.stdout.split("\n").map((s) => s.trim()) : null;
}

const NO_CARGO = {
  fail: "cargo not found",
  fix: "install Rust (https://rustup.rs) or put ~/.cargo/bin on PATH",
};

// Crates that depend on `names`, from cargo metadata under the build lock.
function withDependents(names, m, notes) {
  announceLock(m);
  const [cmd, args] = commandFor("light", ["metadata", "--no-deps", "--format-version", "1"], m);
  const r = spawnSync(cmd, args, { cwd: root, env: m.env, encoding: "utf8", maxBuffer: 64 << 20 });
  try {
    if (r.status !== 0) throw new Error("cargo metadata failed");
    return dependents(names, JSON.parse(r.stdout));
  } catch {
    notes.push("--quick: could not read cargo metadata; crates that depend on the changed ones are not checked.");
    return names;
  }
}

// What each step runs: [cmd, args], { skip, scope? }, { fail, fix } or { busy, fix }.
function plan(step, opts, m, notes) {
  const cargoStep = (args) => (m.hasCargo ? commandFor("cargo", args, m) : NO_CARGO);
  const windows = (target) => {
    const targets = installedTargets(m);
    if (targets && !targets.includes(target)) {
      return { skip: `target ${target} not installed (rustup target add ${target})` };
    }
    return cargoStep(["build", "--workspace", "--locked", "--target", target]);
  };
  // --quick: null for the whole workspace, { skip } or { packages: [-p ...] }.
  const scoped = () => {
    if (!opts.quick) return null;
    if (opts.scope === undefined) {
      const base = quickBase();
      const paths = changedPaths(base);
      if (paths === null) {
        notes.push(`--quick: no merge base with ${base}; checking the whole workspace.`);
        opts.scope = null;
      } else {
        const changed = changedPackages(paths, workspacePackages());
        if (changed.all) {
          notes.push("--quick: a workspace file changed; checking the whole workspace.");
          opts.scope = null;
        } else if (changed.packages.length === 0) {
          opts.scope = { skip: `no crate changed against ${base}`, scope: true };
        } else {
          const all = m.hasCargo ? withDependents(changed.packages, m, notes) : changed.packages;
          const extra = all.filter((p) => !changed.packages.includes(p));
          notes.push(`--quick: changed crates: ${changed.packages.join(", ")}`
            + (extra.length ? `; crates that depend on them: ${extra.join(", ")}` : ""));
          opts.scope = { packages: all.flatMap((p) => ["-p", p]) };
        }
      }
    }
    return opts.scope;
  };
  switch (step.name) {
    case "fmt": return cargoStep(["fmt", "--all", "--check"]);
    case "clippy": {
      const s = scoped();
      if (s?.skip) return s;
      return cargoStep(["clippy", ...(s ? s.packages : ["--workspace"]), "--all-targets", "--locked", "--", "-D", "warnings"]);
    }
    case "test": {
      const s = scoped();
      if (s?.skip) return s;
      return cargoStep(["test", ...(s ? s.packages : ["--workspace"]), "--locked"]);
    }
    case "deny": {
      if (!m.hasCargo) return NO_CARGO;
      announceLock(m);
      const [cmd, args] = commandFor("light", ["deny", "--version"], m);
      const r = spawnSync(cmd, args, { cwd: root, env: m.env, encoding: "utf8", maxBuffer: 16 << 20 });
      const probe = r.error ? null : r.status;
      if (lockTimedOut(probe, String(r.stderr ?? "").split("\n"))) {
        return { busy: `another build held the build lock for over ${m.lockWait}s`, fix: "retry later" };
      }
      if (probe !== 0) return { skip: "cargo-deny not installed (cargo install --locked cargo-deny)" };
      return cargoStep(["deny", "--locked", "check"]);
    }
    case "zip-miniz":
    case "zip-zlib-rs": {
      const feature = step.name === "zip-miniz" ? "test-miniz" : "test-zlib-rs";
      return cargoStep(["test", "-p", "carrysheet-zip", "--features", feature, "--locked"]);
    }
    case "wasm": return cargoStep(["wasm-check"]);
    case "windows-x64": return windows("x86_64-pc-windows-gnu");
    case "windows-x86": return windows("i686-pc-windows-gnu");
    // release-check.mjs takes the build lock itself; it must not run under it.
    case "release": return commandFor("node", [join(root, "scripts", "release-check.mjs"), "x86_64-unknown-linux-gnu"], m);
    default: throw new Error(`no plan for ${step.name}`);
  }
}

// Run a command; resolve to { code, signal, error, tail } with the last
// output lines (bounded, scrubbed). `onLine` sees each kept line as it comes.
export function run([cmd, args], { cwd = root, env = process.env, onLine = null, ctx } = {}) {
  return new Promise((done) => {
    const tail = new LineTail({ onLine, ctx, drop: isFlockChatter });
    let finished = false;
    let error = null;
    const finish = (code, signal) => {
      if (finished) return;
      finished = true;
      if (error) tail.add(`Could not start ${cmd}: ${error}`);
      done({ code, signal: signal ?? null, error, tail: tail.lines });
    };
    let child;
    try {
      child = spawn(cmd, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    } catch (err) {
      error = err.code || err.message;
      return finish(null, null);
    }
    for (const stream of [child.stdout, child.stderr]) {
      const sink = tail.stream();
      stream.on("data", (chunk) => sink.write(chunk));
      stream.on("end", () => sink.end());
    }
    child.on("error", (err) => {
      error = err.code || err.message;
      if (child.pid === undefined) finish(null, null);
    });
    child.on("close", (code, signal) => finish(code, signal));
  });
}

// A step's result from how its command ended.
export function stepResult(name, ran, seconds, m = {}) {
  const { code, signal, error, tail } = ran;
  if (error) return { name, status: "failed", seconds, exitCode: null, note: `could not start the command (${error})`, tail };
  if (signal) return { name, status: "failed", seconds, exitCode: null, signal, note: `killed by ${signal}`, tail };
  if (code === 0) return { name, status: "ok", seconds };
  if (m.heavy && code === BUSY_EXIT && tail.some((l) => HEAVY_REFUSED.test(l))) {
    return { name, status: "busy", seconds, exitCode: code, note: "the machine was busy; the heavy-jobs wrapper ran nothing", fix: "retry later", tail };
  }
  if (lockTimedOut(code, tail)) {
    return { name, status: "busy", seconds, exitCode: code, note: "another build held the build lock until the wait ran out (CARRYSHEET_LOCK_WAIT)", fix: "retry later", tail };
  }
  return { name, status: "failed", seconds, exitCode: code, tail };
}

export function formatTime(ms) {
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  return `${m}m${String(Math.round(s - m * 60)).padStart(2, "0")}s`;
}

// The outcome of a run. `commit` and `tree` are { start, end }: HEAD and
// treeState() read before and after the run (null if git couldn't tell, which
// counts as a change).
// Returns { result, ok, summary, report (lines to print), json }.
export function summarize(results, opts, commit, tree, ms = 0) {
  const count = (s) => results.filter((r) => r.status === s).length;
  const failed = results.find((r) => r.status === "failed");
  const busy = results.find((r) => r.status === "busy");
  const unknown = [commit.start, commit.end, tree.start, tree.end].some((v) => v == null);
  const treeChanged = unknown || commit.start !== commit.end || tree.start !== tree.end;
  const dirty = tree.start == null ? null : tree.start.length > 0;
  const partial = opts.quick || !!opts.from; // a full run is always strict; don't say so
  const flags = [opts.quick && "--quick", opts.from && `--from ${opts.from}`, partial && opts.strict && "--strict"].filter(Boolean).join(" ");
  const resume = (step) => `node scripts/check.mjs ${opts.quick ? "--quick " : ""}--from ${step}${opts.strict ? " --strict" : ""}`;

  let result;
  let headline;
  if (failed) { result = "failed"; headline = `FAILED at ${failed.name}`; }
  else if (busy) { result = "busy"; headline = `BUSY at ${busy.name} (nothing ran)`; }
  else if (count("ok") === 0) { result = "nothing checked"; headline = "NOTHING CHECKED (no step passed)"; }
  else { result = "passed"; headline = "passed"; }
  if (treeChanged) {
    const why = unknown ? "git could not report the tree" : "tree changed during the run";
    headline = result === "failed" ? `${headline}; ${why}` : `FAILED: ${why}`;
    result = "failed";
  }

  const report = [];
  if (failed) {
    if (failed.tail?.length) {
      report.push("", `Last ${failed.tail.length} lines of ${failed.name}:`, ...failed.tail.map((l) => `  ${l}`));
    }
    const why = failed.note ? `the ${failed.name} step failed: ${failed.note}` : `the ${failed.name} step exited with ${failed.exitCode}`;
    report.push("", `Why: ${why}.`, `Fix: ${failed.fix ?? "fix what the output above shows"}, then resume with: ${resume(failed.name)}`);
  } else if (busy) {
    report.push("", `Why: ${busy.note}.`, `Fix: ${busy.fix}: ${resume(busy.name)}`);
  } else if (result === "nothing checked") {
    report.push("", "Why: every step was skipped, so nothing was checked.",
      "Fix: install what the skipped steps name, or run without --from.");
  }
  if (treeChanged) {
    report.push("", unknown
      ? "The tree may have changed during the run: git could not report HEAD or the working tree at the start or the end."
      : "The tree changed during the run: HEAD or the working tree is not what it was at the start.",
    "Why: the result would not belong to one commit.",
      "Fix: don't commit or edit files while it runs; run it again.");
  }

  const short = commit.start ? commit.start.slice(0, 12) : "unknown";
  const state = dirty === null ? "tree state unknown" : dirty ? "dirty tree" : "clean tree";
  const summary = `check.mjs${flags ? ` ${flags}` : ""}: ${headline}`
    + ` (${count("ok")} ok, ${count("skipped")} skipped, ${count("failed")} failed${count("busy") ? `, ${count("busy")} busy` : ""})`
    + ` on ${short}, ${state}, ${formatTime(ms)}`;
  report.push("", `Commit: ${commit.start ?? "unknown"} (${state})`, summary);
  const json = {
    result, ok: result === "passed", commit: commit.start, commitEnd: commit.end, dirty, treeChanged,
    quick: opts.quick, from: opts.from, strict: !!opts.strict, skipped: count("skipped"), summary, steps: results,
  };
  return { result, ok: result === "passed", summary, report, json };
}

// sccache's hit and miss counts, or null.
export function cacheCounts(stats) {
  const sum = (o) => Object.values(o?.counts ?? {}).reduce((a, b) => a + b, 0);
  if (!stats?.stats) return null;
  return { hits: sum(stats.stats.cache_hits), misses: sum(stats.stats.cache_misses) };
}

// The hit rate between two sccache stats readings, as one line, or null.
export function cacheLine(before, after) {
  const a = cacheCounts(before);
  const b = cacheCounts(after);
  if (!a || !b) return null;
  const hits = b.hits - a.hits;
  const misses = b.misses - a.misses;
  if (hits < 0 || misses < 0 || hits + misses === 0) return null; // server restarted, or no compiles
  const rate = Math.round((100 * hits) / (hits + misses));
  return `sccache: ${rate}% hits (${hits} hits, ${misses} misses; every build on the machine during the run)`;
}

function sccacheStats(m) {
  if (!m.sccache) return null;
  const r = spawnSync("sccache", ["--show-stats", "--stats-format", "json"], { env: m.env, encoding: "utf8" });
  try { return r.status === 0 ? JSON.parse(r.stdout) : null; } catch { return null; }
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.error) {
    console.error(scrub(opts.error));
    console.error(USAGE);
    process.exit(2);
  }
  const steps = selectSteps(opts);
  if (opts.list) {
    if (opts.json) console.log(JSON.stringify(steps.map(({ name, what, quick }) => ({ name, what, quick })), null, 2));
    else for (const s of steps) console.log(`${s.name.padEnd(12)} ${s.what}${s.quick ? "" : "  (not in --quick)"}`);
    process.exit(0);
  }

  const m = machineConfig();
  const say = (line) => { if (!opts.json) console.log(scrub(line)); };
  const refuse = (what, why, fix) => {
    if (opts.json) console.log(JSON.stringify({ result: "refused", ok: false, error: scrub(what), why: scrub(why), fix: scrub(fix) }, null, 2));
    else for (const line of [what, `Why: ${why}`, `Fix: ${fix}`]) console.error(scrub(line));
    process.exit(1);
  };
  if (m.heavy && !existsSync(m.heavy)) {
    refuse(`CARRYSHEET_HEAVY is ${m.heavy}, which does not exist.`,
      "check.mjs runs each heavy step through that wrapper.",
      "point CARRYSHEET_HEAVY at the wrapper (e.g. ~/bin/heavy), or unset it.");
  }
  const commitStart = headCommit();
  const treeStart = treeState();
  const refused = refusal(opts, commitStart, treeStart, m);
  if (refused) refuse(refused.what, refused.why, refused.fix);

  const notes = [];
  if (!m.flock) notes.push("No flock here: cargo runs without the shared build lock.");
  if (m.heavy) notes.push(`Heavy steps run through ${m.heavy}.`);
  if (m.customLock) notes.push(`CARRYSHEET_BUILD_LOCK is set: using ${m.lock}, not the shared build lock.`);
  const allNotes = [];
  const flush = () => { for (const n of notes.splice(0)) { allNotes.push(scrub(n)); say(`note: ${n}`); } };
  flush();
  const cacheBefore = sccacheStats(m);
  // The wrapper's waiting messages go to the terminal as they come.
  const onLine = (line) => { if (line.startsWith("heavy:")) process.stderr.write(`${line}\n`); };

  const started = Date.now();
  const results = [];
  for (const step of steps) {
    const t0 = Date.now();
    const p = plan(step, opts, m, notes);
    flush();
    let result;
    if (p.fail) result = { name: step.name, status: "failed", seconds: 0, exitCode: null, note: p.fail, fix: p.fix, tail: [] };
    else if (p.busy) result = { name: step.name, status: "busy", seconds: 0, note: p.busy, fix: p.fix };
    else if (p.skip && opts.strict && !p.scope) {
      result = { name: step.name, status: "failed", seconds: 0, exitCode: null, note: `--strict turns a skip into a failure: ${p.skip}`, fix: "install what it names", tail: [] };
    } else if (p.skip) result = { name: step.name, status: "skipped", seconds: 0, note: p.skip };
    else {
      if (!opts.json && process.stderr.isTTY) process.stderr.write(`${step.name} ...\r`);
      if (step.name !== "release") announceLock(m);
      const ran = await run(p, { env: m.env, onLine });
      result = stepResult(step.name, ran, (Date.now() - t0) / 1000, m);
    }
    results.push(result);
    const label = { ok: "ok", failed: "FAILED", skipped: "skipped", busy: "BUSY" }[result.status];
    const extra = result.note && result.status !== "failed" ? `  (${result.note})` : "";
    say(`${step.name.padEnd(12)} ${label.padEnd(8)} ${result.seconds ? formatTime(result.seconds * 1000) : "-"}${extra}`);
    if (result.status === "failed" || result.status === "busy") break;
  }

  const commit = { start: commitStart, end: headCommit() };
  const tree = { start: treeStart, end: treeState() };
  const out = summarize(results, opts, commit, tree, Date.now() - started);
  const cache = cacheLine(cacheBefore, sccacheStats(m));
  if (opts.json) {
    // Values are scrubbed one by one; scrubbing the JSON text could hit a key.
    console.log(JSON.stringify({ ...out.json, summary: scrub(out.summary), notes: allNotes, sccache: cache }, null, 2));
  } else {
    for (const line of out.report) console.log(scrub(line));
    if (cache) console.log(cache);
  }
  process.exit(out.ok ? 0 : 1);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    await main();
  } catch (err) {
    // Never a raw stack trace: it shows paths.
    console.error(`check.mjs stopped: ${scrub(err?.message ?? String(err))}`);
    console.error("Why: an unexpected error inside check.mjs.");
    console.error("Fix: run it again; if it repeats, report this message.");
    process.exit(1);
  }
}
