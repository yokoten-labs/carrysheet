use crate::files::*;
use carrysheet::error::ErrorCode;
use std::path::{Path, PathBuf};

fn names(names: &[&str]) -> Vec<Step> {
    names
        .iter()
        .map(|n| {
            if *n == ".." {
                Step::Parent
            } else {
                Step::Name((*n).to_owned())
            }
        })
        .collect()
}

const WIN_ROOT: &str = r"C:\work\books";

#[test]
fn windows_network_and_device_paths_are_refused_on_text_alone() {
    for (given, refusal) in [
        (r"\\192.0.2.1\share\a.xlsx", Refusal::OtherShare),
        (r"\\host@SSL\DavWWWRoot\a.xlsx", Refusal::OtherShare),
        (r"//192.0.2.1/share/a.xlsx", Refusal::OtherShare),
        (r"\\?\UNC\192.0.2.1\share\a.xlsx", Refusal::OtherShare),
        (r"\\.\pipe\x", Refusal::DeviceNamespace),
        (r"\\.\COM1", Refusal::DeviceNamespace),
        (r"//./pipe/x", Refusal::DeviceNamespace),
        (
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\a",
            Refusal::DeviceNamespace,
        ),
        (r"\\?\Volume{1234}\a.xlsx", Refusal::DeviceNamespace),
        (r"\??\GLOBALROOT\x", Refusal::DeviceNamespace),
        (r"\\server", Refusal::DeviceNamespace),
        (r"D:\work\books\a.xlsx", Refusal::OtherDrive),
        (r"C:a.xlsx", Refusal::DriveRelative),
        (r"C:", Refusal::DriveRelative),
        ("a:b.xlsx", Refusal::DriveRelative),
        (r"\a.xlsx", Refusal::RootRelative),
        (r"C:\work\other\a.xlsx", Refusal::NotUnderRoot),
        (r"C:\work\booksx\a.xlsx", Refusal::NotUnderRoot),
        (r"C:\work", Refusal::NotUnderRoot),
        (r"C:\work\books\..\other\a.xlsx", Refusal::NotUnderRoot),
        (r"..\a.xlsx", Refusal::NotUnderRoot),
        (r"sub\..\..\a.xlsx", Refusal::NotUnderRoot),
    ] {
        assert_eq!(plan(WIN_ROOT, given, true), Err(refusal), "{given}");
    }
}

#[test]
fn windows_paths_inside_the_root_are_planned() {
    for (given, expected) in [
        ("a.xlsx", names(&["a.xlsx"])),
        (r"sub\..\a.xlsx", names(&["sub", "..", "a.xlsx"])),
        ("sub/./a.xlsx", names(&["sub", "a.xlsx"])),
        (r"c:\WORK\Books\a.xlsx", names(&["a.xlsx"])),
        (r"C:/work/books/sub/a.xlsx", names(&["sub", "a.xlsx"])),
        (r"\\?\C:\work\books\a.xlsx", names(&["a.xlsx"])),
        (r"\??\C:\work\books\a.xlsx", names(&["a.xlsx"])),
        (r"C:\work\books", names(&[])),
        ("表格.xlsx", names(&["表格.xlsx"])),
    ] {
        assert_eq!(plan(WIN_ROOT, given, true), Ok(expected), "{given}");
    }
}

#[test]
fn a_root_on_a_share_allows_only_that_share() {
    let root = r"\\Srv\Team\books";
    assert_eq!(
        plan(root, r"\\srv\team\BOOKS\a.xlsx", true),
        Ok(names(&["a.xlsx"]))
    );
    assert_eq!(
        plan(root, r"\\?\UNC\srv\team\books\a.xlsx", true),
        Ok(names(&["a.xlsx"]))
    );
    assert_eq!(
        plan(root, r"\\srv\other\a.xlsx", true),
        Err(Refusal::OtherShare)
    );
    assert_eq!(
        plan(root, r"\\evil\team\books\a.xlsx", true),
        Err(Refusal::OtherShare)
    );
    assert_eq!(plan(root, r"C:\a.xlsx", true), Err(Refusal::OtherDrive));
}

