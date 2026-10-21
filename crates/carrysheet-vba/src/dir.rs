//! The decompressed `dir` stream, MS-OVBA 2.3.4.2: project information,
//! references and the module list.
//!
//! Strings are kept as raw bytes here; the caller decodes them once the
//! project code page is known.

use crate::Limits;
use crate::error::VbaError;

/// A reference as stored in the `dir` stream.
#[derive(Debug, Default)]
pub(crate) struct RawReference {
    pub name: Option<Vec<u8>>,
    pub name_unicode: Option<Vec<u8>>,
    pub kind: RawReferenceKind,
    pub libid: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawReferenceKind {
    #[default]
    Registered,
    Project,
    Control,
}

/// A MODULE record.
#[derive(Debug, Default)]
pub(crate) struct RawModule {
    /// Where the MODULENAME record starts, for errors.
    pub offset: usize,
    pub name: Vec<u8>,
    pub name_unicode: Option<Vec<u8>>,
    pub stream_name: Vec<u8>,
    pub stream_name_unicode: Option<Vec<u8>>,
    pub text_offset: Option<u32>,
    /// `Some(true)` for MODULETYPE 0x0021 (standard module), `Some(false)`
    /// for 0x0022 (document, class or designer module).
    pub procedural: Option<bool>,
    pub read_only: bool,
    pub private: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Dir {
    pub sys_kind: Option<u32>,
    pub code_page: Option<u16>,
    pub name: Vec<u8>,
    pub version: Option<(u32, u16)>,
    pub references: Vec<RawReference>,
    pub modules: Vec<RawModule>,
    /// PROJECTMODULES.Count, when present.
    pub declared_modules: Option<u16>,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn bytes(&mut self, n: usize, what: &str) -> Result<&'a [u8], VbaError> {
        if n > self.remaining() {
            return Err(VbaError::dir(
                self.pos,
                format!(
                    "{what} needs {n} bytes, but only {} are left in the stream",
                    self.remaining()
                ),
            ));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u16(&mut self, what: &str) -> Result<u16, VbaError> {
        let b = self.bytes(2, what)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, what: &str) -> Result<u32, VbaError> {
        let b = self.bytes(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn peek_u16(&self) -> Option<u16> {
        self.buf
            .get(self.pos..self.pos + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    /// A u32 length followed by that many bytes.
    fn sized(&mut self, what: &str) -> Result<&'a [u8], VbaError> {
        let at = self.pos;
        let n = self.u32(what)? as usize;
        if n > self.remaining() {
            return Err(VbaError::dir(
                at,
                format!(
                    "{what} claims {n} bytes, but only {} are left in the stream",
                    self.remaining()
                ),
            ));
        }
        self.bytes(n, what)
    }
}

fn record_u32(data: &[u8]) -> Option<u32> {
    data.get(..4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parses a decompressed `dir` stream.
pub(crate) fn parse(buf: &[u8], limits: &Limits) -> Result<Dir, VbaError> {
    let mut r = Reader { buf, pos: 0 };
    let mut dir = Dir::default();
    let mut pending_name: Option<Vec<u8>> = None;
    let mut pending_name_unicode: Option<Vec<u8>> = None;
    // Set by REFERENCEORIGINAL: the REFERENCECONTROL that follows belongs
    // to the reference it already added.
    let mut original_open = false;
    let mut module: Option<RawModule> = None;
    let mut terminated = false;

    while r.remaining() > 0 {
        let at = r.pos;
        let id = r.u16("a record id")?;
        match id {
            // PROJECTVERSION: its "size" field is a reserved 4 followed by
            // six bytes of data, not a size.
            0x0009 => {
                r.u32("PROJECTVERSION")?;
                let major = r.u32("PROJECTVERSION")?;
                let minor = r.u16("PROJECTVERSION")?;
                dir.version = Some((major, minor));
            }
            // REFERENCECONTROL: its size fields MUST be ignored on read, so
            // walk its fields one by one.
            0x002F => {
                let libid = read_reference_control(&mut r)?;
                if original_open {
                    original_open = false;
                } else {
                    push_reference(
                        &mut dir,
                        limits,
                        RawReference {
                            name: pending_name.take(),
                            name_unicode: pending_name_unicode.take(),
                            kind: RawReferenceKind::Control,
                            libid,
                        },
                    )?;
                }
            }
            // REFERENCEREGISTERED: Size is ignored on read.
            0x000D => {
                r.u32("REFERENCEREGISTERED")?;
                let libid = r.sized("REFERENCEREGISTERED.Libid")?.to_vec();
                r.bytes(6, "REFERENCEREGISTERED")?;
                original_open = false;
                push_reference(
                    &mut dir,
                    limits,
                    RawReference {
                        name: pending_name.take(),
                        name_unicode: pending_name_unicode.take(),
                        kind: RawReferenceKind::Registered,
                        libid,
                    },
                )?;
            }
            // REFERENCEPROJECT.
            0x000E => {
                r.u32("REFERENCEPROJECT")?;
                let absolute = r.sized("REFERENCEPROJECT.LibidAbsolute")?.to_vec();
                r.sized("REFERENCEPROJECT.LibidRelative")?;
                r.bytes(6, "REFERENCEPROJECT")?;
                original_open = false;
                push_reference(
                    &mut dir,
                    limits,
                    RawReference {
                        name: pending_name.take(),
                        name_unicode: pending_name_unicode.take(),
                        kind: RawReferenceKind::Project,
                        libid: absolute,
                    },
                )?;
            }
            // MODULETYPE (procedural / other), MODULEREADONLY, MODULEPRIVATE,
            // the module terminator and the dir terminator: a reserved u32
            // that MUST be ignored, so never read it as a size.
            0x0021 | 0x0022 | 0x0025 | 0x0028 | 0x002B | 0x0010 => {
                if id == 0x0010 && r.remaining() < 4 {
                    // Some writers end the stream right after the id.
                    terminated = true;
                    break;
                }
                r.u32("a reserved field")?;
                match id {
                    0x0010 => {
                        terminated = true;
                        break;
                    }
                    0x002B => finish_module(&mut dir, &mut module, limits)?,
                    _ => {
                        let m = module.as_mut().ok_or_else(|| {
                            VbaError::dir(at, format!("record 0x{id:04X} appears outside a module"))
                        })?;
                        match id {
                            0x0021 => m.procedural = Some(true),
                            0x0022 => m.procedural = Some(false),
                            0x0025 => m.read_only = true,
                            _ => m.private = true,
                        }
                    }
                }
            }
            _ => {
                let data = r.sized("a record")?;
                match id {
                    0x0001 => dir.sys_kind = record_u32(data),
                    0x0003 => {
                        dir.code_page = data.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
                    }
                    0x0004 => dir.name = data.to_vec(),
                    // REFERENCENAME and its Unicode half.
                    0x0016 => {
                        pending_name = Some(data.to_vec());
                        pending_name_unicode = None;
                    }
                    0x003E => pending_name_unicode = Some(data.to_vec()),
                    // REFERENCEORIGINAL, always followed by a REFERENCECONTROL.
                    0x0033 => {
                        push_reference(
                            &mut dir,
                            limits,
                            RawReference {
                                name: pending_name.take(),
                                name_unicode: pending_name_unicode.take(),
                                kind: RawReferenceKind::Control,
                                libid: data.to_vec(),
                            },
                        )?;
                        original_open = true;
                    }
                    // PROJECTMODULES.
                    0x000F => {
                        dir.declared_modules =
                            data.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
                    }
                    // MODULENAME starts a module.
                    0x0019 => {
                        if module.is_some() {
                            // The previous module had no terminator; keep it.
                            finish_module(&mut dir, &mut module, limits)?;
                        }
                        module = Some(RawModule {
                            offset: at,
                            name: data.to_vec(),
                            ..RawModule::default()
                        });
                    }
                    0x0047 | 0x001A | 0x0032 | 0x0031 => {
                        let m = module.as_mut().ok_or_else(|| {
                            VbaError::dir(at, format!("record 0x{id:04X} appears outside a module"))
                        })?;
                        match id {
                            0x0047 => m.name_unicode = Some(data.to_vec()),
                            0x001A => m.stream_name = data.to_vec(),
                            0x0032 => m.stream_name_unicode = Some(data.to_vec()),
                            _ => {
                                m.text_offset = Some(record_u32(data).ok_or_else(|| {
                                    VbaError::dir(at, "MODULEOFFSET is shorter than 4 bytes")
                                })?);
                            }
                        }
                    }
                    // Everything else (LCID, doc strings, help files,
                    // constants, cookies, compat version...) is not needed
                    // for listing macros.
                    _ => {}
                }
            }
        }
    }
    if module.is_some() {
        finish_module(&mut dir, &mut module, limits)?;
    }
    if !terminated && dir.modules.is_empty() && dir.code_page.is_none() {
        return Err(VbaError::dir(
            r.pos,
            "the stream has no project information and no modules",
        ));
    }
    Ok(dir)
}

fn push_reference(dir: &mut Dir, limits: &Limits, reference: RawReference) -> Result<(), VbaError> {
    if dir.references.len() >= limits.max_references {
        return Err(VbaError::limit("references", limits.max_references));
    }
    dir.references.push(reference);
    Ok(())
}

fn finish_module(
    dir: &mut Dir,
    module: &mut Option<RawModule>,
    limits: &Limits,
) -> Result<(), VbaError> {
    if let Some(m) = module.take() {
        if dir.modules.len() >= limits.max_modules {
            return Err(VbaError::limit("modules", limits.max_modules));
        }
        dir.modules.push(m);
    }
    Ok(())
}

/// Reads the body of a REFERENCECONTROL record (after its id) and returns
/// the most useful library id: the extended one.
fn read_reference_control(r: &mut Reader<'_>) -> Result<Vec<u8>, VbaError> {
    r.u32("REFERENCECONTROL.SizeTwiddled")?;
    let twiddled = r.sized("REFERENCECONTROL.LibidTwiddled")?;
    r.bytes(6, "REFERENCECONTROL")?;
    // Optional NameRecordExtended: a REFERENCENAME record.
    if r.peek_u16() == Some(0x0016) {
        r.u16("REFERENCENAME")?;
        r.sized("REFERENCENAME.Name")?;
        if r.peek_u16() == Some(0x003E) {
            r.u16("REFERENCENAME")?;
            r.sized("REFERENCENAME.NameUnicode")?;
        }
    }
    let at = r.pos;
    let reserved = r.u16("REFERENCECONTROL.Reserved3")?;
    if reserved != 0x0030 {
        return Err(VbaError::dir(
            at,
            format!(
                "REFERENCECONTROL expects 0x0030 before its extended part, found 0x{reserved:04X}"
            ),
        ));
    }
    r.u32("REFERENCECONTROL.SizeExtended")?;
    let extended = r.sized("REFERENCECONTROL.LibidExtended")?;
    // Reserved4, Reserved5, OriginalTypeLib, Cookie.
    r.bytes(4 + 2 + 16 + 4, "REFERENCECONTROL")?;
    Ok(if extended.is_empty() {
        twiddled.to_vec()
    } else {
        extended.to_vec()
    })
}
