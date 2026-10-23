use std::{
    collections::VecDeque,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

// port: belongs in provenant-core
pub(crate) fn trim_ecmascript(value: &str) -> &str {
    value.trim_matches(|c: char| {
        matches!(c,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    })
}

// port: belongs in provenant-core
pub(crate) fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "embedded null character in path",
        ));
    }
    enum Part {
        Name(OsString),
        EndLink(PathBuf),
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut pending: VecDeque<_> = absolute
        .components()
        .map(|part| Part::Name(part.as_os_str().into()))
        .collect();
    let mut links = std::collections::BTreeSet::new();
    let mut result = PathBuf::new();
    while let Some(part) = pending.pop_front() {
        let name = match part {
            Part::EndLink(link) => {
                links.remove(&link);
                continue;
            }
            Part::Name(name) => name,
        };
        if name == "." {
            continue;
        }
        if name == ".." {
            result.pop();
            continue;
        }
        result.push(&name);
        if let Ok(link) = fs::read_link(&result) {
            if !links.insert(result.clone()) {
                return Err(io::Error::other("Symlink loop"));
            }
            pending.push_front(Part::EndLink(result.clone()));
            result.pop();
            for component in link.components().rev() {
                pending.push_front(Part::Name(component.as_os_str().into()));
            }
        }
    }
    Ok(result)
}

// port: belongs in provenant-records
pub(crate) fn checked_json(text: &str) -> Result<serde_json::Value, serde_json::Error> {
    use serde::de::IgnoredAny;
    serde_json::from_str::<IgnoredAny>(text)?;
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        normalized.push(ch);
        if ch == '"' {
            while let Some((_, ch)) = chars.next() {
                normalized.push(ch);
                if ch == '\\' {
                    if let Some((_, escaped)) = chars.next() {
                        normalized.push(escaped);
                    }
                } else if ch == '"' {
                    break;
                }
            }
        } else if ch == '-' || ch.is_ascii_digit() {
            let mut end = start + ch.len_utf8();
            while let Some(&(index, next)) = chars.peek() {
                if !(next.is_ascii_digit() || matches!(next, '.' | 'e' | 'E' | '+' | '-')) {
                    break;
                }
                end = index + next.len_utf8();
                chars.next();
            }
            let token = &text[start..end];
            if !token.contains(['.', 'e', 'E'])
                && token.parse::<i64>().is_err()
                && token.parse::<u64>().is_err()
            {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "integer outside i64/u64 range",
                ));
            }
            normalized.pop();
            normalized.push_str(if token == "-0" { "0" } else { token });
        }
    }
    serde_json::from_str(&normalized)
}
