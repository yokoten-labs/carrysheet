#!/usr/bin/env node
// Build a release binary with build-machine paths remapped, then check that no
// personal data ended up inside it: home folder, user name, email addresses,
// or user-folder paths. Prints the size. No dependencies.
//
// Usage: node scripts/release-check.mjs <target> [--no-build]
//   <target>    a Rust target, e.g. x86_64-unknown-linux-gnu, x86_64-pc-windows-gnu
//   --no-build  only check the binary that is already built
//
// Exit codes: 0 clean, 1 build failed or something found, 2 usage,
// 73 the build lock stayed busy for CARRYSHEET_LOCK_WAIT seconds (1800).
// Findings print only their kind and byte offset, never the matched text, so
// the output itself is safe to paste into an issue.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { delimiter, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { LOCK_TIMEOUT_EXIT, LineTail, buildLock, ciOn, flockArgs, isFlockChatter, lockTimedOut, scrub } from "./output.mjs";

const PACKAGE = "carrysheet-cli";
const PUBLIC_EMAIL_DOMAINS = ["carrysheet.com"];
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const home = homedir();
const cargoHome = process.env.CARGO_HOME || join(home, ".cargo");
const rustupHome = process.env.RUSTUP_HOME || join(home, ".rustup");

const args = process.argv.slice(2);
const noBuild = args.includes("--no-build");
const positional = args.filter((a) => !a.startsWith("--"));
const unknown = args.filter((a) => a.startsWith("--") && a !== "--no-build");
if (positional.length !== 1 || unknown.length > 0) {
  console.error("Usage: node scripts/release-check.mjs <target> [--no-build]");
  console.error("Example: node scripts/release-check.mjs x86_64-unknown-linux-gnu");
  process.exit(2);
}
const target = positional[0];

// Release-only rustc flags. Remapping turns absolute source paths (in panic
// messages and leftover debug info) into neutral ones. When several prefixes
// match, rustc applies the last one, so the most specific come last.
function rustflags() {
  const existing = process.env.CARGO_ENCODED_RUSTFLAGS
    ? process.env.CARGO_ENCODED_RUSTFLAGS.split("\x1f")
    : (process.env.RUSTFLAGS || "").split(/\s+/).filter(Boolean);
  const remaps = [
    [home, "~"],
    [rustupHome, "/rustup"],
    [join(cargoHome, "registry", "src"), "/cargo/registry/src"],
    [join(cargoHome, "git", "checkouts"), "/cargo/git/checkouts"],
    [root, "/carrysheet"],
  ];
  return [...existing, ...remaps.map(([from, to]) => `--remap-path-prefix=${from}=${to}`)];
}

function hasCommand(name) {
  return spawnSync(name, ["--version"], { stdio: "ignore" }).status === 0;
}

// cargo runs at low priority with one build job, like every other cargo run
// on the shared machine; only in CI (CI=1) does it use every core.
const gentle = !ciOn();

// Same lock and timeout as every other cargo run. The timeout matters: under
// ~/bin/heavy, a wait without end would hold the machine-wide heavy lock the
// whole time.
function cargoCommand(cargoArgs) {
  if (process.platform === "win32" || !hasCommand("flock")) {
    return ["cargo", cargoArgs];
  }
  const lock = buildLock();
  if (lock.customLock) console.error(`note: CARRYSHEET_BUILD_LOCK is set; using ${scrub(lock.lock)}, not the shared build lock.`);
  mkdirSync(dirname(lock.lock), { recursive: true });
  const [first, ...rest] = flockArgs(lock);
  return [first, [...rest, ...(gentle ? ["nice", "-n", "10"] : []), "cargo", ...cargoArgs]];
}

function build() {
  const env = { ...process.env };
  delete env.RUSTFLAGS; // CARGO_ENCODED_RUSTFLAGS carries them, spaces included
  env.CARGO_ENCODED_RUSTFLAGS = rustflags().join("\x1f");
  if (gentle) env.CARGO_BUILD_JOBS = process.env.CARGO_BUILD_JOBS || "1"; // the machine is shared
  env.PATH = [join(cargoHome, "bin"), process.env.PATH].join(delimiter);
  const [cmd, cmdArgs] = cargoCommand([
    "build", "--release", "--locked", "-p", PACKAGE, "--target", target,
  ]);
  // Read in bounded chunks: a single huge line must not fill the memory.
  const tail = new LineTail({ onLine: (line) => console.error(line), drop: isFlockChatter });
  return new Promise((done) => {
    let finished = false;
    const finish = (code) => { if (!finished) { finished = true; done({ code, lines: tail.lines }); } };
    const child = spawn(cmd, cmdArgs, { cwd: root, env, stdio: ["ignore", "pipe", "pipe"] });
    for (const stream of [child.stdout, child.stderr]) {
      const sink = tail.stream();
      stream.on("data", (chunk) => sink.write(chunk));
      stream.on("end", () => sink.end());
    }
    child.on("error", (err) => {
      console.error(scrub(`Could not start ${cmd}: ${err.code || err.message}`));
      if (child.pid === undefined) finish(1);
    });
    child.on("close", (code) => finish(code ?? 1));
  });
}

const utf16 = (s) => Buffer.from(s, "utf16le");

