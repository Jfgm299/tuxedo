//! Lengths of time as tasq writes them in tags: `dur:1h30m` (how long a
//! task takes) and `remind:15m,1d` (reminders before its time). A value is
//! a run of `<number><unit>` parts, units `m` (minutes), `h` (hours), `d`
//! (days) and `w` (weeks); it is kept in minutes.

/// Minutes in `s` (`45m`, `1h`, `1h30m`, `2d`); `None` if it isn't a
/// length or is zero.
pub fn parse_minutes(s: &str) -> Option<u32> {
    let mut total: u32 = 0;
    let mut num = String::new();
    let mut any = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u32 = num.parse().ok()?;
        num.clear();
        let per = match c {
            'm' => 1,
            'h' => 60,
            'd' => 60 * 24,
            'w' => 60 * 24 * 7,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(per)?)?;
        any = true;
    }
    (any && num.is_empty() && total > 0).then_some(total)
}

/// The tag form of `minutes`: whole weeks, days and hours where they fit
/// (`90` → `1h30m`, `1440` → `1d`).
pub fn format_minutes(minutes: u32) -> String {
    let mut out = String::new();
    let mut rest = minutes;
    for (unit, per) in [('w', 60 * 24 * 7), ('d', 60 * 24), ('h', 60), ('m', 1)] {
        if rest >= per {
            out.push_str(&format!("{}{unit}", rest / per));
            rest %= per;
        }
    }
    if out.is_empty() {
        out.push_str("0m");
    }
    out
}

/// A length for people: `1h 30m`, `45 min`, `1 day`.
pub fn describe(minutes: u32) -> String {
    match minutes {
        m if m % (60 * 24 * 7) == 0 => plural(m / (60 * 24 * 7), "week"),
        m if m % (60 * 24) == 0 => plural(m / (60 * 24), "day"),
        m if m < 60 => format!("{m} min"),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{}h {}m", m / 60, m % 60),
    }
}

fn plural(n: u32, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// The reminders in a `remind:` value (`15m,1d`), in minutes before.
pub fn parse_reminders(s: &str) -> Option<Vec<u32>> {
    s.split(',').map(parse_minutes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (s, m) in [
            ("45m", 45),
            ("1h", 60),
            ("1h30m", 90),
            ("1d", 1440),
            ("2w", 20160),
        ] {
            assert_eq!(parse_minutes(s), Some(m), "{s}");
            assert_eq!(format_minutes(m), s);
        }
        for bad in ["", "m", "15", "15x", "0m", "1h30"] {
            assert_eq!(parse_minutes(bad), None, "{bad}");
        }
        assert_eq!(parse_reminders("15m,1d"), Some(vec![15, 1440]));
        assert_eq!(describe(90), "1h 30m");
        assert_eq!(describe(30), "30 min");
        assert_eq!(describe(120), "2h");
        assert_eq!(describe(1440), "1 day");
    }
}
