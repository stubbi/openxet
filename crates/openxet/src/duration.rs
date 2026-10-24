//! Durations as configuration and flags write them: a whole number and one
//! unit, `s`, `m`, `h` or `d`, such as `30s`, `60m` or `30d`.

use std::time::Duration;

/// The accepted form, for error messages.
pub const FORM: &str = "a whole number followed by s, m, h or d, such as 30s, 60m, 24h or 30d";

/// Parses `text`, or returns `None` if it is not of the form [`FORM`] or
/// overflows.
pub fn parse(text: &str) -> Option<Duration> {
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = text.split_at(split);
    if digits.is_empty() {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// `d` in the largest unit of [`FORM`] that writes it exactly, except that
/// up to a day reads in hours (`24h`, not `1d`).
pub fn format(d: Duration) -> String {
    let secs = d.as_secs();
    for (unit, size) in [("d", 86_400), ("h", 3600), ("m", 60)] {
        if secs > 0 && secs.is_multiple_of(size) && (unit != "d" || secs > 86_400) {
            return format!("{}{unit}", secs / size);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_unit() {
        assert_eq!(parse("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse("60m"), Some(Duration::from_hours(1)));
        assert_eq!(parse("24h"), Some(Duration::from_hours(24)));
        assert_eq!(parse("30d"), Some(Duration::from_hours(720)));
        assert_eq!(parse("0s"), Some(Duration::ZERO));
    }

    #[test]
    fn rejects_anything_else() {
        for bad in [
            "",
            "30",
            "s",
            "1.5h",
            "-1s",
            "1h30m",
            "1 h",
            " 1h",
            "1H",
            "1w",
            "1ms",
            "99999999999999999999d",
            "184467440737095517d",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn formats_in_the_largest_exact_unit() {
        assert_eq!(format(Duration::from_mins(5)), "5m");
        assert_eq!(format(Duration::from_secs(90)), "90s");
        assert_eq!(format(Duration::from_hours(48)), "2d");
        assert_eq!(format(Duration::from_hours(24)), "24h");
        assert_eq!(format(Duration::ZERO), "0s");
        for text in ["30s", "60m", "24h", "30d"] {
            assert_eq!(parse(&format(parse(text).unwrap())), parse(text));
        }
    }
}