// ASCII-lowercase copy so path checks ignore case (Windows paths do).
function asciiLower(buf) {
  const out = Buffer.from(buf);
  for (let i = 0; i < out.length; i++) if (out[i] >= 0x41 && out[i] <= 0x5a) out[i] |= 0x20;
  return out;
}

function findAll(haystack, needle, accept = () => true) {
  const hits = [];
  if (needle.length === 0) return hits;
  for (let i = haystack.indexOf(needle); i !== -1; i = haystack.indexOf(needle, i + 1)) {
    if (accept(i, needle.length)) hits.push(i);
  }
  return hits;
}

const isWordByte = (b) =>
  b !== undefined && ((b >= 0x30 && b <= 0x39) || (b >= 0x61 && b <= 0x7a) || b === 0x5f);

function scan(bytes) {
  const lower = asciiLower(bytes);
  const findings = [];
  const add = (kind, offsets) => offsets.forEach((offset) => findings.push({ kind, offset }));

  // Each text in UTF-8 and UTF-16 (Windows resources), slashes both ways.
  const variants = (text) => {
    const forms = new Set([text, text.replaceAll("\\", "/"), text.replaceAll("/", "\\")]);
    return [...forms].flatMap((t) => [Buffer.from(t.toLowerCase()), utf16(t.toLowerCase())]);
  };

  for (const v of variants(home)) add("home folder path", findAll(lower, v));
  for (const v of variants("/home/")) add("/home/ path", findAll(lower, v));
  for (const v of variants("C:\\Users\\")) add("C:\\Users\\ path", findAll(lower, v));

  // The user name only as a whole word, so short names don't match inside
  // ordinary words.
  const names = new Set();
  try { names.add(userInfo().username); } catch { /* no user database entry */ }
  for (const key of ["USER", "USERNAME", "LOGNAME"]) if (process.env[key]) names.add(process.env[key]);
  for (const name of names) {
    const ascii = Buffer.from(name.toLowerCase());
    add("user name", findAll(lower, ascii, (i, n) => !isWordByte(lower[i - 1]) && !isWordByte(lower[i + n])));
    add("user name (UTF-16)", findAll(lower, utf16(name.toLowerCase())));
  }

  // Any email address except the project's own public ones (support@...).
  // Strings sit back to back in the binary, so a match can run on into the
  // next word; compare the domain by prefix.
  const email = /[a-z0-9._%+-]+@([a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,})/g;
  for (const m of lower.toString("latin1").matchAll(email)) {
    if (PUBLIC_EMAIL_DOMAINS.some((d) => m[1].startsWith(d))) continue;
    findings.push({ kind: "email address", offset: m.index });
  }
  return findings;
}

function formatSize(n) {
  return n >= 1 << 20 ? `${(n / (1 << 20)).toFixed(2)} MiB` : `${(n / 1024).toFixed(1)} KiB`;
}

if (!noBuild) {
  const { code, lines } = await build();
  if (lockTimedOut(code, lines)) {
    console.error(`\nThe release build for ${target} did not start: another build held the build lock until the wait ran out.`);
    console.error("Why: only one build runs at a time on this machine.");
    console.error("Fix: retry later, or wait longer with CARRYSHEET_LOCK_WAIT=<seconds>.");
    process.exit(LOCK_TIMEOUT_EXIT);
  }
  if (code !== 0) {
    console.error(`\nThe release build for ${target} failed (exit ${code}).`);
    console.error("Why: see the cargo output above.");
    console.error(`Fix: check the target is installed (rustup target add ${target}) and the build passes, then run this again.`);
    process.exit(1);
  }
}

const targetDir = process.env.CARGO_TARGET_DIR ? resolve(root, process.env.CARGO_TARGET_DIR) : join(root, "target");
const exe = target.includes("windows") ? `${PACKAGE}.exe` : PACKAGE;
const binary = join(targetDir, target, "release", exe);
const inRepo = relative(root, binary);
const shown = scrub(inRepo.startsWith("..") ? binary : inRepo);
if (!existsSync(binary)) {
  console.error(`No binary at ${shown}.`);
  console.error("Why: it has not been built for this target.");
  console.error("Fix: run this again without --no-build.");
  process.exit(1);
}

const size = statSync(binary).size;
const findings = scan(readFileSync(binary));
console.log(`${shown}: ${size} bytes (${formatSize(size)})`);
if (findings.length === 0) {
  console.log("Privacy check: clean (no home folder, user name, email address or user-folder path).");
  process.exit(0);
}

const LIMIT = 20;
console.log(`Privacy check: ${findings.length} finding(s).`);
const perKind = new Map();
for (const { kind } of findings) perKind.set(kind, (perKind.get(kind) || 0) + 1);
for (const [kind, count] of perKind) console.log(`  ${count} x ${kind}`);
findings.sort((a, b) => a.offset - b.offset);
for (const { kind, offset } of findings.slice(0, LIMIT)) {
  console.log(`  ${kind} at offset 0x${offset.toString(16)}`);
}
if (findings.length > LIMIT) console.log(`  ... and ${findings.length - LIMIT} more`);
console.log("Why: build-machine data inside a release binary identifies the person who built it.");
console.log("Fix: find the source (e.g. a path in a dependency's panic message) and add a");
console.log("--remap-path-prefix rule in this script, or remove the string. To see one, run");
console.log("`xxd -s <offset> -l 64` on the binary yourself; don't paste it anywhere.");
process.exit(1);
