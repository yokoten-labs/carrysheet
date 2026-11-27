//! Just enough of the compound file format ([MS-CFB]) to wrap a BIFF stream
//! built in a test, and to take the `Workbook` stream out of a sample so a
//! mutation reaches the records instead of the container.

const SECTOR: usize = 512;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
const FAT_SECTOR: u32 = 0xFFFF_FFFD;
const FREE: u32 = 0xFFFF_FFFF;
const NO_STREAM: u32 = 0xFFFF_FFFF;
pub const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn get16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn get32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A version 3 compound file holding `streams` under the root storage.
/// Every stream lives in regular sectors: the mini stream cutoff is 0, so
/// a short or cut stream keeps its exact length.
pub fn compound(streams: &[(&str, &[u8])]) -> Vec<u8> {
    let entries = 1 + streams.len();
    let dir_sectors = (entries * 128).div_ceil(SECTOR);
    let data_sectors: Vec<usize> = streams.iter().map(|(_, d)| d.len().div_ceil(SECTOR)).collect();
    let others = dir_sectors + data_sectors.iter().sum::<usize>();
    let mut fat_sectors = 1;
    while fat_sectors * (SECTOR / 4) < others + fat_sectors {
        fat_sectors += 1;
    }
    assert!(fat_sectors <= 109, "the test writer keeps the FAT in the header");
    let total = fat_sectors + others;
    let mut fat = vec![FREE; fat_sectors * SECTOR / 4];
    for slot in fat.iter_mut().take(fat_sectors) {
        *slot = FAT_SECTOR;
    }
    let chain = |start: usize, count: usize, fat: &mut Vec<u32>| {
        for i in 0..count {
            fat[start + i] = if i + 1 == count { END_OF_CHAIN } else { (start + i + 1) as u32 };
        }
    };
    let dir_start = fat_sectors;
    chain(dir_start, dir_sectors, &mut fat);
    let mut starts = Vec::new();
    let mut next = dir_start + dir_sectors;
    for &count in &data_sectors {
        starts.push(if count == 0 { END_OF_CHAIN } else { next as u32 });
        chain(next, count, &mut fat);
        next += count;
    }

    let mut out = vec![0u8; SECTOR * (1 + total)];
    out[..8].copy_from_slice(&SIGNATURE);
    put16(&mut out, 24, 0x3E);
    put16(&mut out, 26, 3);
    put16(&mut out, 28, 0xFFFE);
    put16(&mut out, 30, 9);
    put16(&mut out, 32, 6);
    put32(&mut out, 44, fat_sectors as u32);
    put32(&mut out, 48, dir_start as u32);
    put32(&mut out, 56, 0);
    put32(&mut out, 60, END_OF_CHAIN);
    put32(&mut out, 68, END_OF_CHAIN);
    for i in 0..109 {
        let v = if i < fat_sectors { i as u32 } else { FREE };
        put32(&mut out, 76 + i * 4, v);
    }
    for (i, v) in fat.iter().enumerate() {
        put32(&mut out, SECTOR + i * 4, *v);
    }
    let dir = SECTOR * (1 + dir_start);
    let names = std::iter::once("Root Entry").chain(streams.iter().map(|(n, _)| *n));
    for (k, name) in names.enumerate() {
        let e = dir + k * 128;
        let units: Vec<u16> = name.encode_utf16().take(31).collect();
        for (i, u) in units.iter().enumerate() {
            put16(&mut out, e + i * 2, *u);
        }
        put16(&mut out, e + 64, (units.len() as u16 + 1) * 2);
        out[e + 66] = if k == 0 { 5 } else { 2 };
        out[e + 67] = 1;
        put32(&mut out, e + 68, NO_STREAM);
        // Siblings in a chain to the right: a valid, if unbalanced, tree.
        let right = if k == 0 || k + 1 == entries { NO_STREAM } else { (k + 1) as u32 };
        put32(&mut out, e + 72, right);
        let child = if k == 0 && entries > 1 { 1 } else { NO_STREAM };
        put32(&mut out, e + 76, child);
        if k == 0 {
            put32(&mut out, e + 116, END_OF_CHAIN);
        } else {
            put32(&mut out, e + 116, starts[k - 1]);
            put32(&mut out, e + 120, streams[k - 1].1.len() as u32);
        }
    }
    for (k, (_, data)) in streams.iter().enumerate() {
        if starts[k] != END_OF_CHAIN {
            let at = SECTOR * (1 + starts[k] as usize);
            out[at..at + data.len()].copy_from_slice(data);
        }
    }
    out
}

