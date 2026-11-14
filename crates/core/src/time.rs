//! UTC times as the format writes them ("2026-11-11T14:05:00.000Z"),
//! without a calendar dependency.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970");
    i64::try_from(d.as_millis()).expect("in range")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn iso(ms: i64) -> String {
    let (days, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let (y, mo, d) = civil(days);
    let (h, mi, s, milli) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

pub fn now_iso() -> String {
    iso(now_ms())
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z`.
pub fn parse_iso(text: &str) -> Option<i64> {
    let t = text.strip_suffix('Z')?;
    let (date, clock) = t.split_once('T')?;
    let mut dp = date.splitn(3, '-').map(str::parse::<i64>);
    let (y, mo, d) = (dp.next()?.ok()?, dp.next()?.ok()?, dp.next()?.ok()?);
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, "0"));
    let mut cp = hms.splitn(3, ':').map(str::parse::<i64>);
    let (h, mi, s) = (cp.next()?.ok()?, cp.next()?.ok()?, cp.next()?.ok()?);
    let milli: i64 = format!("{frac:0<3}")[..3].parse().ok()?;
    let days = days_from_civil(y, u32::try_from(mo).ok()?, u32::try_from(d).ok()?);
    Some(days * 86_400_000 + ((h * 60 + mi) * 60 + s) * 1000 + milli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(parse_iso("2026-11-11T15:00:00.000Z"), Some(1_794_409_200_000));
        assert_eq!(iso(1_794_409_200_000), "2026-11-11T15:00:00.000Z");
        assert_eq!(parse_iso("2026-11-11T14:05:00Z"), Some(1_794_405_900_000));
        for ms in [-86_400_001, 951_782_400_123, 4_102_444_800_000] {
            assert_eq!(parse_iso(&iso(ms)), Some(ms));
        }
    }
}
