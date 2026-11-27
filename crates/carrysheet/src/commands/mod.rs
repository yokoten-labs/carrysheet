//! Carrysheet's command table and what belongs to it.
//!
//! - [`table`]: the command table, the single source of truth for the CLI,
//!   the MCP tools and their schemas.
//! - [`schema`]: JSON Schemas and MCP tool definitions generated from it,
//!   with [`schema::validate`] (checks arguments on our side, whatever the
//!   client did). It is also reachable as `validate`. The lint that checks
//!   the schemas against every client's rules lives in `tests/lint`.
//! - [`error`]: errors with a code, what, why and fix, and exit codes.
//! - [`limits`]: every limit on the work and memory of one call.

pub mod limits;
pub mod schema;
pub mod table;

pub use crate::error;
pub use schema::validate;

pub use error::{CommandError, ErrorCode, ExitCode};
pub use table::{Annotations, Arg, ArgType, COMMANDS, Command, command};
pub use validate::{check_confirmation, validate_args};