#[test]
fn windows_reserved_names_are_refused_anywhere_in_the_path() {
    for given in [
        "nul",
        "NUL.txt",
        "nul:",
        r"sub\com1",
        r"com1\a.xlsx",
        "COM¹",
        "lpt³.xlsx",
        "aux .csv",
        "CONIN$",
        "ab:c.xlsx",
        "book.xlsx:stream",
        "book.xlsx.",
        "book.xlsx ",
        "a*.xlsx",
        "a?.xlsx",
        "a\u{1b}.xlsx",
    ] {
        assert_eq!(
            plan(WIN_ROOT, given, true),
            Err(Refusal::BadName),
            "{given}"
        );
    }
}

#[test]
fn unix_rules() {
    let root = "/home/a/books";
    assert_eq!(plan(root, "a.xlsx", false), Ok(names(&["a.xlsx"])));
    assert_eq!(
        plan(root, "/home/a/books/sub/a.xlsx", false),
        Ok(names(&["sub", "a.xlsx"]))
    );
    assert_eq!(plan(root, "/etc/passwd", false), Err(Refusal::NotUnderRoot));
    assert_eq!(
        plan(root, "/home/a/Books/x", false),
        Err(Refusal::NotUnderRoot)
    );
    assert_eq!(plan(root, "../x", false), Err(Refusal::NotUnderRoot));
    // Backslashes and reserved Windows names are ordinary on Unix.
    assert_eq!(plan(root, r"..\x", false), Ok(names(&[r"..\x"])));
    assert_eq!(plan(root, "nul", false), Ok(names(&["nul"])));
    assert_eq!(plan(root, "a\0b", false), Err(Refusal::BadName));
}

#[test]
fn device_names() {
    for name in [
        "NUL",
        "nul.txt",
        "nul:",
        "Con",
        "com1",
        "COM0",
        "LPT9.xlsx",
        "aux ",
        "CONIN$",
        "COM¹",
        "lpt²",
        "Com³.txt",
    ] {
        assert!(is_windows_device_name(name), "{name}");
    }
    for name in [
        "null",
        "com10",
        "book.xlsx",
        "console",
        "lpt",
        "",
        "com⁴",
        "comx",
    ] {
        assert!(!is_windows_device_name(name), "{name}");
    }
}

#[test]
fn verbatim_prefixes_are_dropped_when_safe() {
    let cases = [
        (r"\\?\C:\Users\a\b.xlsx", r"C:\Users\a\b.xlsx"),
        (r"\\?\UNC\server\share\b.xlsx", r"\\server\share\b.xlsx"),
        (r"\\?\C:\a\name.", r"\\?\C:\a\name."),
        (r"\\?\UNC\server\share\name ", r"\\?\UNC\server\share\name "),
        (r"\\?\Volume{x}\a", r"\\?\Volume{x}\a"),
        ("/home/a/b.xlsx", "/home/a/b.xlsx"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            plain_path(Path::new(input)),
            PathBuf::from(expected),
            "{input}"
        );
    }
    let long = format!(r"\\?\C:\{}", "a".repeat(300));
    assert_eq!(plain_path(Path::new(&long)), PathBuf::from(&long));
}

/// A fresh folder with `root/` and `outside/` in it, removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("carrysheet-root-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("root/sub")).unwrap();
    std::fs::create_dir_all(dir.join("outside")).unwrap();
    std::fs::write(dir.join("root/book.xlsx"), b"x").unwrap();
    std::fs::write(dir.join("outside/secret.xlsx"), b"x").unwrap();
    Scratch(dir)
}

fn assert_outside(root: &Root, given: &str) {
    let e = root.resolve(given).unwrap_err();
    assert_eq!(e.code, ErrorCode::OutsideRoot, "{given}");
    assert!(e.fix.starts_with("Use a path inside"), "{given}");
}

#[test]
fn paths_inside_the_root_resolve() {
    let s = scratch("inside");
    let root = Root::new(&s.0.join("root")).unwrap();
    let dir = root.dir();
    assert!(!root.is_filesystem_root());
    assert!(root.is_folder(&s.0.join("root/sub/..")));
    assert!(!root.is_folder(&s.0.join("root/sub")));
    assert_eq!(root.resolve("book.xlsx").unwrap(), dir.join("book.xlsx"));
    assert_eq!(
        root.resolve("sub/../book.xlsx").unwrap(),
        dir.join("book.xlsx")
    );
    assert_eq!(
        root.resolve("./sub/new.csv").unwrap(),
        dir.join("sub").join("new.csv")
    );
    // A file that does not exist yet, such as an output.
    assert_eq!(root.resolve("new.xlsx").unwrap(), dir.join("new.xlsx"));
    assert_eq!(
        root.resolve("sub/deeper/new.xlsx").unwrap(),
        dir.join("sub").join("deeper").join("new.xlsx")
    );
    let absolute = dir.join("book.xlsx");
    assert_eq!(root.resolve(absolute.to_str().unwrap()).unwrap(), absolute);
    assert_eq!(root.resolve(".").unwrap(), dir);
}

