//! From file bytes to records: the OLE2 container, the record stream with
//! its CONTINUE records, and bounds-checked byte readers.

pub(crate) mod bytes;
pub(crate) mod container;
pub(crate) mod record;
