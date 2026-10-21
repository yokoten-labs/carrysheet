//! LIKE and GLOB with SQLite's exact results, but stoppable.
//!
//! SQLite's matcher can take text length times pattern length in one call
//! (26 ms for a 100-character pattern on 256 KiB), and nothing stops it
//! inside the call. This is a port of SQLite's `patternCompare` (3.53) that
//! reads the watchdog flag every few thousand characters and gives up with
//! an interrupt error once the time limit has passed. (SQLite is in the
//! public domain.)

use super::guard::Late;

/// How a pattern is read: SQLite's `compareInfo`.
#[derive(Clone, Copy)]
pub(crate) struct Info {
    /// `%` or `*`; 0 when the escape character took its place.
    match_all: u32,
    /// `_` or `?`; 0 when the escape character took its place.
    match_one: u32,
    /// `[` for GLOB, 0 for LIKE.
    match_set: u32,
    /// LIKE ignores the case of ASCII letters.
    no_case: bool,
}

impl Info {
    pub(crate) fn is_glob(self) -> bool {
        self.match_set != 0
    }
}

pub(crate) const GLOB: Info = Info {
    match_all: '*' as u32,
    match_one: '?' as u32,
    match_set: '[' as u32,
    no_case: false,
};

pub(crate) const LIKE: Info = Info {
    match_all: '%' as u32,
    match_one: '_' as u32,
    match_set: 0,
    no_case: true,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Match,
    NoMatch,
    /// No match in spite of wildcards: no later start can match either.
    NoWildcardMatch,
    /// The time limit passed.
    Stopped,
}

/// Characters looked at between two reads of the watchdog flag.
const CHECK_EVERY: u32 = 4096;

/// Why [`compare`] gave no answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stopped;

/// Whether `string` matches `pattern`, as SQLite's `like()`/`glob()` decide
/// once the arguments are text. `escape` is the ESCAPE character for LIKE
/// (0 for none); GLOB passes `[`.
pub(crate) fn compare(
    pattern: &[u8],
    string: &[u8],
    info: Info,
    escape: u32,
    stop: &Late,
) -> Result<bool, Stopped> {
    let mut info = info;
    // The ESCAPE character takes the place of a wildcard it equals.
    if escape != 0 && info.match_set == 0 {
        if escape == info.match_all {
            info.match_all = 0;
        }
        if escape == info.match_one {
            info.match_one = 0;
        }
    }
    let mut matcher = Matcher {
        info,
        match_other: escape,
        stop,
        work: 0,
    };
    // SQLite reads both as C text: they end at the first NUL.
    match matcher.run(c_text(pattern), c_text(string)) {
        Outcome::Match => Ok(true),
        Outcome::NoMatch | Outcome::NoWildcardMatch => Ok(false),
        Outcome::Stopped => Err(Stopped),
    }
}

/// The bytes before the first NUL.
pub(crate) fn c_text(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&b| b == 0) {
        Some(end) => &bytes[..end],
        None => bytes,
    }
}

/// SQLite's `sqlite3Utf8Read` on C text: the next character, or 0 at the end.
pub(crate) fn read_utf8(s: &[u8], i: &mut usize) -> u32 {
    let Some(&first) = s.get(*i) else {
        return 0;
    };
    *i += 1;
    let mut c = u32::from(first);
    if c >= 0xC0 {
        c = match first {
            0xC0..=0xDF => c & 0x1F,
            0xE0..=0xEF => c & 0x0F,
            0xF0..=0xF7 => c & 0x07,
            0xF8..=0xFB => c & 0x03,
            0xFC..=0xFD => c & 0x01,
            _ => 0,
        };
        while let Some(&b) = s.get(*i) {
            if b & 0xC0 != 0x80 {
                break;
            }
            c = (c << 6).wrapping_add(u32::from(b & 0x3F));
            *i += 1;
        }
        if c < 0x80 || (c & 0xFFFF_F800) == 0xD800 || (c & 0xFFFF_FFFE) == 0xFFFE {
            c = 0xFFFD;
        }
    }
    c
}

/// SQLite's ASCII-only case folding.
fn lower(c: u32) -> u32 {
    if (u32::from(b'A')..=u32::from(b'Z')).contains(&c) {
        c + 32
    } else {
        c
    }
}

fn upper(c: u32) -> u32 {
    if (u32::from(b'a')..=u32::from(b'z')).contains(&c) {
        c - 32
    } else {
        c
    }
}

struct Matcher<'a> {
    info: Info,
    /// The escape character (LIKE) or `[` (GLOB).
    match_other: u32,
    stop: &'a Late,
    work: u32,
}

