//! The lane's view of the host's system and global Git configuration. A
//! sandboxed lane sees host entries only through an allowlist: identity,
//! aliases, presentation settings and the Git LFS filter. Credentials reach a
//! lane only through its granted token helper (ADR 0035), so every other key,
//! including any that can carry a token (`credential.*`, `http.*`,
//! `core.askPass`, `core.sshCommand`) or name another file (`include`,
//! `includeIf`), is withheld, and so is an allowlisted value that carries a
//! credential.
use provenant_os::sys::GitConfigEntry;

/// The setting that lets a person pass further keys deliberately.
pub const ALLOW_KEY: &str = "domains.lanes.git_config_allow";
/// Upper bound on configured extra allowlist entries.
pub const MAX_ALLOW: usize = 64;

/// Keys every sandboxed lane receives from the host when their value carries
/// no credential. A trailing `.*` matches any key below that prefix.
/// Matching ignores ASCII case.
pub const DEFAULT_ALLOW: &[&str] = &[
    "user.name",
    "user.email",
    "alias.*",
    "color.*",
    "core.abbrev",
    "core.attributesfile",
    "core.autocrlf",
    "core.commentchar",
    "core.commentstring",
    "core.editor",
    "core.eol",
    "core.excludesfile",
    "core.pager",
    "core.precomposeunicode",
    "core.quotepath",
    "core.safecrlf",
    "core.whitespace",
    "diff.algorithm",
    "diff.colormoved",
    "diff.colormovedws",
    "diff.context",
    "diff.dirstat",
    "diff.dstprefix",
    "diff.indentheuristic",
    "diff.interhunkcontext",
    "diff.mnemonicprefix",
    "diff.noprefix",
    "diff.relative",
    "diff.renamelimit",
    "diff.renames",
    "diff.srcprefix",
    "diff.statgraphwidth",
    "diff.statnamewidth",
    "diff.submodule",
    "diff.suppressblankempty",
    "diff.wordregex",
    "filter.lfs.clean",
    "filter.lfs.process",
    "filter.lfs.required",
    "filter.lfs.smudge",
    "merge.conflictstyle",
    "merge.stat",
];

/// Whether `pattern` is a valid allowlist entry: a dotted key of printable
/// ASCII without whitespace, quotes or backslashes, optionally ending in `.*`.
/// `include` and `includeIf` never pass, so naming them is invalid, and a
/// pattern that carries a credential is invalid because the receipt records it.
pub fn valid_pattern(pattern: &str) -> bool {
    let body = pattern.strip_suffix(".*").unwrap_or(pattern);
    let lower = body.to_ascii_lowercase();
    !body.is_empty()
        && body.len() <= 256
        && (body.contains('.') || pattern.ends_with(".*"))
        && !body.starts_with('.')
        && !body.ends_with('.')
        && body
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"\"\\*".contains(&byte))
        && !never_passes(&lower)
        && !carries_secret(pattern)
}

fn never_passes(key: &str) -> bool {
    key == "include"
        || key == "includeif"
        || key.starts_with("include.")
        || key.starts_with("includeif.")
}

fn matches(pattern: &str, key: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => {
            key.len() > prefix.len()
                && key.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        }
        None => pattern.eq_ignore_ascii_case(key),
    }
}

/// Whether text carries a credential by the redactor's shape rules: a known
/// token shape, URL userinfo with a password component, or a credential
/// assignment. A bare user (`git@host:`, `ssh://git@host/`) is not one. A
/// scan that cannot decide counts as a credential.
fn carries_secret(text: &str) -> bool {
    !matches!(
        provenant_redact::known_shape_rule(text.as_bytes()),
        Ok(None)
    )
}

/// Git's reading of a boolean: an implicit value is true.
fn git_true(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        let value = value.trim().to_ascii_lowercase();
        matches!(value.as_str(), "true" | "yes" | "on")
            || value.parse::<i64>().is_ok_and(|number| number != 0)
    })
}

