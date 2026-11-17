//! Credential declarations before shared/dedicated root classification.
use crate::{
    CARGO_CREDENTIAL_FILES, CODEX_ACCOUNT_DIRECTORY, CredentialKind, CredentialRoot,
    CredentialScope,
};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CredentialInventory {
    Adapter(String),
    Files(Vec<String>),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CredentialDeclaration {
    pub root: CredentialRoot,
    pub inventory: CredentialInventory,
}

impl CredentialDeclaration {
    /// The only built-in inventory. Launch preparation classifies these just
    /// like account and host declarations. Inventories check for declared
    /// credential files inside lane roots; stores always keep subtree protection.
    pub fn for_home(home: &Path) -> Vec<Self> {
        use CredentialInventory::{Adapter, Files, Unknown};
        use CredentialKind::{CodexHome, Store};
        use CredentialScope::{Exact, Subtree};
        let mut declarations = Vec::new();
        let mut add = |relative: &str, scope, source: &str, kind, inventory| {
            declarations.push(Self {
                root: CredentialRoot {
                    path: home.join(relative),
                    scope,
                    source: source.into(),
                    kind,
                },
                inventory,
            });
        };
        for name in [".ssh", ".aws", ".azure", ".gnupg", ".kube", ".secrets"] {
            add(name, Subtree, "built-in secret store", Store, Unknown);
        }
        for (relative, adapter, kind) in [
            (".claude", "claude", Store),
            (CODEX_ACCOUNT_DIRECTORY.1, "codex", CodexHome),
            (".cursor", "cursor", Store),
            (".gemini", "gemini", Store),
            (".config/cursor", "cursor", Store),
            (".config/opencode", "opencode", Store),
            (".local/share/opencode", "opencode", Store),
        ] {
            add(
                relative,
                Subtree,
                &format!("provider declaration: {adapter} built-in store"),
                kind,
                Adapter(adapter.into()),
            );
        }
        add(
            ".claude.json",
            Exact,
            "provider declaration: claude credentials file",
            Store,
            Unknown,
        );
        add(
            ".agents",
            Subtree,
            "built-in agent credential home",
            Store,
            Unknown,
        );
        for relative in [".config/gh", "Library/Application Support/GitHub CLI"] {
            add(
                relative,
                Subtree,
                "built-in GitHub CLI credentials",
                Store,
                Self::github_inventory(),
            );
        }
        for (relative, source) in [
            (".config/gcloud", "built-in Google Cloud credentials"),
            ("Library/Keychains", "built-in platform keychain"),
            (
                "Archives/git-preservation",
                "built-in Git preservation archive",
            ),
            ("Monero", "built-in Monero credentials"),
        ] {
            add(relative, Subtree, source, Store, Unknown);
        }
        for name in CARGO_CREDENTIAL_FILES {
            add(
                &format!(".cargo/{name}"),
                Exact,
                "built-in Cargo credential file",
                Store,
                Files(vec![(*name).into()]),
            );
        }
        declarations
    }

    pub fn github_inventory() -> CredentialInventory {
        CredentialInventory::Files(vec!["hosts.yml".into(), "config.yml".into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
    #[test]
    fn builtin_package_read_exception_belongs_only_to_default_codex_home() {
        let home = Path::new("/fixture/home");
        let roots = crate::CredentialRoots::for_home(home);
        for declaration in CredentialDeclaration::for_home(home) {
            let root = declaration.root;
            if root.scope != CredentialScope::Subtree {
                continue;
            }
            let package = root
                .path
                .join("packages/standalone/releases/1.0/bin/provider");
            let allowed = root.path == home.join(CODEX_ACCOUNT_DIRECTORY.1);
            assert_eq!(
                !roots.refuses(&package, CredentialScope::Exact, true),
                allowed,
                "{}",
                root.path.display()
            );
            assert!(roots.refuses(&package, CredentialScope::Exact, false));
        }
    }
}
