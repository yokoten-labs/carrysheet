//! What a caller of commands needs, under one name: the commands and
//! their checks ([`crate::args`]), errors ([`crate::error`]) and the
//! [`limits`] on the work and memory of one call.

pub mod limits;

pub use crate::args::{COMMANDS, Command, check_confirmation, command, validate_args};
pub use crate::error::{CommandError, ErrorCode, ExitCode};
