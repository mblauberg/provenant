use super::*;
use crate::tests::{fixture_input, fixture_token, read_only_plan, route};
use std::path::Path;

#[test]
#[allow(clippy::disallowed_methods)]
fn reader_shells_create_large_here_documents_in_private_scratch(
) -> Result<(), Box<dyn std::error::Error>> {
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
