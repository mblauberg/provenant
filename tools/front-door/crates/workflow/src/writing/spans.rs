use super::Matcher;

fn escapes(text: &str) -> Vec<bool> {
    let mut escaped = false;
    text.bytes()
        .map(|b| {
            let prior = escaped;
            escaped = b == b'\\' && !escaped;
            prior
        })
        .collect()
}
fn balanced(text: &str, esc: &[bool], start: usize, open: u8, close: u8) -> usize {
    let mut depth = 0;
    for (offset, b) in text.as_bytes()[start..].iter().enumerate() {
        let i = start + offset;
        if esc[i] {
            continue;
        }
        if *b == open {
            depth += 1;
        }
        if *b == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    text.len()
}
pub(super) fn excluded(matcher: &Matcher, text: &str, latex: bool, quoted: bool) -> Vec<bool> {
    let esc = escapes(text);
    let mut mask = vec![false; text.len()];
    for pattern in &matcher.excluded_patterns {
        for m in pattern.find_iter(text) {
            mask[m.range()].fill(true);
        }
    }
    let mut fence: Option<(u8, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let marker = trimmed.as_bytes().first().copied().unwrap_or(0);
        let width = trimmed.bytes().take_while(|b| *b == marker).count();
        let delimiter =
            line.len() - trimmed.len() <= 3 && matches!(marker, b'`' | b'~') && width >= 3;
        if let Some((opening, count)) = fence {
            mask[offset..offset + line.len()].fill(true);
            if delimiter
                && marker == opening
                && width >= count
                && trimmed[width..].trim().is_empty()
            {
                fence = None;
            }
        } else if delimiter && !trimmed[width..].contains(&trimmed[..width]) {
            fence = Some((marker, width));
            mask[offset..offset + line.len()].fill(true);
        } else if quoted && trimmed.starts_with('>') {
            mask[offset..offset + line.len()].fill(true);
        }
        offset += line.len();
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if mask[i] {
            i += 1;
            continue;
        }
        if bytes[i] == b'`' && !esc[i] {
            let count = bytes[i..].iter().take_while(|b| **b == b'`').count();
            let mut end = i + count;
            while end < bytes.len() && bytes[end] != b'\n' {
                if bytes[end] == b'`' {
                    let width = bytes[end..].iter().take_while(|b| **b == b'`').count();
                    if width == count {
                        end += width;
                        mask[i..end].fill(true);
                        break;
                    }
                    end += width;
                } else {
                    end += 1;
                }
            }
            i += count;
            continue;
        }
        if !latex {
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && !esc[i] {
            let end = text[i..].find('\n').map_or(text.len(), |n| i + n);
            mask[i..end].fill(true);
            i = end;
            continue;
        }
        let mut math_end = None;
        if !esc[i] {
            for [start, end] in &matcher.exclusions.math_delimiters {
                if text.is_char_boundary(i) && text[i..].starts_with(start) {
                    let mut cursor = i + start.len();
                    let mut close = None;
                    while let Some(n) = text[cursor..].find(end) {
                        let position = cursor + n;
                        if !esc[position] {
                            close = Some(position + end.len());
                            break;
                        }
                        cursor = position + end.len();
                    }
                    math_end = Some(close.unwrap_or(text.len()));
                    break;
                }
            }
        }
        if let Some(end) = math_end {
            mask[i..end].fill(true);
            i = end;
            continue;
        }
        if bytes[i] == b'\\' && !esc[i] {
            let start = i;
            i += 1;
            let name_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'@') {
                i += 1;
            }
            if i == name_start {
                if i < bytes.len() {
                    let width = text[i..].chars().next().map_or(0, char::len_utf8);
                    i += width;
                }
                mask[start..i].fill(true);
                continue;
            }
            let name = &text[name_start..i];
            if bytes.get(i) == Some(&b'*') {
                i += 1;
            }
            mask[start..i].fill(true);
            let mut arg = i;
            while bytes.get(arg).is_some_and(u8::is_ascii_whitespace) {
                arg += 1;
            }
            if name == "begin" && bytes.get(arg) == Some(&b'{') {
                let end = balanced(text, &esc, arg, b'{', b'}');
                if end > arg + 1 && bytes.get(end - 1) == Some(&b'}') {
                    let env = &text[arg + 1..end - 1];
                    if matcher
                        .exclusions
                        .latex_environments
                        .iter()
                        .any(|s| s == env.trim_end_matches('*'))
                    {
                        let closing = format!("\\end{{{env}}}");
                        let end = text[end..]
                            .find(&closing)
                            .map_or(text.len(), |n| end + n + closing.len());
                        mask[start..end].fill(true);
                        i = end;
                        continue;
                    }
                }
            }
            if matcher.exclusions.latex_commands.iter().any(|s| s == name) || name == "href" {
                if bytes.get(arg) == Some(&b'[') {
                    arg = balanced(text, &esc, arg, b'[', b']');
                    while bytes.get(arg).is_some_and(u8::is_ascii_whitespace) {
                        arg += 1;
                    }
                }
                let mut end = i;
                while bytes.get(arg) == Some(&b'{') {
                    end = balanced(text, &esc, arg, b'{', b'}');
                    arg = end;
                    if name == "href" {
                        break;
                    }
                    while bytes.get(arg).is_some_and(u8::is_ascii_whitespace) {
                        arg += 1;
                    }
                }
                if matcher
                    .exclusions
                    .verbatim_commands
                    .iter()
                    .any(|s| s == name)
                    && bytes
                        .get(arg)
                        .is_some_and(|b| b.is_ascii_punctuation() && *b != b'{')
                {
                    let delimiter = bytes[arg];
                    let end = bytes[arg + 1..]
                        .iter()
                        .position(|b| *b == delimiter || *b == b'\n')
                        .map_or(text.len(), |n| arg + 2 + n);
                    mask[start..end].fill(true);
                    i = end;
                    continue;
                }
                mask[start..end].fill(true);
                i = end;
            }
            continue;
        }
        i += 1;
    }
    mask
}

/// Reassemble hard-wrapped prose; each list item starts a new unit. Findings
/// cover the sentence itself and retain its actual source position.
pub(super) fn long_sentences(
    text: &str,
    mask: &[bool],
    threshold: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut hits = Vec::new();
    let mut paragraph = None;
    let mut sentence = 0;
    let mut words = 0;
    let mut in_word = false;
    let finish = |end: usize, hits: &mut Vec<_>, words: &mut usize, sentence: &mut usize| {
        if *words > threshold {
            hits.push(*sentence..end);
        }
        *words = 0;
        *sentence = end;
    };
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trim = line.trim();
        let non_prose = trim.is_empty()
            || trim.starts_with(['|', '#', '>'])
            || trim.starts_with("---")
            || mask[offset..offset + line.len()].iter().all(|b| *b);
        let bytes = trim.as_bytes();
        let bullet =
            matches!(bytes.first(), Some(b'-' | b'*' | b'+')) && bytes.get(1) == Some(&b' ');
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        let numbered = digits > 0
            && matches!(bytes.get(digits), Some(b'.' | b')'))
            && bytes.get(digits + 1) == Some(&b' ');
        let list = bullet || numbered;
        if non_prose || list {
            finish(offset, &mut hits, &mut words, &mut sentence);
            paragraph = None;
            in_word = false;
        }
        if !non_prose {
            for (n, c) in line.char_indices() {
                let i = offset + n;
                if mask[i] {
                    in_word = false;
                    continue;
                }
                if !c.is_whitespace() {
                    if paragraph.is_none() {
                        sentence = i;
                        paragraph = Some(i);
                    }
                    if !in_word {
                        words += 1;
                    }
                    in_word = true;
                } else {
                    in_word = false;
                }
                if matches!(c, '.' | '!' | '?')
                    && line[n + c.len_utf8()..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace)
                {
                    finish(i + c.len_utf8(), &mut hits, &mut words, &mut sentence);
                    paragraph = None;
                    in_word = false;
                }
            }
        }
        offset += line.len();
    }
    finish(text.len(), &mut hits, &mut words, &mut sentence);
    hits
}

#[cfg(test)]
mod tests {
    use super::long_sentences;

    #[test]
    fn emphasis_and_years_do_not_split_wrapped_prose_as_list_items() {
        let text = "2026 was busy, and it continued\n*emphasis* through the week.";
        let mask = vec![false; text.len()];
        let ranges = long_sentences(text, &mask, 7);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], 0..text.len());
    }
}
