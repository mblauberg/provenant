//! Installation-check interface. Config and binary inspection remain with their owners.
use crate::CommandOutput;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "check-provenant-install", args_override_self = true)]
pub struct Arguments {
    /// fail on routing or provider warnings
    #[arg(long)]
    pub strict: bool,
}

pub const PROVIDERS: [&str; 6] = ["claude", "codex", "opencode", "agy", "cursor", "kiro"];

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub present: bool,
    pub skills: bool,
    pub registered: bool,
}

/// Ordered validation failure, selected by the installation owner before rendering.
#[derive(Debug, Error)]
pub enum Failure {
    #[error("Agent Fabric instance root must be absolute")]
    RelativeInstance,
    #[error("product-root pointer is invalid or stale at {0}")]
    InvalidPointer(PathBuf),
    #[error("PROVENANT_BIN_DIR must be absolute")]
    RelativeBin,
    #[error("{0} must be a regular managed copy; re-run install-harness")]
    Unmanaged(PathBuf),
    #[error("{0} is not executable; re-run install-harness")]
    NotExecutable(PathBuf),
    #[error("installed front door has no Rust source identity; re-run install-harness")]
    MissingIdentity,
    #[error("installed front door was built from different sources than tools/front-door (embeds {stamped}, checkout is {}); rebuild: re-run install-harness", current.as_deref().unwrap_or("None"))]
    StaleIdentity {
        stamped: String,
        current: Option<String>,
    },
}

#[derive(Debug)]
pub enum Status {
    NotOwned,
    Failed(Failure),
    Installed {
        command: PathBuf,
        providers: [ProviderStatus; 6],
        routing: Result<Vec<String>, String>,
    },
}

pub fn render(status: &Status, strict: bool, advisories: &[String]) -> CommandOutput {
    let mut out = CommandOutput::default();
    for line in advisories {
        out.stderr.push_str(line);
        out.stderr.push('\n');
    }
    match status {
        Status::NotOwned => out
            .stdout
            .push_str("provenant installed stub=not-owned-by-this-checkout\n"),
        Status::Failed(error) => {
            out.stderr.push_str(&format!("FAIL: {error}\n"));
            out.exit_code = 1;
        }
        Status::Installed {
            command,
            providers,
            routing,
        } => {
            out.stdout.push_str(&format!(
                "provenant installed stub=ok path={}\nfront-door=rust\n",
                command.display()
            ));
            let mut missing = false;
            for (name, state) in PROVIDERS.iter().zip(providers) {
                if !state.present {
                    out.stdout
                        .push_str(&format!("provider {name} present=no\n"));
                    continue;
                }
                let complete = state.skills && state.registered;
                missing |= !complete;
                let skills = if state.skills { "ok" } else { "missing" };
                let mcp = if state.registered { "ok" } else { "missing" };
                let repair = if complete {
                    ""
                } else {
                    " repair=install-harness --platform all"
                };
                out.stdout.push_str(&format!(
                    "provider {name} present=yes skills={skills} mcp={mcp}{repair}\n"
                ));
            }
            let unreadable = vec!["unreadable".to_owned()];
            let differences = match routing {
                Ok(keys) => keys,
                Err(error) => {
                    out.stderr.push_str(&format!("warning: routing catalogue {error}; repair=install-harness --platform all --refresh-routing\n"));
                    &unreadable
                }
            };
            for key in differences {
                out.stderr.push_str(&format!(
                    "routing drift={key} repair=install-harness --platform all --refresh-routing\n"
                ));
            }
            out.exit_code = u8::from(strict && (missing || !differences.is_empty()));
        }
    }
    out
}

