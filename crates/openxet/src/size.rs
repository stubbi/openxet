//! Byte sizes as configuration writes them: a whole number and one binary
//! unit, `MiB`, `GiB` or `TiB`, such as `512MiB` or `2GiB`.

/// The accepted form, for error messages.
pub const FORM: &str = "a whole number followed by MiB, GiB or TiB, such as 512MiB or 2GiB";

const UNITS: [(&str, u32); 3] = [("TiB", 40), ("GiB", 30), ("MiB", 20)];

/// Parses `text`, or returns `None` if it is not of the form [`FORM`] or
/// overflows.
pub fn parse(text: &str) -> Option<u64> {
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = text.split_at(split);
    if digits.is_empty() {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let (_, shift) = UNITS.iter().find(|(name, _)| *name == unit)?;
    n.checked_mul(1 << shift)
}

/// `bytes` in the largest unit of [`FORM`] that writes it exactly, or in
/// bytes if none does.
pub fn format(bytes: u64) -> String {
    UNITS
        .iter()
        .find(|(_, shift)| bytes > 0 && bytes.trailing_zeros() >= *shift)
        .map_or_else(
            || format!("{bytes}B"),
            |(name, shift)| format!("{}{name}", bytes >> shift),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_unit() {
        assert_eq!(parse("512MiB"), Some(512 << 20));
        assert_eq!(parse("2GiB"), Some(2 << 30));
        assert_eq!(parse("1TiB"), Some(1 << 40));
        assert_eq!(parse("0MiB"), Some(0));
    }

    #[test]
    fn rejects_anything_else() {
        for bad in [
            "",
            "1024",
            "GiB",
            "1.5GiB",
            "-1GiB",
            "1 GiB",
            " 1GiB",
            "1GB",
            "1gib",
            "1G",
            "1KiB",
            "1GiB1MiB",
            "17179869184TiB",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn formats_in_the_largest_exact_unit() {
        assert_eq!(format(1 << 30), "1GiB");
        assert_eq!(format(1536 << 20), "1536MiB");
        assert_eq!(format(2 << 40), "2TiB");
        assert_eq!(format(1000), "1000B");
        assert_eq!(format(0), "0B");
        for text in ["256MiB", "1GiB", "3TiB"] {
            assert_eq!(parse(&format(parse(text).unwrap())), parse(text));
        }
    }
}
