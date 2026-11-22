//! The bundled macOS app: where the user keeps it, its version and staleness.
//!
//! The release installer (`install.sh`) owns downloading, verifying and
//! swapping the app. This module reads the installed bundle so `install`,
//! `upgrade` and `hosts doctor` can report its version and say plainly when it
//! trails the CLI. It never installs anything itself.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Bundle assembled by `apps/quota-menubar/bundle` and published in the
/// `provenant-apps-*` release asset.
pub const APP_BUNDLE: &str = "Provenant Console.app";
/// Overrides the directory the user keeps the app in. `install.sh` reads the
/// same variable, so a value here always names the path the installer writes.
pub const APPS_DIR_ENV: &str = "PROVENANT_APPS_DIR";
const INFO_PLIST: &str = "Contents/Info.plist";
const VERSION_KEY: &str = "CFBundleShortVersionString";

/// The invoking account's home, as the installer sees it.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The app directory: an explicit absolute override, else `~/Applications`.
pub fn dir(home: &Path) -> PathBuf {
    std::env::var_os(APPS_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Applications"))
}

pub fn app_path(dir: &Path) -> PathBuf {
    dir.join(APP_BUNDLE)
}

/// `CFBundleShortVersionString` of the installed app, when it can be read.
/// The bundle stamps the CLI version at build time; an older bundle may carry
/// the template placeholder `0`, which counts as behind.
pub fn installed_version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app_path(dir).join(INFO_PLIST)).ok()?;
    let key = format!("<key>{VERSION_KEY}</key>");
    let rest = text.split_once(&key)?.1;
    let value = rest.split_once("<string>")?.1.split_once("</string>")?.0;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Compare dotted numeric versions. A missing or non-numeric version is
/// treated as behind: the app cannot prove it matches the CLI.
pub fn behind(installed: &str, cli_version: &str) -> bool {
    fn parts(value: &str) -> Option<Vec<u64>> {
        let core = value.trim().trim_start_matches('v');
        let mut parts = Vec::new();
        for piece in core.split('.') {
            let digits: String = piece.chars().take_while(char::is_ascii_digit).collect();
            parts.push(digits.parse().ok()?);
        }
        (!parts.is_empty()).then_some(parts)
    }
    match (parts(installed), parts(cli_version)) {
        (Some(installed), Some(cli)) => installed < cli,
        _ => true,
    }
}

/// What install, upgrade and doctor report about the installed app.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// Absolute path of the installed bundle.
    pub path: String,
    /// `CFBundleShortVersionString`, when the bundle can be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// CLI version the app is compared against.
    pub cli_version: String,
    /// Whether the installed app trails the CLI.
    pub behind_cli: bool,
    /// The literal advisory, so JSON readers see it without rendering the human text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub fn status(home: &Path, cli_version: &str) -> Status {
    let path = app_path(&dir(home));
    let version = installed_version(&dir(home));
    let behind_cli = version
        .as_deref()
        .is_some_and(|version| behind(version, cli_version))
        || (!path.exists() && cfg!(target_os = "macos"));
    let warning = behind_cli.then(|| {
        format!(
            "app is behind CLI: {} is version {}; CLI is {}. Run `provenant upgrade` to refresh the app.",
            path.display(),
            version.as_deref().unwrap_or("unknown"),
            cli_version
        )
    });
    Status {
        path: path.to_string_lossy().into_owned(),
        version,
        cli_version: cli_version.to_owned(),
        behind_cli,
        warning,
    }
}

/// The one-line advisory doctor and upgrade print when the app is stale.
pub fn advisory(status: &Status) -> String {
    status.warning.clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_stale_or_placeholder_version_is_behind() {
        assert!(super::behind("0", "2.6.6"));
        assert!(super::behind("2.6.5", "2.6.6"));
        assert!(super::behind("unknown", "2.6.6"));
        assert!(!super::behind("2.6.6", "2.6.6"));
        assert!(!super::behind("2.7.0", "2.6.6"));
    }

    #[test]
    fn version_is_read_from_the_installed_bundle() {
        let root =
            std::env::temp_dir().join(format!("provenant-app-version-{}", std::process::id()));
        let plist = root.join(super::APP_BUNDLE).join(super::INFO_PLIST);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(
            &plist,
            "<?xml version=\"1.0\"?><plist><dict>\n  <key>CFBundleShortVersionString</key>\n  <string>2.6.6</string>\n</dict></plist>",
        )
        .unwrap();
        assert_eq!(super::installed_version(&root).as_deref(), Some("2.6.6"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
