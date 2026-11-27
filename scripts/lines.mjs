// The 5,000-line cap of record 0010 (docs/records/0010-five-thousand-lines.md).
// Counts physical lines of our code; tests and vendor/ are not counted.
// Run: node scripts/lines.mjs [--max N]. Tests: node --test scripts/lines.test.mjs
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

export const CAP = 5000, TARGET = 4500, PATCH_MAX = 0; // PATCH_MAX 0: the patch limit is off.
const CODE = /\.(rs|mjs|js|cjs|ts|py|sh|ps1|psm1)$/;
// proto: Numbers schemas, which leave with the numbers reader in record 0010 step 1.
const DATA = /\.(toml|yml|md|lock|json|txt|proto)$|(^|\/)(LICENSE[^/]*|NOTICE|CODEOWNERS|\.[^/]*)$|^\.github\//;
const isTestPath = (path) => /(^|\/)(tests|benches)\//.test(path) || path.endsWith(".test.mjs");
const isPatch = (path) => /^vendor\/gridcraft\/patches\/[^/]+\.patch$/.test(path);
const countLines = (text) => (text === "" ? 0 : text.split("\n").length - (text.endsWith("\n") ? 1 : 0));
const group = (n) => n.toLocaleString("en-US");

// Returns the hits of the three Rust rules in one file, as "path:line: text".
export function findRustHits(path, text) {
  const hits = [];
  let before = ""; // The last line that is not blank, trimmed.
  text.split("\n").forEach((line, index) => {
    const code = line.trim(), isBadMod = /^(pub(\([^)]*\))? )?mod tests;/.test(code) && !before.startsWith("#[cfg(test)]");
    if (code.includes("#[path") || code.includes("include!(") || isBadMod) hits.push(`${path}:${index + 1}: ${code}`);
    if (code) before = code;
  });
  return hits;
}

// files: tracked paths; read(path) gives the text. Returns { code, out }: exit code and lines to print.
export function checkTree(files, read, maxGiven) {
  const max = maxGiven ?? CAP, out = [], errors = [], folders = new Map(), rustHits = [];
  let total = 0, patchAdded = 0;
  for (const path of files) {
    if (isPatch(path)) patchAdded += read(path).split("\n").filter((l) => l.startsWith("+") && !l.startsWith("+++ ")).length;
    if (path.startsWith("vendor/") || isTestPath(path)) continue;
    if (!CODE.test(path)) {
      if (!DATA.test(path)) errors.push(`${path}: not code and not on the data list.\n  Why: a new language must not slip past the cap.\n  Fix: give it a code extension, or add its kind to DATA in scripts/lines.mjs (CODEOWNERS review).`);
      continue;
    }
    const text = read(path), lines = countLines(text), folder = path.split("/").slice(0, -1).slice(0, 2).join("/") || ".";
    total += lines;
    folders.set(folder, (folders.get(folder) ?? 0) + lines);
    if (path.endsWith(".rs")) rustHits.push(...findRustHits(path, text));
  }
  out.push(`lines: ${group(total)} counted, cap ${group(max)}, room left ${group(max - total)}`);
  if (total > TARGET) out.push(`note: above the ${group(TARGET)}-line target; the pull request says why.`);
  if (files.some(isPatch)) out.push(`patches: ${group(patchAdded)} added lines in vendor/gridcraft/patches`);
  if (PATCH_MAX > 0 && patchAdded > PATCH_MAX) errors.push(`patches add ${group(patchAdded)} lines, above PATCH_MAX ${group(PATCH_MAX)}.\n  Why: a patch changes GridCraft's behaviour and must not carry a Carrysheet feature.\n  Fix: move the code into our crates, or raise PATCH_MAX with a new record.`);
  if (rustHits.length > 0) {
    const what = `${rustHits.length} test-path escape(s) in counted Rust files:\n  ${rustHits.join("\n  ")}`;
    if (maxGiven === undefined) errors.push(`${what}\n  Why: #[path], include!( and an unguarded mod tests; can pull test code past the count.\n  Fix: put unit tests in src/tests/ behind #[cfg(test)] mod tests;.`);
    else out.push(`note: ${what}`);
  }
  if (total > max) {
    const table = [...folders].sort((a, b) => b[1] - a[1]).map(([name, n]) => `  ${group(n).padStart(8)}  ${name}`);
    errors.push(`${group(total)} lines of code, above the cap of ${group(max)} by ${group(total - max)}.\n  Why: record 0010 caps our own source at ${group(CAP)} lines.\n  Fix: remove or simplify code; the cap does not rise.\n  Lines per folder:\n${table.join("\n")}`);
  }
  for (const error of errors) out.push(`error: ${error}`);
  return { code: errors.length > 0 ? 1 : 0, out };
}

function main(args) {
  let max;
  if (args.length === 2 && args[0] === "--max" && /^[1-9]\d*$/.test(args[1])) max = Number(args[1]);
  else if (args.length > 0) {
    console.error("error: unknown arguments.\n  Why: only --max N is accepted.\n  Fix: run node scripts/lines.mjs [--max N].");
    return 2;
  }
  const root = execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim();
  const files = execFileSync("git", ["-C", root, "ls-files", "-z"], { encoding: "utf8", maxBuffer: 1 << 26 }).split("\0").filter(Boolean);
  const { code, out } = checkTree(files, (path) => readFileSync(join(root, path), "utf8"), max);
  (code === 0 ? console.log : console.error)(out.join("\n"));
  return code;
}

if (import.meta.main) process.exitCode = main(process.argv.slice(2));
