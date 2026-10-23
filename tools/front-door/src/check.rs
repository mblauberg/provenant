//! `check` custody. Choosing which registered checkout's checker to run stays
//! in Python behind `<root>/scripts/check_target.py`; the Rust front door owns
//! the stale-checkout advisory.

use crate::exec::REPAIR;
use crate::resolve::{self, FABRIC_MCP, Settings};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HELPER: &str = "scripts/check_target.py";
const CHECKER: &str = "scripts/check-harness";

/// The checker to run and the checkout that owns it.
#[derive(Debug, PartialEq)]
pub struct Selection {
    pub checker: PathBuf,
    pub root: PathBuf,
}

/// Parse the helper's `<checker>\0<root>\0` output.
pub fn parse_selection(output: Vec<u8>) -> Option<Selection> {
    let mut fields = output.split(|&byte| byte == 0);
    let (checker, root, end) = (fields.next()?, fields.next()?, fields.next()?);
    let absolute = |bytes: &[u8]| !bytes.is_empty() && bytes[0] == b'/';
    if !end.is_empty() || fields.next().is_some() || !absolute(checker) || !absolute(root) {
        return None;
    }
    let path = |bytes: &[u8]| PathBuf::from(OsString::from_vec(bytes.to_vec()));
    Some(Selection {
        checker: path(checker),
        root: path(root),
    })
}

/// Run the helper from the local checkout, or the resolved product root, and
/// return its selection. Its stderr (refusals) passes through.
/// `Err(None)` means the helper already explained itself.
pub fn select(settings: &Settings) -> Result<Selection, Option<String>> {
    let local = resolve::local_root().filter(|root| {
        resolve::is_file(&root.join(FABRIC_MCP)) && resolve::is_file(&root.join(CHECKER))
    });
    let root = match local {
        Some(root) => root,
        None => resolve::product_root(settings)?,
    };
    run_helper(&root.join(HELPER))
}

fn run_helper(helper: &Path) -> Result<Selection, Option<String>> {
    if !resolve::is_file(helper) {
        return Err(Some(format!(
            "check helper is missing at {}; {REPAIR}",
            helper.display()
        )));
    }
    let output = Command::new("python3")
        .arg(helper)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| {
            Some(format!(
                "cannot run {}: {}; {REPAIR}",
                helper.display(),
                crate::exec::describe(&error)
            ))
        })?;
    if output.status.code() == Some(3) {
        return Err(None);
    }
    if !output.status.success() {
        return Err(Some(format!(
            "check helper failed ({}); {REPAIR}",
            output.status
        )));
    }
    parse_selection(output.stdout)
        .ok_or_else(|| Some(format!("check helper gave unusable output; {REPAIR}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_nul_terminated_absolute_paths() {
        let parsed = parse_selection(b"/a/scripts/check-harness\0/a\0".to_vec()).unwrap();
        assert_eq!(parsed.checker, Path::new("/a/scripts/check-harness"));
        assert_eq!(parsed.root, Path::new("/a"));
    }

    #[test]
    fn keeps_newlines_and_raw_bytes_in_paths() {
        let parsed = parse_selection(b"/a\nb/check\0/a\nb\xff\0".to_vec()).unwrap();
        assert_eq!(parsed.root.as_os_str().as_encoded_bytes(), b"/a\nb\xff");
    }

    #[test]
    fn rejects_malformed_helper_output() {
        for bad in [
            &b""[..],
            b"/a\0",
            b"/a\0/b",
            b"/a\0/b\0x",
            b"rel\0/b\0",
            b"/a\0rel\0",
            b"\0/b\0",
            b"/a\0/b\0\0",
        ] {
            assert_eq!(parse_selection(bad.to_vec()), None, "{bad:?}");
        }
    }
}
