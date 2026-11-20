//! The small slice of `cargo metadata --format-version 1` the selector needs.
//!
//! Only package identity, manifest location, declared dependencies and the
//! resolve graph are read. Unknown fields are ignored; parsing never reaches
//! network or filesystem state beyond the JSON the caller supplies.

use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    workspace_root: String,
    resolve: Option<Resolve>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: String,
    dependencies: Vec<ManifestDependency>,
}

#[derive(Debug, Deserialize)]
struct ManifestDependency {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    req: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    /// `deps` includes normal, build and dev edges, so no dependency kind is
    /// lost when the reverse graph is built.
    #[serde(default)]
    deps: Vec<DependencyEdge>,
}

#[derive(Debug, Deserialize)]
struct DependencyEdge {
    pkg: String,
}

#[derive(Debug)]
struct PackageInfo {
    /// Manifest directory relative to the workspace root. Empty for a package
    /// whose manifest is the workspace manifest itself.
    directory: PathBuf,
    /// Sorted descriptors of the manifest dependency set, including its kind.
    dependencies: Vec<String>,
    member: bool,
}

/// A workspace's members, ownership map and reverse dependency graph.
#[derive(Debug)]
pub(super) struct Graph {
    /// Workspace member names, sorted and deduplicated.
    members: Vec<String>,
    /// Package id to workspace member name.
    member_name: HashMap<String, String>,
    /// Workspace member name to its package ids (names need not be unique).
    ids_by_name: HashMap<String, Vec<String>>,
    packages: HashMap<String, PackageInfo>,
    /// Dependency package id to the ids that depend on it, over every kind.
    reverse: HashMap<String, Vec<String>>,
}

/// Parses a `cargo metadata` document into a selection graph.
pub(super) fn parse(bytes: &[u8]) -> Result<Graph, String> {
    let metadata: Metadata = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let root = PathBuf::from(&metadata.workspace_root);
    let member_ids: HashSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();

    let mut packages = HashMap::new();
    let mut members = Vec::new();
    let mut member_name = HashMap::new();
    let mut ids_by_name: HashMap<String, Vec<String>> = HashMap::new();

    for package in &metadata.packages {
        let directory = Path::new(&package.manifest_path)
            .parent()
            .and_then(|parent| parent.strip_prefix(&root).ok())
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        let mut dependencies: Vec<String> = package
            .dependencies
            .iter()
            .map(|dependency| {
                // Cargo reports absolute dependency paths; relativise them so
                // a base worktree and head checkout compare like for like.
                let relative_path = dependency
                    .path
                    .as_deref()
                    .and_then(|path| Path::new(path).strip_prefix(&root).ok())
                    .map(|path| path.to_string_lossy().into_owned());
                format!(
                    "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
                    dependency.name,
                    dependency.kind.as_deref().unwrap_or("normal"),
                    dependency.rename.as_deref().unwrap_or(""),
                    dependency.req,
                    relative_path.as_deref().unwrap_or(""),
                    dependency.source.as_deref().unwrap_or(""),
                )
            })
            .collect();
        dependencies.sort();
        dependencies.dedup();

        let member = member_ids.contains(package.id.as_str());
        if member {
            members.push(package.name.clone());
            member_name.insert(package.id.clone(), package.name.clone());
            ids_by_name
                .entry(package.name.clone())
                .or_default()
                .push(package.id.clone());
        }
        packages.insert(
            package.id.clone(),
            PackageInfo {
                directory,
                dependencies,
                member,
            },
        );
    }

    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    if let Some(resolve) = &metadata.resolve {
        for node in &resolve.nodes {
            for edge in &node.deps {
                reverse
                    .entry(edge.pkg.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }
    }
    for dependents in reverse.values_mut() {
        dependents.sort();
        dependents.dedup();
    }

    members.sort();
    members.dedup();
    Ok(Graph {
        members,
        member_name,
        ids_by_name,
        packages,
        reverse,
    })
}

impl Graph {
    /// All workspace member names, sorted for deterministic output.
    pub(super) fn member_names(&self) -> Vec<String> {
        self.members.clone()
    }

    /// The member whose manifest directory is the deepest prefix of `path`, or
    /// `None` when no member owns it. Component-wise matching prevents `leaf`
    /// from owning `leafy/...`.
    pub(super) fn owner_of(&self, path: &Path) -> Option<&str> {
        let mut best: Option<(&str, usize)> = None;
        for (id, info) in &self.packages {
            if !info.member || !path.starts_with(&info.directory) {
                continue;
            }
            let depth = info.directory.components().count();
            if best.is_none_or(|(_, deepest)| depth > deepest)
                && let Some(name) = self.member_name.get(id)
            {
                best = Some((name.as_str(), depth));
            }
        }
        best.map(|(name, _)| name)
    }

    /// Members whose manifest dependency set differs from `base`, plus members
    /// that are new in this graph. A missing base member is treated as a change.
    pub(super) fn dependency_changes_since(&self, base: &Graph) -> Vec<String> {
        self.members
            .iter()
            .filter(|name| {
                !base.ids_by_name.contains_key(*name)
                    || self.dependencies_for(name) != base.dependencies_for(name)
            })
            .cloned()
            .collect()
    }

    fn dependencies_for(&self, name: &str) -> Vec<String> {
        let mut descriptors: Vec<String> = self
            .ids_by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|id| self.packages.get(id))
            .flat_map(|info| info.dependencies.iter().cloned())
            .collect();
        descriptors.sort();
        descriptors.dedup();
        descriptors
    }

    /// `owners` plus every workspace member that transitively depends on them
    /// through this graph, sorted and deduplicated.
    pub(super) fn affected_packages(&self, owners: &[String]) -> Vec<String> {
        let mut reachable: HashSet<&str> = HashSet::new();
        let mut pending: Vec<&str> = Vec::new();
        for owner in owners {
            if let Some(ids) = self.ids_by_name.get(owner) {
                for id in ids {
                    if reachable.insert(id.as_str()) {
                        pending.push(id.as_str());
                    }
                }
            }
        }
        while let Some(id) = pending.pop() {
            if let Some(dependents) = self.reverse.get(id) {
                for dependent in dependents {
                    if reachable.insert(dependent.as_str()) {
                        pending.push(dependent.as_str());
                    }
                }
            }
        }
        let mut names: Vec<String> = reachable
            .into_iter()
            .filter_map(|id| self.member_name.get(id))
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
    }
}
