//! Seed inputs: the committed test files of each crate, read in place.

use std::path::{Path, PathBuf};

use crate::mutate::Seed;
use crate::say;
use crate::targets::Target;

/// Files that sit next to test data but are not inputs.
const NOT_INPUTS: &[&str] = &["NOTICE", ".gitattributes", ".gitignore"];

/// The workspace root this binary was built from.
pub fn default_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Loads a target's seeds from `root`, plus every file under `extra`.
/// Sorted by name, so a run with a fixed `--seed` repeats.
pub fn load(target: &Target, root: &Path, extra: &[PathBuf]) -> std::io::Result<Vec<Seed>> {
    let mut seeds: Vec<Seed> = target
        .builtin_seeds
        .iter()
        .enumerate()
        .map(|(i, b)| Seed::new(format!("builtin:{i}"), b.to_vec()))
        .collect();
    let mut files = Vec::new();
    for dir in target.seeds {
        let path = root.join(dir.dir);
        if !path.exists() {
            say!(
                "fuzzlite: no seed folder {}; check --root",
                crate::run::paths::display_path(&path)
            );
            continue;
        }
        walk(&path, 0, &mut |p| {
            if wanted(p, dir.extensions) {
                files.push((format!("{}/{}", dir.dir, rel(&path, p)), p.to_path_buf()));
            }
        })?;
    }
    for dir in extra {
        walk(dir, 0, &mut |p| {
            files.push((crate::run::paths::display_path(p), p.to_path_buf()));
        })?;
    }
    files.sort();
    files.dedup();
    for (name, path) in files {
        seeds.push(Seed::new(name, std::fs::read(path)?));
    }
    Ok(seeds)
}

fn wanted(path: &Path, extensions: &[&str]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if NOT_INPUTS.contains(&name) || name.starts_with("LICENSE") {
        return false;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    extensions.is_empty() || extensions.contains(&ext.as_str())
}

fn rel(base: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(&Path)) -> std::io::Result<()> {
    if depth > 8 {
        return Ok(());
    }
    let meta = std::fs::metadata(dir)?;
    if meta.is_file() {
        f(dir);
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, depth + 1, f)?;
        } else if path.is_file() {
            f(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_target_has_seeds() {
        for t in crate::targets::all() {
            let seeds = load(t, &default_root(), &[]).unwrap();
            assert!(!seeds.is_empty(), "{} has no seeds", t.name);
            assert!(seeds.iter().all(|s| !s.name.contains("NOTICE")));
        }
    }

    #[test]
    fn biff_seeds_cover_old_versions_and_parse_as_containers() {
        let t = crate::targets::find("biff").unwrap();
        let seeds = load(t, &default_root(), &[]).unwrap();
        assert!(seeds.iter().any(|s| s.name.ends_with("testEXCEL_3.xls")));
        let parsed = seeds.iter().filter(|s| s.container.is_some()).count();
        assert!(parsed * 2 > seeds.len(), "{parsed} of {}", seeds.len());
    }
}
