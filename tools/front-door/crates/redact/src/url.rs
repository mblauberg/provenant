//! URL authority parsing shared by output redaction and lane Git
//! configuration, so both read the same userinfo from the same URL.
//!
//! After `scheme://`, Git and curl skip any number of further slashes before
//! the authority: `https:///user:pw@host/` sends `user:pw` as credentials.

/// Whether `byte` ends an authority in free text.
pub(crate) fn delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || b"/?#\\\"'<>`".contains(&byte)
}

/// Whether `byte` continues an authority that has collected `sofar` after
/// `scheme://`. A leading run of slashes belongs to the authority's prefix.
pub(crate) fn authority_continues(sofar: &[u8], byte: u8) -> bool {
    !delimiter(byte) || (byte == b'/' && sofar.iter().all(|byte| *byte == b'/'))
}

/// The number of leading slashes before an authority's host part.
pub(crate) fn leading_slashes(authority: &[u8]) -> usize {
    authority.iter().take_while(|byte| **byte == b'/').count()
}

/// The authority after `scheme://`, its leading slashes skipped.
pub fn authority(after_scheme: &[u8]) -> &[u8] {
    let mut length = 0;
    while length < after_scheme.len()
        && authority_continues(&after_scheme[..length], after_scheme[length])
    {
        length += 1;
    }
    &after_scheme[leading_slashes(&after_scheme[..length])..length]
}

/// The userinfo of an authority: the text before its last `@`.
pub fn userinfo(authority: &[u8]) -> Option<&[u8]> {
    authority
        .iter()
        .rposition(|byte| *byte == b'@')
        .map(|end| &authority[..end])
}

/// A URL in text: its scheme and authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Url<'a> {
    pub scheme: &'a [u8],
    pub authority: &'a [u8],
}

impl Url<'_> {
    /// Whether the scheme is an SSH transport (`ssh`, `git+ssh`, `ssh+git`),
    /// compared case-insensitively.
    pub fn ssh(&self) -> bool {
        [b"ssh".as_slice(), b"git+ssh", b"ssh+git"]
            .iter()
            .any(|ssh| self.scheme.eq_ignore_ascii_case(ssh))
    }
}

/// Every `scheme://` URL in text, in order. A scheme is the run of scheme
/// characters before `://` after the last `.`, so a Git key such as
/// `url.https://host/.insteadof` yields `https`.
pub fn urls(text: &[u8]) -> impl Iterator<Item = Url<'_>> {
    let mut rest = 0;
    std::iter::from_fn(move || {
        let found = text[rest..]
            .windows(3)
            .position(|window| window == b"://")?;
        let separator = rest + found;
        rest = separator + 3;
        let start = text[..separator]
            .iter()
            .rposition(|byte| !(byte.is_ascii_alphanumeric() || b"+-".contains(byte)))
            .map_or(0, |index| index + 1);
        Some(Url {
            scheme: &text[start..separator],
            authority: authority(&text[rest..]),
        })
    })
}

/// Whether text holds a URL that names a user over a transport other than
/// SSH. A token can be the whole user of an HTTP URL in shapes no scanner
/// knows; an SSH user names the account.
pub fn names_non_ssh_user(text: &[u8]) -> bool {
    urls(text).any(|url| !url.ssh() && userinfo(url.authority).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_skips_any_number_of_leading_slashes() {
        for (after, expected) in [
            ("user:pw@host/path", "user:pw@host"),
            ("/user:pw@host/path", "user:pw@host"),
            ("//user@host/", "user@host"),
            ("///", ""),
            ("host?q=a@b", "host"),
            ("host#a@b", "host"),
            ("host path@x", "host"),
            ("", ""),
        ] {
            assert_eq!(authority(after.as_bytes()), expected.as_bytes(), "{after}");
        }
    }

    #[test]
    fn urls_read_scheme_and_authority_case_and_slashes_aside() {
        let text = b"url.HTTPS:////user@host/.insteadof x=Git+SSH:///git@h/ file:///etc";
        let found: Vec<_> = urls(text).collect();
        assert_eq!(
            found,
            [
                Url {
                    scheme: b"HTTPS",
                    authority: b"user@host"
                },
                Url {
                    scheme: b"Git+SSH",
                    authority: b"git@h"
                },
                Url {
                    scheme: b"file",
                    authority: b""
                },
            ]
        );
        assert!(!found[0].ssh());
        assert!(found[1].ssh());
        assert_eq!(userinfo(b"a:b@c@host"), Some(b"a:b@c".as_slice()));
        assert_eq!(userinfo(b"host"), None);
    }

    #[test]
    fn non_ssh_users_are_named_at_any_slash_count() {
        for text in [
            "https://user@host/",
            "https:///user@host/",
            "HtTpS:////user:pw@host/",
            "url.http://user@host/.insteadof",
        ] {
            assert!(names_non_ssh_user(text.as_bytes()), "{text}");
        }
        for text in [
            "https://host/",
            "https:///host/path@x",
            "ssh://git@host/",
            "SSH:///git@host/",
            "git@host:path",
            "gh:",
        ] {
            assert!(!names_non_ssh_user(text.as_bytes()), "{text}");
        }
    }
}
