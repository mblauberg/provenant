//! Thin hand-off to a provider cloud session: submit a brief, print the
//! session link, and later collect the branch it pushed. Provenant keeps no
//! lifecycle for the session; the provider owns it.
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum Cloud {
    /// Submit a brief to a provider cloud session and print its link
    Launch(Launch),
    /// Add a worktree holding a launched session's result
    Collect(Collect),
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Debug, Args)]
pub struct Launch {
    /// Brief file to send as the session's prompt
    #[arg(long)]
    pub file: PathBuf,
    /// Cloud provider
    #[arg(long, value_enum, default_value = provenant_records::Harness::Claude.spec().name)]
    pub provider: Provider,
    /// Provider environment name (required for codex; claude defaults to hosted)
    #[arg(long)]
    pub environment: Option<String>,
    /// Model to request (claude only)
    #[arg(long)]
    pub model: Option<String>,
    /// Effort to request (claude only)
    #[arg(long)]
    pub effort: Option<String>,
    /// Branch the session should push (default cloud/<nonce>)
    #[arg(long)]
    pub branch: Option<String>,
    /// Base branch the session starts from (default the current branch)
    #[arg(long)]
    pub base: Option<String>,
}

#[derive(Debug, Args)]
pub struct Collect {
    /// Launch key that `cloud launch` printed
    pub id: String,
    /// Worktree path (default from the project worktree layout)
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Remote branch to collect when the session pushed a different name
    #[arg(long)]
    pub branch: Option<String>,
}

/// What `collect` needs from one launch, kept beside the repository's Git data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub provider: Provider,
    pub id: String,
    pub url: Option<String>,
    pub branch: String,
    pub base: String,
    pub head: String,
    pub nonce: String,
}

/// The trailer the session ends its last commit message with, so `collect`
/// can find a branch the platform renamed (hosted Claude pushes `claude/*`).
pub fn trailer(nonce: &str) -> String {
    format!("Provenant-Launch: {nonce}")
}

/// The brief plus the hand-off the session needs to deliver a branch.
pub fn handoff_brief(brief: &str, record: &LaunchRecord) -> String {
    let push = match record.provider {
        Provider::Claude => format!(
            "Commit your work and push it to the branch `{}` without force. If the platform only lets you push another branch name, push that instead.",
            record.branch
        ),
        Provider::Codex => "Leave your work as the task's diff; it is applied locally.".to_owned(),
    };
    format!(
        "{}\n\n## Hand-off\n\nStart from commit `{}` on `{}`. {push} End the last commit message with the line `{}`. Never put credentials in commits or output.\n",
        brief.trim_end(),
        record.head,
        record.base,
        trailer(&record.nonce),
    )
}

/// The provider command for a launch, before any terminal wrapping, and the
/// bytes it reads on stdin. Codex needs an environment; Claude does not.
pub fn launch_argv(
    launch: &Launch,
    record: &LaunchRecord,
    prompt: &str,
) -> Result<(Vec<OsString>, Option<Vec<u8>>), &'static str> {
    let given = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(OsString::from)
    };
    match launch.provider {
        Provider::Claude => {
            let mut argv = vec![
                provenant_records::Harness::Claude.spec().binary.into(),
                "--cloud".into(),
                prompt.into(),
            ];
            for (flag, value) in [
                ("--environment", &launch.environment),
                ("--model", &launch.model),
                ("--effort", &launch.effort),
            ] {
                if let Some(value) = given(value) {
                    argv.extend([flag.into(), value]);
                }
            }
            // A closed stdin: `claude --cloud` must not wait for input.
            Ok((argv, Some(Vec::new())))
        }
        Provider::Codex => {
            let environment = given(&launch.environment).ok_or("codex needs --environment")?;
            let argv = vec![
                provenant_records::Harness::Codex.spec().binary.into(),
                "cloud".into(),
                "exec".into(),
                "--env".into(),
                environment,
                "--branch".into(),
                record.base.as_str().into(),
            ];
            Ok((argv, Some(prompt.as_bytes().to_vec())))
        }
    }
}

