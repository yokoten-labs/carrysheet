//! VBA macros: committed samples and the decompression bomb from the
//! fuzzing review (issue 8).

use carrysheet_vba::{decompress, read_vba_project_bin, read_xls_vba};

use super::{Case, Input, Op, Operation, VBA_BOMB, one, tally};
use crate::generate::ovba_bomb;

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "vba-samples/open",
            op: Operation::Open,
            about: "read the macro project of every committed sample",
            input: Input::Files {
                dirs: &["crates/carrysheet-vba/tests/data"],
                exts: &["xls", "bin"],
            },
            inner: 50,
            setup: read_each,
        },
        Case {
            name: "vba-bomb-16mib/decompress",
            op: Operation::Decompress,
            about: "hostile: decompress 24 KB that expands to 16 MiB",
            input: Input::None,
            inner: 100,
            setup: |_| {
                let bomb = ovba_bomb(4095);
                Ok(Box::new(move || {
                    decompress(&bomb, 16 << 20)
                        .map(|out| out.len() as u64)
                        .map_err(|e| e.to_string())
                }))
            },
        },
        Case {
            name: "vba-hostile-bomb-module/open",
            op: Operation::Open,
            about: "hostile: a project whose one module's source expands to 16 MiB",
            input: Input::Generated(&VBA_BOMB),
            inner: 5,
            setup: |inputs| {
                let bin = one(inputs)?;
                Ok(Box::new(move || {
                    read_vba_project_bin(&bin)
                        .map(|p| p.flags.len() as u64 + p.modules.len() as u64)
                        .map_err(|e| e.to_string())
                }))
            },
        },
    ]
}

/// Reads each input's macro project; the check counts modules.
fn read_each(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    Ok(Box::new(move || {
        Ok(tally(
            inputs.iter().map(|b| {
                if b.len() > 8 && is_xls(b) {
                    read_xls_vba(b).map(|p| p.map_or(0, |p| p.modules.len() as u64))
                } else {
                    read_vba_project_bin(b).map(|p| p.modules.len() as u64)
                }
            }),
            |n| *n,
        ))
    }))
}

/// A compound file with a `Workbook` stream, as opposed to a bare
/// `vbaProject.bin`. Both are compound files, so look for the name.
fn is_xls(bytes: &[u8]) -> bool {
    let name: Vec<u8> = "Workbook"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    bytes.windows(name.len()).any(|w| w == name)
}
