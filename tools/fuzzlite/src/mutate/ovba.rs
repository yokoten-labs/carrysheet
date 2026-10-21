//! VBA project mutations (MS-OVBA): records of the decompressed `dir`
//! stream, chunk headers and copy tokens of compressed containers,
//! decompression bombs, and the `PROJECT` text stream.

use super::le::{put_u16, put_u32, u16_at, u32_at};
use super::{Container, INTERESTING_U16, INTERESTING_U32};
use crate::rng::Rng;

const MODULE_NAME: u16 = 0x0019;
const MODULE_END: u16 = 0x002B;
const MODULES: u16 = 0x000F;
const MODULE_OFFSET: u16 = 0x0031;
const CODE_PAGE: u16 = 0x0003;
const PROJECT_VERSION: u16 = 0x0009;

/// Applies one to three mutations to the project's streams and returns
/// the rebuilt file with their names. Stacking matters: a bomb in one
/// module is cheap, a bomb in every module is not.
pub fn mutate(
    rng: &mut Rng,
    c: &Container,
    max_len: usize,
) -> Option<(Vec<u8>, Vec<&'static str>)> {
    let mut c = c.clone();
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(3) {
        if let Some(op) = apply(rng, &mut c, max_len / 2) {
            ops.push(op);
        }
    }
    if ops.is_empty() {
        return None;
    }
    Some((c.build()?, ops))
}

fn apply(rng: &mut Rng, c: &mut Container, budget: usize) -> Option<&'static str> {
    let dir = c.find(|p| p == "vba/dir" || p.ends_with("/vba/dir"))?;
    let vba_streams = c.find_all(|p| p.starts_with("vba/") || p.contains("/vba/"));
    let (index, data, op) = match rng.below(7) {
        0..=2 => {
            let plain = carrysheet_vba::decompress(&c.nodes[dir].data, 16 << 20).ok()?;
            let (plain, op) = dir_records(rng, plain, budget)?;
            (dir, compress_literal(&plain), op)
        }
        3 => {
            // A bomb: each 6-byte chunk expands to 4096 bytes.
            let i = vba_streams[rng.below(vba_streams.len())];
            let chunks = rng.count(budget / 6);
            let mut data = c.nodes[i].data.clone();
            if i == dir || rng.one_in(2) {
                data = bomb(chunks);
            } else {
                data.extend_from_slice(&bomb(chunks)[1..]);
            }
            (i, data, "ovba:bomb")
        }
        4 => {
            // A bomb after the source of every module, each just under or
            // over the per-stream decompression limit.
            let chunks = rng.pick(&[256usize, 1024, 4000, 4096]);
            let tail = bomb(chunks);
            for &i in &vba_streams {
                let name = c.nodes[i].path.to_lowercase();
                if i != dir && !name.ends_with("/_vba_project") && !name.contains("/__srp_") {
                    c.nodes[i].data.extend_from_slice(&tail[1..]);
                }
            }
            return Some("ovba:bomb_all");
        }
        5 => {
            // A chunk header or token of a compressed container.
            let i = vba_streams[rng.below(vba_streams.len())];
            let mut data = c.nodes[i].data.clone();
            let starts = chunk_starts(&data);
            let at = *starts.get(rng.below(starts.len()))?;
            match rng.below(4) {
                0 => data[at + 1] ^= 0x80,
                1 => put_u16(&mut data, at, 0xB000 | rng.pick(&[0u16, 1, 0xFFF])),
                2 => put_u16(&mut data, at, rng.pick(INTERESTING_U16)),
                _ => {
                    // Copy tokens with offsets before the chunk start.
                    let end = (at + 2 + 64).min(data.len());
                    for b in data.get_mut(at + 2..end)? {
                        if rng.one_in(3) {
                            *b = 0xFF;
                        }
                    }
                }
            }
            (i, data, "ovba:chunk")
        }
        _ => {
            let i = c.find(|p| p == "project" || p.ends_with("/project"))?;
            let mut text = c.nodes[i].data.clone();
            let line: &[u8] = match rng.below(5) {
                0 => b"Module=M\r\n",
                1 => b"Document=ThisWorkbook/&H00000000\r\n",
                2 => b"DPB=\"0000\"\r\n",
                3 => b"\r\n",
                _ => b"=",
            };
            let times = rng.count(budget / line.len());
            let at = rng.below(text.len() + 1);
            text.splice(at..at, line.repeat(times));
            (i, text, "ovba:project")
        }
    };
    c.nodes.get_mut(index)?.data = data;
    Some(op)
}

#[derive(Debug, Clone, Copy)]
struct Rec {
    at: usize,
    id: u16,
    end: usize,
}

fn records(d: &[u8]) -> Vec<Rec> {
    let mut out = Vec::new();
    let mut at = 0;
    while let (Some(id), Some(size)) = (u16_at(d, at), u32_at(d, at + 2)) {
        // PROJECTVERSION says 4 but has 6 bytes of data.
        let size = if id == PROJECT_VERSION {
            6
        } else {
            size as usize
        };
        let end = at.saturating_add(6).saturating_add(size);
        if end > d.len() {
            break;
        }
        out.push(Rec { at, id, end });
        at = end;
    }
    out
}