/// The key split into section, optional subsection and variable.
fn split(key: &str) -> Option<(&str, Option<&str>, &str)> {
    let (section, rest) = key.split_once('.')?;
    match rest.rsplit_once('.') {
        Some((subsection, variable)) => Some((section, Some(subsection), variable)),
        None => Some((section, None, rest)),
    }
}

/// The name recorded for a withheld key. A subsection can itself hold a
/// secret (`url."https://user:TOKEN@host/".insteadOf`), so it is replaced by
/// `*`; values are never recorded.
fn recorded_name(key: &[u8]) -> String {
    let key = String::from_utf8_lossy(key);
    let name = match split(&key) {
        Some((section, Some(_), variable)) => format!("{section}.*.{variable}"),
        Some((section, None, variable)) => format!("{section}.{variable}"),
        None => "<unnamed>".to_owned(),
    };
    if name.chars().all(|c| c.is_ascii_graphic()) {
        name
    } else {
        "<unrepresentable>".to_owned()
    }
}

fn identity(key: &str) -> bool {
    key.eq_ignore_ascii_case("user.name") || key.eq_ignore_ascii_case("user.email")
}

fn admitted(key: &str, value: Option<&str>, extra: &[String]) -> bool {
    let lower = key.to_ascii_lowercase();
    if never_passes(&lower) {
        return false;
    }
    // The person's own grant passes verbatim.
    if extra.iter().any(|pattern| matches(pattern, key)) {
        return true;
    }
    let clean = value.is_none_or(|value| !carries_secret(value));
    if DEFAULT_ALLOW.iter().any(|pattern| matches(pattern, key)) {
        return clean;
    }
    match split(key) {
        Some((section, Some(base), variable)) => {
            section == "url"
                && variable == "insteadof"
                && clean_rewrite(base)
                && value.is_some_and(clean_rewrite)
        }
        _ => false,
    }
}

/// Whether a rewrite URL or prefix is free of credentials. A token can be
/// the whole user of an HTTP URL (`https://TOKEN@host/`) in shapes no
/// scanner knows, so any user there withholds the rewrite. Only SSH forms
/// keep a bare user (`ssh://git@host/`, `git@host:`), where it names the
/// account rather than carrying a secret.
fn clean_rewrite(url: &str) -> bool {
    if carries_secret(url) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return true;
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    !authority.contains('@')
        || matches!(
            scheme.to_ascii_lowercase().as_str(),
            "ssh" | "git+ssh" | "ssh+git"
        )
}

/// Characters a quoted configuration value can carry.
fn representable(value: &str) -> bool {
    !value
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\u{0008}'))
}

