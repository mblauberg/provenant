use super::*;
use provenant_config::{REPOSITORY_CONFIG, RepositoryPolicy};

/// Classify once on the admitting host, before local/peer routing. Replays
/// retain the sealed classification even if files or repository policy change.
pub fn seal_launch_policy(
    attempt: &mut provenant_fabric_runtime::graph::AttemptSpec,
    cwd: &Path,
    prompt: &str,
    access: &run::LaneAccess,
    base: Option<&str>,
    head: Option<&str>,
) -> Result<(), Fail> {
    let checkout =
        provenant_os::git::repository_path(cwd, provenant_os::git::RepositoryPath::Toplevel)
            .unwrap_or_else(|_| cwd.to_path_buf());
    if let Some(sealed) = &attempt.launch_policy
        && attempt.policy_sealed
    {
        attempt.confidential |= sealed.confidential;
        if sealed.repository_config.is_some() && !checkout.join(REPOSITORY_CONFIG).is_file() {
            policy_warning(
                "recorded repository launch policy is missing; retaining sealed classification",
            );
        }
        return Ok(());
    }
    let policy =
        RepositoryPolicy::read(&checkout).map_err(|error| Fail::config(Status::Invalid, error))?;
    let mut sealed = attempt.launch_policy.clone().unwrap_or_default();
    if attempt.confidential && !sealed.confidential {
        sealed.confidential = true;
        sealed.sources.push("attempt.confidential".into());
    }
    if let Some(policy) = policy {
        sealed.repository_config = Some(REPOSITORY_CONFIG.into());
        if policy.confidential {
            sealed.confidential = true;
            sealed
                .sources
                .push(format!("{REPOSITORY_CONFIG} [launch_policy].confidential"));
        }
        let mut paths = attempt
            .policy_inputs
            .iter()
            .chain(access.attachments.iter())
            .chain(access.read_roots.iter());
        if paths.any(|path| policy.protects(&checkout, cwd, path))
            || policy.protects_brief(&checkout, cwd, prompt)
        {
            sealed.confidential = true;
            sealed.sources.push(format!(
                "{REPOSITORY_CONFIG} [launch_policy].protected_paths: inputs/brief"
            ));
        }
        if !policy.protected_paths.is_empty() {
            match diff_paths(&checkout, base, head) {
                Some(paths)
                    if paths
                        .iter()
                        .any(|path| policy.protects(&checkout, &checkout, Path::new(path))) =>
                {
                    sealed.confidential = true;
                    sealed.sources.push(format!(
                        "{REPOSITORY_CONFIG} [launch_policy].protected_paths: diff"
                    ));
                }
                None => {
                    // Incomplete observations may narrow model treatment but
                    // cannot silently declare protected data safe to train on.
                    sealed.confidential = true;
                    sealed
                        .sources
                        .push(format!("{REPOSITORY_CONFIG}: diff observation incomplete"));
                }
                Some(_) => {}
            }
        }
    }
    if sealed.sources.is_empty() {
        sealed
            .sources
            .push("default: no repository confidentiality declaration".into());
    }
    sealed.sources.sort();
    sealed.sources.dedup();
    attempt.confidential |= sealed.confidential;
    attempt.launch_policy = Some(sealed);
    attempt.policy_sealed = true;
    Ok(())
}

fn policy_warning(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
}

/// Observe the reviewed range, staged/unstaged edits and untracked paths.
/// Renames expose both sides, so moving a protected file cannot declassify it.
fn diff_paths(checkout: &Path, base: Option<&str>, head: Option<&str>) -> Option<Vec<String>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let git = provenant_os::git::git_executable()?;
    if base
        .into_iter()
        .chain(head)
        .any(|revision| revision.starts_with('-'))
    {
        return None;
    }
    let environment = [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .map(|(key, value)| (key.into(), value.into()));
    let mut commands = vec![
        vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-only",
            "-z",
            "HEAD",
            "--",
        ],
        vec!["ls-files", "--others", "--exclude-standard", "-z", "--"],
    ];
    if let Some(base) = base {
        commands.push(vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--name-only",
            "-z",
            base,
            head.unwrap_or("HEAD"),
            "--",
        ]);
    }
    let mut paths = Vec::new();
    for args in commands {
        let argv = [
            git.as_os_str().to_owned(),
            "--no-pager".into(),
            "--no-optional-locks".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
        ]
        .into_iter()
        .chain(args.into_iter().map(std::ffi::OsString::from))
        .collect::<Vec<_>>();
        let mut options = ExecOptions::new(&argv, checkout, Duration::from_secs(5));
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = SystemExecutor::new(1).ok()?.execute(options).ok()?;
        if output.termination != Termination::Exit(0)
            || output.timed_out
            || output.cancelled
            || output.stdout.truncated
            || output.stdout.read_failed
            || output.stdout.sink_lost_bytes != 0
            || output.drain_truncated
        {
            return None;
        }
        paths.extend(
            String::from_utf8(output.stdout.bytes)
                .ok()?
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        );
    }
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Medium: real repository and public launch classification, no providers.
    #[test]
    fn repository_classification_covers_inputs_briefs_diff_and_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        provenant_os::fixture::run_git(&root, &["init", "-q"])?;
        provenant_os::fixture::run_git(&root, &["commit", "--allow-empty", "-qm", "base"])?;
        let classify = |mut attempt, prompt: &str| {
            seal_launch_policy(
                &mut attempt,
                &root,
                prompt,
                &run::LaneAccess::default(),
                None,
                None,
            )
            .map_err(|error| error.message)?;
            Ok::<_, String>(attempt)
        };
        assert!(!classify(Default::default(), "Read private/brief.md")?.confidential);
        std::fs::create_dir(root.join(".provenant"))?;
        std::fs::write(
            root.join(REPOSITORY_CONFIG),
            "[launch_policy]\nprotected_paths=['private']\n",
        )?;
        let declared = provenant_fabric_runtime::graph::AttemptSpec {
            policy_inputs: vec![root.join("private/brief.md")],
            ..Default::default()
        };
        let sealed = classify(declared, "A brief containing no path")?;
        assert!(sealed.confidential);
        assert!(classify(Default::default(), "Read private/brief.md")?.confidential);
        assert!(!classify(Default::default(), "Read public/brief.md")?.confidential);
        std::fs::create_dir(root.join("private"))?;
        std::fs::write(root.join("private/changed.md"), "private change")?;
        assert!(classify(Default::default(), "A brief containing no path")?.confidential);
        // The wire form preserves effective classification but cannot forge
        // the host-only replay marker.
        let mut recovered: provenant_fabric_runtime::graph::AttemptSpec =
            serde_json::from_value(serde_json::to_value(&sealed)?)?;
        assert!(!recovered.policy_sealed);
        recovered.policy_sealed = true;
        std::fs::remove_file(root.join(REPOSITORY_CONFIG))?;
        let replayed = classify(recovered, "Changed working tree")?;
        assert!(replayed.confidential);
        assert_eq!(replayed.launch_policy, sealed.launch_policy);
        Ok(())
    }
}
