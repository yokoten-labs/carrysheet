use crate::commands::engine::FileFormat;
use crate::commands::engine::save::*;
use crate::commands::error::ErrorCode;

#[cfg(unix)]
#[test]
fn confined_writes_refuse_a_link_out_of_the_root() {
    let base = std::env::temp_dir().join(format!("carrysheet-confine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    // As if `sub` was swapped for a link after the path was resolved.
    std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
    let guard = confine_writes(&root);
    write_atomic(&root.join("sub/a.csv"), b"1").unwrap();
    let e = write_atomic(&root.join("away/b.csv"), b"1").unwrap_err();
    assert_eq!(e.code, ErrorCode::OutsideRoot, "{e:?}");
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    // A link to a file outside, too.
    std::fs::write(outside.join("c.csv"), b"old").unwrap();
    std::os::unix::fs::symlink(outside.join("c.csv"), root.join("c.csv")).unwrap();
    assert!(write_atomic(&root.join("c.csv"), b"new").is_err());
    assert_eq!(std::fs::read(outside.join("c.csv")).unwrap(), b"old");
    drop(guard);
    // Without a root (the CLI), links are written through as before.
    write_atomic(&root.join("c.csv"), b"new").unwrap();
    assert_eq!(std::fs::read(outside.join("c.csv")).unwrap(), b"new");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn file_text_is_quoted_escaped_and_cut() {
    assert_eq!(quote_from_file("xl/a.xml", 64), "\"xl/a.xml\"");
    assert_eq!(
        quote_from_file("a\u{202E}b\n\"", 64),
        "\"a\\u{202e}b\\u{a}\\\"\""
    );
    assert_eq!(quote_from_file(&"x".repeat(100), 3), "\"xxx...\"");
}

#[test]
fn reports_say_what_is_lost() {
    let mut r = SaveReport::new(FileFormat::Xlsx);
    assert!(!r.loses_something());
    r.lost.push(Loss {
        feature: "charts".into(),
        before: 3,
        after: 1,
    });
    assert!(r.loses_something());
    assert_eq!(r.summary(), "charts (2 of 3)");
}

#[cfg(unix)]
#[test]
fn writes_keep_permissions_and_links() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("carrysheet-perm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("real")).unwrap();
    // A private file stays private when replaced (review M2).
    let book = dir.join("real/book.xlsx");
    std::fs::write(&book, b"one").unwrap();
    std::fs::set_permissions(&book, std::fs::Permissions::from_mode(0o600)).unwrap();
    write_atomic(&book, b"two").unwrap();
    let mode = std::fs::metadata(&book).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Writing to a link replaces the file it points to; the link stays.
    let link = dir.join("link.xlsx");
    std::os::unix::fs::symlink("real/book.xlsx", &link).unwrap();
    write_atomic(&link, b"three").unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&book).unwrap(), b"three");
    let mode = std::fs::metadata(&book).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(
        std::fs::read_dir(dir.join("real")).unwrap().count(),
        1,
        "no temporary file left"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writes_replace_whole_files() {
    let dir = std::env::temp_dir().join(format!("carrysheet-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let book = dir.join("book.xlsx");
    write_atomic(&book, b"one").unwrap();
    write_atomic(&book, b"two").unwrap();
    assert_eq!(std::fs::read(&book).unwrap(), b"two");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "no temporary file left"
    );
    assert_eq!(default_output(&book).unwrap(), dir.join("book-edited.xlsx"));
    std::fs::write(dir.join("book-edited.xlsx"), b"x").unwrap();
    assert_eq!(
        default_output(&book).unwrap(),
        dir.join("book-edited-2.xlsx")
    );
    assert_eq!(
        check_output(&book, false).unwrap_err().code,
        ErrorCode::FileExists
    );
    assert!(check_output(&book, true).is_ok());
    let missing = dir.join("no/such/folder/x.xlsx");
    assert!(write_atomic(&missing, b"x").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