fn quote_subsection(subsection: &str) -> String {
    let mut quoted = String::from("\"");
    for character in subsection.chars() {
        if matches!(character, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// The host identity cannot be written without forging Git's headers.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvalidIdentity;

/// The rendered allowlisted entries, the names of every withheld key, and
/// the withheld filter drivers the host marks required.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LaneView {
    pub(crate) text: String,
    pub(crate) dropped: Vec<String>,
    /// Without its driver Git applies no filter at all, so content the host
    /// would transform (Git LFS, encryption) is committed raw.
    pub(crate) required_filters: Vec<String>,
}

/// The name a warning shows for a withheld required filter driver: the
/// driver itself when it is a plain name carrying no credential.
fn required_filter_name(driver: &str) -> String {
    let plain = !driver.is_empty()
        && driver.len() <= 64
        && driver
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !carries_secret(driver);
    if plain {
        format!("filter.{driver}.required")
    } else {
        "filter.*.required".to_owned()
    }
}

/// Render host entries, in their order, through the allowlist and the
/// person's `extra` patterns. An identity value with a control character
/// refuses; any other unrepresentable entry is withheld and named.
pub(crate) fn lane_view(
    entries: &[GitConfigEntry],
    extra: &[String],
) -> Result<LaneView, InvalidIdentity> {
    let mut view = LaneView::default();
    let withhold = |key: &[u8], view: &mut LaneView| {
        let name = recorded_name(key);
        if !view.dropped.contains(&name) {
            view.dropped.push(name);
        }
    };
    for entry in entries {
        let Ok(key) = std::str::from_utf8(&entry.key) else {
            withhold(&entry.key, &mut view);
            continue;
        };
        let value = match entry.value.as_deref().map(std::str::from_utf8) {
            None => None,
            Some(Ok(value)) => Some(value),
            Some(Err(_)) => {
                if identity(key) {
                    return Err(InvalidIdentity);
                }
                withhold(&entry.key, &mut view);
                continue;
            }
        };
        if !admitted(key, value, extra) {
            if let Some(("filter", Some(driver), "required")) = split(key)
                && git_true(value)
            {
                let name = required_filter_name(driver);
                if !view.required_filters.contains(&name) {
                    view.required_filters.push(name);
                }
            }
            withhold(&entry.key, &mut view);
            continue;
        }
        if identity(key) && value.is_some_and(|value| value.chars().any(char::is_control)) {
            return Err(InvalidIdentity);
        }
        let Some((section, subsection, variable)) = split(key) else {
            withhold(&entry.key, &mut view);
            continue;
        };
        let name = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        };
        if !name(section)
            || !name(variable)
            || subsection.is_some_and(|sub| sub.contains(['\n', '\0']))
            || !value.is_none_or(representable)
        {
            withhold(&entry.key, &mut view);
            continue;
        }
        match subsection {
            Some(subsection) => view
                .text
                .push_str(&format!("[{section} {}]\n", quote_subsection(subsection))),
            None => view.text.push_str(&format!("[{section}]\n")),
        }
        match value {
            Some(value) => view.text.push_str(&format!(
                "\t{variable} = {}\n",
                crate::launch::git_config_quote(value)
            )),
            None => view.text.push_str(&format!("\t{variable}\n")),
        }
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghp_fixtureSecretToken123";

    fn entry(key: &str, value: Option<&str>) -> GitConfigEntry {
        GitConfigEntry {
            key: key.into(),
            value: value.map(Into::into),
        }
    }

    fn host() -> Vec<GitConfigEntry> {
        let header = format!("AUTHORIZATION: bearer {TOKEN}");
        let rewrite = format!("url.https://x-access-token:{TOKEN}@github.com/.insteadof");
        let helper = format!("!f() {{ echo password={TOKEN}; }}; f");
        let scp = "url.git@github.com:.insteadof".to_owned();
        vec![
            entry("user.name", Some("Host Name")),
            entry("user.email", Some("host@example.com")),
            entry("alias.st", Some("status --short")),
            entry("alias.lg", Some("!git log --graph")),
            entry(
                "alias.pub",
                Some(&format!(
                    "!git push https://x-access-token:{TOKEN}@github.com/o/r"
                )),
            ),
            entry("alias.tok", Some(&format!("!echo {TOKEN}"))),
            entry("core.editor", Some("vim")),
            entry("color.diff.meta", Some("blue")),
            entry("diff.colormoved", Some("zebra")),
            entry("merge.conflictstyle", Some("zdiff3")),
            entry("filter.lfs.clean", Some("git-lfs clean -- %f")),
            entry("filter.lfs.smudge", Some("git-lfs smudge -- %f")),
            entry("filter.lfs.process", Some("git-lfs filter-process")),
            entry("filter.lfs.required", Some("true")),
            entry("filter.crypt.clean", Some("git-crypt clean")),
            entry("filter.crypt.required", None),
            entry("filter.optional.required", Some("false")),
            entry("http.https://github.com/.extraheader", Some(&header)),
            entry(&rewrite, Some("https://github.com/")),
            entry(&scp, Some("https://github.com/")),
            entry("url.ssh://git@host.example/.insteadof", Some("host:")),
            entry("url.https://mirror.example/.insteadof", Some("mirror:")),
            entry(
                "url.https://mirror.example/.insteadof",
                Some(&format!("https://u:{TOKEN}@old.example/")),
            ),
            entry("credential.helper", Some(&helper)),
            entry("credential.https://github.com.helper", Some(&helper)),
            entry("core.askpass", Some(&format!("/bin/echo {TOKEN}"))),
            entry("core.sshcommand", Some(&format!("ssh -i {TOKEN}"))),
            entry("include.path", Some("/host/secret.gitconfig")),
            entry(
                "includeif.gitdir:~/work/.path",
                Some("/host/work.gitconfig"),
            ),
            entry("diff.secret.textconv", Some(&format!("cat {TOKEN}"))),
            entry(
                "http.proxy",
                Some(&format!("http://proxy:{TOKEN}@proxy.example:8080")),
            ),
        ]
    }

    #[test]
    fn host_tokens_never_reach_the_lane_view_while_identity_and_aliases_pass() {
        let view = lane_view(&host(), &[]).unwrap();
        assert!(!view.text.contains(TOKEN), "{}", view.text);
        assert!(!view.dropped.iter().any(|name| name.contains(TOKEN)));
        for kept in [
            "[user]\n\tname = \"Host Name\"\n",
            "[user]\n\temail = \"host@example.com\"\n",
            "[alias]\n\tst = \"status --short\"\n",
            "[alias]\n\tlg = \"!git log --graph\"\n",
            "[core]\n\teditor = \"vim\"\n",
            "[color \"diff\"]\n\tmeta = \"blue\"\n",
            "[diff]\n\tcolormoved = \"zebra\"\n",
            "[merge]\n\tconflictstyle = \"zdiff3\"\n",
            "[url \"https://mirror.example/\"]\n\tinsteadof = \"mirror:\"\n",
            "[url \"git@github.com:\"]\n\tinsteadof = \"https://github.com/\"\n",
            "[url \"ssh://git@host.example/\"]\n\tinsteadof = \"host:\"\n",
            "[filter \"lfs\"]\n\tclean = \"git-lfs clean -- %f\"\n",
            "[filter \"lfs\"]\n\tsmudge = \"git-lfs smudge -- %f\"\n",
            "[filter \"lfs\"]\n\tprocess = \"git-lfs filter-process\"\n",
            "[filter \"lfs\"]\n\trequired = \"true\"\n",
        ] {
            assert!(
                view.text.contains(kept),
                "{kept} missing from {}",
                view.text
            );
        }
        assert!(!view.text.contains("[include"));
        assert!(!view.text.contains("crypt"));
        assert_eq!(view.required_filters, ["filter.crypt.required"]);
        assert_eq!(
            view.dropped,
            [
                "alias.pub",
                "alias.tok",
                "filter.*.clean",
                "filter.*.required",
                "http.*.extraheader",
                "url.*.insteadof",
                "credential.helper",
                "credential.*.helper",
                "core.askpass",
                "core.sshcommand",
                "include.path",
                "includeif.*.path",
                "diff.*.textconv",
                "http.proxy",
            ]
        );
    }

    #[test]
    fn extra_patterns_pass_keys_deliberately_but_never_includes() {
        let extra = vec![
            "credential.helper".to_owned(),
            "HTTP.*".to_owned(),
            "alias.pub".to_owned(),
        ];
        let view = lane_view(&host(), &extra).unwrap();
        // A deliberate grant passes its value verbatim, credential included.
        assert!(view.text.contains(&format!(
            "[alias]\n\tpub = \"!git push https://x-access-token:{TOKEN}@github.com/o/r\"\n"
        )));
        assert!(view.dropped.contains(&"alias.tok".to_owned()));
        assert!(view.text.contains("[credential]\n\thelper = "));
        assert!(
            view.text
                .contains("[http \"https://github.com/\"]\n\textraheader = ")
        );
        assert!(!view.dropped.contains(&"credential.helper".to_owned()));
        assert!(view.dropped.contains(&"credential.*.helper".to_owned()));
        assert!(view.dropped.contains(&"include.path".to_owned()));
        assert!(!valid_pattern("include.path"));
        assert!(!valid_pattern("includeIf.*"));
        assert!(!valid_pattern("*"));
        assert!(!valid_pattern("user"));
        assert!(!valid_pattern("user name"));
        assert!(valid_pattern("push.default"));
        assert!(valid_pattern("pull.*"));
        // The receipt records the allowlist, so a credential cannot be one.
        assert!(!valid_pattern(&format!(
            "url.https://x-access-token:{TOKEN}@github.com/.insteadof"
        )));
        assert!(!valid_pattern(&format!("alias.{TOKEN}")));
    }

    #[test]
    fn http_rewrites_with_any_user_are_withheld_and_ssh_users_pass() {
        // Tokens no shape rule knows, used as the HTTP user: an Azure DevOps
        // personal access token and a legacy 40-hex OAuth token.
        let azure = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0u1v2w3x4y5z6";
        let oauth = "0123456789abcdef0123456789abcdef01234567";
        let entries = vec![
            entry(
                &format!("url.https://{azure}@dev.azure.com/.insteadof"),
                Some("https://dev.azure.com/"),
            ),
            entry(
                "url.https://legacy.example/.insteadof",
                Some(&format!("HTTP://{oauth}@legacy.example/")),
            ),
            entry(
                "url.https://deploy@plain.example/.insteadof",
                Some("plain:"),
            ),
            entry("url.ssh://git@host.example/.insteadof", Some("host:")),
            entry(
                "url.git+ssh://git@ssh.example/.insteadof",
                Some("ssh-host:"),
            ),
            entry("url.git@github.com:.insteadof", Some("gh:")),
            entry(
                "url.https://mirror.example/.insteadof",
                Some("git@github.com:"),
            ),
        ];
        let view = lane_view(&entries, &[]).unwrap();
        for withheld in [azure, oauth, "dev.azure.com", "legacy.example", "plain"] {
            assert!(!view.text.contains(withheld), "{}", view.text);
        }
        assert_eq!(view.dropped, ["url.*.insteadof"]);
        for kept in [
            "[url \"ssh://git@host.example/\"]\n\tinsteadof = \"host:\"\n",
            "[url \"git+ssh://git@ssh.example/\"]\n\tinsteadof = \"ssh-host:\"\n",
            "[url \"git@github.com:\"]\n\tinsteadof = \"gh:\"\n",
            "[url \"https://mirror.example/\"]\n\tinsteadof = \"git@github.com:\"\n",
        ] {
            assert!(
                view.text.contains(kept),
                "{kept} missing from {}",
                view.text
            );
        }
    }

    #[test]
    fn rendered_view_round_trips_through_git() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("lane.gitconfig");
        let entries = vec![
            entry("user.name", Some("Ann \"Lee\"; $HOME `git` \\ path")),
            entry("alias.multi", Some("!f() {\n\techo hi\n}; f")),
            entry("core.quotepath", None),
            entry(
                "url.https://quote\"back\\slash.example/.insteadof",
                Some("q:"),
            ),
        ];
        let view = lane_view(&entries, &[]).unwrap();
        assert!(view.dropped.is_empty());
        std::fs::write(&config, &view.text).unwrap();
        let git = provenant_os::git::git_executable().expect("Git unavailable");
        let listed = provenant_os::sys::git_config_file(&git, &config, temp.path()).unwrap();
        assert_eq!(listed, entries);
    }

    #[test]
    fn identity_with_a_control_character_refuses() {
        for value in ["Ann\rLee", "Ann\n", "Ann\u{000c}Lee"] {
            assert_eq!(
                lane_view(&[entry("user.name", Some(value))], &[]),
                Err(InvalidIdentity)
            );
        }
        let view = lane_view(&[entry("alias.bad", Some("a\u{000c}b"))], &[]).unwrap();
        assert_eq!(view.dropped, ["alias.bad"]);
        assert!(view.text.is_empty());
    }
}
