pub(crate) fn parse(text: &str) -> Option<f64> {
    if let Some(digits) = text.strip_prefix("0x") {
        return integer(digits, 16);
    }
    if let Some(digits) = text.strip_prefix("0o") {
        return integer(digits, 8);
    }
    if text == "-Infinity" {
        return Some(f64::NEG_INFINITY);
    }
    decimal(text)
}

fn integer(digits: &str, radix: u32) -> Option<f64> {
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let bits = u32::from_str_radix(digits, radix).ok()?;
    Some(f64::from(bits as i32))
}

fn digits(part: &str) -> bool {
    part.chars().all(|c| c.is_ascii_digit())
}

fn decimal(text: &str) -> Option<f64> {
    let body = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) {
        return None;
    }
    if let Some(exponent) = exponent {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if exponent.is_empty() || !digits(exponent) {
            return None;
        }
    }
    text.parse().ok()
}

pub(crate) fn convert(text: &str) -> f64 {
    let text = text.trim();
    if text.is_empty() {
        return 0.0;
    }
    if let Some(value) = parse(text) {
        return value;
    }
    if text.eq_ignore_ascii_case("infinity") {
        f64::INFINITY
    } else {
        f64::NAN
    }
}

pub(crate) fn text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let scientific = format!("{value:.14e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if mantissa.starts_with('-') { "-" } else { "" };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = match digits.trim_end_matches('0') {
        "" => "0",
        digits => digits,
    };
    if (-4..15).contains(&exponent) {
        if exponent < 0 {
            let zeros = "0".repeat(exponent.unsigned_abs() as usize - 1);
            format!("{sign}0.{zeros}{digits}")
        } else {
            let whole = exponent.unsigned_abs() as usize + 1;
            if digits.len() <= whole {
                format!("{sign}{digits}{}", "0".repeat(whole - digits.len()))
            } else {
                format!("{sign}{}.{}", &digits[..whole], &digits[whole..])
            }
        }
    } else {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let direction = if exponent < 0 { '-' } else { '+' };
        format!(
            "{sign}{first}{fraction}E{direction}{:02}",
            exponent.unsigned_abs()
        )
    }
}
