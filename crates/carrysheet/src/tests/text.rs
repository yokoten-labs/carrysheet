use crate::text::*;

/// One character of each kind that must be hidden.
const SAMPLES: &[(char, &str)] = &[
    ('\u{0}', "Cc"),
    ('\u{1b}', "Cc"),
    ('\n', "Cc"),
    ('\u{7f}', "Cc"),
    ('\u{85}', "Cc"),
    ('\u{ad}', "Cf"),
    ('\u{600}', "Cf"),
    ('\u{200b}', "Cf"),
    ('\u{202e}', "Cf"),
    ('\u{2066}', "Cf"),
    ('\u{feff}', "Cf"),
    ('\u{fff9}', "Cf"),
    ('\u{110bd}', "Cf"),
    ('\u{1d173}', "Cf"),
    ('\u{e0001}', "Cf (tag)"),
    ('\u{e0041}', "Cf (tag)"),
    ('\u{e007f}', "Cf (tag)"),
    ('\u{e000}', "Co"),
    ('\u{f0000}', "Co"),
    ('\u{10fffd}', "Co"),
    ('\u{2028}', "Zl"),
    ('\u{2029}', "Zp"),
    ('\u{34f}', "DI"),
    ('\u{115f}', "DI"),
    ('\u{3164}', "DI"),
    ('\u{fe0f}', "DI"),
    ('\u{ffa0}', "DI"),
    ('\u{2065}', "DI (reserved)"),
    ('\u{e0000}', "DI (reserved tag)"),
    ('\u{e0100}', "DI"),
    ('\u{e0fff}', "DI (reserved)"),
];

#[test]
fn every_class_is_escaped() {
    for &(c, class) in SAMPLES {
        assert!(is_hidden(c), "{class} U+{:04X}", u32::from(c));
        let want = format!("a\\u{{{:x}}}b", u32::from(c));
        assert_eq!(escape(&format!("a{c}b"), 100), want, "{class}");
    }
    assert_eq!(escape("a\\u{1b}", 100), "a\\\\u{1b}");
    assert_eq!(quote("say \"hi\"\\", 100), "\"say \\\"hi\\\"\\\\\"");
    assert_eq!(escape("\"", 100), "\"");
}

#[test]
fn visible_text_is_kept() {
    for text in [
        "plain",
        "Módulo1",
        "日本語",
        "😀",
        "e\u{301}",
        "a b",
        "\u{a0}",
    ] {
        assert_eq!(escape(text, 100), text);
    }
}

#[test]
fn no_scalar_value_survives_escaping() {
    for cp in 0..=0x10_FFFFu32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let one = c.to_string();
        for out in [escape(&one, 16), quote(&one, 16)] {
            assert!(!out.chars().any(is_hidden), "U+{cp:04X} -> {out:?}");
        }
        assert!(!escape(&one, 16).contains(CUT), "U+{cp:04X}");
    }
}

/// Splits escaped text into pieces; `None` when an escape is broken.
fn pieces(body: &str) -> Option<usize> {
    let mut n = 0;
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                '\\' | '"' => {}
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let mut digits = 0;
                    loop {
                        match chars.next()? {
                            '}' if digits > 0 => break,
                            '0'..='9' | 'a'..='f' => digits += 1,
                            _ => return None,
                        }
                    }
                }
                _ => return None,
            }
        }
        n += 1;
    }
    Some(n)
}

#[test]
fn a_cut_never_splits_an_escape() {
    let text = "ab\u{1b}\\c\u{e0041}\"d\u{10fffd}e";
    let total = text.chars().count();
    for max in 0..40 {
        for quoted in [false, true] {
            let out = if quoted {
                quote(text, max)
            } else {
                escape(text, max)
            };
            let (body, rest) = match out.find(CUT) {
                Some(at) => {
                    let mark = &out[at + CUT.len()..];
                    let n: usize = mark.split(' ').next().unwrap().parse().unwrap();
                    (&out[..at], n)
                }
                None => (&out[..], 0),
            };
            let body = if quoted {
                body.strip_prefix('"')
                    .and_then(|b| b.strip_suffix('"'))
                    .unwrap()
            } else {
                body
            };
            assert!(body.chars().count() <= max, "{max}: {out:?}");
            let shown = pieces(body).unwrap_or_else(|| panic!("{max}: {out:?}"));
            assert_eq!(shown + rest, total, "{max}: {out:?}");
        }
    }
    assert_eq!(escape("abcdef", 3), "abc…[cut: 3 more characters]");
    assert_eq!(escape("abcd", 3), "abc…[cut: 1 more character]");
    assert_eq!(escape("a\u{1b}", 6), "a…[cut: 1 more character]");
    assert_eq!(quote("abcd", 2), "\"ab\"…[cut: 2 more characters]");
    assert_eq!(escape("", 0), "");
}

#[test]
fn table_is_sorted_and_named() {
    assert!(HIDDEN.windows(2).all(|w| w[0].1 + 1 < w[1].0));
    assert!(HIDDEN.iter().all(|&(a, b)| a <= b && b <= 0x10_FFFF));
    for c in (0..=0x10_FFFFu32).filter_map(char::from_u32) {
        if c.is_control() {
            assert!(is_hidden(c), "U+{:04X}", u32::from(c));
        }
    }
}

#[test]
fn file_text_in_an_error_message_is_escaped() {
    let e = crate::internal("opening the file", "x\u{E0041}\u{202E}\u{1b}[31m\\u{41}");
    let want = "x\\u{e0041}\\u{202e}\\u{1b}[31m\\\\u{41}";
    assert!(e.why.ends_with(want), "{}", e.why);
}
