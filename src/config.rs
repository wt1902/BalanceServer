use chrono::TimeDelta;
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Config {
    pub address: String,
    pub database_path: String,
    pub jwt_secret: String,
    pub access_ttl: TimeDelta,
    pub refresh_ttl: TimeDelta,
    pub allow_registration: bool,
    pub cors_origins: Vec<String>,
    pub attachments_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let access_ttl = duration("BALANCE_ACCESS_TTL", TimeDelta::minutes(15))?;
        let refresh_ttl = duration("BALANCE_REFRESH_TTL", TimeDelta::days(30))?;
        let allow_registration = match value("BALANCE_ALLOW_REGISTRATION").as_str() {
            "" | "1" | "t" | "T" | "TRUE" | "true" | "True" => true,
            "0" | "f" | "F" | "FALSE" | "false" | "False" => false,
            _ => return Err("BALANCE_ALLOW_REGISTRATION must be true or false".into()),
        };
        let jwt_secret = value("BALANCE_JWT_SECRET");
        if jwt_secret.len() < 32 {
            return Err("BALANCE_JWT_SECRET must contain at least 32 characters".into());
        }
        let database_path = fallback("BALANCE_DB_PATH", "./data/balance.db");
        let attachments_dir = match value("BALANCE_ATTACHMENTS_DIR") {
            dir if dir.is_empty() => default_attachments_dir(&database_path),
            dir => PathBuf::from(dir),
        };
        Ok(Self {
            address: fallback("BALANCE_ADDR", ":8080"),
            database_path,
            jwt_secret,
            access_ttl,
            refresh_ttl,
            allow_registration,
            cors_origins: value("BALANCE_CORS_ORIGINS")
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect(),
            attachments_dir,
        })
    }
}

/// `attachments` next to the database file.
pub fn default_attachments_dir(database_path: &str) -> PathBuf {
    Path::new(database_path)
        .parent()
        .unwrap_or(Path::new(""))
        .join("attachments")
}

fn value(key: &str) -> String {
    env::var(key).unwrap_or_default().trim().to_owned()
}
fn fallback(key: &str, default: &str) -> String {
    let value = value(key);
    if value.is_empty() {
        default.to_owned()
    } else {
        value
    }
}
fn duration(key: &str, default: TimeDelta) -> Result<TimeDelta, String> {
    let value = value(key);
    if value.is_empty() {
        return Ok(default);
    }
    parse_go_duration(&value)
        .ok_or_else(|| format!("{key} must be a Go duration such as 15m or 720h"))
}

/// Parse Go's signed, nanosecond-precision duration syntax and int64 range.
pub fn parse_go_duration(input: &str) -> Option<TimeDelta> {
    let (negative, mut remaining) = match input.as_bytes().first()? {
        b'-' => (true, &input[1..]),
        b'+' => (false, &input[1..]),
        _ => (false, input),
    };
    if remaining == "0" {
        return Some(TimeDelta::zero());
    }
    if remaining.is_empty() {
        return None;
    }
    let mut total = 0i128;
    while !remaining.is_empty() {
        let integer_len = remaining.bytes().take_while(u8::is_ascii_digit).count();
        let integer = if integer_len == 0 {
            0
        } else {
            remaining[..integer_len].parse::<i128>().ok()?
        };
        remaining = &remaining[integer_len..];
        let mut fraction = 0i128;
        let mut scale = 1i128;
        let mut fraction_len = 0;
        if remaining.starts_with('.') {
            remaining = &remaining[1..];
            fraction_len = remaining.bytes().take_while(u8::is_ascii_digit).count();
            // Beyond 18 decimal places no supported unit can affect nanoseconds.
            for digit in remaining.bytes().take(fraction_len.min(18)) {
                fraction = fraction * 10 + i128::from(digit - b'0');
                scale *= 10;
            }
            remaining = &remaining[fraction_len..];
        }
        if integer_len == 0 && fraction_len == 0 {
            return None;
        }
        let (unit_text, unit) = [
            ("ns", 1i128),
            ("us", 1_000),
            ("µs", 1_000),
            ("μs", 1_000),
            ("ms", 1_000_000),
            ("s", 1_000_000_000),
            ("m", 60_000_000_000),
            ("h", 3_600_000_000_000),
        ]
        .into_iter()
        .find(|(unit, _)| remaining.starts_with(unit))?;
        remaining = &remaining[unit_text.len()..];
        total = total
            .checked_add(integer.checked_mul(unit)?)?
            .checked_add(fraction * unit / scale)?;
        if total > i128::from(i64::MAX) + i128::from(negative) {
            return None;
        }
    }
    let nanos = i64::try_from(if negative { -total } else { total }).ok()?;
    Some(TimeDelta::nanoseconds(nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachments_default_next_to_database() {
        assert_eq!(
            default_attachments_dir("/data/balance.db"),
            Path::new("/data/attachments")
        );
        assert_eq!(
            default_attachments_dir("balance.db"),
            Path::new("attachments")
        );
    }
    #[test]
    fn duration_matches_go_syntax_and_range() {
        assert_eq!(parse_go_duration("1h30m"), Some(TimeDelta::minutes(90)));
        assert_eq!(
            parse_go_duration("-.5s"),
            Some(TimeDelta::milliseconds(-500))
        );
        assert_eq!(parse_go_duration("1µs"), Some(TimeDelta::microseconds(1)));
        assert_eq!(parse_go_duration("1.s"), Some(TimeDelta::seconds(1)));
        assert_eq!(parse_go_duration("0"), Some(TimeDelta::zero()));
        assert_eq!(
            parse_go_duration("9223372036854775807ns"),
            Some(TimeDelta::nanoseconds(i64::MAX))
        );
        assert_eq!(
            parse_go_duration("-9223372036854775808ns"),
            Some(TimeDelta::nanoseconds(i64::MIN))
        );
        for invalid in [
            "",
            "1d",
            "1",
            "9223372036854775808ns",
            "1h 2m",
            ".s",
            "--1s",
        ] {
            assert!(parse_go_duration(invalid).is_none(), "{invalid}");
        }
    }
}
