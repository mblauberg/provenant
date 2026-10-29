//! The role library: named role profiles shared by presets, composed with
//! `extends`, and the `skill:` references that give a role its long-form
//! guidance. A role grants no authority; access, routing and write scope are
//! still decided by the node that uses it.

use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Skill reference text compiled into the binary, keyed by the path after
/// `skill:`. Used when no release root with `skills/` is available (tests and
/// bare builds); an installed release root always wins.
const EMBEDDED: [(&str, &str); 14] = [
    (
        "code-review/references/reviewer-context.md",
        include_str!("../../../skills/code-review/references/reviewer-context.md"),
    ),
    (
        "code-review/references/lenses/correctness-spec.md",
        include_str!("../../../skills/code-review/references/lenses/correctness-spec.md"),
    ),
    (
        "code-review/references/lenses/design-fit-reuse.md",
        include_str!("../../../skills/code-review/references/lenses/design-fit-reuse.md"),
    ),
    (
        "code-review/references/lenses/simplicity.md",
        include_str!("../../../skills/code-review/references/lenses/simplicity.md"),
    ),
    (
        "code-review/references/lenses/conventions-readability.md",
        include_str!("../../../skills/code-review/references/lenses/conventions-readability.md"),
    ),
    (
        "code-review/references/lenses/tests.md",
        include_str!("../../../skills/code-review/references/lenses/tests.md"),
    ),
    (
        "code-review/references/lenses/security-privacy.md",
        include_str!("../../../skills/code-review/references/lenses/security-privacy.md"),
    ),
    (
        "code-review/references/lenses/concurrency-data-errors.md",
        include_str!("../../../skills/code-review/references/lenses/concurrency-data-errors.md"),
    ),
    (
        "code-review/references/lenses/adversarial.md",
        include_str!("../../../skills/code-review/references/lenses/adversarial.md"),
    ),
    (
        "scope/references/scout-areas.md",
        include_str!("../../../skills/scope/references/scout-areas.md"),
    ),
    (
        "scope/references/researcher.md",
        include_str!("../../../skills/scope/references/researcher.md"),
    ),
    (
        "scope/references/architect.md",
        include_str!("../../../skills/scope/references/architect.md"),
    ),
    (
        "scope/references/acceptance-writer.md",
        include_str!("../../../skills/scope/references/acceptance-writer.md"),
    ),
    (
        "scope/references/critic.md",
        include_str!("../../../skills/scope/references/critic.md"),
    ),
];

/// The skill path a legacy `prompt_file` name stands for:
/// `code-review/lenses/tests` is `code-review/references/lenses/tests.md`.
pub fn prompt_file_reference(name: &str) -> Result<String, String> {
    let (skill, rest) = name
        .split_once('/')
        .ok_or_else(|| format!("prompt_file {name} must be <skill>/<file>"))?;
    Ok(format!("skill:{skill}/references/{rest}.md"))
}

/// A resolved `skill:` reference.
#[derive(Clone, Debug)]
pub struct Reference {
    /// The `skill:<skill>/<path>` spelling the role declared.
    pub name: String,
    /// Content digest recorded in the graph for deterministic replay.
    pub digest: String,
    pub text: String,
    /// Absolute path on disk, when the release root supplied it.
    pub path: Option<PathBuf>,
}

pub struct RoleLibrary {
    entries: BTreeMap<String, Vec<(String, Value)>>,
    /// Origins of repository role files, which the landing floor leaves out.
    repo: BTreeSet<String>,
    /// The release root whose `skills/` directory `skill:` references resolve
    /// against. `None` uses the references compiled into the binary.
    pub skill_root: Option<PathBuf>,
}

/// A role with its `extends` chain applied.
#[derive(Clone, Debug)]
pub struct ResolvedRole {
    pub profile: Value,
    /// Where the winning definition came from.
    pub origin: String,
    /// The role name, then each parent it extends.
    pub chain: Vec<String>,
}