/// Run `argv` under a pseudo-terminal through `script(1)`; `claude --cloud`
/// refuses a non-terminal.
pub fn pty_wrap(argv: Vec<OsString>, bsd: bool) -> Vec<OsString> {
    if bsd {
        let mut wrapped = vec!["script".into(), "-q".into(), "/dev/null".into()];
        wrapped.extend(argv);
        return wrapped;
    }
    let line = argv
        .iter()
        .map(|arg| format!("'{}'", arg.to_string_lossy().replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        "script".into(),
        "-qec".into(),
        line.into(),
        "/dev/null".into(),
    ]
}

/// Remove terminal escapes, carriage returns and `script`'s echoed EOF.
fn visible(text: &str) -> String {
    let text = text.replace("^D\u{8}\u{8}", "").replace('\r', "");
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The session identity and link a provider printed; never guessed from prose.
pub fn parse_session(output: &str) -> Option<(String, Option<String>)> {
    let output = visible(output);
    let url = output
        .split_whitespace()
        .find(|word| word.starts_with("https://"))
        .map(str::to_owned);
    let json = serde_json::from_str::<serde_json::Value>(output.trim()).ok();
    let from_json = json.as_ref().and_then(|value| {
        ["id", "session_id", "task_id", "url"]
            .iter()
            .find_map(|key| value.get(*key)?.as_str().filter(|id| !id.trim().is_empty()))
            .map(str::to_owned)
    });
    let teleport = output.lines().find_map(|line| {
        let (label, command) = line.trim().split_once(':')?;
        label
            .trim()
            .eq_ignore_ascii_case("resume with")
            .then_some(())?;
        let mut words = command.split_whitespace();
        words.find(|word| *word == "--teleport")?;
        words
            .next()
            .filter(|id| !id.starts_with('-'))
            .map(str::to_owned)
    });
    let labelled = output.lines().find_map(|line| {
        let (label, value) = line.trim().split_once(':')?;
        let label = label.trim().to_ascii_lowercase();
        let value = value.trim();
        (matches!(
            label.as_str(),
            "id" | "session id" | "session_id" | "task id" | "task_id"
        ) && !value.is_empty())
        .then(|| value.to_owned())
    });
    let id = from_json
        .or(teleport)
        .or(labelled)
        .or_else(|| url.clone())?;
    let url = url.or_else(|| {
        id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            .then(|| format!("https://claude.ai/code/{id}"))
    });
    Some((id, url))
}

/// The absolute worktree `collect` adds. An explicit path resolves against
/// the project root, not the process directory, so every Git call agrees.
/// The default follows the project's sourced worktree layout.
pub fn collect_path(
    project: &Path,
    main: &Path,
    path: Option<&Path>,
    branch: &str,
) -> std::io::Result<PathBuf> {
    match path {
        Some(path) => Ok(project.join(path)),
        None => {
            let ports = provenant_config::PortTable::read_project(project)?;
            Ok(main.join(ports.managed_writer_path(branch)?))
        }
    }
}

/// A file-name-safe key for a session identity, which may be a URL.
pub fn record_key(id: &str) -> String {
    let tail = id.split(['?', '#']).next().unwrap_or(id);
    let tail = tail
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(tail);
    tail.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_default_uses_the_sourced_worktree_layout() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".provenant");
        std::fs::create_dir_all(&config).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        ports.worktree_layout.roots = vec![PathBuf::from(".claude/worktrees")];
        ports.worktree_layout.default = PathBuf::from(".claude/worktrees/<branch>");
        std::fs::write(
            config.join("ports.json"),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();
        assert_eq!(
            collect_path(temp.path(), Path::new("/repo"), None, "cloud/abc").unwrap(),
            Path::new("/repo/.claude/worktrees/cloud-abc")
        );
    }

    #[test]
    fn launch_arguments_assemble_without_a_network_call() {
        let record = LaunchRecord {
            provider: Provider::Claude,
            id: String::new(),
            url: None,
            branch: "cloud/abc".into(),
            base: "main".into(),
            head: "f".repeat(40),
            nonce: "abc".into(),
        };
        let mut launch = Launch {
            file: "brief.md".into(),
            provider: Provider::Claude,
            environment: None,
            model: Some("claude-sonnet-5-5".into()),
            effort: Some(" ".into()),
            branch: None,
            base: None,
        };
        let prompt = handoff_brief("Fix it.\n", &record);
        assert!(prompt.contains(&trailer("abc")) && prompt.contains(&record.head));
        let (argv, stdin) = launch_argv(&launch, &record, &prompt).unwrap();
        assert_eq!(
            argv,
            [
                "claude",
                "--cloud",
                prompt.as_str(),
                "--model",
                "claude-sonnet-5-5"
            ]
            .map(OsString::from)
        );
        assert_eq!(stdin, Some(Vec::new()));
        assert_eq!(
            pty_wrap(argv.clone(), true)[..3],
            ["script", "-q", "/dev/null"].map(OsString::from)
        );
        assert_eq!(pty_wrap(argv, false).len(), 4);

        launch.provider = Provider::Codex;
        assert!(launch_argv(&launch, &record, &prompt).is_err());
        launch.environment = Some("env".into());
        let (argv, stdin) = launch_argv(&launch, &record, &prompt).unwrap();
        assert_eq!(
            argv,
            ["codex", "cloud", "exec", "--env", "env", "--branch", "main"].map(OsString::from)
        );
        assert_eq!(stdin.as_deref(), Some(prompt.as_bytes()));

        let printed = "\u{1b}[1mCreated cloud session: x\u{1b}[0m\r\nView: https://claude.ai/code/session_01X?from=cli\nResume with: claude --teleport session_01X\n";
        assert_eq!(
            parse_session(printed),
            Some((
                "session_01X".into(),
                Some("https://claude.ai/code/session_01X?from=cli".into())
            ))
        );
        assert_eq!(parse_session("no identity here"), None);
        assert_eq!(
            record_key("https://chatgpt.com/codex/tasks/task_e_1?x=1"),
            "task_e_1"
        );
        let (project, main) = (Path::new("/repo/sub"), Path::new("/repo"));
        assert_eq!(
            collect_path(project, main, Some(Path::new("wt")), "cloud/abc").unwrap(),
            Path::new("/repo/sub/wt")
        );
        assert_eq!(
            collect_path(project, main, Some(Path::new("/elsewhere/wt")), "x").unwrap(),
            Path::new("/elsewhere/wt")
        );
        assert_eq!(
            collect_path(project, main, None, "cloud/abc").unwrap(),
            Path::new("/repo/.worktrees/cloud-abc")
        );
    }
}
