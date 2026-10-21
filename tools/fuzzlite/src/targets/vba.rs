//! carrysheet-vba: `vbaProject.bin` parts, `.xls` files with macros, and
//! the MS-OVBA decompressor on its own.

use carrysheet_vba::{Limits, VbaPart};

use super::{SeedDir, Target};
use crate::mutate::Shape;

const DATA: &str = "crates/carrysheet-vba/tests/data";

pub const TARGETS: &[Target] = &[
    Target {
        name: "vba",
        about: "carrysheet-vba: VbaPart::from_bin(..).read for vbaProject.bin",
        run,
        seeds: &[SeedDir {
            dir: DATA,
            extensions: &["bin"],
        }],
        builtin_seeds: &[],
        shapes: &[Shape::Ovba, Shape::Cfb],
        hidden: false,
    },
    Target {
        name: "vba-xls",
        about: "carrysheet-vba: VbaPart::from_xls(..) and read for .xls files",
        run: run_xls,
        seeds: &[SeedDir {
            dir: DATA,
            extensions: &["xls"],
        }],
        builtin_seeds: &[],
        shapes: &[Shape::Ovba, Shape::Cfb],
        hidden: false,
    },
    Target {
        name: "vba-decompress",
        about: "carrysheet-vba: decompress, one MS-OVBA compressed container",
        run: run_decompress,
        seeds: &[],
        builtin_seeds: &[
            b"\x01\x13\xb0\x00Attribut\x00e VB_Nam\x00e",
            b"\x01\x03\xb0\x02a\xfc\x0f",
        ],
        shapes: &[],
        hidden: false,
    },
];

/// Reads a `vbaProject.bin` part.
pub fn run(data: &[u8]) {
    let part = VbaPart::from_bin(data.to_vec());
    let _ = part.read(&Limits::default());
}

/// Finds and reads the VBA project of an `.xls` file.
pub fn run_xls(data: &[u8]) {
    let limits = Limits::default();
    if let Ok(Some(part)) = VbaPart::from_xls(data, &limits) {
        let _ = part.read(&limits);
    }
}

/// Decompresses one container with the default output limit.
pub fn run_decompress(data: &[u8]) {
    let _ = carrysheet_vba::decompress(data, Limits::default().max_decompressed_bytes);
}
