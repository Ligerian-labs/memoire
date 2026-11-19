//! Durations as the CLI writes them: `90s`, `30m`, `24h`, `7d`.

/// Parses a positive whole number followed by one unit (`s`, `m`, `h`,
/// `d`) into seconds.
///
/// # Errors
///
/// A short reason when the text has no unit, no digits, a zero value, or
/// overflows.
pub fn parse_seconds(text: &str) -> Result<u64, String> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("`{text}` needs a unit: s, m, h or d"))?;
    let (digits, unit) = text.split_at(split);
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("`{text}` must start with a whole number"))?;
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return Err(format!("`{text}` has an unknown unit; use s, m, h or d")),
    };
    match value.checked_mul(scale) {
        Some(0) => Err("a duration must be positive".into()),
        Some(seconds) => Ok(seconds),
        None => Err(format!("`{text}` is too long")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_seconds;

    #[test]
    fn parses_each_unit() {
        assert_eq!(parse_seconds("90s"), Ok(90));
        assert_eq!(parse_seconds("30m"), Ok(1_800));
        assert_eq!(parse_seconds("24h"), Ok(86_400));
        assert_eq!(parse_seconds("7d"), Ok(604_800));
    }

    #[test]
    fn rejects_malformed_text() {
        for bad in [
            "",
            "24",
            "h",
            "0h",
            "1.5h",
            "-1h",
            "24hours",
            "1w",
            "99999999999999999999d",
        ] {
            assert!(parse_seconds(bad).is_err(), "{bad}");
        }
    }
}