/// A workbook stream in a compound file, as Excel names it.
pub fn workbook(stream: &[u8]) -> Vec<u8> {
    compound(&[("Workbook", stream)])
}

/// Follows a sector chain from `start` through `table`, reading each
/// sector with `data`; bounded by the table's length.
fn chain(start: u32, table: &[u32], data: &dyn Fn(u32) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut id = start;
    for _ in 0..table.len() {
        if id == END_OF_CHAIN {
            return Some(out);
        }
        out.extend(data(id)?);
        id = *table.get(id as usize)?;
    }
    (id == END_OF_CHAIN).then_some(out)
}

/// The sector size, the FAT and the directory of a compound file.
fn directory(file: &[u8]) -> Option<(usize, Vec<u32>, Vec<u8>)> {
    if !file.starts_with(&SIGNATURE) || get16(file, 32)? != 6 {
        return None;
    }
    let size = 1usize << get16(file, 30)?;
    let mut fat = Vec::new();
    for i in 0..109 {
        let id = get32(file, 76 + i * 4)?;
        if id < 0xFFFF_FFFA {
            let s = file.get((id as usize + 1) * size..(id as usize + 2) * size)?;
            fat.extend((0..size / 4).filter_map(|j| get32(s, j * 4)));
        }
    }
    let regular =
        |id: u32| file.get((id as usize + 1) * size..(id as usize + 2) * size).map(<[u8]>::to_vec);
    let dir = chain(get32(file, 48)?, &fat, &regular)?;
    Some((size, fat, dir))
}

/// The name of a directory entry when it is a stream.
fn stream_name(e: &[u8]) -> Option<String> {
    let len = usize::from(get16(e, 64)?).clamp(2, 64);
    let units: Vec<u16> = (0..(len - 2) / 2).filter_map(|i| get16(e, i * 2)).collect();
    (e[66] == 2).then(|| String::from_utf16_lossy(&units))
}

/// Whether the directory of a compound file lists a stream called `name`,
/// even when the stream itself cannot be read (a cut file).
pub fn has_stream(file: &[u8], name: &str) -> bool {
    directory(file).is_some_and(|(_, _, dir)| {
        (1..dir.len() / 128)
            .filter_map(|k| stream_name(&dir[k * 128..(k + 1) * 128]))
            .any(|n| n.eq_ignore_ascii_case(name))
    })
}

/// The stream called `name` under the root storage of a compound file, or
/// `None` when the file cannot be followed. For trusted samples: the FAT
/// is read from the header only, and every loop is bounded.
pub fn read_stream(file: &[u8], name: &str) -> Option<Vec<u8>> {
    let (size, fat, dir) = directory(file)?;
    let sector = |id: u32| file.get((id as usize + 1) * size..(id as usize + 2) * size);
    let regular = |id: u32| sector(id).map(<[u8]>::to_vec);
    let cutoff = u64::from(get32(file, 56)?);
    let entry = |k: usize| dir.get(k * 128..(k + 1) * 128);
    let root = entry(0)?;
    for k in 1..dir.len() / 128 {
        let e = entry(k)?;
        if !stream_name(e).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            continue;
        }
        let start = get32(e, 116)?;
        let length = get32(e, 120)? as usize;
        let mut data = if (length as u64) < cutoff {
            let container = chain(get32(root, 116)?, &fat, &regular)?;
            let mini_fat_bytes = chain(get32(file, 60)?, &fat, &regular)?;
            let mini_fat: Vec<u32> = (0..mini_fat_bytes.len() / 4)
                .filter_map(|i| get32(&mini_fat_bytes, i * 4))
                .collect();
            let mini = |id: u32| {
                container.get(id as usize * 64..id as usize * 64 + 64).map(<[u8]>::to_vec)
            };
            chain(start, &mini_fat, &mini)?
        } else {
            chain(start, &fat, &regular)?
        };
        if data.len() < length {
            return None;
        }
        data.truncate(length);
        return Some(data);
    }
    None
}
