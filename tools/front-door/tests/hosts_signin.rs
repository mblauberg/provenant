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
    let rows = report["data"]["installed_sign_ins"]
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

#[test]
fn quota_route_and_doctor_share_account_readiness_fixture() -> Result {
    use provenant_core::Clock;
    use provenant_quota::{
        AccountRecord, AccountScope, LOCAL_HOST, ReadinessFile, ReadinessReason,
    };
    let mut rig = TestRig::new()?;
    // No provider executable is available, so these read-only commands cannot
    // contact a provider or inspect its credentials.
    let bin = rig.root.join("empty-bin");
    fs::create_dir(&bin)?;
    rig.env.retain(|(name, _)| name != "PATH");
    rig.env.push(("PATH".into(), bin.into_os_string()));
    let directory = rig.home.join("fixture-account");
    fs::create_dir(&directory)?;
    support::register_account(&rig, "codex", "work", "fixture-key", &directory)?;
    let account = AccountRecord::empty(AccountScope {
        adapter: "codex".into(),
        account_key: "fixture-key".into(),
        label: "work".into(),
    });
    let observed = provenant_os::clock::SystemClock::new().unix_seconds();
    let mut file = ReadinessFile::default();
    file.sync_probe_result(
        &account,
        LOCAL_HOST,
        observed,
        300,
        Some(ReadinessReason::SignedOut),
    )?;
    let dir = rig.state.join("readiness/fixture-key");
    fs::create_dir_all(&dir)?;
    for path in [rig.state.join("readiness"), dir.clone()] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join("codex.json");
    fs::write(&path, serde_json::to_vec(&file)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let read = |args: &[&str]| -> Result<serde_json::Value> {
        let out = rig.command(args)?;
        assert_eq!(code(&out), 0, "{out:?}");
        Ok(serde_json::from_slice(&out.stdout.bytes)?)
    };
    let quota = read(&["quota", "show", "--json"])?;
    let quota_row = quota["data"]["accounts"]
        .as_array()
        .ok_or("quota accounts")?
        .iter()
        .find(|row| row["account"]["account_key"] == "fixture-key")
        .ok_or("quota row")?;
    let doctor = read(&["hosts", "doctor", "--json"])?;
    let doctor_row = doctor["data"]["login_accounts"]
        .as_array()
        .ok_or("doctor accounts")?
        .iter()
        .find(|row| row["account_key"] == "fixture-key")
        .ok_or("doctor row")?;
    let route = read(&["route", "resolve", "codex/gpt-6-luna", "--json"])?;
    let route_row = route["data"]["recorded_readiness"]
        .as_array()
        .ok_or("route readiness")?
        .iter()
        .find(|row| row["account_key"] == "fixture-key")
        .ok_or("route row")?;
    for readiness in [
        &quota_row["readiness"],
        &doctor_row["readiness"],
        &route_row["observation"],
    ] {
        assert_eq!(readiness["reason"], "signed_out");
        assert_eq!(readiness["observed_unix_seconds"], observed);
        assert_eq!(readiness["observed_scope"]["account_key"], "fixture-key");
    }
    Ok(())
}
