use crate::files::*;

#[test]
fn home_becomes_tilde() {
    let home = "/home/alice";
    let shown = |p: &str| display_path_text(p, home, false);
    assert_eq!(shown("/home/alice/books/a.xlsx"), "~/books/a.xlsx");
    assert_eq!(shown(home), "~");
    assert_eq!(shown("/home/alice/"), "~/");
    assert_eq!(shown("/home/alicex/a"), "/home/alicex/a");
    assert_eq!(shown("rel/a"), "rel/a");
    // Unix names are case-sensitive.
    assert_eq!(shown("/home/Alice/a"), "/home/Alice/a");
    assert_eq!(display_path_text("/x/a", "/", false), "/x/a");
}

#[test]
fn windows_paths_ignore_case_separators_and_verbatim_prefixes() {
    let home = r"C:\Users\Alice";
    let shown = |p: &str| display_path_text(p, home, true);
    assert_eq!(shown(r"C:\Users\Alice\books\a.xlsx"), r"~\books\a.xlsx");
    assert_eq!(shown(r"c:\users\ALICE\a.xlsx"), r"~\a.xlsx");
    assert_eq!(shown("C:/Users/Alice/a.xlsx"), "~/a.xlsx");
    assert_eq!(shown(r"\\?\C:\Users\Alice\a.xlsx"), r"~\a.xlsx");
    assert_eq!(shown(r"\\?\c:\users\alice"), "~");
    assert_eq!(shown(r"C:\Users\Alicex\a"), r"C:\Users\Alicex\a");
    assert_eq!(shown(r"D:\data\a.xlsx"), r"D:\data\a.xlsx");
    // A verbatim or slash-ended home folder.
    assert_eq!(
        display_path_text(r"C:\Users\Alice\a", r"\\?\C:\Users\Alice\", true),
        r"~\a"
    );
    // A home folder on a network share.
    assert_eq!(
        display_path_text(
            r"\\?\UNC\server\home\alice\a.xlsx",
            r"\\server\home\alice",
            true
        ),
        r"~\a.xlsx"
    );
    // A drive root is not a home folder worth hiding.
    assert_eq!(display_path_text(r"C:\a", r"C:\", true), r"C:\a");
}

#[test]
fn redacts_home_in_text() {
    let home = "/home/alice";
    assert_eq!(
        redact_home_text("failed at /home/alice/a.xlsx and /home/alice", home, false),
        "failed at ~/a.xlsx and ~"
    );
    assert_eq!(
        redact_home_text("/HOME/ALICE/a", home, false),
        "/HOME/ALICE/a"
    );
    assert_eq!(redact_home_text("/x", "/", false), "/x");
    assert_eq!(
        redact_home_text("日本 /home/alice/表", home, false),
        "日本 ~/表"
    );
}

#[test]
fn redacts_windows_style_home() {
    let home = r"C:\Users\alice";
    assert_eq!(
        redact_home_text(
            r"C:\Users\alice\a.xlsx or C:/Users/alice/b.xlsx or c:\USERS\Alice\c",
            home,
            true
        ),
        r"~\a.xlsx or ~/b.xlsx or ~\c"
    );
    assert_eq!(
        redact_home_text(r"open \\?\C:\Users\alice\d.xlsx failed", home, true),
        r"open ~\d.xlsx failed"
    );
    assert_eq!(
        redact_home_text(r"x C:\Users\alice\e", r"\\?\C:\Users\alice", true),
        r"x ~\e"
    );
    // A name escaped by carrysheet::text shows each `\` as `\\`.
    assert_eq!(
        redact_home_text(r#""C:\\Users\\alice\\q1.xlsx"!A1"#, home, true),
        r#""~\\q1.xlsx"!A1"#
    );
    assert_eq!(redact_home_text(r"C:\a C:\b", r"C:\", true), r"C:\a C:\b");
}

#[test]
fn real_home_is_hidden() {
    if let Some(home) = home_dir() {
        let p = home.join("x.xlsx");
        let shown = display_path(&p);
        assert!(shown.starts_with('~'), "{shown}");
        assert!(!shown.contains(&home.display().to_string()));
        let text = format!("see {}", p.display());
        assert!(!redact_home(&text).contains(&home.display().to_string()));
    }
}
