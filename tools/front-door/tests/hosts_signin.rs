//! Doctor probes only disposable fake provider executables in this fixture.
mod support;

use std::{fs, os::unix::fs::PermissionsExt};
use support::{Result, TestRig, code};

#[test]
fn doctor_reports_installed_fake_provider_signin() -> Result {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    for (name, script) in [
        (
            "codex",
            "#!/bin/sh\nprintf 'signed in private-fixture\\n' >&2\nexit 0\n",
        ),
        (
            "cursor-agent",
            "#!/bin/sh\nprintf 'Not logged in\\n' >&2\nexit 1\n",
        ),
    ] {
        let path = bin.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    rig.env.retain(|(name, _)| name != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    let output = rig.command(&["hosts", "doctor", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    let rows = report["data"]["installed_signins"]
        .as_array()
        .ok_or("sign-in rows")?;
    for (adapter, state) in [("codex", "signed_in"), ("cursor", "signed_out")] {
        assert!(
            rows.iter()
                .any(|row| row["adapter"] == adapter && row["state"] == state),
            "{rows:?}"
        );
    }
    assert!(!String::from_utf8_lossy(&output.stdout.bytes).contains("private-fixture"));
    assert!(!String::from_utf8_lossy(&output.stderr.bytes).contains("private-fixture"));
    Ok(())
}
