//! Stable byte text encodings.
/// Lowercase hexadecimal with two digits per byte.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                char::from(DIGITS[usize::from(b >> 4)]),
                char::from(DIGITS[usize::from(b & 15)]),
            ]
        })
        .collect()
}
#[cfg(test)]
mod tests {
    #[test]
    fn hex_matches_python_for_every_byte_and_empty_input() {
        let values = vec![Vec::new(), (0..=255u8).collect()];
        let Some(expected) = crate::test_support::python_values(
            "print(json.dumps([bytes(v).hex() for v in values]))",
            &serde_json::json!(values),
        ) else {
            return;
        };
        assert_eq!(
            serde_json::json!(values.iter().map(|v| super::hex(v)).collect::<Vec<_>>()),
            expected
        );
    }
}
