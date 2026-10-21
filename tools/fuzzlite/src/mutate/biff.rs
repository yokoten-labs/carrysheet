//! BIFF record mutations on the workbook stream: record lengths, CONTINUE
//! chains, SST counts, shared-formula and array ranges, deep formulas,
//! repeated records and sheets that share one substream.
//!
//! Record ids cover BIFF2 to BIFF8. Inserting or removing bytes moves the
//! BOUNDSHEET offsets that point past the change, so the file stays
//! readable enough for the damage to reach the code behind the checks.

use super::le::{put_u16, put_u32, u16_at, u32_at};
use super::{INTERESTING_U16, INTERESTING_U32, Seed};
use crate::rng::Rng;

const BOF: &[u16] = &[0x0009, 0x0209, 0x0409, 0x0809];
const CONTINUE: u16 = 0x003C;
const SST: u16 = 0x00FC;
const EXTSST: u16 = 0x00FF;
const BOUNDSHEET: u16 = 0x0085;
const FORMULA: &[u16] = &[0x0006, 0x0206, 0x0406];
const SHARED: &[u16] = &[0x04BC, 0x00BC, 0x0221, 0x0021, 0x0236, 0x0036];
const NAME: &[u16] = &[0x0018, 0x0218];
const DIMENSIONS: &[u16] = &[0x0200, 0x0000];
const MERGEDCELLS: u16 = 0x00E5;
const MUL: &[u16] = &[0x00BD, 0x00BE, 0x007D, 0x0208];
const FILEPASS: u16 = 0x002F;
/// Records worth repeating: cells, formulas, ranges, names, sheets.
const REPEAT: &[u16] = &[
    0x0203, 0x027E, 0x00FD, 0x0204, 0x0006, 0x04BC, 0x0221, 0x0236, 0x0085, 0x0018, 0x00E5, 0x007D,
    0x0208, 0x00BD, 0x00BE, 0x0207, 0x003C, 0x00FC, 0x0809, 0x01B6, 0x00EC,
];
const MAX_RECORD: usize = 8224;

#[derive(Debug, Clone, Copy)]
struct Rec {
    at: usize,
    typ: u16,
    len: usize,
}

impl Rec {
    fn data(self) -> usize {
        self.at + 4
    }

    fn end(self) -> usize {
        self.at + 4 + self.len
    }
}

fn records(s: &[u8]) -> Vec<Rec> {
    let mut out = Vec::new();
    let mut at = 0;
    while let (Some(typ), Some(len)) = (u16_at(s, at), u16_at(s, at + 2)) {
        let len = usize::from(len);
        if at + 4 + len > s.len() || out.len() >= 1 << 20 {
            break;
        }
        out.push(Rec { at, typ, len });
        at += 4 + len;
    }
    out
}

/// The workbook stream of a seed: a stream in its compound file, or the
/// file itself for raw BIFF2 to BIFF4.
fn stream(seed: &Seed) -> Option<(Option<usize>, &[u8])> {
    if let Some(c) = &seed.container {
        let i = c.find(|p| p == "workbook" || p == "book")?;
        return Some((Some(i), &c.nodes[i].data));
    }
    let first = u16_at(&seed.bytes, 0)?;
    BOF.contains(&first).then_some((None, &seed.bytes[..]))
}

/// Applies one to three record mutations (amplification often needs two:
/// say, a wider shared range and more cells that use it) and returns the
/// rebuilt file with the names of the mutations.
pub fn mutate(rng: &mut Rng, seed: &Seed, max_len: usize) -> Option<(Vec<u8>, Vec<&'static str>)> {
    let (index, s) = stream(seed)?;
    let mut s = s.to_vec();
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(3) {
        let recs = records(&s);
        if recs.is_empty() {
            break;
        }
        let budget = max_len.saturating_sub(seed.bytes.len().max(s.len()));
        if let Some(op) = apply(rng, &mut s, &recs, budget) {
            ops.push(op);
        }
    }
    if ops.is_empty() {
        return None;
    }
    let bytes = match index {
        Some(i) => seed.container.as_ref()?.with_stream(i, s)?,
        None => s,
    };
    Some((bytes, ops))
}

