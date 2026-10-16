// Parse calendar timestamps directly from ASCII, truncating fractional seconds.
pub(crate) fn parse(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = number(&bytes[..4])?;
    let month = number(&bytes[5..7])?;
    let day = number(&bytes[8..10])?;
    let hour = number(&bytes[11..13])?;
    let minute = number(&bytes[14..16])?;
    let second = number(&bytes[17..19])?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if !(1..=month_days).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut zone = &bytes[19..];
    if zone[0] == b'.' {
        let digits = zone[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        zone = &zone[1 + digits..];
    }
    let offset = match zone {
        b"Z" | b"z" => 0,
        zone if zone.len() == 6 && matches!(zone[0], b'+' | b'-') && zone[3] == b':' => {
            let hours = number(&zone[1..3])?;
            let minutes = number(&zone[4..6])?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = hours * 3600 + minutes * 60;
            if zone[0] == b'-' {
                -seconds
            } else {
                seconds
            }
        }
        _ => return None,
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

// All fields have at most four digits, so the arithmetic fits in i64.
fn number(bytes: &[u8]) -> Option<i64> {
    bytes.iter().try_fold(0, |value, &byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + i64::from(byte - b'0'))
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
