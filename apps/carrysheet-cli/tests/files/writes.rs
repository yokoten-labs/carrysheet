use crate::files::*;
use carrysheet::error::{CommandError, ErrorCode};

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
    let confined = Some(root.as_path());
    write_atomic(&root.join("sub/a.csv"), b"1", confined).unwrap();
    let e = write_atomic(&root.join("away/b.csv"), b"1", confined).unwrap_err();
    assert_eq!(e.code, ErrorCode::OutsideRoot, "{e:?}");
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    // A link to a file outside, too.
    std::fs::write(outside.join("c.csv"), b"old").unwrap();
    std::os::unix::fs::symlink(outside.join("c.csv"), root.join("c.csv")).unwrap();
    assert!(write_atomic(&root.join("c.csv"), b"new", confined).is_err());
    assert_eq!(std::fs::read(outside.join("c.csv")).unwrap(), b"old");
    // Without a root (the CLI), links are written through as before.
    write_atomic(&root.join("c.csv"), b"new", None).unwrap();
    assert_eq!(std::fs::read(outside.join("c.csv")).unwrap(), b"new");
    let _ = std::fs::remove_dir_all(&base);
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
    write_atomic(&book, b"two", None).unwrap();
    let mode = std::fs::metadata(&book).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Writing to a link replaces the file it points to; the link stays.
    let link = dir.join("link.xlsx");
    std::os::unix::fs::symlink("real/book.xlsx", &link).unwrap();
    write_atomic(&link, b"three", None).unwrap();
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
    write_atomic(&book, b"one", None).unwrap();
    write_atomic(&book, b"two", None).unwrap();
    assert_eq!(std::fs::read(&book).unwrap(), b"two");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "no temporary file left"
    );
    assert!(exists(&book));
    let missing = dir.join("no/such/folder/x.xlsx");
    assert!(!exists(&missing));
    assert!(write_atomic(&missing, b"x", None).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_way_of_showing_hides_the_home_folder() {
    let Some(home) = home_dir() else {
        return;
    };
    let p = home.join("q1.xlsx").display().to_string();
    let e = redact_error(&CommandError::new(
        ErrorCode::Io,
        format!("cannot open {p}"),
        &p,
        &p,
    ));
    let home = home.display().to_string();
    for shown in [
        e.to_text(),
        e.to_json().to_string(),
        e.to_envelope().to_string(),
        serde_json::to_string(&e).unwrap(),
        e.to_string(),
    ] {
        assert!(!shown.contains(&home), "{shown}");
        assert!(shown.contains('~'), "{shown}");
    }
    // A reference error repeats what was typed.
    let typed = format!("'{p}'!A1");
    let e: CommandError = carrysheet::range::SheetRange::parse(&typed)
        .unwrap_err()
        .into();
    assert!(e.what.contains(&home), "the field keeps the text");
    assert!(!redact_error(&e).to_json().to_string().contains(&home));
}