fn apply(rng: &mut Rng, s: &mut Vec<u8>, recs: &[Rec], budget: usize) -> Option<&'static str> {
    let any = recs[rng.below(recs.len())];
    Some(match rng.below(15) {
        0 | 1 => {
            let v = match rng.below(6) {
                0 => 0,
                1 => any.len as u16 + 1,
                2 => (any.len as u16).wrapping_sub(1),
                3 => MAX_RECORD as u16,
                4 => MAX_RECORD as u16 + 1,
                _ => rng.pick(INTERESTING_U16),
            };
            put_u16(s, any.at + 2, v);
            "biff:length"
        }
        2 | 3 => {
            let r = pick_of(
                rng,
                recs,
                &[SST, CONTINUE, 0x01B6, 0x00EC, 0x0018, 0x0006, 0x0207],
            )
            .unwrap_or(any);
            let payload: Vec<u8> = match rng.below(4) {
                0 => Vec::new(),
                1 => vec![rng.pick(&[0u8, 1, 8, 9])],
                2 => s[r.data()..r.end()].to_vec(),
                _ => (0..1 + rng.below(MAX_RECORD)).map(|_| rng.byte()).collect(),
            };
            let one = record(CONTINUE, &payload);
            let times = rng.count(budget / one.len());
            insert(s, r.end(), &one.repeat(times));
            "biff:continue_chain"
        }
        4 => {
            put_u16(s, any.at, CONTINUE);
            "biff:to_continue"
        }
        5 | 6 => {
            let r = pick_of(rng, recs, &[SST]).or_else(|| pick_of(rng, recs, &[EXTSST]))?;
            let d = r.data();
            match rng.below(5) {
                0 => put_u32(s, d, rng.pick(INTERESTING_U32)),
                1 => put_u32(s, d + 4, rng.pick(INTERESTING_U32)),
                2 => put_u16(s, d + 8, rng.pick(&[0xFFFF, 0x7FFF, 0x2000, 0])),
                3 => {
                    if let Some(b) = s.get_mut(d + 10) {
                        *b ^= rng.pick(&[0x01u8, 0x04, 0x08, 0xFF]);
                    }
                }
                _ => put_u32(s, d + 13, rng.pick(INTERESTING_U32)),
            }
            "biff:sst"
        }
        7 => {
            let r = pick_of(rng, recs, SHARED)?;
            let d = r.data();
            let (r1, r2, c1, c2) = match rng.below(4) {
                0 => (0, 0xFFFF, 0, 0xFF),
                1 => (0xFFFF, 0, 0xFF, 0),
                2 => (rng.below(65_536) as u16, 0xFFFF, rng.byte(), 0xFF),
                _ => (rng.byte().into(), rng.byte().into(), rng.byte(), rng.byte()),
            };
            put_u16(s, d, r1);
            put_u16(s, d + 2, r2);
            if let Some(cols) = s.get_mut(d + 4..d + 6) {
                cols.copy_from_slice(&[c1, c2]);
            }
            if rng.one_in(3) {
                put_u16(s, d + 8, rng.pick(INTERESTING_U16));
            }
            "biff:shared_range"
        }
        8 => {
            let r = pick_of(rng, recs, FORMULA)?;
            formula(rng, s, recs, r)?;
            "biff:formula"
        }
        9 => {
            let r = pick_of(rng, recs, REPEAT).unwrap_or(any);
            let one = s[r.at..r.end()].to_vec();
            let times = rng.count(budget / one.len());
            insert(s, r.end(), &one.repeat(times));
            "biff:repeat_record"
        }
        10 => {
            let r = pick_of(rng, recs, &[BOUNDSHEET])?;
            if rng.one_in(2) {
                let one = s[r.at..r.end()].to_vec();
                let times = rng.count(budget / one.len());
                insert(s, r.end(), &one.repeat(times));
            } else {
                let other = pick_of(rng, recs, &[BOUNDSHEET]).unwrap_or(r);
                let v = match rng.below(4) {
                    0 => u32_at(s, other.data())?,
                    1 => 0,
                    2 => r.at as u32,
                    _ => rng.pick(INTERESTING_U32),
                };
                put_u32(s, r.data(), v);
            }
            "biff:boundsheet"
        }
        11 => {
            if let Some(r) = pick_of(rng, recs, DIMENSIONS) {
                let d = r.data();
                if r.typ == 0x0200 {
                    put_u32(s, d + 4 * rng.below(2), rng.pick(INTERESTING_U32));
                } else {
                    put_u16(s, d + 2 * rng.below(4), rng.pick(INTERESTING_U16));
                }
            } else if let Some(r) = pick_of(rng, recs, &[MERGEDCELLS]) {
                let d = r.data();
                put_u16(s, d, rng.pick(INTERESTING_U16));
                put_u16(s, d + 4, 0xFFFF);
                put_u16(s, d + 8, 0xFF);
            } else {
                let r = pick_of(rng, recs, MUL)?;
                let at = if rng.one_in(2) {
                    r.data() + 2
                } else {
                    r.end() - 2
                };
                put_u16(s, at, rng.pick(INTERESTING_U16));
            }
            "biff:dimensions"
        }
        12 => {
            name(rng, s, recs, budget);
            "biff:name"
        }
        13 => {
            let bof = recs.iter().find(|r| BOF.contains(&r.typ))?;
            let body: Vec<u8> = match rng.below(3) {
                0 => [0u16, rng.byte().into(), rng.byte().into()]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
                1 => [&[1u8, 0, 1, 0][..], &[rng.byte(); 48]].concat(),
                _ => {
                    let mut b = vec![1, 0, rng.pick(&[2u8, 3, 4]), 0, 2, 0];
                    b.extend_from_slice(&rng.pick(INTERESTING_U32).to_le_bytes());
                    b.extend_from_slice(&rng.pick(INTERESTING_U32).to_le_bytes());
                    b.extend((0..64).map(|_| rng.byte()));
                    b
                }
            };
            insert(s, bof.end(), &record(FILEPASS, &body));
            "biff:filepass"
        }
        _ => {
            if rng.one_in(2) {
                // Substreams inside substreams.
                let bof = *recs.iter().find(|r| BOF.contains(&r.typ))?;
                let mut one = s[bof.at..bof.end()].to_vec();
                put_u16(
                    &mut one,
                    6,
                    rng.pick(&[0x0005, 0x0010, 0x0020, 0x0040, 0x0100]),
                );
                let times = rng.count(budget / one.len());
                insert(s, any.at, &one.repeat(times));
                "biff:nested_bof"
            } else {
                remove(s, any.at, any.end());
                "biff:remove_record"
            }
        }
    })
}

