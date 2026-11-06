use super::*;
use crate::tests::{fixture_input, fixture_token, read_only_plan, route};
use std::path::Path;

#[test]
#[allow(clippy::disallowed_methods)]
fn reader_shells_create_large_here_documents_in_private_scratch()
-> Result<(), Box<dyn std::error::Error>> {
    for id in ["claude", "codex"] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        for dir in ["scratch", "state", "repo", "home"] {
            std::fs::create_dir(root.join(dir))?;
        }
        let plan = read_only_plan(
            root.to_str().ok_or("root path")?,
            (id == "codex").then_some("login"),
        );
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        let prompt = Prompt::new("fixture".into());
        let token = fixture_token();
        let cwd = root.join("repo");
        let home = root.join("home");
        let mut input = fixture_input(
            &route,
            &plan,
            &prompt,
            &cwd,
            (id == "claude").then_some(&token),
        );
        input.home = &home;
        let spec = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .unwrap()
            .adapter
            .prepare(input)?;
        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            &home,
        )?;
        assert!(!profile.contains(&format!(
            "(allow file-write* (subpath \"{}\"))",
            cwd.display()
        )));
        // Above the shells' pipe threshold, forcing their temporary-file path.
        let data = "x".repeat(32 * 1024);
        let script = format!("/usr/bin/wc -c <<'END'\n{data}\nEND\n");
        for shell in ["/bin/bash", "/bin/zsh"] {
            if !Path::new(shell).exists() {
                continue;
            }
            let mut command = if cfg!(target_os = "macos") {
                let mut command = std::process::Command::new("/usr/bin/sandbox-exec");
                command.args(["-p", &profile, shell]);
                command
            } else {
                std::process::Command::new(shell)
            };
            let output = command
                .args(["-c", &script])
                .current_dir(&cwd)
                .env_clear()
                .envs(spec.env.iter().cloned())
                .output()?;
            assert!(
                output.status.success(),
                "{id} {shell}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout)?.trim(),
                (data.len() + 1).to_string()
            );
            #[cfg(target_os = "macos")]
            {
                let output = std::process::Command::new("/usr/bin/sandbox-exec")
                    .args([
                        "-p",
                        &profile,
                        shell,
                        "-c",
                        "printf forbidden > repository-file",
                    ])
                    .current_dir(&cwd)
                    .env_clear()
                    .envs(spec.env.iter().cloned())
                    .output()?;
                assert!(
                    !output.status.success(),
                    "repository writes must remain denied"
                );
                assert!(!cwd.join("repository-file").exists());
            }
        }
    }
    Ok(())
}

#[test]
fn codex_reader_keeps_only_explicit_environment_names_in_shells() -> Result<(), ProviderError> {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = crate::tests::fixture_network_plan(
        crate::Access::ReadOnly,
        Some("login"),
        provenant_confinement::permissions::NetworkAccess::Open,
    );
    let prompt = Prompt::new("fixture".into());
    let spec = crate::codex::Codex.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    assert!(
        spec.argv
            .iter()
            .any(|arg| arg == "shell_environment_policy.ignore_default_excludes=true")
    );
    let names = spec
        .argv
        .iter()
        .find_map(|arg| {
            arg.to_str()?
                .strip_prefix("shell_environment_policy.include_only=")
        })
        .expect("explicit shell names");
    let names: Vec<String> = serde_json::from_str(names).unwrap();
    assert!(names.iter().any(|name| name == "GH_TOKEN"));
    assert!(names.iter().any(|name| name == "PATH"));
    assert!(!names.iter().any(|name| name == "AWS_SECRET_ACCESS_KEY"));
    assert!(
        names
            .iter()
            .all(|name| name == "GH_TOKEN" || spec.env.iter().any(|(key, _)| key == name.as_str()))
    );
    Ok(())
}
