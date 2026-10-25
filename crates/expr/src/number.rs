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
