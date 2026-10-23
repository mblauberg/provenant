//! Restore-view interface, independent of provider discovery and Git effects.
use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};

/// Git's mode and object ID retain the Python two-element array encoding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Entry(pub String, pub String);
pub type Entries = BTreeMap<String, Entry>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstructionSnapshot {
    pub head: String,
    pub object_format: String,
    pub tree: Entries,
    pub index: Entries,
    pub disk: Entries,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreViews {
    pub head: Entries,
    pub index: Entries,
    pub disk: Entries,
    pub integrated: BTreeSet<String>,
}

pub const PATCHES: [(&str, &str); 3] = [
    ("disk", "protected.patch"),
    ("index", "protected.index.patch"),
    ("head", "protected.head.patch"),
];
pub const QUARANTINE_NAME: &str = "Provenant Fabric";
pub const QUARANTINE_EMAIL: &str = "fabric@provenant.invalid";

pub fn quarantinable<'a>(changes: impl IntoIterator<Item = &'a str>) -> bool {
    changes.into_iter().all(|path| path.starts_with(".agents/")
        && !path.ends_with(" (unresolved conflict)"))
}

/// Merge a qualifying integration ref after the caller establishes ancestry.
/// On a diverged branch, a lane-authored path wins over the incoming entry.
pub fn integrate(target: &Entries, incoming: &Entries, base: &Entries) -> Entries {
    target.keys().chain(incoming.keys()).filter_map(|path| {
        let source = if target.get(path) == base.get(path) { incoming } else { target };
        source.get(path).map(|entry| (path.clone(), entry.clone()))
    }).collect()
}

pub fn restore_views(start: &InstructionSnapshot, target: Entries) -> RestoreViews {
    let integrated: BTreeSet<_> = target.keys().chain(start.tree.keys())
        .filter(|path| target.get(*path) != start.tree.get(*path)).cloned().collect();
    let view = |entries: &Entries| entries.keys().chain(integrated.iter()).filter_map(|path| {
        let source = if integrated.contains(path) { &target } else { entries };
        source.get(path).map(|entry| (path.clone(), entry.clone()))
    }).collect();
    RestoreViews { index: view(&start.index), disk: view(&start.disk), head: target, integrated }
}

/// Removals precede updates. BTree collections make unspecified set order deterministic.
pub fn index_info(entries: &Entries, paths: &BTreeSet<String>, object_format: &str) -> Vec<u8> {
    let zero = "0".repeat(if object_format == "sha256" { 64 } else { 40 });
    let mut result = Vec::new();
    for path in paths.iter().filter(|path| !entries.contains_key(*path)) {
        result.extend_from_slice(format!("0 {zero}\t{path}\0").as_bytes());
    }
    for path in paths {
        if let Some(Entry(mode, oid)) = entries.get(path) {
            result.extend_from_slice(format!("{mode} {oid}\t{path}\0").as_bytes());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entries(rows: &[(&str, &str)]) -> Entries {
        rows.iter().map(|(path, oid)| (path.to_string(), Entry("100644".into(), oid.to_string()))).collect()
    }

    #[test]
    fn only_ordinary_instruction_changes_are_quarantinable() {
        assert!(quarantinable([]));
        assert!(quarantinable([".agents/skills/x", ".agents/new"]));
        for path in [".agents", ".AGENTS/x", "src/x", ".agents/x (unresolved conflict)"] {
            assert!(!quarantinable([path]));
        }
    }

    #[test]
    fn integration_preserves_lane_edits_and_incoming_deletions() {
        let base = entries(&[("a", "old"), ("b", "old"), ("c", "old")]);
        let lane = entries(&[("a", "lane"), ("b", "old"), ("c", "old")]);
        let incoming = entries(&[("a", "new"), ("b", "new"), ("d", "new")]);
        assert_eq!(integrate(&lane, &incoming, &base), entries(&[("a", "lane"), ("b", "new"), ("d", "new")]));
    }

    #[test]
    fn restored_index_and_disk_keep_starting_edits_except_integrated_paths() {
        let start = InstructionSnapshot { head: "sha".into(), object_format: "sha1".into(),
            tree: entries(&[("a", "old"), ("b", "old")]), index: entries(&[("a", "staged"), ("b", "old")]),
            disk: entries(&[("a", "dirty"), ("b", "old")]) };
        let restored = restore_views(&start, entries(&[("a", "old")]));
        assert_eq!(restored.index, entries(&[("a", "staged")]));
        assert_eq!(restored.disk, entries(&[("a", "dirty")]));
        assert_eq!(restored.integrated, BTreeSet::from(["b".into()]));
    }

    #[test]
    fn index_info_removes_before_updates_and_preserves_nul_framing() {
        let paths = BTreeSet::from(["a\t雪".into(), "z".into()]);
        let entries = entries(&[("a\t雪", "abc")]);
        for (format, size) in [("sha1", 40), ("sha256", 64)] {
            assert_eq!(index_info(&entries, &paths, format),
                format!("0 {}\tz\0{}", "0".repeat(size), "100644 abc\ta\t雪\0").as_bytes());
        }
    }

    #[test]
    fn snapshot_wire_entries_remain_pairs() {
        let entry = Entry("120000".into(), "abc".into());
        assert_eq!(serde_json::to_string(&entry).unwrap(), "[\"120000\",\"abc\"]");
    }
}
