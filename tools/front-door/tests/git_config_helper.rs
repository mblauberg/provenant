//! Exercise the private hooks-path protocol through the installed binary.
mod support;

use crate::support::{Result, TestRig, code};
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::Path};

const HELPER_ARG: &str = "--__provenant_internal_git_hooks_path";

fn set_env(rig: &mut TestRig, name: &str, value: impl Into<OsString>) {
    if let Some((_, current)) = rig.env.iter_mut().find(|(key, _)| key == name) {
        *current = value.into();
    } else {
        rig.env.push((name.into(), value.into()));
    }
}

fn helper(rig: &TestRig, include: &str) -> Result<provenant_os::exec::ExecOutcome> {
    rig.execute(
        Path::new(env!("CARGO_BIN_EXE_provenant")),
        &[HELPER_ARG, include],
        None,
    )
}

#[test]
fn hidden_hooks_path_helper_reads_config_include_and_failures() -> Result {
    let mut rig = TestRig::new()?;
    let git =
        provenant_os::git::resolve_git_executable(None).ok_or("Git executable unavailable")?;
    // Clear fixture command-scope settings so the test controls the effective
    // config view through the repository and the helper's explicit include.
    rig.env
        .retain(|(name, _)| !name.to_string_lossy().starts_with("GIT_"));
    set_env(&mut rig, "PROVENANT_GIT", git.as_os_str().to_owned());
    set_env(&mut rig, "GIT_CONFIG_GLOBAL", "/dev/null");
    set_env(&mut rig, "GIT_CONFIG_NOSYSTEM", "1");

    let configured = rig.root.join("configured hooks");
    let overridden = rig.root.join("override hooks");
    fs::create_dir_all(&configured)?;
    fs::create_dir_all(&overridden)?;
    let set = rig.execute_os(
        &git,
        &[
            "config".into(),
            "--local".into(),
            "core.hooksPath".into(),
            configured.as_os_str().to_owned(),
        ],
        None,
    )?;
    assert_eq!(code(&set), 0);

    let configured_result = helper(&rig, "")?;
    assert_eq!(code(&configured_result), 0);
    let mut configured_line = configured.as_os_str().as_encoded_bytes().to_vec();
    configured_line.push(b'\n');
    assert_eq!(configured_result.stdout.bytes, configured_line);

    let include = format!(
        "core.hooksPath={}",
        overridden.to_str().ok_or("override path")?
    );
    let override_result = helper(&rig, &include)?;
    assert_eq!(code(&override_result), 0);
    let mut override_line = overridden.as_os_str().as_encoded_bytes().to_vec();
    override_line.push(b'\n');
    assert_eq!(override_result.stdout.bytes, override_line);

    let unset = rig.execute_os(
        &git,
        &[
            "config".into(),
            "--local".into(),
            "--unset-all".into(),
            "core.hooksPath".into(),
        ],
        None,
    )?;
    assert_eq!(code(&unset), 0);
    let missing = helper(&rig, "")?;
    assert_eq!(code(&missing), 1);
    assert!(missing.stdout.bytes.is_empty());
    assert!(missing.stderr.bytes.is_empty());

    let invalid_global = rig.root.join("invalid-global-config");
    fs::write(&invalid_global, b"not a valid Git config\n")?;
    set_env(
        &mut rig,
        "GIT_CONFIG_GLOBAL",
        invalid_global.as_os_str().to_owned(),
    );
    let invalid_config = helper(&rig, "")?;
    assert_eq!(code(&invalid_config), 2);
    assert!(invalid_config.stdout.bytes.is_empty());
    assert!(
        String::from_utf8_lossy(&invalid_config.stderr.bytes)
            .contains("Git configuration could not be read")
    );

    let failed_git = rig.root.join("failed-git");
    fs::write(&failed_git, "#!/bin/sh\nexit 88\n")?;
    fs::set_permissions(&failed_git, fs::Permissions::from_mode(0o700))?;
    set_env(&mut rig, "PROVENANT_GIT", failed_git);
    let failed = helper(&rig, "")?;
    assert_eq!(code(&failed), 2);
    assert!(failed.stdout.bytes.is_empty());
    assert!(
        String::from_utf8_lossy(&failed.stderr.bytes)
            .contains("Git configuration could not be read")
    );
    Ok(())
}
