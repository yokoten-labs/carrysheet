//! Paths: where findings may go. Hiding the home folder is the sink's job
//! (`sink.rs`).

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use super::sink::scrub;

/// Windows' verbatim prefix, which `canonicalize` adds there.
const VERBATIM: &str = r"\\?\";

/// A path for people, with the home folder as `~`.
pub fn display_path(p: &Path) -> String {
    scrub(&p.display().to_string())
}

/// Where findings go by default: `$XDG_CACHE_HOME/carrysheet-fuzz`, else
/// `~/.cache/carrysheet-fuzz`, or `%LOCALAPPDATA%\carrysheet-fuzz` on
/// Windows.
pub fn default_out_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")?
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))?
    };
    Some(base.join("carrysheet-fuzz"))
}

/// Creates the output folder. Refuses one inside `root` or inside any git
/// work tree (the baked-in root can be stale when the binary moves).
pub fn prepare_out_dir(out: &Path, root: &Path) -> Result<PathBuf, String> {
    let out = resolve(out)?;
    let inside_root = root
        .canonicalize()
        .is_ok_and(|r| out.starts_with(strip_verbatim(&r)));
    if inside_root || git_toplevel(&out).is_some() {
        return Err(format!(
            "the output folder {} is inside a repository; findings must not be written there. Use --out with a folder outside it",
            display_path(&out)
        ));
    }
    std::fs::create_dir_all(&out).map_err(|e| {
        format!(
            "cannot create the output folder {}: {e}",
            display_path(&out)
        )
    })?;
    Ok(out)
}

/// The git work tree that holds `path` (or its nearest existing parent).
fn git_toplevel(path: &Path) -> Option<PathBuf> {
    let mut dir = path;
    while !dir.is_dir() {
        dir = dir.parent()?;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let top = String::from_utf8(out.stdout).ok()?;
    (out.status.success() && !top.trim().is_empty()).then(|| PathBuf::from(top.trim()))
}
/// An absolute path with its existing part resolved (symbolic links,
/// `..`), without creating anything, and without the verbatim prefix.
///
/// `..` is resolved by the system only where the folders exist; in the
/// part that does not exist yet it is refused, because nothing can say
/// where it would lead (`missing/../../repo/x`).
fn resolve(p: &Path) -> Result<PathBuf, String> {
    let full = std::env::current_dir().unwrap_or_default().join(p);
    let parts: Vec<Component> = full.components().collect();
    // The longest prefix that exists.
    let mut cut = parts.len();
    while cut > 0 {
        let prefix: PathBuf = parts[..cut].iter().collect();
        if prefix.exists() {
            break;
        }
        cut -= 1;
    }
    let base: PathBuf = parts[..cut].iter().collect();
    let mut out = base
        .canonicalize()
        .map(|c| strip_verbatim(&c))
        .unwrap_or(base);
    for part in &parts[cut..] {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => out.push(OsString::from(name)),
            _ => {
                return Err(format!(
                    "the output folder {} uses `..` after a folder that does not exist yet; give the path without it",
                    display_path(p)
                ));
            }
        }
    }
    Ok(out)
}

/// `\\?\C:\x` as `C:\x`.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(VERBATIM) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_verbatim_prefix_from_paths() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\Users\x")),
            PathBuf::from(r"C:\Users\x")
        );
        assert_eq!(strip_verbatim(Path::new("/tmp/x")), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn refuses_an_output_folder_in_the_repository() {
        let root = crate::seeds::default_root();
        let inside = root.join("no-such-folder").join("findings");
        let err = prepare_out_dir(&inside, &root).unwrap_err();
        assert!(err.contains("inside a repository"), "{err}");
        assert!(!root.join("no-such-folder").exists());
        // Even when the root given is somewhere else (a moved binary).
        let other = std::env::temp_dir();
        let err = prepare_out_dir(&inside, &other).unwrap_err();
        assert!(err.contains("inside a repository"), "{err}");
    }

    #[test]
    fn dot_dot_cannot_climb_into_a_repository() {
        let root = crate::seeds::default_root().canonicalize().unwrap();
        let other = std::env::temp_dir();
        // `..` after a folder that does not exist: refused outright.
        let ups = "../".repeat(root.components().count() + 2);
        let sneaky = std::env::temp_dir()
            .join("fuzzlite-no-such-dir")
            .join(format!("{ups}{}", root.display()))
            .join("zz");
        let err = prepare_out_dir(&sneaky, &other).unwrap_err();
        assert!(err.contains("`..`"), "{err}");
        assert!(!root.join("zz").exists());
        // `..` inside folders that exist resolves for real, and the
        // result is checked: still inside the repository.
        let via_existing = root.join("tools").join("..").join("zz");
        let err = prepare_out_dir(&via_existing, &other).unwrap_err();
        assert!(err.contains("inside a repository"), "{err}");
        assert!(!root.join("zz").exists());
    }

    #[test]
    fn a_plain_folder_outside_is_fine() {
        let dir = std::env::temp_dir().join(format!("fuzzlite-out-{}", std::process::id()));
        let got = prepare_out_dir(&dir.join(".").join("a"), Path::new("/nonexistent")).unwrap();
        assert!(got.ends_with("a") && got.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