impl Matcher<'_> {
    /// Counts work; true once the time limit has passed.
    fn late(&mut self, units: usize) -> bool {
        self.work = self
            .work
            .wrapping_add(u32::try_from(units).unwrap_or(u32::MAX));
        if self.work >= CHECK_EVERY {
            self.work = 0;
            return self.stop.passed();
        }
        false
    }

    /// `patternCompare(zPattern, zString, pInfo, matchOther)`, with the
    /// pattern and string as slices and positions instead of pointers.
    fn run(&mut self, pat: &[u8], s: &[u8]) -> Outcome {
        self.compare(pat, 0, s, 0)
    }

    #[allow(clippy::too_many_lines)]
    fn compare(&mut self, pat: &[u8], mut p: usize, s: &[u8], mut z: usize) -> Outcome {
        let Info {
            match_all,
            match_one,
            match_set,
            no_case,
        } = self.info;
        let match_other = self.match_other;
        // One past the last escaped pattern character.
        let mut escaped: Option<usize> = None;
        loop {
            if self.late(1) {
                return Outcome::Stopped;
            }
            let mut c = read_utf8(pat, &mut p);
            if c == 0 {
                break;
            }
            if c == match_all {
                // Skip more "*", and a character of the string per "?".
                loop {
                    c = read_utf8(pat, &mut p);
                    if c == match_all {
                        continue;
                    }
                    if c == match_one && match_one != 0 {
                        if read_utf8(s, &mut z) == 0 {
                            return Outcome::NoWildcardMatch;
                        }
                        continue;
                    }
                    break;
                }
                if c == 0 {
                    return Outcome::Match;
                } else if c == match_other {
                    if match_set == 0 {
                        c = read_utf8(pat, &mut p);
                        if c == 0 {
                            return Outcome::NoWildcardMatch;
                        }
                    } else {
                        // "[...]" right after "*": try every start.
                        while z < s.len() {
                            if self.late(1) {
                                return Outcome::Stopped;
                            }
                            let m = self.compare(pat, p - 1, s, z);
                            if m != Outcome::NoMatch {
                                return m;
                            }
                            // SQLITE_SKIP_UTF8
                            z += 1;
                            if s[z - 1] >= 0xC0 {
                                while z < s.len() && s[z] & 0xC0 == 0x80 {
                                    z += 1;
                                }
                            }
                        }
                        return Outcome::NoWildcardMatch;
                    }
                }
                // Look for the next character of the pattern in the string
                // and continue matching from each place it occurs.
                if c < 0x80 {
                    let (a, b) = if no_case {
                        (upper(c), lower(c))
                    } else {
                        (c, c)
                    };
                    loop {
                        // strcspn: the first byte that is a or b.
                        let from = z;
                        while z < s.len() && u32::from(s[z]) != a && u32::from(s[z]) != b {
                            z += 1;
                        }
                        if self.late(z - from) {
                            return Outcome::Stopped;
                        }
                        if z >= s.len() {
                            break;
                        }
                        z += 1;
                        let m = self.compare(pat, p, s, z);
                        if m != Outcome::NoMatch {
                            return m;
                        }
                    }
                } else {
                    loop {
                        if self.late(1) {
                            return Outcome::Stopped;
                        }
                        let c2 = read_utf8(s, &mut z);
                        if c2 == 0 {
                            break;
                        }
                        if c2 != c {
                            continue;
                        }
                        let m = self.compare(pat, p, s, z);
                        if m != Outcome::NoMatch {
                            return m;
                        }
                    }
                }
                return Outcome::NoWildcardMatch;
            }
            if c == match_other {
                if match_set == 0 {
                    c = read_utf8(pat, &mut p);
                    if c == 0 {
                        return Outcome::NoMatch;
                    }
                    escaped = Some(p);
                } else {
                    let mut prior_c = 0;
                    let mut seen = false;
                    let mut invert = false;
                    let c = read_utf8(s, &mut z);
                    if c == 0 {
                        return Outcome::NoMatch;
                    }
                    let mut c2 = read_utf8(pat, &mut p);
                    if c2 == u32::from(b'^') {
                        invert = true;
                        c2 = read_utf8(pat, &mut p);
                    }
                    if c2 == u32::from(b']') {
                        if c == u32::from(b']') {
                            seen = true;
                        }
                        c2 = read_utf8(pat, &mut p);
                    }
                    while c2 != 0 && c2 != u32::from(b']') {
                        let next = pat.get(p).copied().unwrap_or(0);
                        if c2 == u32::from(b'-') && next != b']' && next != 0 && prior_c > 0 {
                            c2 = read_utf8(pat, &mut p);
                            if c >= prior_c && c <= c2 {
                                seen = true;
                            }
                            prior_c = 0;
                        } else {
                            if c == c2 {
                                seen = true;
                            }
                            prior_c = c2;
                        }
                        c2 = read_utf8(pat, &mut p);
                    }
                    if c2 == 0 || seen == invert {
                        return Outcome::NoMatch;
                    }
                    continue;
                }
            }
            let c2 = read_utf8(s, &mut z);
            if c == c2 {
                continue;
            }
            if no_case && lower(c) == lower(c2) && c < 0x80 && c2 < 0x80 {
                continue;
            }
            if c == match_one && escaped != Some(p) && c2 != 0 {
                continue;
            }
            return Outcome::NoMatch;
        }
        if z >= s.len() {
            Outcome::Match
        } else {
            Outcome::NoMatch
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_late_flag_stops_a_long_match() {
        let stop = Late::default();
        stop.set();
        let text = "a".repeat(100_000);
        let pattern = format!("%{}b", "a".repeat(98));
        assert_eq!(
            compare(pattern.as_bytes(), text.as_bytes(), LIKE, 0, &stop),
            Err(Stopped)
        );
        let go = Late::default();
        assert_eq!(
            compare(pattern.as_bytes(), text.as_bytes(), LIKE, 0, &go),
            Ok(false)
        );
    }
}
