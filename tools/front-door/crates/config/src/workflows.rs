//! Repository-owned Claude workflows and their ownership receipt.
use crate::{Result, invalid, io, pending};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

pub const MANIFEST_NAME: &str = ".agent-harness-workflows-installation.json";

pub fn valid_name(name: &str) -> bool {
    name.strip_suffix(".js").is_some_and(|stem| {
        let bytes = stem.as_bytes();
        !bytes.is_empty()
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub owner: String,
    pub source_target: String,
    pub source_sha256: String,
    // The legacy validator deliberately treats this as opaque.
    pub installed_at: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: Value,
    pub owner: String,
    pub surface: String,
    pub target_root: String,
    pub managed: BTreeMap<String, Entry>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn manifest_path(target: &Path) -> PathBuf {
    target
        .parent()
        .unwrap_or(Path::new("."))
        .join(MANIFEST_NAME)
}
fn sha(path: &Path) -> Result<String> {
    Ok(pending::hex(&Sha256::digest(
        fs::read(path).map_err(|e| io(path, e))?,
    )))
}

pub(crate) fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn load(target: &Path, now: &str) -> Result<Manifest> {
    let path = manifest_path(target);
    if !path.exists() {
        return Ok(Manifest {
            schema_version: Value::from(1),
            owner: "agent-harness".into(),
            surface: "claude-workflows".into(),
            target_root: pending::path_text(&crate::resolve(target)?)?,
            managed: BTreeMap::new(),
            extra: BTreeMap::from([("updated_at".into(), Value::from(now))]),
        });
    }
    let bytes = fs::read(&path).map_err(|source| crate::Error::Read {
        context: "installation manifest is unreadable",
        path: path.clone(),
        source,
    })?;
    let original = crate::ordered::decode(&bytes, "installation manifest is unreadable", &path)?;
    let value = original.value();
    let valid_header = value.is_object()
        && pending::schema_one(&value["schema_version"])
        && value["owner"] == "agent-harness"
        && value["surface"] == "claude-workflows"
        && value["target_root"].as_str()
            == Some(pending::path_text(&crate::resolve(target)?)?.as_str())
        && value["managed"].is_object();
    if !valid_header {
        return Err(invalid("workflow installation manifest is invalid"));
    }
    if let Some(entries) = value["managed"].as_object() {
        for (name, value) in crate::ordered::entries(entries, original.get("managed")) {
            if value.as_object().is_none_or(|v| {
                v.len() != 4
                    || !["owner", "source_target", "source_sha256", "installed_at"]
                        .iter()
                        .all(|k| v.contains_key(*k))
            }) {
                return Err(invalid(format!(
                    "workflow installation manifest entry is invalid: {name}"
                )));
            }
            let entry: Entry = serde_json::from_value(value.clone()).map_err(|_| {
                invalid(format!(
                    "workflow installation manifest entry is invalid: {name}"
                ))
            })?;
            if !valid_name(name)
                || entry.owner != "agent-harness"
                || !Path::new(&entry.source_target).is_absolute()
                || !valid_sha(&entry.source_sha256)
            {
                return Err(invalid(format!(
                    "workflow installation manifest entry is invalid: {name}"
                )));
            }
        }
    }
    serde_json::from_value(value).map_err(|_| invalid("workflow installation manifest is invalid"))
}

fn write(target: &Path, manifest: &mut Manifest, now: &str) -> Result<()> {
    manifest.extra.insert("updated_at".into(), now.into());
    crate::filesystem::atomic_write(
        &manifest_path(target),
        ".workflows-installation.",
        &pending::json_bytes(manifest)?,
    )
}

fn sources(source: &Path) -> Result<BTreeMap<String, PathBuf>> {
    if !source.is_dir() {
        return Err(invalid("source must be an existing workflow directory"));
    }
    let mut paths = BTreeMap::new();
    for entry in fs::read_dir(source).map_err(|e| io(source, e))? {
        let path = entry.map_err(|e| io(source, e))?.path();
        if !path
            .file_name()
            .is_some_and(|name| name.as_bytes().ends_with(b".js"))
        {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid("source contains an invalid workflow"))?;
        if !valid_name(name) || path.is_symlink() || !path.is_file() {
            return Err(invalid("source contains an invalid workflow"));
        }
        paths.insert(name.into(), crate::resolve(&path)?);
    }
    if paths.is_empty() {
        return Err(invalid("source contains an invalid workflow"));
    }
    Ok(paths)
}

fn same_link(destination: &Path, source: &Path) -> bool {
    destination.is_symlink()
        && crate::resolve(destination)
            .ok()
            .zip(crate::resolve(source).ok())
            .is_some_and(|(a, b)| a == b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Missing,
    Unmanaged,
    Managed,
    Stale,
    Conflicting,
}

fn state(destination: &Path, source: &Path, entry: Option<&Entry>) -> Result<State> {
    let Some(entry) = entry else {
        return Ok(if destination.exists() || destination.is_symlink() {
            State::Unmanaged
        } else {
            State::Missing
        });
    };
    if same_link(destination, source) {
        return Ok(
            if crate::resolve(Path::new(&entry.source_target))? == crate::resolve(source)?
                && entry.source_sha256 == sha(source)?
            {
                State::Managed
            } else {
                State::Stale
            },
        );
    }
    Ok(
        if same_link(destination, Path::new(&entry.source_target))
            || (!destination.exists() && !destination.is_symlink())
        {
            State::Stale
        } else {
            State::Conflicting
        },
    )
}

fn entry(source: &Path, now: &str) -> Result<Entry> {
    Ok(Entry {
        owner: "agent-harness".into(),
        source_target: pending::path_text(source)?,
        source_sha256: sha(source)?,
        installed_at: now.into(),
    })
}

pub fn canonical_directory_link(source: &Path, target: &Path) -> bool {
    target.is_symlink()
        && fs::canonicalize(target)
            .ok()
            .zip(fs::canonicalize(source).ok())
            .is_some_and(|(a, b)| a == b)
}

pub fn preflight(source: &Path, target: &Path, reject_unmanaged: bool, now: &str) -> Result<()> {
    if target.is_symlink() {
        if canonical_directory_link(source, target) {
            return Ok(());
        }
        return Err(invalid("target must not be a non-canonical symlink"));
    }
    let target = crate::resolve(target)?;
    let sources = sources(source)?;
    let manifest = load(&target, now)?;
    let mut conflicts = Vec::new();
    for (name, source) in &sources {
        let state = state(&target.join(name), source, manifest.managed.get(name))?;
        if state == State::Conflicting || (reject_unmanaged && state == State::Unmanaged) {
            conflicts.push(name.clone());
        }
    }
    for (name, entry) in &manifest.managed {
        let destination = target.join(name);
        if !sources.contains_key(name)
            && (destination.exists() || destination.is_symlink())
            && !same_link(&destination, Path::new(&entry.source_target))
        {
            conflicts.push(name.clone());
        }
    }
    if !conflicts.is_empty() {
        return Err(invalid(format!(
            "conflicting managed targets: {}",
            conflicts.join(", ")
        )));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub linked: usize,
    pub existing: i64,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Caller handles the canonical-directory-link fast path before invoking this.
pub fn install(source: &Path, target: &Path, now: &str) -> Result<Installed> {
    preflight(source, target, false, now)?;
    let target = crate::resolve(target)?;
    let sources = sources(source)?;
    let mut manifest = load(&target, now)?;
    let states = sources
        .iter()
        .map(|(name, source)| {
            Ok((
                name.clone(),
                state(&target.join(name), source, manifest.managed.get(name))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    fs::create_dir_all(&target).map_err(|e| io(&target, e))?;
    let mut newly_managed = false;
    for (name, state) in &states {
        if *state == State::Missing {
            manifest
                .managed
                .insert(name.clone(), entry(&sources[name], now)?);
            newly_managed = true;
        }
    }
    // Ownership is durable before publishing any new link: retry repairs gaps.
    if newly_managed {
        write(&target, &mut manifest, now)?;
    }
    let mut linked = 0;
    for (name, source) in &sources {
        if matches!(states[name], State::Missing | State::Stale) {
            crate::filesystem::replace_link(&target.join(name), source)?;
            manifest.managed.insert(name.clone(), entry(source, now)?);
            linked += 1;
        }
    }
    let retired = manifest
        .managed
        .keys()
        .filter(|name| !sources.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    for name in retired {
        let destination = target.join(&name);
        if destination.is_symlink() {
            fs::remove_file(&destination).map_err(|e| io(&destination, e))?;
        }
        manifest.managed.remove(&name);
        linked += 1;
    }
    write(&target, &mut manifest, now)?;
    let mut failures = vec![];
    for (name, source) in &sources {
        let dest = target.join(name);
        let integrity = if states[name] == State::Unmanaged {
            "unmanaged"
        } else if same_link(&dest, source) {
            "present"
        } else if !dest.exists() && !dest.is_symlink() {
            "missing"
        } else if dest.is_symlink() {
            "foreign"
        } else if sha(&dest).ok() == Some(sha(source)?) {
            "present"
        } else {
            "noncanonical"
        };
        if integrity != "present" {
            failures.push(format!("{name}={integrity}"));
        }
    }
    Ok(Installed {
        linked,
        existing: sources.len() as i64 - linked as i64,
        failures,
    })
}

/// Run the script's summary boundary with an explicit clock value.
pub fn run(source: &Path, target: &Path, preflight_only: bool, now: &str) -> CommandResult {
    if preflight_only {
        return match preflight(source, target, true, now) {
            Ok(()) => CommandResult {
                exit_code: 0,
                stdout: format!("workflows preflight=ok target={}\n", target.display()),
                stderr: String::new(),
            },
            Err(error) => CommandResult {
                exit_code: 3,
                stdout: String::new(),
                stderr: format!("conflicting: {error}\n"),
            },
        };
    }
    if canonical_directory_link(source, target) {
        return CommandResult {
            exit_code: 0,
            stdout: format!(
                "workflows existing=directory-link target={}\n",
                target.display()
            ),
            stderr: String::new(),
        };
    }
    match install(source, target, now) {
        Err(error) => CommandResult {
            exit_code: 3,
            stdout: String::new(),
            stderr: format!("conflicting: {error}\n"),
        },
        Ok(installed) => {
            let stdout = format!(
                "workflows linked={} existing={} target={}\n",
                installed.linked,
                installed.existing,
                target.display()
            );
            if installed.failures.is_empty() {
                CommandResult {
                    exit_code: 0,
                    stdout,
                    stderr: String::new(),
                }
            } else {
                CommandResult {
                    exit_code: 3,
                    stdout,
                    stderr: format!(
                        "conflicting: workflow installation integrity failed: {}\n",
                        installed.failures.join(", ")
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    const NOW: &str = "2026-10-23T00:00:00Z";
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("workflows");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("one.js"), "one").unwrap();
        fs::write(source.join("two.js"), "two").unwrap();
        let target = dir.path().join("claude/workflows");
        (dir, source, target)
    }
    #[test]
    fn installs_repairs_and_retires_owned_links() {
        let (_dir, source, target) = fixture();
        assert_eq!(install(&source, &target, NOW).unwrap().linked, 2);
        assert_eq!(install(&source, &target, NOW).unwrap().existing, 2);
        fs::remove_file(target.join("one.js")).unwrap();
        assert_eq!(install(&source, &target, NOW).unwrap().linked, 1);
        fs::remove_file(source.join("two.js")).unwrap();
        assert_eq!(install(&source, &target, NOW).unwrap().linked, 1);
        assert!(!target.join("two.js").is_symlink());
    }
    #[test]
    fn unmanaged_equivalent_files_and_links_are_not_adopted() {
        for link in [false, true] {
            let (_dir, source, target) = fixture();
            fs::create_dir_all(&target).unwrap();
            if link {
                symlink(source.join("one.js"), target.join("one.js")).unwrap();
            } else {
                fs::write(target.join("one.js"), "one").unwrap();
            }
            assert!(preflight(&source, &target, true, NOW).is_err());
            assert!(!manifest_path(&target).exists());
            let result = install(&source, &target, NOW).unwrap();
            assert_eq!(result.failures, ["one.js=unmanaged"]);
            assert!(!load(&target, NOW).unwrap().managed.contains_key("one.js"));
        }
    }
    #[test]
    fn foreign_broken_managed_link_refuses_all_mutation() {
        let (dir, source, target) = fixture();
        install(&source, &target, NOW).unwrap();
        let before = fs::read(manifest_path(&target)).unwrap();
        fs::remove_file(target.join("one.js")).unwrap();
        let foreign = dir.path().join("missing");
        symlink(&foreign, target.join("one.js")).unwrap();
        assert!(install(&source, &target, NOW).is_err());
        assert_eq!(fs::read_link(target.join("one.js")).unwrap(), foreign);
        assert_eq!(fs::read(manifest_path(&target)).unwrap(), before);
    }
    #[test]
    fn durable_ownership_repairs_interrupted_publication() {
        let (_dir, source, target) = fixture();
        fs::create_dir_all(&target).unwrap();
        let mut manifest = load(&target, NOW).unwrap();
        for (name, path) in sources(&source).unwrap() {
            manifest.managed.insert(name, entry(&path, NOW).unwrap());
        }
        write(&target, &mut manifest, NOW).unwrap();
        symlink(source.join("one.js"), target.join("one.js")).unwrap();
        assert_eq!(install(&source, &target, NOW).unwrap().linked, 1);
    }
    #[test]
    fn source_validation_and_canonical_directory_link() {
        let (dir, source, target) = fixture();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        symlink(&source, &target).unwrap();
        assert!(canonical_directory_link(&source, &target));
        preflight(&source, &target, true, NOW).unwrap();
        assert!(!manifest_path(&target).exists());
        symlink(dir.path().join("missing"), source.join("bad.js")).unwrap();
        assert!(sources(&source).is_err());
    }
    #[test]
    fn dot_js_is_an_invalid_candidate_rather_than_ignored() {
        let (_dir, source, target) = fixture();
        fs::write(source.join(".js"), "invalid").unwrap();
        assert!(preflight(&source, &target, true, NOW).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn relocation_updates_only_the_recorded_link() {
        let (dir, source, target) = fixture();
        install(&source, &target, NOW).unwrap();
        let next = dir.path().join("next");
        fs::rename(&source, &next).unwrap();
        assert_eq!(install(&next, &target, NOW).unwrap().linked, 2);
        assert_eq!(
            fs::read_link(target.join("one.js")).unwrap(),
            next.canonicalize().unwrap().join("one.js")
        );
    }
    #[test]
    fn python_differential_install_and_receipt() {
        let (_dir, source, target) = fixture();
        let result = install(&source, &target, NOW).unwrap();
        let Some(output) = crate::oracle::python(
            "scripts/install-workflows",
            "import json\nm.SOURCE=p.parent/'workflows'\nm._now=lambda: '2026-10-23T00:00:00Z'\nt=p.parent/'python'/'workflows'\nr=m.install(t)\nd=m._load_manifest(t)\nd['target_root']='TARGET'\nprint(json.dumps([r,d]))",
            &source,
        ) else {
            return;
        };
        let old: Value = serde_json::from_slice(&output).unwrap();
        let mut new = serde_json::to_value(load(&target, NOW).unwrap()).unwrap();
        new["target_root"] = "TARGET".into();
        assert_eq!(
            old,
            serde_json::json!([[result.linked, result.existing, result.failures], new])
        );
    }

    #[test]
    fn conflicting_target_refusal_matches_python() {
        let (_dir, source, target) = fixture();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("one.js"), "user-owned").unwrap();
        let Some(output) = crate::oracle::python(
            "scripts/install-workflows",
            "m.SOURCE=p.parents[1]/'workflows'\nt=p\ntry: m._preflight(t,reject_unmanaged=True)\nexcept m.InstallError as e: print(str(e))",
            &target,
        ) else {
            return;
        };
        assert_eq!(
            preflight(&source, &target, true, NOW)
                .unwrap_err()
                .to_string()
                .as_bytes(),
            output.strip_suffix(b"\n").unwrap_or(&output)
        );
    }

    #[test]
    fn conflicting_preflight_command_matches_python() {
        let (_dir, source, target) = fixture();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("one.js"), "user-owned").unwrap();
        let Some(output) = crate::oracle::python(
            "scripts/install-workflows",
            "import contextlib,io,json\nm.SOURCE=p.parents[1]/'workflows'\nout=io.StringIO();err=io.StringIO()\nwith contextlib.redirect_stdout(out),contextlib.redirect_stderr(err): code=m.main(['--target',str(p),'--preflight'])\nprint(json.dumps([code,out.getvalue(),err.getvalue()]))",
            &target,
        ) else {
            return;
        };
        let old: Value = serde_json::from_slice(&output).unwrap();
        let result = run(&source, &target, true, NOW);
        assert_eq!(
            old,
            serde_json::json!([result.exit_code, result.stdout, result.stderr])
        );
    }

    #[test]
    fn command_summary_exit_code_and_streams_match_python() {
        for preflight_only in [true, false] {
            let (_dir, source, target) = fixture();
            let Some(output) = crate::oracle::python(
                "scripts/install-workflows",
                &format!(
                    "import contextlib,io,json\nm.SOURCE=p.parents[1]/'workflows'\nm._now=lambda: {NOW:?}\nout=io.StringIO();err=io.StringIO()\nargs=['--target',str(p)] + ({:?} if {} == 1 else [])\nwith contextlib.redirect_stdout(out),contextlib.redirect_stderr(err): code=m.main(args)\nprint(json.dumps([code,out.getvalue(),err.getvalue()]))",
                    vec!["--preflight"],
                    u8::from(preflight_only)
                ),
                &target,
            ) else {
                return;
            };
            let old: serde_json::Value = serde_json::from_slice(&output).unwrap();
            let _ = fs::remove_dir_all(&target);
            let _ = fs::remove_file(manifest_path(&target));
            let result = run(&source, &target, preflight_only, NOW);
            assert_eq!(
                old,
                serde_json::json!([result.exit_code, result.stdout, result.stderr])
            );
        }
    }
}

#[derive(Debug, clap::Parser)]
#[command(name = "install-workflows")]
pub struct Arguments {
    #[arg(long)]
    pub target: PathBuf,
    #[arg(long)]
    pub preflight: bool,
}
impl Arguments {
    pub fn run(&self, source: &Path, now: &str) -> CommandResult {
        run(source, &self.target, self.preflight, now)
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    #[test]
    fn invalid_receipt_uses_first_inserted_entry_not_alphabetical_order() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("workflows");
        let value = format!(
            "{{\"schema_version\":1,\"owner\":\"agent-harness\",\"surface\":\"claude-workflows\",\"target_root\":{},\"managed\":{{\"z.js\":{{}},\"a.js\":{{}}}}}}",
            serde_json::to_string(&target).unwrap()
        );
        fs::write(manifest_path(&target), value).unwrap();
        let Some(old) = crate::oracle::python(
            "scripts/install-workflows",
            "try: m._load_manifest(p)\nexcept m.InstallError as e: print(str(e))",
            &target,
        ) else {
            return;
        };
        assert_eq!(
            format!("{}\n", load(&target, "NOW").unwrap_err()).as_bytes(),
            old
        );
    }
}
