//! Carrysheet's command layer.
//!
//! - [`table`]: the command table, the single source of truth for the CLI,
//!   the MCP tools and their schemas.
//! - [`schema`]: JSON Schemas and MCP tool definitions generated from it,
//!   with [`schema::lint`] (checks them against every client's rules) and
//!   [`schema::validate`] (checks arguments on our side, whatever the
//!   client did). Both are also reachable as `lint` and `validate`.
//! - [`error`]: errors with a code, what, why and fix, and exit codes.
//! - [`engine`]: what commands need from a calculation engine, and (with
//!   the `test-engine` feature) an in-memory engine for tests.
//! - [`limits`]: every limit on the work and memory of one call.
//! - [`paths`]: showing paths with `~` for the home folder.
//! - [`root`]: keeping the paths a client gives inside a root folder.

pub mod engine;
pub mod error;
pub mod limits;
pub mod paths;
pub mod root;
pub mod schema;
pub mod table;

pub use schema::{lint, validate};

pub use engine::Engine;
#[cfg(any(test, feature = "test-engine"))]
pub use engine::MemoryEngine;
pub use error::{CommandError, ErrorCode, ExitCode};
pub use table::{Annotations, Arg, ArgType, COMMANDS, Command, command};
pub use validate::{check_confirmation, validate_args};