/// Byte boundary used by the executable edge. Unix Python stdout/stderr use
/// surrogateescape for filesystem paths, so a path is never formatted via
/// `Path::display()` at this boundary.
pub fn render_bytes(status: &Status, strict: bool, advisories: &[String]) -> crate::CommandBytes {
    use std::os::unix::ffi::OsStrExt;
    let text = render(status, strict, advisories);
    let mut result = crate::CommandBytes::from(text);
    match status {
        Status::Installed { command, .. } => {
            let suffix = result
                .stdout
                .iter()
                .position(|&b| b == b'\n')
                .map_or(result.stdout.len(), |i| i + 1);
            let mut stdout = b"provenant installed stub=ok path=".to_vec();
            stdout.extend_from_slice(command.as_os_str().as_bytes());
            stdout.push(b'\n');
            stdout.extend_from_slice(&result.stdout[suffix..]);
            result.stdout = stdout;
        }
        Status::Failed(error) => {
            let path_and_text = match error {
                Failure::InvalidPointer(path) => Some((
                    b"product-root pointer is invalid or stale at ".as_slice(),
                    path,
                    b"".as_slice(),
                )),
                Failure::Unmanaged(path) => Some((
                    b"".as_slice(),
                    path,
                    b" must be a regular managed copy; re-run install-harness".as_slice(),
                )),
                Failure::NotExecutable(path) => Some((
                    b"".as_slice(),
                    path,
                    b" is not executable; re-run install-harness".as_slice(),
                )),
                _ => None,
            };
            if let Some((prefix, path, suffix)) = path_and_text {
                result.stderr = advisories
                    .iter()
                    .flat_map(|s| s.bytes().chain([b'\n']))
                    .collect();
                result.stderr.extend_from_slice(b"FAIL: ");
                result.stderr.extend_from_slice(prefix);
                result.stderr.extend_from_slice(path.as_os_str().as_bytes());
                result.stderr.extend_from_slice(suffix);
                result.stderr.push(b'\n');
            }
        }
        Status::NotOwned => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_checkout_identity_renders_python_none() {
        let out = render(
            &Status::Failed(Failure::StaleIdentity {
                stamped: "abc".into(),
                current: None,
            }),
            false,
            &[],
        );
        assert!(out.stderr.contains("checkout is None)"));
    }

    #[test]
    fn installation_failure_streams_and_provider_rows_match_source() {
        use serde_json::json;
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../scripts")
            .canonicalize()
            .unwrap();
        let scripts = dir.path().join("scripts");
        fs::create_dir(&scripts).unwrap();
        fs::copy(
            source.join("check-provenant-install.py"),
            scripts.join("check-provenant-install.py"),
        )
        .unwrap();
        std::os::unix::fs::symlink(source.join("lib"), scripts.join("lib")).unwrap();
        std::os::unix::fs::symlink(
            source.join("instance_installation.py"),
            scripts.join("instance_installation.py"),
        )
        .unwrap();
        let instance = dir.path().join("instance");
        let pointer = instance.join(".agent-fabric/product-root.json");
        fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        let Some(expected): Option<Vec<(String, String, u8)>> = crate::oracle::python(
            r#"
import json,sys,os,subprocess
from pathlib import Path
x=json.load(sys.stdin); root=Path(x['root']); p=root/'scripts/check-provenant-install.py'; instance=root/'instance'
base={'PATH':os.environ.get('PATH',''),'HOME':str(root/'home'),'AGENT_FABRIC_INSTANCE_ROOT':str(instance),'PROVENANT_STALE_CHECKED':'1'}
results=[]
for mode in ['relative','invalid','unowned','binrelative','unmanaged','notexec','unstamped','stale','ok','warn','warn_strict']:
    env=dict(base)
    if mode=='relative': env['AGENT_FABRIC_INSTANCE_ROOT']='relative'
    pointer=instance/'.agent-fabric/product-root.json'
    if mode=='invalid': pointer.write_text('not json')
    if mode=='unowned': pointer.unlink()
    env['PROVENANT_BIN_DIR']=str(root/'bin')
    if mode not in {'relative','invalid','unowned'}: pointer.write_text(json.dumps({'schema_version':1,'product_root':str(root)}))
    if mode=='binrelative': env['PROVENANT_BIN_DIR']='relative'
    command=root/'bin/provenant'
    if mode in {'notexec','unstamped','stale'}:
        command.parent.mkdir(exist_ok=True); command.write_text('provenant-front-door source-sha256='+'0'*64+'\n' if mode=='stale' else 'no identity\n'); command.chmod(0o644 if mode=='notexec' else 0o755)
    if mode=='ok':
        from scripts.lib.front_door_build import source_digest
        crate=root/'tools/front-door';(crate/'src').mkdir(parents=True);(crate/'Cargo.toml').write_text('[package]\nname="fixture"\n');(crate/'src/main.rs').write_text('fn main() {}\n')
        command.write_text('provenant-front-door source-sha256='+source_digest(crate)+'\n')
    if mode=='warn': (root/'home/.codex').mkdir(parents=True)
    code='import sys,runpy; sys.path.insert(0,sys.argv[1]); script=sys.argv[2]; sys.argv=[script,*sys.argv[3:]]; runpy.run_path(script,run_name="__main__")' 
    r=subprocess.run([sys.executable,'-I','-c',code,str(p.parent),str(p),*(['--strict'] if mode=='warn_strict' else [])],env=env,capture_output=True,text=True,timeout=10)
    results.append((r.stdout,r.stderr,r.returncode))
print(json.dumps(results))
"#,
            &json!({"root":dir.path()}),
        ) else {
            return;
        };
        let statuses = [
            Status::Failed(Failure::RelativeInstance),
            Status::Failed(Failure::InvalidPointer(pointer)),
            Status::NotOwned,
            Status::Failed(Failure::RelativeBin),
            Status::Failed(Failure::Unmanaged(dir.path().join("bin/provenant"))),
            Status::Failed(Failure::NotExecutable(dir.path().join("bin/provenant"))),
            Status::Failed(Failure::MissingIdentity),
            Status::Failed(Failure::StaleIdentity {
                stamped: "0".repeat(64),
                current: None,
            }),
            Status::Installed {
                command: dir.path().join("bin/provenant"),
                providers: [ProviderStatus::default(); 6],
                routing: Ok(vec![]),
            },
            Status::Installed {
                command: dir.path().join("bin/provenant"),
                providers: std::array::from_fn(|i| ProviderStatus {
                    present: i == 1,
                    ..ProviderStatus::default()
                }),
                routing: Ok(vec![]),
            },
            Status::Installed {
                command: dir.path().join("bin/provenant"),
                providers: std::array::from_fn(|i| ProviderStatus {
                    present: i == 1,
                    ..ProviderStatus::default()
                }),
                routing: Ok(vec![]),
            },
        ];
        assert_eq!(statuses.len(), expected.len());
        for (index, (status, (stdout, stderr, exit_code))) in
            statuses.iter().zip(expected).enumerate()
        {
            assert_eq!(
                render(status, index == 10, &[]),
                CommandOutput {
                    stdout,
                    stderr,
                    exit_code
                }
            );
        }
        let Some(expected): Option<(Vec<String>, bool)> = crate::oracle::python(
            r#"
import json,sys,importlib.util
from pathlib import Path
sys.path.insert(0,str(Path(sys.argv[1])/'scripts'))
spec=importlib.util.spec_from_file_location('install_oracle',Path(sys.argv[1])/'scripts/check-provenant-install.py'); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
x=json.load(sys.stdin); print(json.dumps(m._provider_lines(Path(x['home']))))
"#,
            &json!({"home":dir.path().join("home")}),
        ) else {
            return;
        };
        assert!(expected.1);
        let output = render(
            &Status::Installed {
                command: "/bin/provenant".into(),
                providers: std::array::from_fn(|i| ProviderStatus {
                    present: i == 1,
                    ..ProviderStatus::default()
                }),
                routing: Ok(vec![]),
            },
            false,
            &[],
        );
        assert_eq!(
            output.stdout.lines().skip(2).collect::<Vec<_>>(),
            expected.0
        );
    }

    #[test]
    fn ownership_and_failures_have_stable_exit_and_streams() {
        assert_eq!(
            render(&Status::NotOwned, true, &[]),
            CommandOutput {
                stdout: "provenant installed stub=not-owned-by-this-checkout\n".into(),
                ..Default::default()
            }
        );
        let out = render(
            &Status::Failed(Failure::InvalidPointer("/p".into())),
            false,
            &[],
        );
        assert_eq!(out.exit_code, 1);
        assert_eq!(
            out.stderr,
            "FAIL: product-root pointer is invalid or stale at /p\n"
        );
    }

    #[test]
    fn strict_only_fails_present_incomplete_providers_or_routing_drift() {
        let mut providers = [ProviderStatus::default(); 6];
        providers[3].registered = true;
        let status = |providers, routing| Status::Installed {
            command: "/bin/provenant".into(),
            providers,
            routing,
        };
        assert_eq!(
            render(&status(providers, Ok(vec![])), true, &[]).exit_code,
            0
        );
        providers[2].present = true;
        let value = status(providers, Ok(vec![]));
        assert_eq!(render(&value, false, &[]).exit_code, 0);
        let out = render(&value, true, &[]);
        assert_eq!(out.exit_code, 1);
        assert!(out.stdout.contains("provider opencode present=yes skills=missing mcp=missing repair=install-harness --platform all\n"));
        assert!(out.stdout.contains("provider agy present=no\n"));
        let out = render(
            &status([ProviderStatus::default(); 6], Err("bad data".into())),
            true,
            &[],
        );
        assert_eq!(out.exit_code, 1);
        assert_eq!(out.stderr, "warning: routing catalogue bad data; repair=install-harness --platform all --refresh-routing\nrouting drift=unreadable repair=install-harness --platform all --refresh-routing\n");
    }

    #[test]
    fn flags_are_structural() {
        assert!(
            Arguments::try_parse_from(["check", "--strict"])
                .unwrap()
                .strict
        );
        assert!(Arguments::try_parse_from(["check", "--unknown"]).is_err());
        assert!(
            Arguments::try_parse_from(["check", "--strict", "--strict"])
                .unwrap()
                .strict
        );
    }
    #[test]
    fn filesystem_paths_preserve_python_surrogateescape_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/bad\xff\nname".to_vec()));
        for (mode, status) in [
            (
                "pointer",
                Status::Failed(Failure::InvalidPointer(path.clone())),
            ),
            (
                "unmanaged",
                Status::Failed(Failure::Unmanaged(path.clone())),
            ),
            (
                "executable",
                Status::Failed(Failure::NotExecutable(path.clone())),
            ),
            (
                "installed",
                Status::Installed {
                    command: path.clone(),
                    providers: [ProviderStatus::default(); 6],
                    routing: Ok(vec![]),
                },
            ),
        ] {
            let Some(expected): Option<Vec<u8>> = crate::oracle::python(
                "import os,json,sys; x=json.load(sys.stdin); p=os.fsdecode(bytes(x['path'])); messages={'pointer':f'FAIL: product-root pointer is invalid or stale at {p}\\n','unmanaged':f'FAIL: {p} must be a regular managed copy; re-run install-harness\\n','executable':f'FAIL: {p} is not executable; re-run install-harness\\n','installed':f'provenant installed stub=ok path={p}\\n'}; print(json.dumps(list(messages[x['mode']].encode('utf-8','surrogateescape'))))",
                &serde_json::json!({"path": b"/tmp/bad\xff\nname".to_vec(), "mode":mode}),
            ) else { return; };
            let output = render_bytes(&status, false, &[]);
            if mode == "installed" {
                assert!(output.stdout.starts_with(&expected));
            } else {
                assert_eq!(output.stderr, expected);
            }
        }
    }
}
