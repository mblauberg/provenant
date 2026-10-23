//! Restore-view interface, independent of provider discovery and Git effects.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// Git's mode and object ID retain the Python two-element array encoding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Entry(pub String, pub String);
pub type Entries = BTreeMap<OsString, Entry>;

/// In-memory views preserve Unix filename bytes. Snapshot wire encoding is owned
/// by the later record integration; this interface emits no snapshot JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub integrated: BTreeSet<OsString>,
}

pub const PATCHES: [(&str, &str); 3] = [
    ("disk", "protected.patch"),
    ("index", "protected.index.patch"),
    ("head", "protected.head.patch"),
];
pub const QUARANTINE_NAME: &str = "Provenant Fabric";
pub const QUARANTINE_EMAIL: &str = "fabric@provenant.invalid";

pub fn quarantinable(changes: impl IntoIterator<Item = impl AsRef<OsStr>>) -> bool {
    changes.into_iter().all(|path| {
        let path = path.as_ref().as_bytes();
        path.starts_with(b".agents/") && !path.ends_with(b" (unresolved conflict)")
    })
}

/// Merge a qualifying integration ref after the caller establishes ancestry.
/// On a diverged branch, a lane-authored path wins over the incoming entry.
pub fn integrate(target: &Entries, incoming: &Entries, base: &Entries) -> Entries {
    target
        .keys()
        .chain(incoming.keys())
        .filter_map(|path| {
            let source = if target.get(path) == base.get(path) {
                incoming
            } else {
                target
            };
            source.get(path).map(|entry| (path.clone(), entry.clone()))
        })
        .collect()
}

pub fn restore_views(start: &InstructionSnapshot, target: Entries) -> RestoreViews {
    let integrated: BTreeSet<_> = target
        .keys()
        .chain(start.tree.keys())
        .filter(|path| target.get(*path) != start.tree.get(*path))
        .cloned()
        .collect();
    let view = |entries: &Entries| {
        entries
            .keys()
            .chain(integrated.iter())
            .filter_map(|path| {
                let source = if integrated.contains(path) {
                    &target
                } else {
                    entries
                };
                source.get(path).map(|entry| (path.clone(), entry.clone()))
            })
            .collect()
    };
    RestoreViews {
        index: view(&start.index),
        disk: view(&start.disk),
        head: target,
        integrated,
    }
}

/// Removals precede updates. BTree collections make unspecified set order deterministic.
pub fn index_info(entries: &Entries, paths: &BTreeSet<OsString>, object_format: &str) -> Vec<u8> {
    let zero = "0".repeat(if object_format == "sha256" { 64 } else { 40 });
    let mut result = Vec::new();
    for path in paths.iter().filter(|path| !entries.contains_key(*path)) {
        result.extend_from_slice(format!("0 {zero}\t").as_bytes());
        result.extend_from_slice(path.as_bytes());
        result.push(0);
    }
    for path in paths {
        if let Some(Entry(mode, oid)) = entries.get(path) {
            result.extend_from_slice(format!("{mode} {oid}\t").as_bytes());
            result.extend_from_slice(path.as_bytes());
            result.push(0);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entries(rows: &[(&str, &str)]) -> Entries {
        rows.iter()
            .map(|(path, oid)| {
                (
                    OsString::from(path),
                    Entry("100644".into(), oid.to_string()),
                )
            })
            .collect()
    }

    #[test]
    fn only_ordinary_instruction_changes_are_quarantinable() {
        assert!(quarantinable(std::iter::empty::<&str>()));
        assert!(quarantinable([".agents/skills/x", ".agents/new"]));
        for path in [
            ".agents",
            ".AGENTS/x",
            "src/x",
            ".agents/x (unresolved conflict)",
        ] {
            assert!(!quarantinable([path]));
        }
    }

    #[test]
    fn integration_preserves_lane_edits_and_incoming_deletions() {
        let base = entries(&[("a", "old"), ("b", "old"), ("c", "old")]);
        let lane = entries(&[("a", "lane"), ("b", "old"), ("c", "old")]);
        let incoming = entries(&[("a", "new"), ("b", "new"), ("d", "new")]);
        assert_eq!(
            integrate(&lane, &incoming, &base),
            entries(&[("a", "lane"), ("b", "new"), ("d", "new")])
        );
    }

    #[test]
    fn restored_index_and_disk_keep_starting_edits_except_integrated_paths() {
        let start = InstructionSnapshot {
            head: "sha".into(),
            object_format: "sha1".into(),
            tree: entries(&[("a", "old"), ("b", "old")]),
            index: entries(&[("a", "staged"), ("b", "old")]),
            disk: entries(&[("a", "dirty"), ("b", "old")]),
        };
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
            assert_eq!(
                index_info(&entries, &paths, format),
                format!("0 {}\tz\0{}", "0".repeat(size), "100644 abc\ta\t雪\0").as_bytes()
            );
        }
    }

    #[test]
    fn snapshot_wire_entries_remain_pairs() {
        let entry = Entry("120000".into(), "abc".into());
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            "[\"120000\",\"abc\"]"
        );
    }
    #[test]
    fn non_utf8_instruction_names_survive_restore_and_index_framing() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(b".agents/invalid-\xff".to_vec());
        let deleted = OsString::from_vec(b".agents/deleted-\xfe".to_vec());
        let entry = Entry("100644".into(), "a".repeat(40));
        let tree = Entries::from([
            (path.clone(), entry.clone()),
            (deleted.clone(), entry.clone()),
        ]);
        let start = InstructionSnapshot {
            head: "a".repeat(40),
            object_format: "sha1".into(),
            tree: tree.clone(),
            index: tree.clone(),
            disk: tree.clone(),
        };
        let target = Entries::from([(path.clone(), entry)]);
        let restored = restore_views(&start, integrate(&tree, &target, &tree));
        assert!(quarantinable([&path]));
        assert_eq!(restored.disk, target);
        assert_eq!(restored.integrated, BTreeSet::from([deleted.clone()]));
        let framed = index_info(&restored.index, &BTreeSet::from([path, deleted]), "sha1");
        let Some(expected) = crate::python::oracle(
            r#"
import instruction_quarantine, os
path=os.fsdecode(b'.agents/invalid-\xff')
deleted=os.fsdecode(b'.agents/deleted-\xfe')
entries={path: ('100644', 'a'*40)}
print(json.dumps(list(instruction_quarantine._index_info(entries, sorted([path, deleted]), '0'*40))))
"#,
            &serde_json::json!({}),
        ) else {
            return;
        };
        assert_eq!(serde_json::to_value(framed).unwrap(), expected);
    }
}
