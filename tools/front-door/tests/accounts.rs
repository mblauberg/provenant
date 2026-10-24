mod support;
use std::{fs, os::unix::fs::PermissionsExt};
use support::{Result, TestRig, code};

#[test]
fn register_writes_typed_quota_options_to_state_config() -> Result {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let claude = bin.join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\":true,\"email\":\"fixture@example.test\"}'\n",
    )?;
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o700))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let directory = rig.home.join(".claude-work");
    fs::create_dir(&directory)?;
    let output = rig.command(&[
        "accounts",
        "register",
        "work",
        "--adapter",
        "claude",
        "--directory",
        directory.to_str().ok_or("directory path")?,
        "--plan",
        "pro",
        "--priority",
        "12",
        "--billing-anchor",
        "2026-01-01",
        "--json",
    ])?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let config = fs::read_to_string(rig.state.join("config.toml"))?;
    assert!(config.contains("plan = \"pro\""));
    assert!(config.contains("priority = 12"));
    assert!(config.contains("billing_anchor = \"2026-01-01\""));
    assert!(config.contains("directory = "));

    support::register_account(&rig, "opencode", "local", "opencode-local", &rig.root)?;
    let config_path = rig.state.join("config.toml");
    let mut doc: toml_edit::DocumentMut = fs::read_to_string(&config_path)?.parse()?;
    let limits = doc["domains"]["accounts"]["entries"]["opencode-local"]
        .as_table_mut()
        .ok_or("OpenCode registration")?;
    let mut values = toml_edit::Table::new();
    values["monthly_usd"] = toml_edit::value(32.0);
    values["cycle_anchor"] = toml_edit::value("2026-01-01T00:00:00Z");
    values["weekly_anchor"] = toml_edit::value("2026-01-05T00:00:00Z");
    limits["limits"] = toml_edit::Item::Table(values);
    fs::write(config_path, doc.to_string())?;
    let listed = rig.command(&["accounts", "list", "--json"])?;
    assert_eq!(
        code(&listed),
        0,
        "{}",
        String::from_utf8_lossy(&listed.stderr.bytes)
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout.bytes)?;
    assert_eq!(
        listed["data"]["entries"]["opencode-local"]["limits"]["monthly_usd"],
        32.0
    );
    Ok(())
}
