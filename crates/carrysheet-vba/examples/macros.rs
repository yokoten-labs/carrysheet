//! Lists the macros of a `vbaProject.bin` or `.xls` file.
//!
//! cargo run -p carrysheet-vba --example macros -- FILE [--source]

// An example prints its results to stdout; the workspace denies println! only
// so that library and MCP code can never write there by accident.
#![allow(clippy::print_stdout)]

use carrysheet_vba::{Limits, VbaPart};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: macros FILE [--source]");
        std::process::exit(2);
    };
    let show_source = args.any(|a| a == "--source");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        }
    };
    let limits = Limits::default();
    let part = if path.to_ascii_lowercase().ends_with(".xls") {
        match VbaPart::from_xls(&bytes, &limits) {
            Ok(Some(p)) => p,
            Ok(None) => {
                println!("no macros");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    } else {
        VbaPart::from_bin(bytes)
    };
    let project = match part.read(&limits) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!(
        "project {:?}, code page {} ({}), version {:?}",
        project.name, project.code_page, project.encoding, project.version
    );
    println!("protection {:?}", project.protection);
    for r in &project.references {
        println!("reference {:?} {:?} {}", r.kind, r.name, r.libid);
    }
    for m in &project.modules {
        println!(
            "module {:?} {:?} stream {:?} lines {} source {} p-code {} compressed {} hidden {} error {:?}",
            m.name,
            m.kind,
            m.stream_name,
            m.line_count,
            m.has_source,
            m.pcode_bytes,
            m.compressed_source_bytes,
            m.hidden,
            m.error.as_ref().map(|e| e.to_string())
        );
        if show_source && let Some(s) = &m.source {
            println!("{s}");
        }
    }
    for f in &project.flags {
        println!(
            "flag {} {:?} in {} line {:?}: {}",
            f.kind.as_str(),
            f.rule,
            f.module,
            f.line,
            f.why
        );
    }
}