/// FORMULA damage: shared flag, token length, a ptgExp to a shared
/// formula, or a very deep expression.
fn formula(rng: &mut Rng, s: &mut Vec<u8>, recs: &[Rec], r: Rec) -> Option<()> {
    let d = r.data();
    // BIFF8 layout: rw, col, ixfe, num (8), grbit, chn (4), cce, rgce.
    let (grbit, cce) = if r.typ == 0x0006 && r.len >= 22 {
        (d + 14, d + 20)
    } else {
        (d + 12, d + 16)
    };
    if cce + 2 > r.end() {
        return None;
    }
    match rng.below(4) {
        0 => {
            let flags = u16_at(s, grbit)?;
            put_u16(s, grbit, flags | 0x0008);
        }
        1 => put_u16(s, cce, rng.pick(INTERESTING_U16)),
        2 => {
            let (row, col) = match pick_of(rng, recs, SHARED) {
                Some(sh) => (u16_at(s, sh.data())?, u16::from(*s.get(sh.data() + 4)?)),
                None => (rng.byte().into(), rng.byte().into()),
            };
            let mut body = s[d..cce].to_vec();
            let tokens = [
                0x01,
                row as u8,
                (row >> 8) as u8,
                col as u8,
                (col >> 8) as u8,
            ];
            body.extend_from_slice(&5u16.to_le_bytes());
            body.extend_from_slice(&tokens);
            replace(s, r, &body);
        }
        _ => {
            // ptgInt x n then ptgAdd x (n-1), or one value in n ptgParen.
            let room = MAX_RECORD - (cce - d) - 2;
            let mut tokens = Vec::new();
            if rng.one_in(2) {
                let n = 1 + rng.below(room / 4);
                for _ in 0..n {
                    tokens.extend_from_slice(&[0x1E, 1, 0]);
                }
                tokens.extend(std::iter::repeat_n(0x03, n - 1));
            } else {
                tokens.extend_from_slice(&[0x1E, 1, 0]);
                tokens.extend(std::iter::repeat_n(0x15, rng.below(room - 3)));
            }
            let mut body = s[d..cce].to_vec();
            body.extend_from_slice(&(tokens.len() as u16).to_le_bytes());
            body.extend_from_slice(&tokens);
            replace(s, r, &body);
        }
    }
    Some(())
}

