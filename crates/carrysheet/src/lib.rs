//! Carrysheet as a library: bytes in, JSON out.
//!
//! - [`commands`]: the command table, JSON schemas, errors and the engine
//!   interface.
//! - [`model`]: a neutral workbook model and A1 references.
//! - [`kernel`]: the engine on GridCraft (vendored in `vendor/gridcraft`).

pub mod commands;
pub mod kernel;
pub mod model;

#[cfg(test)]
mod tests;
