use std::time::Duration;

pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f64 = number
        .parse()
        .map_err(|_| format!("`{text}` is not a duration like 60s or 250ms"))?;
    let seconds = match unit.trim() {
        "us" | "µs" => value / 1e6,
        "ms" => value / 1e3,
        "s" | "" => value,
        "m" | "min" => value * 60.0,
        "h" => value * 3600.0,
        "d" => value * 86400.0,
        other => return Err(format!("`{other}` is not a time unit in `{text}`")),
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(format!("`{text}` is out of range"));
    }
    Ok(Duration::from_secs_f64(seconds))
}

pub fn format_ms(value: Duration) -> String {
    let ms = value.as_secs_f64() * 1e3;
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else if ms >= 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1.5h").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("40us").unwrap(), Duration::from_micros(40));
        assert!(parse_duration("5 parsecs").is_err());
        assert!(parse_duration("ms").is_err());
    }
}
