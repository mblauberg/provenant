//! Byte-preserving stream path mapping for slot output.
use std::io::{self, BufRead, Write};
const LIMIT: usize = 1024 * 1024;

/// Map path prefixes at component boundaries; oversized lines pass unchanged.
pub fn map<R: BufRead, W: Write>(
    mut input: R,
    mut output: W,
    from: &[u8],
    to: &[u8],
) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.len() > LIMIT || from.is_empty() {
            output.write_all(&line)?;
            continue;
        }
        let mut start = 0;
        let mut cursor = 0;
        while cursor + from.len() <= line.len() {
            let end = cursor + from.len();
            let boundary = line.get(end).is_none_or(|b| {
                matches!(
                    b,
                    b'/' | b'\n'
                        | b'\r'
                        | b'\t'
                        | b':'
                        | b' '
                        | b'"'
                        | b'\''
                        | b','
                        | b')'
                        | b']'
                        | b'}'
                        | b';'
                )
            });
            let starts_path = cursor == 0
                || matches!(
                    line[cursor - 1],
                    b' ' | b'\t' | b'"' | b'\'' | b'(' | b'[' | b'=' | b':'
                );
            if starts_path && &line[cursor..end] == from && boundary {
                output.write_all(&line[start..cursor])?;
                output.write_all(to)?;
                cursor = end;
                start = end;
            } else {
                cursor += 1;
            }
        }
        output.write_all(&line[start..])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_bytes_and_component_boundaries() {
        let mut data = b" --> /slot/repo/a: x\n/slot/repository\n\xff\n".to_vec();
        data.extend_from_slice(b"/slot/repo/");
        data.extend(vec![b'x'; LIMIT + 1]);
        data.push(b'\n');
        let mut out = Vec::new();
        map(data.as_slice(), &mut out, b"/slot/repo", b"/lane").unwrap();
        let mut expected = b" --> /lane/a: x\n/slot/repository\n\xff\n".to_vec();
        expected.extend_from_slice(b"/slot/repo/");
        expected.extend(vec![b'x'; LIMIT + 1]);
        expected.push(b'\n');
        assert_eq!(out, expected);
    }
}
