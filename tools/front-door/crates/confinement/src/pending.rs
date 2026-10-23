//! Minimal compatibility helpers whose shared owner has no equivalent API yet.
use provenant_core::py::python_repr;
use std::ffi::OsStr;

// port: belongs in provenant-core
#[cfg(unix)]
pub(crate) fn python_repr_os(path: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    enum Item {
        Character(char),
        Surrogate(u8),
    }
    let bytes = path.as_bytes();
    let mut items = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                items.extend(valid.chars().map(Item::Character));
                break;
            }
            Err(error) => {
                let valid_end = offset + error.valid_up_to();
                if valid_end > offset {
                    let valid = String::from_utf8_lossy(&bytes[offset..valid_end]);
                    items.extend(valid.chars().map(Item::Character));
                }
                offset = valid_end;
                items.push(Item::Surrogate(bytes[offset]));
                offset += 1;
            }
        }
    }
    if items.iter().all(|item| matches!(item, Item::Character(_))) {
        let text: String = items
            .iter()
            .filter_map(|item| match item {
                Item::Character(character) => Some(*character),
                Item::Surrogate(_) => None,
            })
            .collect();
        return python_repr(&text);
    }
    let has_single = items
        .iter()
        .any(|item| matches!(item, Item::Character('\'')));
    let has_double = items
        .iter()
        .any(|item| matches!(item, Item::Character('"')));
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut rendered = String::from(quote);
    for item in items {
        match item {
            Item::Surrogate(byte) => rendered.push_str(&format!("\\udc{byte:02x}")),
            Item::Character(character) if character == quote => {
                rendered.push('\\');
                rendered.push(character);
            }
            Item::Character(character @ ('\'' | '"')) => rendered.push(character),
            Item::Character(character) => {
                let one = python_repr(&character.to_string());
                let end = one.len().saturating_sub(1);
                rendered.push_str(one.get(1..end).unwrap_or(""));
            }
        }
    }
    rendered.push(quote);
    rendered
}

// port: belongs in provenant-core
#[cfg(not(unix))]
pub(crate) fn python_repr_os(path: &OsStr) -> String {
    path.to_str()
        .map(python_repr)
        .unwrap_or_else(|| format!("{path:?}"))
}

// port: belongs in provenant-core
pub(crate) fn python_decimal_digit(character: char) -> Option<u8> {
    // Unicode 16.0 decimal blocks, matching the pinned Python oracle's `int` parser.
    const ZEROES: &[u32] = &[
        0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66,
        0x0CE6, 0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946,
        0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0,
        0xA9F0, 0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136,
        0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0,
        0x11950, 0x11BF0, 0x11C50, 0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50,
        0x16D70, 0x1CCF0, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0,
        0x1E5F1, 0x1E950, 0x1FBF0,
    ];
    let code = character as u32;
    ZEROES.iter().find_map(|&zero| {
        if (zero..zero + 10).contains(&code) {
            Some((code - zero) as u8)
        } else {
            None
        }
    })
}
