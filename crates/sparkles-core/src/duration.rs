//! The one parser of written durations, such as retention ages, timeouts and the age of
//! a transcript to import.

use std::time::Duration;

/// A duration of one or more `<n><unit>` terms, spaces allowed around the units and
/// between terms (`500ms`, `90s`, `30m`, `12h`, `7d`, `2w`, `1d 12h`), in any case.
/// Units: `ms`, `s`, `m`, `h`, `d` and `w`, also spelled `msec`, `sec`, `min`, `hr`,
/// `hour`, `day` and `week`, and their plurals. `None` for anything else, for a bare
/// number and when the total overflows. A caller that takes a bare number in a unit of
/// its own, or refuses a zero duration, checks that itself.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let src = s.trim().to_ascii_lowercase();
    let mut rest = src.as_str();
    if rest.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let n: u64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let unit: u64 = match &rest[..letters] {
            "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => 1,
            "s" | "sec" | "secs" | "second" | "seconds" => 1_000,
            "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000,
            "d" | "day" | "days" => 86_400_000,
            "w" | "week" | "weeks" => 604_800_000,
            _ => return None,
        };
        total = n.checked_mul(unit).and_then(|x| total.checked_add(x))?;
        rest = rest[letters..].trim_start();
    }
    Some(Duration::from_millis(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_units_and_refusals() {
        let ms = |s: &str| parse_duration(s).map(|d| d.as_millis());
        assert_eq!(ms("500ms"), Some(500));
        assert_eq!(ms("90s"), Some(90_000));
        assert_eq!(ms(" 30M "), Some(1_800_000));
        assert_eq!(ms("1d 12h"), Some(129_600_000));
        assert_eq!(ms("1d12h"), Some(129_600_000));
        assert_eq!(ms("2 weeks"), Some(1_209_600_000));
        assert_eq!(ms("0s"), Some(0));
        for bad in ["", "90", "s", "5y", "1.5h", "-1s", "99999999999999999999s"] {
            assert_eq!(ms(bad), None, "{bad:?}");
        }
        assert_eq!(ms(&format!("{}w", u64::MAX / 604_800_000 + 1)), None);
    }
}
