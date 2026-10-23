use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::rc::Rc;

#[derive(Clone)]
struct Node {
    info: EntryInfo,
    children: BTreeMap<OsString, usize>,
}
struct Fs {
    nodes: RefCell<Vec<Node>>,
    swaps: RefCell<BTreeMap<(usize, OsString), usize>>,
    read: Rc<Cell<usize>>,
}
struct Entries {
    names: std::vec::IntoIter<OsString>,
    read: Rc<Cell<usize>>,
}
impl Iterator for Entries {
    type Item = io::Result<OsString>;
    fn next(&mut self) -> Option<Self::Item> {
        self.names.next().map(|name| {
            self.read.set(self.read.get() + 1);
            Ok(name)
        })
    }
}
impl Fs {
    fn new() -> Self {
        Self {
            nodes: RefCell::new(vec![Node {
                info: EntryInfo {
                    device: 1,
                    inode: 0,
                    links: 1,
                    allocated: 512,
                    directory: true,
                    symlink: false,
                },
                children: BTreeMap::new(),
            }]),
            swaps: RefCell::new(BTreeMap::new()),
            read: Rc::new(Cell::new(0)),
        }
    }
    fn add(&self, parent: usize, name: &str, directory: bool, allocated: u64) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        let id = nodes.len();
        nodes.push(Node {
            info: EntryInfo {
                device: 1,
                inode: id as u64,
                links: 1,
                allocated,
                directory,
                symlink: false,
            },
            children: BTreeMap::new(),
        });
        nodes[parent].children.insert(name.into(), id);
        id
    }
    fn child(&self, parent: &usize, name: &OsStr) -> io::Result<usize> {
        self.nodes.borrow()[*parent]
            .children
            .get(name)
            .copied()
            .ok_or(io::ErrorKind::NotFound.into())
    }
}
impl StorageFs for Fs {
    type Dir = usize;
    type Entries = Entries;
    fn open_anchor(&self, path: &Path) -> io::Result<usize> {
        let mut id = 0;
        for part in path.components() {
            if let std::path::Component::Normal(name) = part {
                id = self.open_child(&id, name)?;
            }
        }
        Ok(id)
    }
    fn info(&self, dir: &usize) -> io::Result<EntryInfo> {
        Ok(self.nodes.borrow()[*dir].info)
    }
    fn stat(&self, parent: &usize, name: &OsStr) -> io::Result<EntryInfo> {
        self.info(&self.child(parent, name)?)
    }
    fn open_child(&self, parent: &usize, name: &OsStr) -> io::Result<usize> {
        if let Some(next) = self.swaps.borrow_mut().remove(&(*parent, name.to_owned())) {
            self.nodes.borrow_mut()[*parent]
                .children
                .insert(name.to_owned(), next);
        }
        let id = self.child(parent, name)?;
        let info = self.info(&id)?;
        if !info.directory || info.symlink {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(id)
    }
    fn entries(&self, dir: &usize) -> io::Result<Entries> {
        Ok(Entries {
            names: self.nodes.borrow()[*dir]
                .children
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .into_iter(),
            read: self.read.clone(),
        })
    }
    fn make_writable(&self, _: &usize) -> io::Result<()> {
        Ok(())
    }
    fn unlink(&self, parent: &usize, name: &OsStr) -> io::Result<()> {
        self.child(parent, name)?;
        self.nodes.borrow_mut()[*parent].children.remove(name);
        Ok(())
    }
    fn rmdir(&self, parent: &usize, name: &OsStr) -> io::Result<()> {
        let id = self.child(parent, name)?;
        if !self.nodes.borrow()[id].children.is_empty() {
            return Err(io::ErrorKind::DirectoryNotEmpty.into());
        }
        self.unlink(parent, name)
    }
}
fn fixture() -> (Fs, usize) {
    let fs = Fs::new();
    let run = fs.add(0, "run", true, 512);
    let attempt = fs.add(run, "attempt", true, 512);
    let tmp = fs.add(attempt, "tmp", true, 512);
    (fs, tmp)
}

#[test]
fn accounts_allocated_blocks_hardlinks_once_and_no_symlinks() {
    let (fs, tmp) = fixture();
    let file = fs.add(tmp, "a", false, 4096);
    fs.nodes.borrow_mut()[file].info.links = 2;
    fs.nodes.borrow_mut()[tmp].children.insert("b".into(), file);
    let link = fs.add(tmp, "link", false, 8192);
    fs.nodes.borrow_mut()[link].info.symlink = true;
    assert_eq!(tree_bytes(&fs, Path::new("/run/attempt/tmp")), 4608);
    assert_eq!(tree_bytes(&fs, Path::new("/absent")), 0);
}
#[test]
fn bounds_depth_without_counting_deeper_files() {
    let (fs, tmp) = fixture();
    let mut dir = tmp;
    for i in 0..=TREE_MAX_DEPTH {
        dir = fs.add(dir, &format!("d{i}"), true, 512);
    }
    fs.add(dir, "large", false, 1_000_000);
    assert_eq!(
        tree_bytes(&fs, Path::new("/run/attempt/tmp")),
        512 * (TREE_MAX_DEPTH as u64 + 1)
    );
}
#[test]
fn entry_budget_does_not_read_the_whole_tree() {
    let (fs, tmp) = fixture();
    for i in 0..20 {
        fs.add(tmp, &format!("f{i}"), false, 512);
    }
    let parent = fs.open_anchor(Path::new("/run/attempt")).unwrap();
    assert_eq!(
        walk(
            &fs,
            &parent,
            OsStr::new("tmp"),
            0,
            &mut 3,
            &mut HashSet::new()
        ),
        2048
    );
    assert_eq!(fs.read.get(), 4);
}
#[test]
fn accounting_does_not_enter_a_swapped_link() {
    let (fs, tmp) = fixture();
    let link = fs.add(0, "victim", false, 512);
    fs.nodes.borrow_mut()[link].info.symlink = true;
    fs.swaps.borrow_mut().insert((2, "tmp".into()), link);
    fs.add(tmp, "large", false, 1_000_000);
    assert_eq!(tree_bytes(&fs, Path::new("/run/attempt/tmp")), 512);
}
#[test]
fn removal_refuses_ancestors_links_outside_paths_and_anchor_itself() {
    let (fs, _) = fixture();
    let target = Path::new("/run/attempt/tmp");
    fs.nodes.borrow_mut()[2].info.symlink = true;
    assert_eq!(remove_tree(&fs, Path::new("/run"), target), vec![target]);
    assert_eq!(
        remove_tree(&fs, Path::new("/run"), Path::new("/outside")),
        vec![Path::new("/outside")]
    );
    assert_eq!(
        remove_tree(&fs, Path::new("/run"), Path::new("/run")),
        vec![Path::new("/run")]
    );
}
#[test]
fn removal_refuses_directory_identity_swap() {
    let (fs, tmp) = fixture();
    let old = fs.add(tmp, "sub", true, 512);
    fs.add(old, "old", false, 512);
    let victim = fs.add(0, "victim", true, 512);
    fs.add(victim, "keep", false, 512);
    fs.swaps.borrow_mut().insert((tmp, "sub".into()), victim);
    assert_eq!(
        remove_tree(&fs, Path::new("/run"), Path::new("/run/attempt/tmp")),
        vec![Path::new("/run/attempt/tmp/sub")]
    );
    assert!(fs.child(&victim, OsStr::new("keep")).is_ok());
}
#[test]
fn removal_keeps_mounts_and_removes_siblings() {
    let (fs, tmp) = fixture();
    let mount = fs.add(tmp, "mount", true, 512);
    fs.nodes.borrow_mut()[mount].info.device = 2;
    fs.add(mount, "keep", false, 512);
    fs.add(tmp, "gone", false, 512);
    assert_eq!(
        remove_tree(&fs, Path::new("/run"), Path::new("/run/attempt/tmp")),
        vec![Path::new("/run/attempt/tmp/mount")]
    );
    assert!(fs.child(&tmp, OsStr::new("gone")).is_err());
    assert!(fs.child(&mount, OsStr::new("keep")).is_ok());
}
#[test]
fn removal_refuses_cross_device_ancestor() {
    let (fs, _) = fixture();
    fs.nodes.borrow_mut()[2].info.device = 2;
    assert!(!remove_tree(&fs, Path::new("/run"), Path::new("/run/attempt/tmp")).is_empty());
}
#[test]
fn remove_tree_refuses_leaf_link_but_remove_at_unlinks_it() {
    let (fs, tmp) = fixture();
    fs.nodes.borrow_mut()[tmp].info.directory = false;
    fs.nodes.borrow_mut()[tmp].info.symlink = true;
    assert!(!remove_tree(&fs, Path::new("/run"), Path::new("/run/attempt/tmp")).is_empty());
    assert!(remove_at(&fs, &2, OsStr::new("tmp")).is_empty());
    assert!(remove_at(&fs, &2, OsStr::new("missing")).is_empty());
}
#[test]
fn prune_retains_resumable_failed_and_requested_tmp() {
    for (status, keep, size, retained) in [
        ("ok", false, 512, false),
        ("ok", true, 512, true),
        ("input_required", false, KEEP_FAILED_TMP_BYTES + 1, true),
        ("failed", false, KEEP_FAILED_TMP_BYTES, true),
        ("failed", false, KEEP_FAILED_TMP_BYTES + 1, false),
        ("succeeded", false, 512, true),
    ] {
        let (fs, tmp) = fixture();
        fs.nodes.borrow_mut()[tmp].info.allocated = size;
        let (freed, left) = prune_private_tmp(
            &fs,
            Path::new("/run"),
            Path::new("/run/attempt"),
            status,
            keep,
        );
        assert!(left.is_empty());
        assert_eq!(freed, if retained { 0 } else { size }, "{status}");
        assert_eq!(fs.child(&2, OsStr::new("tmp")).is_ok(), retained);
    }
}
#[test]
fn prune_reports_no_bytes_freed_when_any_path_remains() {
    let (fs, tmp) = fixture();
    let mount = fs.add(tmp, "mount", true, 512);
    fs.nodes.borrow_mut()[mount].info.device = 2;
    let (freed, left) = prune_private_tmp(
        &fs,
        Path::new("/run"),
        Path::new("/run/attempt"),
        "ok",
        false,
    );
    assert_eq!(freed, 0);
    assert_eq!(left, vec![Path::new("/run/attempt/tmp/mount")]);
}

#[test]
fn differential_python_allocations_and_pruning() {
    let oracle = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../skills/orchestrate/scripts/attempt_storage.py");
    let mut command = match crate::oracle::python() {
        Some(command) => command,
        None => return,
    };
    command.args(["-c",r#"
import importlib.util,json,os,stat,sys,tempfile
from pathlib import Path
spec=importlib.util.spec_from_file_location('storage_oracle',sys.argv[1]);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
with tempfile.TemporaryDirectory() as root:
 root=Path(root);tmp=root/'run/attempt/tmp';(tmp/'sub').mkdir(parents=True)
 (tmp/'sub/data').write_bytes(b'hello'*2000);os.link(tmp/'sub/data',tmp/'hardlink')
 (tmp/'sparse').touch()
 with (tmp/'sparse').open('wb') as f:f.truncate(100000000)
 (tmp/'link').symlink_to(root/'missing')
 def snap(p):
  s=p.lstat();d={'device':s.st_dev,'inode':s.st_ino,'links':s.st_nlink,'allocated':s.st_blocks*512,'directory':stat.S_ISDIR(s.st_mode),'symlink':stat.S_ISLNK(s.st_mode)}
  d['children']={c.name:snap(c) for c in p.iterdir()} if d['directory'] else {}
  return d
 tree=snap(root);size=m.tree_bytes(tmp);freed,left=m.prune_private_tmp(root,root/'run/attempt','ok')
 print(json.dumps({'tree':tree,'size':size,'freed':freed,'left':left,'exists':tmp.exists()}))
"#]).arg(oracle).env_remove(KEEP_ENV);
    let output = crate::oracle::run(&mut command)
        .unwrap_or_else(|error| panic!("cannot run Python oracle: {error}"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    fn load(fs: &Fs, id: usize, v: &serde_json::Value) {
        fs.nodes.borrow_mut()[id].info = EntryInfo {
            device: v["device"].as_u64().unwrap(),
            inode: v["inode"].as_u64().unwrap(),
            links: v["links"].as_u64().unwrap(),
            allocated: v["allocated"].as_u64().unwrap(),
            directory: v["directory"].as_bool().unwrap(),
            symlink: v["symlink"].as_bool().unwrap(),
        };
        for (name, child) in v["children"].as_object().unwrap() {
            let next = fs.add(id, name, false, 0);
            load(fs, next, child);
        }
    }
    let fs = Fs::new();
    load(&fs, 0, &value["tree"]);
    assert_eq!(
        tree_bytes(&fs, Path::new("/run/attempt/tmp")),
        value["size"].as_u64().unwrap()
    );
    let (freed, left) =
        prune_private_tmp(&fs, Path::new("/"), Path::new("/run/attempt"), "ok", false);
    assert_eq!(freed, value["freed"].as_u64().unwrap());
    assert!(left.is_empty() && value["left"].as_array().unwrap().is_empty());
    assert_eq!(
        fs.open_anchor(Path::new("/run/attempt/tmp")).is_ok(),
        value["exists"].as_bool().unwrap()
    );
}

#[test]
fn removal_refuses_parent_components_before_opening() {
    let (fs, _) = fixture();
    fs.nodes.borrow_mut()[1].children.insert("..".into(), 0);
    let victim = fs.add(0, "victim", true, 512);
    fs.add(victim, "keep", false, 512);
    let path = Path::new("/run/../victim");
    assert_eq!(remove_tree(&fs, Path::new("/run"), path), vec![path]);
    assert!(fs.child(&victim, OsStr::new("keep")).is_ok());
}
