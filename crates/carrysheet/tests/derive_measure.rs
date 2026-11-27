//! Temporary (record 0010, step 6, measure first): the derived schemas
//! against the table's, and the lint.

mod lint;

use carrysheet::{args, commands};

#[test]
fn derived_tools_match_the_table() {
    let mut size = 0;
    let mut tools = Vec::new();
    for old in commands::table::COMMANDS {
        let new = args::command(old.name).unwrap();
        let violations = lint::lint_schema(&args::input_schema(new));
        eprintln!("{}: lint {violations:?}", old.name);
        let tool = args::tool_definition(new);
        eprintln!("{tool}");
        assert!(violations.is_empty());
        assert_eq!(tool.to_string(), commands::schema::tool_definition(old).to_string());
        assert_eq!(new.doc().0, old.summary);
        assert_eq!(new.cli_name(), old.cli_name());
        assert_eq!(new.changes_files(), old.changes_files);
        assert_eq!(new.mcp_tool, old.mcp_tool);
        if new.mcp_tool {
            size += tool.to_string().chars().count();
            tools.push(tool);
        }
    }
    let violations = lint::lint_tools(&tools);
    eprintln!("tools/list: {size} characters; lint {violations:?}");
    assert!(violations.is_empty());
}
