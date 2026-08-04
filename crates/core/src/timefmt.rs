/// Civil-from-days (Howard Hinnant's algorithm): days since 1970-01-01 -> (y, m, d).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Format unix seconds (UTC) as "YYYYMMDD-HHMMSS".
pub fn format_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(format_timestamp(0), "19700101-000000");
    }

    #[test]
    fn known_dates() {
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
        assert_eq!(civil_from_days(20669), (2026, 8, 4));
    }

    #[test]
    fn timestamp_includes_time_of_day() {
        let secs = 20669 * 86400 + 15 * 3600 + 32 * 60 + 45;
        assert_eq!(format_timestamp(secs), "20260804-153245");
    }

    #[test]
    fn negative_epoch_pre_1970() {
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(format_timestamp(-1), "19691231-235959");
    }
}
