use crate::commands::table::*;
use std::collections::HashSet;

fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[test]
fn names_are_unique_snake_case_and_match_the_cli_path() {
    let mut seen = HashSet::new();
    for c in COMMANDS {
        assert!(is_snake(c.name), "{}", c.name);
        assert!(seen.insert(c.name), "duplicate {}", c.name);
        assert_eq!(c.name, c.cli_path.join("_"), "{}", c.name);
        assert_eq!(command(c.name), Some(c));
        assert_eq!(command_by_path(c.cli_path), Some(c));
    }
}

#[test]
fn mvp_commands_are_all_there() {
    for name in [
        "overview",
        "read",
        "eval",
        "convert",
        "edit",
        "mcp_serve",
        "commands",
    ] {
        assert!(command(name).is_some(), "{name}");
    }
}

#[test]
fn groups_cover_multi_word_commands() {
    for c in COMMANDS {
        assert!(
            !c.cli_path.is_empty() && c.cli_path.len() <= 2,
            "{}",
            c.name
        );
        if c.cli_path.len() == 2 {
            assert!(group(c.cli_path[0]).is_some(), "{}", c.name);
        } else {
            assert!(
                group(c.cli_path[0]).is_none(),
                "{} clashes with a group",
                c.name
            );
        }
    }
}

#[test]
fn text_fits_help_and_tool_lists() {
    for c in COMMANDS {
        assert!(!c.summary.is_empty() && c.summary.len() <= 72, "{}", c.name);
        assert!(!c.summary.ends_with('.'), "{}", c.name);
        assert!(
            !c.description.is_empty() && c.description.len() <= 400,
            "{}",
            c.name
        );
        for a in c.all_args() {
            assert!(!a.description.is_empty(), "{}.{}", c.name, a.name);
            assert!(a.description.len() <= 200, "{}.{}", c.name, a.name);
        }
    }
}

#[test]
fn args_are_well_formed() {
    for c in COMMANDS {
        let mut names = HashSet::new();
        let mut flags = HashSet::new();
        let mut optional_positional_seen = false;
        for a in c.all_args() {
            assert!(is_snake(a.name), "{}.{}", c.name, a.name);
            assert!(names.insert(a.name), "{}.{} twice", c.name, a.name);
            assert!(flags.insert(a.flag()), "{}.{}: flag twice", c.name, a.name);
            if a.positional {
                assert!(
                    !matches!(
                        a.ty,
                        ArgType::Boolean | ArgType::TextList | ArgType::ObjectList(_)
                    ),
                    "{}.{} cannot be positional",
                    c.name,
                    a.name
                );
                assert!(
                    !optional_positional_seen,
                    "{}.{}: a positional argument follows an optional one",
                    c.name, a.name
                );
                optional_positional_seen |= !a.required;
            }
            if let ArgType::Integer { min, max } = a.ty {
                assert!(min <= max, "{}.{}", c.name, a.name);
            }
            if let ArgType::Enum(values) = a.ty {
                assert!(!values.is_empty(), "{}.{}", c.name, a.name);
                let unique: HashSet<_> = values.iter().collect();
                assert_eq!(unique.len(), values.len(), "{}.{}", c.name, a.name);
            }
            if let ArgType::ObjectList(fields) = a.ty {
                for f in fields {
                    assert!(is_snake(f.name));
                    assert!(!f.positional);
                    assert!(!matches!(f.ty, ArgType::ObjectList(_)), "no nesting");
                    if f.ty == ArgType::Scalar {
                        // Its schema is an anyOf, which must end in null.
                        assert!(!f.required, "{}.{}.{}", c.name, a.name, f.name);
                    }
                }
            }
            assert_ne!(
                a.ty,
                ArgType::Scalar,
                "{}.{}: a typed value only fits inside a JSON list",
                c.name,
                a.name
            );
            if a.ty == ArgType::Boolean {
                assert!(!a.required, "{}.{}: flags are optional", c.name, a.name);
            }
        }
    }
}

#[test]
fn flags_are_kebab_case() {
    let read = command("read").unwrap();
    assert_eq!(read.arg("max_rows").unwrap().flag(), "--max-rows");
    assert_eq!(DRY_RUN.flag(), "--dry-run");
}

#[test]
fn dry_run_exactly_on_commands_that_change_files() {
    for c in COMMANDS {
        assert_eq!(c.arg("dry_run").is_some(), c.changes_files, "{}", c.name);
        if c.changes_files {
            assert!(!c.annotations.read_only, "{}", c.name);
            assert_eq!(c.all_args().last().unwrap().name, "dry_run");
        }
    }
}

#[test]
fn deleting_and_overwriting_can_be_confirmed() {
    for op in DESTRUCTIVE_EDIT_OPS {
        assert!(EDIT_OPS.contains(op), "{op}");
    }
    for c in COMMANDS {
        if c.arg("overwrite").is_some() {
            assert_eq!(c.arg("confirm"), Some(&CONFIRM), "{}", c.name);
            assert!(c.annotations.destructive, "{}", c.name);
        }
    }
    assert!(command("edit").unwrap().arg("confirm").is_some());
}

#[test]
fn read_only_commands_are_not_destructive() {
    for c in COMMANDS {
        if c.annotations.read_only {
            assert!(!c.annotations.destructive, "{}", c.name);
            assert!(!c.changes_files, "{}", c.name);
        }
    }
}

#[test]
fn mcp_tools_in_fixed_order_and_few() {
    let tools: Vec<_> = mcp_tools().map(|c| c.name).collect();
    assert_eq!(tools, ["overview", "read", "eval", "convert", "edit"]);
    assert!(tools.len() < 40);
}
