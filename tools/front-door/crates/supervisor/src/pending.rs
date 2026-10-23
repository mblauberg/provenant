//! Temporary pure number/text policy until core exposes equivalent APIs.

// port: belongs in provenant-core
pub(crate) fn python_space(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}')
}

// port: belongs in provenant-core
pub(crate) fn float_whole(value: f64) -> String {
    // Python int(f64) truncates the exact binary value; decimal Display may round
    // its last digits. Build its integral mantissa with decimal double-and-add.
    if value < 1.0 {
        return "0".into();
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let mantissa = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    if exponent <= 52 {
        return (mantissa >> (52 - exponent)).to_string();
    }
    let mut digits: Vec<_> = mantissa
        .to_string()
        .bytes()
        .rev()
        .map(|b| b - b'0')
        .collect();
    for _ in 0..exponent - 52 {
        let mut carry = 0;
        for digit in &mut digits {
            let value = *digit * 2 + carry;
            *digit = value % 10;
            carry = value / 10;
        }
        if carry != 0 {
            digits.push(carry);
        }
    }
    digits
        .into_iter()
        .rev()
        .map(|digit| char::from(b'0' + digit))
        .collect()
}

// port: belongs in provenant-core
pub(crate) fn divmod_decimal(digits: &str, divisor: u32) -> (String, u32) {
    let mut quotient = String::new();
    let mut remainder = 0;
    for digit in digits.bytes() {
        remainder = remainder * 10 + u32::from(digit - b'0');
        let value = remainder / divisor;
        remainder %= divisor;
        if value != 0 || !quotient.is_empty() {
            quotient.push(char::from(b'0' + value as u8));
        }
    }
    if quotient.is_empty() {
        quotient.push('0');
    }
    (quotient, remainder)
}