fn dir_records(rng: &mut Rng, mut d: Vec<u8>, budget: usize) -> Option<(Vec<u8>, &'static str)> {
    let recs = records(&d);
    let any = *recs.get(rng.below(recs.len()))?;
    let find = |id: u16| recs.iter().copied().find(|r| r.id == id);
    let op = match rng.below(6) {
        0 => {
            put_u32(&mut d, any.at + 2, rng.pick(INTERESTING_U32));
            "ovba:dir_size"
        }
        1 => {
            let r = find(MODULES).or_else(|| find(CODE_PAGE))?;
            put_u16(&mut d, r.at + 6, rng.pick(INTERESTING_U16));
            "ovba:dir_count"
        }
        2 => {
            let r = find(MODULE_OFFSET)?;
            put_u32(&mut d, r.at + 6, rng.pick(INTERESTING_U32));
            "ovba:module_offset"
        }
        3 => {
            let r = find(CODE_PAGE)?;
            let cp = rng.pick(&[0u16, 1200, 1201, 65001, 50220, 54936, 932, 0xFFFF]);
            put_u16(&mut d, r.at + 6, cp);
            "ovba:code_page"
        }
        4 => {
            // The same module many times: work per module times count.
            let start = find(MODULE_NAME)?;
            let end = recs
                .iter()
                .find(|r| r.at > start.at && r.id == MODULE_END)?
                .end;
            let one = d.get(start.at..end)?.to_vec();
            let times = rng.count(budget / one.len()).min(8192);
            d.splice(end..end, one.repeat(times));
            "ovba:repeat_module"
        }
        _ => {
            let one = d[any.at..any.end].to_vec();
            let times = rng.count(budget / one.len().max(1));
            d.splice(any.end..any.end, one.repeat(times));
            "ovba:repeat_record"
        }
    };
    Some((d, op))
}

/// Offsets of compressed chunk headers: after each byte 0x01 that is
/// followed by a header with the 0b011 signature bits (a container start),
/// and then chunk by chunk to the end of that container.
fn chunk_starts(d: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let is_header = |at: usize| u16_at(d, at).is_some_and(|h| (h >> 12) & 0x7 == 0b011);
    for start in 1..d.len().saturating_sub(1) {
        if out.len() >= 4096 {
            break;
        }
        if d[start - 1] != 0x01 || !is_header(start) {
            continue;
        }
        let mut at = start;
        while is_header(at) && out.len() < 4096 {
            out.push(at);
            at += usize::from(u16_at(d, at).unwrap_or(0) & 0x0FFF) + 3;
        }
    }
    out
}

/// A valid container that stores `data` as literals only (no copy
/// tokens), 3,584 bytes per chunk so each chunk fits its 4,098-byte limit.
pub fn compress_literal(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x01];
    for piece in data.chunks(3584) {
        let mut body = Vec::with_capacity(piece.len() + piece.len() / 8 + 1);
        for group in piece.chunks(8) {
            body.push(0x00);
            body.extend_from_slice(group);
        }
        let header = 0xB000 | (body.len() as u16 + 2 - 3);
        out.extend_from_slice(&header.to_le_bytes());
        out.extend_from_slice(&body);
    }
    out
}

/// A container of `chunks` chunks that each expand 6 bytes to 4,096: one
/// literal, then one copy token of length 4,095 at offset 1.
pub fn bomb(chunks: usize) -> Vec<u8> {
    let mut out = vec![0x01];
    for _ in 0..chunks {
        out.extend_from_slice(&[0x03, 0xB0, 0b10, b'a', 0xFC, 0x0F]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_containers_and_bombs_decompress() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
        let back = carrysheet_vba::decompress(&compress_literal(&data), 1 << 20).unwrap();
        assert_eq!(back, data);
        assert_eq!(compress_literal(&[]), [0x01]);
        let big = carrysheet_vba::decompress(&bomb(3), 1 << 20).unwrap();
        assert_eq!(big, vec![b'a'; 3 * 4096]);
    }

    #[test]
    fn vba_mutations_keep_a_compound_file() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/carrysheet-vba/tests/data/SimpleMacro-vbaProject.bin"
        );
        let c = Container::parse(&std::fs::read(path).unwrap()).unwrap();
        let mut rng = Rng::new(9);
        let mut names = std::collections::BTreeSet::new();
        for _ in 0..200 {
            if let Some((bytes, ops)) = mutate(&mut rng, &c, 1 << 20) {
                assert!(Container::parse(&bytes).is_some(), "{ops:?}");
                names.extend(ops);
            }
        }
        for want in [
            "ovba:bomb",
            "ovba:repeat_module",
            "ovba:project",
            "ovba:dir_size",
        ] {
            assert!(names.contains(want), "{want} not in {names:?}");
        }
    }
}
