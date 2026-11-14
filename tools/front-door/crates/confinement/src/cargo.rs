//! Host Cargo identity and the lane's declared cache access.
use crate::permissions::{PathGrant, PathScope};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub const DEFAULT_SOURCE_WRITES: bool = false;
pub const SOURCE_WRITES_KNOB: &str = "domains.fabric.lanes.cargo_source_writes";
pub const SOURCE_DIRECTORIES: &[&str] = &[
    "registry/index",
    "registry/cache",
    "registry/src",
    "git/db",
    "git/checkouts",
];
// Cargo takes both package locks and records cache use in SQLite on builds.
// Cargo avoids WAL; allow its rollback journal without granting the parent.
// Cargo sources: util/cache_lock.rs and core/global_cache_tracker.rs.
pub const TRACKING_FILES: &[&str] = &[
    ".package-cache",
    ".package-cache-mutate",
    ".global-cache",
    ".global-cache-journal",
];
pub const CONFIG_FILES: &[&str] = &["config", "config.toml"];
pub const CREDENTIAL_FILES: &[&str] = &["credentials", "credentials.toml"];

#[derive(Clone, Debug, serde::Serialize)]
pub struct CargoHome {
    pub path: PathBuf,
    pub source: &'static str,
    pub source_writes: bool,
    pub source_writes_source: &'static str,
}

impl CargoHome {
    /// Resolve the host's path once, never from an adapter's child environment.
    /// Preserve its spelling: canonicalising a symlink changes Cargo fingerprints.
    pub fn resolve(
        home: &Path,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> std::io::Result<Self> {
        let (path, source) = match lookup("CARGO_HOME").filter(|value| !value.is_empty()) {
            Some(path) => (std::path::absolute(PathBuf::from(path))?, "host CARGO_HOME"),
            None => (home.join(".cargo"), "host HOME/.cargo"),
        };
        Ok(Self {
            path,
            source,
            source_writes: DEFAULT_SOURCE_WRITES,
            source_writes_source: "compiled default",
        })
    }

    pub fn configure_source_writes(&mut self, setting: Option<bool>) {
        self.source_writes = setting.unwrap_or(DEFAULT_SOURCE_WRITES);
        self.source_writes_source = if setting.is_some() {
            "host configuration"
        } else {
            "compiled default"
        };
    }

    pub fn sources(&self) -> impl Iterator<Item = PathGrant> + '_ {
        SOURCE_DIRECTORIES.iter().map(move |name| PathGrant {
            path: self.path.join(name),
            scope: PathScope::Subtree,
        })
    }

    pub fn tracking(&self) -> impl Iterator<Item = PathGrant> + '_ {
        TRACKING_FILES.iter().map(move |name| PathGrant {
            path: self.path.join(name),
            scope: PathScope::Exact,
        })
    }

    pub fn credentials(&self) -> impl Iterator<Item = PathGrant> + '_ {
        CREDENTIAL_FILES.iter().map(move |name| PathGrant {
            path: self.path.join(name),
            scope: PathScope::Exact,
        })
    }

    pub fn config(&self) -> impl Iterator<Item = PathGrant> + '_ {
        CONFIG_FILES.iter().map(move |name| PathGrant {
            path: self.path.join(name),
            scope: PathScope::Exact,
        })
    }

    pub fn reads(&self) -> impl Iterator<Item = PathGrant> + '_ {
        self.sources()
            .filter(|grant| grant.path.is_dir())
            .chain(self.config())
            .chain(self.tracking())
    }

    pub fn writes(&self) -> impl Iterator<Item = PathGrant> + '_ {
        self.tracking().chain(self.source_writes())
    }

    pub fn source_writes(&self) -> impl Iterator<Item = PathGrant> + '_ {
        // The opt-in covers the source stores, including Cargo's store metadata
        // and initialisation. It never widens to the Cargo home or its config.
        let stores: std::collections::BTreeSet<_> = self
            .sources()
            .filter_map(|grant| grant.path.parent().map(Path::to_owned))
            .filter(|path| path != &self.path)
            .collect();
        stores
            .into_iter()
            .map(|path| PathGrant {
                path,
                scope: PathScope::Subtree,
            })
            .filter(move |_| self.source_writes)
    }

    pub fn source_environment(
        &self,
        network: crate::permissions::NetworkAccess,
        environment: &mut Vec<(OsString, OsString)>,
    ) {
        let selectors = [
            (
                "CARGO_NET_OFFLINE",
                (!self.source_writes || network != crate::permissions::NetworkAccess::Open)
                    .to_string(),
            ),
            (
                "PROVENANT_CARGO_SOURCE_WRITES",
                self.source_writes.to_string(),
            ),
            (
                "PROVENANT_CARGO_SOURCE_WRITES_SOURCE",
                self.source_writes_source.to_owned(),
            ),
        ];
        environment.retain(|(key, _)| !selectors.iter().any(|(name, _)| key == *name));
        environment.extend(selectors.map(|(key, value)| (key.into(), value.into())));
    }

    pub fn lane_cargo_environment(
        &self,
        home: &Path,
        cache: &Path,
        environment: &mut Vec<(OsString, OsString)>,
    ) {
        let selectors = [
            ("CARGO_HOME", self.path.clone().into_os_string()),
            ("RUSTUP_HOME", home.join(".rustup").into_os_string()),
            ("RUSTUP_AUTO_INSTALL", "0".into()),
            ("CARGO_TARGET_DIR", cache.join("target").into_os_string()),
            (
                "CARGO_BUILD_BUILD_DIR",
                cache.join("target").into_os_string(),
            ),
            ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "never".into()),
        ];
        environment.retain(|(key, _)| !selectors.iter().any(|(name, _)| key == *name));
        environment.extend(selectors.map(|(key, value)| (key.into(), value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_binding_overrides_every_child_selector_without_changing_source_identity() {
        let home = Path::new("/host/home");
        for host_value in [None, Some(OsString::from("/host/custom-cargo"))] {
            let binding = CargoHome::resolve(home, |_| host_value.clone()).unwrap();
            let expected = host_value
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cargo"));
            for mut env in [
                vec![],
                vec![
                    ("CARGO_HOME".into(), "/launch/one".into()),
                    ("CARGO_HOME".into(), "/launch/two".into()),
                ],
            ] {
                binding.lane_cargo_environment(home, Path::new("/lane/cache"), &mut env);
                let values: Vec<_> = env
                    .iter()
                    .filter(|(key, _)| key == "CARGO_HOME")
                    .map(|(_, value)| PathBuf::from(value))
                    .collect();
                assert_eq!(values, [expected.clone()]);
            }
        }
    }

    #[test]
    fn source_access_is_overridable_and_credentials_never_overlap_a_cargo_grant() {
        let mut binding =
            CargoHome::resolve(Path::new("/host/home"), |_| Some("/host/cargo".into())).unwrap();
        for setting in [None, Some(false), Some(true)] {
            binding.configure_source_writes(setting);
            let writes: Vec<_> = binding.writes().collect();
            for source in binding.sources() {
                assert_eq!(
                    writes.iter().any(|grant| grant.overlaps(&source)),
                    setting == Some(true)
                );
            }
            for credential in binding.credentials() {
                assert!(
                    !binding
                        .reads()
                        .chain(binding.writes())
                        .any(|grant| grant.overlaps(&credential))
                );
            }
            assert!(
                binding
                    .tracking()
                    .all(|grant| grant.scope == PathScope::Exact && writes.contains(&grant))
            );
        }
    }
}
