// Shared by check.mjs and release-check.mjs: hiding personal data in output,
// a bounded reader for a command's output, and the shared build lock.
// No dependencies.

import { userInfo, homedir } from "node:os";
import { join } from "node:path";
import { StringDecoder } from "node:string_decoder";

const home = homedir();
const TAIL = 40; // lines kept from a command's output
const TAIL_BYTES = 16 * 1024; // and at most this much text
const LINE_MAX = 500; // characters kept from one line
const RAW_MAX = 4 * LINE_MAX; // characters held in memory for one line
const LOCK_WAIT = 1800; // seconds to wait for the build lock
const LINE_SLACK = 60; // a line up to LINE_MAX + LINE_SLACK passes whole
const KEPT = 40; // failure lines kept even after they leave the tail
const KEPT_AFTER = 3; // lines kept after a failure line, from the same stream
// A failed test (libtest's or a harness = false test's) or a panic.
const FAILURE_LINE = /^test .+ \.\.\. FAILED\b|\bpanicked at\b/;
export const LOCK_TIMEOUT_EXIT = 73; // flock gave up waiting for the build lock

// Running in CI (CI=1 or CI=true)? There cargo may use every core at normal
// priority; everywhere else it builds gently (one job, nice), because the
// development machine is shared.
export const ciOn = (environ = process.env) => ["1", "true"].includes(String(environ.CI ?? "").toLowerCase());

// The shared build lock: path, how long to wait for it, and whether
// CARRYSHEET_BUILD_LOCK replaced it (for tests and partial runs only).
export function buildLock(environ = process.env) {
  const wait = Number.parseInt(environ.CARRYSHEET_LOCK_WAIT ?? "", 10);
  return {
    lock: environ.CARRYSHEET_BUILD_LOCK || join(home, ".cache", "carrysheet-build.lock"),
    lockWait: wait > 0 ? wait : LOCK_WAIT,
    customLock: !!environ.CARRYSHEET_BUILD_LOCK,
  };
}

// The command prefix that takes the lock with a timeout. --verbose makes
// flock say "timeout while waiting to get lock" when it gives up, so its exit
// code can be told apart from the same code from the command it runs; flock
// runs with LC_ALL=C so that line is in English whatever the locale, and the
// command after it gets the caller's LC_ALL back.
export function flockArgs({ lock, lockWait }, environ = process.env) {
  const localeBack = environ.LC_ALL ? [`LC_ALL=${environ.LC_ALL}`] : ["-u", "LC_ALL"];
  return ["env", "LC_ALL=C", "flock", "--verbose", "-w", String(lockWait), "-E", String(LOCK_TIMEOUT_EXIT), lock,
    "env", ...localeBack];
}

const FLOCK_TIMEOUT = /^flock: timeout while waiting to get lock/;
const FLOCK_CHATTER = /^flock: (getting lock took|executing) /;
// flock --verbose lines that say nothing useful.
export const isFlockChatter = (line) => FLOCK_CHATTER.test(line);

// Did a command end because flock gave up waiting for the lock?
export function lockTimedOut(code, lines) {
  return code === LOCK_TIMEOUT_EXIT && lines.some((l) => FLOCK_TIMEOUT.test(l));
}

// Replace the home folder with ~ (both slash styles, for Windows paths).
export function scrubHome(text, homeDir = home) {
  if (!homeDir || homeDir === "/") return text;
  const forms = new Set([homeDir, homeDir.replaceAll("\\", "/"), homeDir.replaceAll("/", "\\")]);
  let out = text;
  for (const form of [...forms].sort((a, b) => b.length - a.length)) out = out.split(form).join("~");
  return out;
}

// The user names to hide: the same list release-check.mjs scans for.
export function userNames(environ = process.env) {
  const names = new Set();
  try { names.add(userInfo().username); } catch { /* no user database entry */ }
  for (const key of ["USER", "USERNAME", "LOGNAME"]) if (environ[key]) names.add(environ[key]);
  return [...names].filter(Boolean);
}