fn overlay(parent: &Value, child: &Value) -> Value {
    let mut merged = parent.clone();
    if let (Some(target), Some(source)) = (merged.as_object_mut(), child.as_object()) {
        for (key, value) in source {
            if key != "extends" {
                target.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

impl Default for RoleLibrary {
    fn default() -> Self {
        Self::bundled()
    }
}

impl RoleLibrary {
    #[allow(
        clippy::expect_used,
        reason = "the bundled role library is a built-in invariant"
    )]
    pub fn bundled() -> Self {
        let mut library = Self {
            entries: BTreeMap::new(),
            repo: BTreeSet::new(),
            skill_root: None,
        };
        library
            .add("bundled", include_str!("../assets/roles.toml"))
            .expect("valid bundled roles");
        library
    }

    /// Add one `[roles.<name>]` document. Later layers override earlier ones.
    pub fn add(&mut self, origin: &str, text: &str) -> Result<(), String> {
        let document: Value =
            toml_edit::de::from_str(text).map_err(|error| format!("{origin}: {error}"))?;
        if let Some(extra) = document
            .as_object()
            .and_then(|map| map.keys().find(|key| *key != "roles"))
        {
            return Err(format!("{origin}: unknown top-level key: {extra}"));
        }
        let Some(roles) = document.get("roles") else {
            return Ok(());
        };
        let roles = roles
            .as_object()
            .ok_or_else(|| format!("{origin}: expected [roles.<name>] tables"))?;
        for (name, role) in roles {
            if !role.is_object() {
                return Err(format!("{origin}: role {name} must be a table"));
            }
            if !valid_role_name(name) {
                return Err(format!("{origin}: invalid role name: {name}"));
            }
            self.entries
                .entry(name.clone())
                .or_default()
                .push((origin.to_owned(), role.clone()));
        }
        Ok(())
    }

    /// Add a roles file when it exists. `repo` marks a repository file.
    pub fn add_file(&mut self, path: &Path, repo: bool) -> Result<(), String> {
        let mut file = match provenant_os::fs::FileHandle::open_source(path) {
            Ok(file) => file,
            Err(_) if !path.exists() => return Ok(()),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let bytes = file
            .read_bounded(1_048_576)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let text = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        let origin = path.display().to_string();
        self.add(&origin, &text)?;
        if repo {
            self.repo.insert(origin);
        }
        Ok(())
    }

    /// Whether a repository role file was loaded.
    pub fn has_repo(&self) -> bool {
        !self.repo.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    fn lookup(
        &self,
        name: &str,
        inline: Option<&Value>,
        use_inline: bool,
        exclude_repo: bool,
    ) -> Option<(String, Value, String)> {
        let mut candidate = name;
        loop {
            if use_inline && let Some(role) = inline.and_then(|roles| roles.get(candidate)) {
                return Some((candidate.to_owned(), role.clone(), "preset".into()));
            }
            let layers: Vec<_> = self
                .entries
                .get(candidate)
                .into_iter()
                .flatten()
                .filter(|(origin, _)| !exclude_repo || !self.repo.contains(origin))
                .collect();
            if !layers.is_empty() {
                let mut merged = json!({});
                for (_, layer) in &layers {
                    merged = overlay(&merged, layer);
                    // `extends` is per definition: the last layer to name one wins.
                    if let Some(parent) = layer.get("extends") {
                        merged["extends"] = parent.clone();
                    }
                }
                let origin = layers.last().map(|(origin, _)| origin.clone())?;
                return Some((candidate.to_owned(), merged, origin));
            }
            // `lens.tests` falls back to `lens`: a dotted name is a variant.
            candidate = candidate.rsplit_once('.')?.0;
        }
    }

    /// Resolve `name`, applying `extends`. An inline preset role of the same
    /// name wins over the library; an inline `extends` may name a library role.
    /// `exclude_repo` leaves out repository role files.
    pub fn resolve(
        &self,
        name: &str,
        inline: Option<&Value>,
        exclude_repo: bool,
    ) -> Result<ResolvedRole, String> {
        self.resolve_depth(name, inline, true, exclude_repo, 0)
    }

    fn resolve_depth(
        &self,
        name: &str,
        inline: Option<&Value>,
        use_inline: bool,
        exclude_repo: bool,
        depth: usize,
    ) -> Result<ResolvedRole, String> {
        if depth > 8 {
            return Err(format!("role {name}: extends chain is too deep or cyclic"));
        }
        let (found, raw, origin) = self
            .lookup(name, inline, use_inline, exclude_repo)
            .ok_or_else(|| {
                format!(
                    "unknown preset role: {name}; library roles: {}",
                    self.names().join(", ")
                )
            })?;
        if !raw.is_object() {
            return Err(format!("role {found} must be a table"));
        }
        let mut chain = vec![found.clone()];
        let profile = match raw.get("extends") {
            None => overlay(&json!({}), &raw),
            Some(parent) => {
                let parent = parent
                    .as_str()
                    .ok_or_else(|| format!("role {found}: extends must be a role name"))?;
                let from_inline = origin == "preset";
                // A preset role may extend the library role of its own name.
                let parent = self.resolve_depth(
                    parent,
                    inline,
                    use_inline && !(from_inline && parent == found),
                    exclude_repo,
                    depth + 1,
                )?;
                chain.extend(parent.chain.clone());
                overlay(&parent.profile, &raw)
            }
        };
        Ok(ResolvedRole {
            profile,
            origin,
            chain,
        })
    }

    /// Resolve one `skill:<skill>/<path>` reference against the release root.
    /// Unknown references, `..`, absolute paths and links leaving the skills
    /// directory are refused.
    pub fn reference(&self, name: &str) -> Result<Reference, String> {
        let rest = name
            .strip_prefix("skill:")
            .ok_or_else(|| format!("reference {name}: expected skill:<skill>/<path>"))?;
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() < 2
            || parts
                .iter()
                .any(|part| part.is_empty() || *part == "." || *part == ".." || part.contains('\\'))
        {
            return Err(format!(
                "reference {name}: expected skill:<skill>/<path> with plain path parts"
            ));
        }
        if let Some(root) = &self.skill_root {
            let skills = root.join("skills");
            if skills.is_dir() {
                let path = skills.join(rest);
                let base = std::fs::canonicalize(&skills)
                    .map_err(|error| format!("{}: {error}", skills.display()))?;
                let resolved = std::fs::canonicalize(&path).map_err(|_| {
                    format!("reference {name}: no such file under {}", skills.display())
                })?;
                if !resolved.starts_with(&base) {
                    return Err(format!(
                        "reference {name}: resolves outside the skills root"
                    ));
                }
                let bytes = std::fs::read(&resolved)
                    .map_err(|error| format!("reference {name}: {error}"))?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| format!("reference {name}: not UTF-8 text"))?;
                return Ok(Reference {
                    name: name.to_owned(),
                    digest: provenant_records::CanonicalV1::file_digest(text.as_bytes())
                        .to_string(),
                    text,
                    path: Some(resolved),
                });
            }
        }
        let text = EMBEDDED
            .iter()
            .find(|(key, _)| *key == rest)
            .map(|(_, text)| (*text).to_owned())
            .ok_or_else(|| format!("unknown reference: {name}"))?;
        Ok(Reference {
            name: name.to_owned(),
            digest: provenant_records::CanonicalV1::file_digest(text.as_bytes()).to_string(),
            text,
            path: None,
        })
    }
}

fn valid_role_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
