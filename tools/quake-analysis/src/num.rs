//! Integer parsing and float formatting with the rules the reports use.

/// Parse an integer the way a permissive scripting runtime does.
///
/// `base` is 2..=36, or 0 to pick the base from a `0x`, `0o` or `0b` prefix
/// (decimal otherwise). Surrounding whitespace, a leading sign and single
/// underscores between digits are accepted. Returns `None` for anything else.
pub fn parse_int(text: &str, base: u32) -> Option<i128> {
    let text = text.trim();
    let (negative, rest) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let lower = rest.to_ascii_lowercase();
    let prefixed = |p: &str| lower.strip_prefix(p).map(str::to_owned);
    let (radix, digits) = match base {
        0 => {
            if let Some(d) = prefixed("0x") {
                (16, d)
            } else if let Some(d) = prefixed("0o") {
                (8, d)
            } else if let Some(d) = prefixed("0b") {
                (2, d)
            } else {
                (10, lower.clone())
            }
        }
        16 => (16, prefixed("0x").unwrap_or_else(|| lower.clone())),
        8 => (8, prefixed("0o").unwrap_or_else(|| lower.clone())),
        2 => (2, prefixed("0b").unwrap_or_else(|| lower.clone())),
        other => (other, lower.clone()),
    };
    // A prefix may be followed by one underscore ("0x_ff").
    let digits = if radix != 10 || base == 0 {
        digits.strip_prefix('_').unwrap_or(&digits).to_owned()
    } else {
        digits
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let digits: String = digits.chars().filter(|c| *c != '_').collect();
    if !digits.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    // Base 0 decimal rejects leading zeros unless the value is zero.
    if base == 0 && radix == 10 && digits.len() > 1 && digits.starts_with('0') {
        if digits.chars().all(|c| c == '0') {
            return Some(0);
        }
        return None;
    }
    let value = i128::from_str_radix(&digits, radix).ok()?;
    Some(if negative { -value } else { value })
}

/// Format a float with the shortest digits that round-trip, laid out as
/// Python's `repr(float)` does: fixed notation for decimal exponents in
/// `-4..16`, otherwise `d.ddde+XX`. Whole numbers keep a trailing `.0`.
pub fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0.0");
    }
    // `{:e}` yields the shortest round-trip digits as `d[.ddd]e<exp>`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("LowerExp output always has an exponent");
    let exponent: i32 = exponent.parse().expect("LowerExp exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let decimal_point = exponent + 1;
    if !(-3..=16).contains(&decimal_point) {
        let mut out = String::from(sign);
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exponent.abs()));
        return out;
    }
    let length = digits.len() as i32;
    let body = if decimal_point <= 0 {
        format!("0.{}{}", "0".repeat((-decimal_point) as usize), digits)
    } else if decimal_point >= length {
        format!(
            "{}{}.0",
            digits,
            "0".repeat((decimal_point - length) as usize)
        )
    } else {
        let (whole, fraction) = digits.split_at(decimal_point as usize);
        format!("{whole}.{fraction}")
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::{float_repr, parse_int};

    #[test]
    fn parses_python_style_integers() {
        assert_eq!(parse_int("0x1F", 0), Some(31));
        assert_eq!(parse_int("1f", 16), Some(31));
        assert_eq!(parse_int("0x1f", 16), Some(31));
        assert_eq!(parse_int(" 42 ", 10), Some(42));
        assert_eq!(parse_int("-7", 0), Some(-7));
        assert_eq!(parse_int("1_000", 0), Some(1000));
        assert_eq!(parse_int("0b101", 0), Some(5));
        assert_eq!(parse_int("0o17", 0), Some(15));
        assert_eq!(parse_int("0", 0), Some(0));
        assert_eq!(parse_int("000", 0), Some(0));
        assert_eq!(parse_int("007", 0), None);
        assert_eq!(parse_int("", 10), None);
        assert_eq!(parse_int("zz", 16), None);
        assert_eq!(parse_int("1__0", 10), None);
        assert_eq!(parse_int("0xffffffffffffffff", 0), Some(u64::MAX as i128));
    }

    #[test]
    fn float_repr_matches_python() {
        let cases: [(f64, &str); 14] = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (50.0, "50.0"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.3333333333333333"),
            (100.0, "100.0"),
            (123456789.125, "123456789.125"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e17, "1.5e+17"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.234e-7, "1.234e-07"),
            (-2.5, "-2.5"),
        ];
        for (value, expected) in cases {
            assert_eq!(float_repr(value), expected, "{value:e}");
        }
    }

    #[test]
    fn fixed_precision_rounds_the_exact_binary_value() {
        // Ties in the decimal text are decided by the exact binary value and
        // then round to even, exactly as C printf and Python do.
        assert_eq!(format!("{:.2}", 0.125), "0.12");
        assert_eq!(format!("{:.2}", 0.375), "0.38");
        assert_eq!(format!("{:.2}", 2.675), "2.67");
        assert_eq!(format!("{:.2}", 1.005), "1.00");
        assert_eq!(format!("{:.1}", 0.25), "0.2");
        assert_eq!(format!("{:.0}", 2.5), "2");
    }
}

/// Split text into lines on the same boundaries as Python's `str.splitlines`
/// (`\n`, `\r\n`, `\r`, VT, FF, FS, GS, RS, NEL, LS, PS). The terminators are
/// dropped and a trailing terminator does not start another line.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        start = index + c.len_utf8();
        if c == '\r' {
            if let Some(&(next, '\n')) = chars.peek() {
                chars.next();
                start = next + 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod line_tests {
    use super::split_lines;

    #[test]
    fn splits_like_splitlines() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), ["a", "b", "c", "d"]);
        assert_eq!(split_lines("a\n"), ["a"]);
        assert_eq!(split_lines("a\n\nb"), ["a", "", "b"]);
        assert!(split_lines("").is_empty());
        assert_eq!(split_lines("x\u{c}y\u{2028}z"), ["x", "y", "z"]);
    }
}