let defaultContext = null;
const scrubContext = () => (defaultContext ??= { home, names: userNames() });
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const MARK = "\uE000"; // stands for <user> while scrubbing, so a name "user" can't match it
const SHARED_FOLDERS = new Set(["public", "default", "default user", "all users"]);
// /mnt/c/Users/<name> (WSL) and C:\Users\<name>, C:/Users/<name>, C:\\Users\\<name>.
const WINDOWS_USER = /(\/mnt\/[a-z]\/users\/|\b[a-z]:(?:\\{1,2}|\/)users(?:\\{1,2}|\/))([^\\/\r\n"'<>|*?:]+)/gi;

// Hide the home folder (as ~), Windows user folders and the user name (as <user>).
export function scrub(text, ctx = scrubContext()) {
  let out = scrubHome(String(text), ctx.home).replaceAll("<user>", MARK);
  out = out.replace(WINDOWS_USER, (all, prefix, name) =>
    SHARED_FOLDERS.has(name.trim().toLowerCase()) ? all : prefix + MARK);
  for (const name of ctx.names) {
    out = out.replace(new RegExp(`(?<![A-Za-z0-9_])${escapeRe(name)}(?![A-Za-z0-9_])`, "gi"), MARK);
  }
  return out.replaceAll(MARK, "<user>");
}

// A line cut short may end in the start of the home folder or the user name,
// which scrub() can't recognise; drop such a partial ending.
function chopPartial(text, ctx) {
  const lower = text.toLowerCase();
  const secrets = [...ctx.names];
  if (ctx.home && ctx.home !== "/") secrets.push(ctx.home, ctx.home.replaceAll("\\", "/"), ctx.home.replaceAll("/", "\\"));
  for (const secret of secrets) {
    const s = secret.toLowerCase();
    for (let k = Math.min(s.length - 1, lower.length); k > 0; k--) {
      if (lower.endsWith(s.slice(0, k))) return chopPartial(text.slice(0, -k), ctx);
    }
  }
  return text;
}

// Remove terminal colour codes (rustfmt prints them even into a pipe).
export function stripAnsi(text) {
  return text.replace(/\x1b(?:\[[0-9;?]*[A-Za-z]|\([A-Za-z0-9])/g, "");
}

// One output line made safe to show: no colour codes or control characters,
// nothing personal. A line longer than LINE_MAX + LINE_SLACK (560) characters
// keeps its first LINE_MAX (500) and a note "[... line cut, N characters]";
// the slack lets a line another script already cut pass through whole.
// `cut` says the raw line was longer than what we held; `total` is its full length.
export function cleanLine(raw, cut = false, total = raw.length, ctx = scrubContext()) {
  let text = stripAnsi(raw).replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
  if (cut) text = chopPartial(text, ctx);
  text = scrub(text, ctx);
  if (cut || text.length > LINE_MAX + LINE_SLACK) {
    text = text.slice(0, LINE_MAX).replace(/[\uD800-\uDBFF]$/, "");
    text += ` [... line cut, ${total} characters]`;
  }
  return text;
}

// The last lines of a command's output, bounded in lines, bytes and line
// length. Reads raw chunks, so one huge line never sits in memory whole.
// `kept` holds the first failed tests and panics with the few lines after
// each, so they can be shown when they scrolled out of the tail.
export class LineTail {
  // A line for which drop() is true is left out; onLine sees each other line as it comes.
  constructor({ lines = TAIL, bytes = TAIL_BYTES, onLine = null, drop = null, ctx = undefined } = {}) {
    Object.assign(this, { max: lines, maxBytes: bytes, onLine, drop, ctx, lines: [], size: 0, kept: [], after: 0 });
  }

  // `from` counts the lines still to keep after a failure line: one per stream.
  add(line, cut = false, total = line.length, from = this) {
    const text = cleanLine(line, cut, total, this.ctx ?? scrubContext());
    if (this.drop?.(text)) return;
    this.onLine?.(text);
    if (FAILURE_LINE.test(text)) from.after = KEPT_AFTER + 1;
    if (from.after > 0) {
      from.after -= 1;
      if (this.kept.length < KEPT) this.kept.push(text);
    }
    this.lines.push(text);
    this.size += Buffer.byteLength(text) + 1;
    while (this.lines.length > 1 && (this.lines.length > this.max || this.size > this.maxBytes)) {
      this.size -= Buffer.byteLength(this.lines.shift()) + 1;
    }
  }

  // A sink for one stream: write(chunk) and end().
  stream() {
    const decoder = new StringDecoder("utf8");
    const from = { after: 0 };
    let held = "";
    let total = 0;
    let cut = false;
    const append = (piece) => {
      total += piece.length;
      const room = RAW_MAX - held.length;
      if (piece.length > room) cut = true;
      if (room > 0) held += piece.slice(0, room);
    };
    const finish = () => {
      this.add(held.endsWith("\r") ? held.slice(0, -1) : held, cut, total, from);
      held = ""; total = 0; cut = false;
    };
    const feed = (text) => {
      let start = 0;
      for (let nl = text.indexOf("\n"); nl !== -1; nl = text.indexOf("\n", start)) {
        append(text.slice(start, nl));
        finish();
        start = nl + 1;
      }
      append(text.slice(start));
    };
    return {
      write: (chunk) => feed(decoder.write(chunk)),
      end: () => { feed(decoder.end()); if (total > 0) finish(); },
    };
  }
}
