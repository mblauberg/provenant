use crate::command_model;

/// Every `provenant <words...>` written in a user-facing Rust string
/// literal must name a live command, so a retired or misspelt name in
/// advice, a `next:` value or an error fails CI like
/// `scripts/check-retired-names` does for module names.
///
/// The extraction is deliberately simple: scan the `.rs` files under
/// `tools/front-door/src` and `tools/front-door/crates/*/src` at test time,
/// skip `#[cfg(test)]` modules and their split source files (their strings
/// are assertions, not advice), plus clap derive attributes. Read ordinary
/// double-quoted string literals (byte strings are hashes, not advice) and
/// take the words after a
/// lowercase `provenant ` while they look like subcommands. A flag, a
/// placeholder (`NAME`, `N`, `PATH`), a format brace or a value such as
/// `fix-login/review` ends the path. Resolution walks the same clap command
/// model the CLI parses with; once a command is a leaf, the remaining words
/// are example arguments and are ignored.
#[test]
fn advice_in_string_literals_names_live_commands() {
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is tools/front-door/crates/cli.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("cli crate lives under tools/front-door/crates")
            .to_owned()
    }

    fn rust_sources(root: &Path) -> Vec<PathBuf> {
        let mut roots = vec![root.join("src")];
        if let Ok(crates) = std::fs::read_dir(root.join("crates")) {
            for entry in crates.flatten() {
                let src = entry.path().join("src");
                if src.is_dir() {
                    roots.push(src);
                }
            }
        }
        let mut files = Vec::new();
        let mut stack = roots;
        while let Some(dir) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|ext| ext == "rs") {
                        files.push(path);
                    }
                }
            }
        }
        files.sort();
        files
    }

    fn test_only_path(path: &Path) -> bool {
        path.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            name == "tests"
                || name == "tests.rs"
                || name.ends_with("_tests")
                || name.ends_with("_tests.rs")
        })
    }

    fn external_test_modules(files: &[PathBuf]) -> std::collections::BTreeSet<PathBuf> {
        let mut modules = std::collections::BTreeSet::new();
        for file in files {
            let Ok(source) = std::fs::read_to_string(file) else {
                continue;
            };
            let mut pending_test = false;
            for line in source.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("#[cfg(test)]") {
                    pending_test = true;
                    continue;
                }
                if !pending_test {
                    continue;
                }
                if trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.is_empty() {
                    continue;
                }
                if let Some(name) = trimmed
                    .strip_prefix("mod ")
                    .and_then(|name| name.strip_suffix(';'))
                    .map(str::trim)
                    .filter(|name| {
                        name.chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                    })
                    && let Some(parent) = file.parent()
                {
                    for candidate in [
                        parent.join(format!("{name}.rs")),
                        parent.join(name).join("mod.rs"),
                    ] {
                        if candidate.is_file() {
                            modules.insert(candidate);
                        }
                    }
                }
                pending_test = false;
            }
        }
        modules
    }

    // One-line ordinary double-quoted string literal contents. A `b"`
    // prefix marks a byte string (a hash or discriminator), not advice.
    fn string_literals(code: &str) -> Vec<String> {
        let bytes = code.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let byte_string =
                i > 0 && bytes[i - 1] == b'b' && (i < 2 || !bytes[i - 2].is_ascii_alphanumeric());
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += usize::from(bytes[i] == b'\\') + 1;
            }
            if !byte_string && i <= bytes.len() {
                out.push(code[start..i].to_owned());
            }
            i += 1;
        }
        out
    }

    // The words after each `provenant ` in a literal, while they look like
    // subcommands: lowercase identifiers. Uppercase placeholders, flags,
    // braces and value punctuation stop the path. An empty path is prose
    // such as "provenant OpenCode hook", not a command.
    fn command_words(literal: &str) -> Vec<Vec<String>> {
        const NEEDLE: &[u8] = b"provenant ";
        let bytes = literal.as_bytes();
        let mut found = Vec::new();
        let mut pos = 0;
        while pos + NEEDLE.len() <= bytes.len() {
            if &bytes[pos..pos + NEEDLE.len()] != NEEDLE
                || (pos > 0 && bytes[pos - 1].is_ascii_alphanumeric())
            {
                pos += 1;
                continue;
            }
            let mut cursor = pos + NEEDLE.len();
            let mut words = Vec::new();
            loop {
                while bytes.get(cursor) == Some(&b' ') {
                    cursor += 1;
                }
                let begin = cursor;
                while cursor < bytes.len()
                    && (bytes[cursor].is_ascii_lowercase()
                        || bytes[cursor].is_ascii_digit()
                        || bytes[cursor] == b'-'
                        || bytes[cursor] == b'_')
                {
                    cursor += 1;
                }
                if cursor == begin || !bytes[begin].is_ascii_lowercase() {
                    break;
                }
                words.push(literal[begin..cursor].to_owned());
            }
            if !words.is_empty() {
                found.push(words);
            }
            pos += NEEDLE.len();
        }
        found
    }

    fn resolves(model: &clap::Command, words: &[String]) -> Result<(), String> {
        let mut current = model;
        for (index, word) in words.iter().enumerate() {
            if let Some(child) = current.find_subcommand(word) {
                current = child;
            } else if current.get_subcommands().next().is_none() {
                // A leaf command: the rest are example arguments.
                return Ok(());
            } else {
                return Err(format!(
                    "`{word}` is not a subcommand of `provenant {}`",
                    words[..index].join(" ")
                ));
            }
        }
        Ok(())
    }

    let root = workspace_root();
    let model = command_model();
    let mut failures = Vec::new();
    let mut checked = 0;
    let files = rust_sources(&root);
    let external_tests = external_test_modules(&files);
    for file in files {
        let relative = file.strip_prefix(&root).unwrap_or(&file);
        if test_only_path(relative) || external_tests.contains(&file) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read workspace Rust source");
        let mut skip_depth: Option<usize> = None;
        let mut pending_test = false;
        let mut depth = 0usize;
        for (line_number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // Track braces outside string literals so a `#[cfg(test)]`
            // module can be skipped without breaking the file's depth.
            let code = line.split("//").next().unwrap_or(line);
            let mut mask = String::with_capacity(code.len());
            let mut in_string = false;
            let mut escaped = false;
            for ch in code.chars() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        in_string = false;
                    }
                    mask.push(' ');
                } else if ch == '"' {
                    in_string = true;
                    mask.push(' ');
                } else {
                    mask.push(ch);
                }
            }
            let before = depth;
            for ch in mask.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if skip_depth.is_some_and(|target| before <= target && depth < target) {
                skip_depth = None;
            }
            if skip_depth.is_some() {
                continue;
            }
            if pending_test {
                if trimmed.starts_with("mod ") {
                    if trimmed.contains('{') {
                        skip_depth = Some(depth);
                    }
                    pending_test = false;
                    continue;
                }
                if trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.is_empty() {
                    continue;
                }
                pending_test = false;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                pending_test = true;
                continue;
            }
            if trimmed.starts_with('#') {
                continue; // clap derive attributes declare the tree itself
            }
            if trimmed.starts_with("//") {
                continue;
            }
            for literal in string_literals(code) {
                for words in command_words(&literal) {
                    checked += 1;
                    if let Err(reason) = resolves(&model, &words) {
                        failures.push(format!(
                            "{}:{}: `provenant {}`: {reason}",
                            relative.display(),
                            line_number + 1,
                            words.join(" ")
                        ));
                    }
                }
            }
        }
    }
    assert!(checked > 0, "the advice scan found no command strings");
    assert!(
        failures.is_empty(),
        "advice names commands that are not live:\n{}",
        failures.join("\n")
    );
}
