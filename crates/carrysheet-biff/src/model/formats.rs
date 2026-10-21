//! Built-in number formats and date detection.

/// The en-US text of a built-in number format id, for ids that a BIFF5 or
/// BIFF8 file uses without a FORMAT record ([MS-XLS] 2.4.126, ECMA-376
/// 18.8.30; ids 5 to 8 and 41 to 44 are the en-US currency forms).
pub fn builtin_format(id: u16) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "\"$\"#,##0_);\\(\"$\"#,##0\\)",
        6 => "\"$\"#,##0_);[Red]\\(\"$\"#,##0\\)",
        7 => "\"$\"#,##0.00_);\\(\"$\"#,##0.00\\)",
        8 => "\"$\"#,##0.00_);[Red]\\(\"$\"#,##0.00\\)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0_);\\(#,##0\\)",
        38 => "#,##0_);[Red]\\(#,##0\\)",
        39 => "#,##0.00_);\\(#,##0.00\\)",
        40 => "#,##0.00_);[Red]\\(#,##0.00\\)",
        41 => "_(* #,##0_);_(* \\(#,##0\\);_(* \"-\"_);_(@_)",
        42 => "_(\"$\"* #,##0_);_(\"$\"* \\(#,##0\\);_(\"$\"* \"-\"_);_(@_)",
        43 => "_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)",
        44 => "_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// True if a number format shows numbers as dates or times.
///
/// Looks at the first section only, ignoring quoted text, escaped and
/// padding characters, and bracketed colors and locales (but not elapsed
/// time such as `[h]`).
pub fn is_date_format(format: &str) -> bool {
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ';' => return false,
            '"' => {
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let mut inner = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    inner.push(d.to_ascii_lowercase());
                }
                if !inner.is_empty() && inner.chars().all(|d| matches!(d, 'h' | 'm' | 's')) {
                    return true;
                }
            }
            'y' | 'Y' | 'm' | 'M' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' => return true,
            // "General" holds an 'e' and 'n' but no date letters; 'E' is
            // the exponent marker.
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_not_dates() {
        for f in [
            "m/d/yyyy",
            "d-mmm-yy",
            "h:mm AM/PM",
            "[h]:mm:ss",
            "yyyy\\-mm\\-dd",
            "[$-409]mmmm d, yyyy",
        ] {
            assert!(is_date_format(f), "{f}");
        }
        for f in [
            "General",
            "0.00",
            "#,##0_);[Red]\\(#,##0\\)",
            "0.00E+00",
            "\"Days\" 0",
            "@",
            "[Red]0.0",
            "0;\"d\"",
        ] {
            assert!(!is_date_format(f), "{f}");
        }
        for id in [14, 15, 16, 17, 18, 19, 20, 21, 22, 45, 46, 47] {
            assert!(is_date_format(builtin_format(id).unwrap()), "{id}");
        }
    }
}