/// NAME damage: lengths, or names whose formulas refer to names (and to
/// themselves).
fn name(rng: &mut Rng, s: &mut Vec<u8>, recs: &[Rec], budget: usize) {
    let existing = recs.iter().filter(|r| NAME.contains(&r.typ)).count();
    if let Some(r) = pick_of(rng, recs, NAME)
        && rng.one_in(2)
    {
        let d = r.data();
        match rng.below(3) {
            0 => super::le::put(s, d + 3, &[0xFF]),
            1 => put_u16(s, d + 4, rng.pick(INTERESTING_U16)),
            _ => put_u16(s, d, rng.pick(INTERESTING_U16)),
        }
        return;
    }
    let n = rng.count(budget / 40).min(4096);
    let mut all = Vec::new();
    for i in 0..n {
        let target = match rng.below(3) {
            0 => existing + i + 1,
            1 => existing + i + 2,
            _ => 1 + rng.below(existing + n),
        } as u16;
        let label = format!("N{i}");
        let rgce = [0x23, target as u8, (target >> 8) as u8, 0, 0];
        let mut body = vec![0, 0, 0, label.len() as u8];
        body.extend_from_slice(&(rgce.len() as u16).to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        body.push(0);
        body.extend_from_slice(label.as_bytes());
        body.extend_from_slice(&rgce);
        all.extend_from_slice(&record(NAME[0], &body));
    }
    let at = recs
        .iter()
        .rev()
        .find(|r| NAME.contains(&r.typ))
        .or_else(|| recs.iter().find(|r| BOF.contains(&r.typ)))
        .map_or(0, |r| r.end());
    insert(s, at, &all);
}

fn pick_of(rng: &mut Rng, recs: &[Rec], types: &[u16]) -> Option<Rec> {
    let hits: Vec<Rec> = recs
        .iter()
        .copied()
        .filter(|r| types.contains(&r.typ))
        .collect();
    hits.get(rng.below(hits.len())).copied()
}

fn record(typ: u16, body: &[u8]) -> Vec<u8> {
    let mut out = typ.to_le_bytes().to_vec();
    out.extend_from_slice(&(body.len().min(0xFFFF) as u16).to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// Inserts bytes and moves BOUNDSHEET offsets at or past `at`.
fn insert(s: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    s.splice(at..at, bytes.iter().copied());
    shift_sheets(s, at, bytes.len() as i64);
}

fn remove(s: &mut Vec<u8>, from: usize, to: usize) {
    s.drain(from..to);
    shift_sheets(s, from, -((to - from) as i64));
}

fn replace(s: &mut Vec<u8>, r: Rec, body: &[u8]) {
    let new = record(r.typ, body);
    let delta = new.len() as i64 - (4 + r.len) as i64;
    s.splice(r.at..r.end(), new);
    shift_sheets(s, r.end(), delta);
}

fn shift_sheets(s: &mut [u8], at: usize, delta: i64) {
    for r in records(s) {
        if r.typ == BOUNDSHEET
            && r.len >= 4
            && let Some(pos) = u32_at(s, r.data())
            && pos as usize >= at
        {
            put_u32(s, r.data(), (i64::from(pos) + delta).max(0) as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_biff_mutation_keeps_a_compound_file() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/carrysheet-biff/tests/data/SharedFormulaTest.xls"
        );
        let seed = Seed::new("s", std::fs::read(path).unwrap());
        let mut rng = Rng::new(5);
        let mut names = std::collections::BTreeSet::new();
        for _ in 0..400 {
            if let Some((bytes, ops)) = mutate(&mut rng, &seed, 1 << 20) {
                assert!(bytes.len() <= 2 << 20);
                assert!(super::super::Container::parse(&bytes).is_some(), "{ops:?}");
                names.extend(ops);
            }
        }
        for want in [
            "biff:length",
            "biff:continue_chain",
            "biff:sst",
            "biff:shared_range",
        ] {
            assert!(names.contains(want), "{want} not in {names:?}");
        }
    }

    #[test]
    fn insert_moves_sheet_offsets() {
        let mut s = record(0x0809, &[0; 16]);
        s.extend(record(BOUNDSHEET, &[30, 0, 0, 0, 0, 0, 1, 0, b'A']));
        let before = u32_at(&s, 24).unwrap();
        insert(&mut s, 20, &record(CONTINUE, &[1, 2]));
        assert_eq!(u32_at(&s, 30).unwrap(), before + 6);
    }
}