#[test]
fn paths_outside_the_root_are_refused() {
    let s = scratch("outside");
    let root = Root::new(&s.0.join("root")).unwrap();
    assert_outside(&root, "../outside/secret.xlsx");
    assert_outside(&root, "sub/../../outside/secret.xlsx");
    assert_outside(&root, "..");
    assert_outside(&root, "missing/../../outside/secret.xlsx");
    // `..` after a missing folder: refused on every system.
    assert_outside(&root, "missing/..");
    assert_outside(&root, "missing/../book.xlsx");
    let absolute = s.0.join("outside").join("secret.xlsx");
    assert_outside(&root, absolute.to_str().unwrap());
    // A sibling whose name starts with the root's name.
    std::fs::create_dir_all(s.0.join("rootx")).unwrap();
    assert_outside(&root, "../rootx/a.xlsx");
    // Long paths are shortened in the message, so the fix stays near the start.
    let long = format!("../{}", "x".repeat(500));
    let e = root.resolve(&long).unwrap_err();
    assert!(e.what.chars().count() < 120, "{}", e.what);
}

#[cfg(unix)]
#[test]
fn links_out_of_the_root_are_refused() {
    use std::os::unix::fs::symlink;
    let s = scratch("links");
    let root_dir = s.0.join("root");
    symlink(s.0.join("outside"), root_dir.join("escape")).unwrap();
    symlink(
        s.0.join("outside/secret.xlsx"),
        root_dir.join("secret.xlsx"),
    )
    .unwrap();
    symlink(s.0.join("outside/nothing"), root_dir.join("dangling")).unwrap();
    symlink("../outside", root_dir.join("relative-escape")).unwrap();
    symlink("loop-b", root_dir.join("loop-a")).unwrap();
    symlink("loop-a", root_dir.join("loop-b")).unwrap();
    symlink(root_dir.join("book.xlsx"), root_dir.join("alias.xlsx")).unwrap();
    symlink("sub", root_dir.join("to-sub")).unwrap();
    let root = Root::new(&root_dir).unwrap();
    for given in [
        "escape/secret.xlsx",
        "escape/new.xlsx",
        "secret.xlsx",
        "dangling",
        "relative-escape/secret.xlsx",
        "loop-a",
    ] {
        assert_outside(&root, given);
    }
    // Links that stay inside are followed.
    let dir = root.dir();
    assert_eq!(root.resolve("alias.xlsx").unwrap(), dir.join("book.xlsx"));
    assert_eq!(
        root.resolve("to-sub/new.csv").unwrap(),
        dir.join("sub").join("new.csv")
    );
    // `..` after a link goes to the real parent of its target.
    assert_eq!(
        root.resolve("to-sub/../book.xlsx").unwrap(),
        dir.join("book.xlsx")
    );
    // The root itself may be reached through a link.
    symlink(&root_dir, s.0.join("link-to-root")).unwrap();
    let via_link = Root::new(&s.0.join("link-to-root")).unwrap();
    assert_eq!(via_link.dir(), root.dir());
    assert!(via_link.resolve("book.xlsx").is_ok());
    // Absolute paths may name the root the way it was given.
    let through_link = s.0.join("link-to-root").join("book.xlsx");
    assert_eq!(
        via_link.resolve(through_link.to_str().unwrap()).unwrap(),
        dir.join("book.xlsx")
    );
}

#[test]
fn root_must_be_an_existing_folder() {
    let s = scratch("rootcheck");
    let e = Root::new(&s.0.join("nope")).unwrap_err();
    assert_eq!(e.code, ErrorCode::FileNotFound);
    assert!(e.fix.contains("--root"));
    let e = Root::new(&s.0.join("root/book.xlsx")).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
}

#[test]
fn filesystem_roots_are_recognized() {
    let top = if cfg!(windows) { r"C:\" } else { "/" };
    if let Ok(root) = Root::new(Path::new(top)) {
        assert!(root.is_filesystem_root());
    }
}
