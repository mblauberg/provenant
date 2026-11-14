//! Command edge for `provenant cloud`: one provider submission and one result
//! fetch. No polling, retry or supervision; the provider owns the session.
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_cli::cloud::{self, Cloud, LaunchRecord, Provider};
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_records::{CommandResult, Outcome};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

type Error = Box<dyn std::error::Error>;

pub fn run(command: Cloud, globals: &Globals) -> u8 {
    let leaf = match &command {
        Cloud::Launch(_) => "cloud launch",
        Cloud::Collect(_) => "cloud collect",
    };
    let result = roots::selected_project(globals.project.as_deref(), true)
        .map_err(Error::from)
        .and_then(|project| match command {
            Cloud::Launch(launch) => {
                let result = launch_session(&project, &launch)?;
                let capture = || -> Result<(), Error> {
                    let resolved = roots::resolve(
                        globals,
                        roots::Workspace::Checkout,
                        roots::Product::Required,
                    )?;
                    use provenant_records::custody::{Subject, SubjectKind};
                    let key = result.0["key"].as_str().ok_or("cloud key missing")?;
                    let branch = result.0["record"]["branch"]
                        .as_str()
                        .ok_or("cloud branch missing")?;
                    crate::custody::acquire(
                        &resolved,
                        &[
                            Subject::new(SubjectKind::CloudKey, key),
                            Subject::new(SubjectKind::Branch, branch),
                        ],
                    )
                };
                if let Err(error) = capture() {
                    provenant_fabric_runtime::custody::warning(format_args!(
                        "cloud custody pending: {error}"
                    ));
                }
                Ok(result)
            }
            Cloud::Collect(collect) => {
                let resolved = roots::resolve(
                    globals,
                    roots::Workspace::Checkout,
                    roots::Product::Required,
                )?;
                let result = collect_result(&project, resolved.roots.state.as_path(), &collect)?;
                let capture = || -> Result<(), Error> {
                    let resolved = roots::resolve(
                        globals,
                        roots::Workspace::Checkout,
                        roots::Product::Required,
                    )?;
                    crate::custody::collect_cloud(
                        &resolved,
                        result.0["key"].as_str().ok_or("cloud key missing")?,
                        result.0["branch"].as_str().ok_or("cloud branch missing")?,
                        result.0["worktree"]
                            .as_str()
                            .ok_or("cloud worktree missing")?,
                    )
                };
                if let Err(error) = capture() {
                    provenant_fabric_runtime::custody::warning(format_args!(
                        "cloud collection custody pending: {error}"
                    ));
                }
                Ok(result)
            }
        });
    match result {
        Ok((data, human)) => emit(
            &CommandResult::new(leaf, Outcome::Ok, data),
            globals,
            &human,
            false,
        ),
        Err(error) => {
            let code = if leaf == "cloud launch" {
                "CLOUD_LAUNCH"
            } else {
                "CLOUD_COLLECT"
            };
            fail(leaf, Outcome::Error, code, &error.to_string(), globals)
        }
    }
}

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn exec(
    argv: &[OsString],
    cwd: &Path,
    stdin: Option<&[u8]>,
    seconds: u64,
) -> Result<Output, Error> {
    let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(seconds));
    options.stdin = stdin;
    options.process_group = true;
    options.stdout = StreamPolicy::Capture {
        limit: 8 * 1024 * 1024,
    };
    options.stderr = StreamPolicy::Capture { limit: 256 * 1024 };
    let outcome = SystemExecutor::new(1)?.execute(options)?;
    Ok(Output {
        ok: outcome.termination == Termination::Exit(0) && !outcome.timed_out,
        stdout: String::from_utf8_lossy(&outcome.stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&outcome.stderr.bytes).into_owned(),
    })
}

fn git(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String, Error> {
    let argv: Vec<OsString> = std::iter::once("git")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let output = exec(&argv, cwd, stdin, 300)?;
    if !output.ok {
        return Err(format!("git {} failed: {}", args.join(" "), excerpt(&output.stderr)).into());
    }
    Ok(output.stdout.trim().to_owned())
}

fn excerpt(text: &str) -> String {
    provenant_confinement::redact::bounded_line(text.trim(), 600)
}

/// Launch records live with the repository's Git data, not the state root.
fn record_path(project: &Path, key: &str) -> Result<PathBuf, Error> {
    let common = git(
        project,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        None,
    )?;
    Ok(PathBuf::from(common).join(format!("provenant/cloud/{key}.json")))
}

fn nonce() -> Result<String, Error> {
    let mut bytes = [0_u8; 6];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(provenant_core::hex(&bytes))
}

fn launch_session(project: &Path, launch: &cloud::Launch) -> Result<(Value, String), Error> {
    let brief = std::fs::read_to_string(&launch.file)?;
    let head = git(project, &["rev-parse", "HEAD"], None)?;
    let base = match &launch.base {
        Some(base) => base.clone(),
        None => match git(project, &["rev-parse", "--abbrev-ref", "HEAD"], None)? {
            branch if branch != "HEAD" => branch,
            _ => return Err("HEAD is detached; pass --base BRANCH".into()),
        },
    };
    let mut human = String::new();
    // Warn rather than refuse: the base may be pushed by another route.
    if git(project, &["branch", "-r", "--contains", &head], None)?.is_empty() {
        human.push_str(
            "warning: HEAD is on no remote branch; push it so the session can start from it\n",
        );
    }
    let nonce = nonce()?;
    let mut record = LaunchRecord {
        provider: launch.provider,
        id: String::new(),
        url: None,
        branch: launch
            .branch
            .clone()
            .unwrap_or_else(|| format!("cloud/{nonce}")),
        base,
        head,
        nonce,
    };
    let prompt = cloud::handoff_brief(&brief, &record);
    let (argv, stdin) = cloud::launch_argv(launch, &record, &prompt)?;
    let argv = match launch.provider {
        Provider::Claude => cloud::pty_wrap(argv, cfg!(target_os = "macos")),
        Provider::Codex => argv,
    };
    let output = exec(&argv, project, stdin.as_deref(), 300)?;
    let printed = format!("{}\n{}", output.stdout, output.stderr);
    let session = cloud::parse_session(&printed);
    let Some((id, url)) = session.filter(|_| output.ok) else {
        return Err(format!(
            "the provider did not start a session: {}",
            excerpt(&printed)
        )
        .into());
    };
    record.id = id;
    record.url = url;
    let key = cloud::record_key(&record.id);
    if key.is_empty() {
        return Err("the provider printed an unusable session identity".into());
    }
    let path = record_path(project, &key)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&record)?)?;
    let collect = format!("provenant cloud collect {key}");
    human.push_str(&format!(
        "session {}\nlink {}\nbranch {}\ncollect with `{collect}`\n",
        record.id,
        record.url.as_deref().unwrap_or("unknown"),
        record.branch,
    ));
    Ok((
        json!({"key": key, "record": record, "record_path": path.display().to_string(), "collect": collect}),
        human,
    ))
}

fn collect_result(
    project: &Path,
    host_state: &Path,
    collect: &cloud::Collect,
) -> Result<(Value, String), Error> {
    let key = cloud::record_key(&collect.id);
    let path = record_path(project, &key)?;
    let record: LaunchRecord = serde_json::from_slice(
        &std::fs::read(&path).map_err(|_| format!("no launch record for `{key}`"))?,
    )?;
    let link = record
        .url
        .as_deref()
        .unwrap_or("the provider's session list");
    let branch = collect
        .branch
        .clone()
        .unwrap_or_else(|| record.branch.clone());
    let common = PathBuf::from(git(
        project,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        None,
    )?);
    let main = common.parent().map(Path::to_path_buf).unwrap_or(common);
    let worktree =
        cloud::collect_path(project, host_state, &main, collect.path.as_deref(), &branch)?;
    let target = worktree.display().to_string();
    match record.provider {
        Provider::Claude => {
            let (remote, oid) = find_branch(project, &branch, &record.nonce)?.ok_or_else(|| {
                format!("no pushed branch found for this session yet; open {link}")
            })?;
            git(
                project,
                &[
                    "-c",
                    "fetch.fsckObjects=true",
                    "fetch",
                    "--no-tags",
                    "origin",
                    &remote,
                ],
                None,
            )?;
            git(
                project,
                &["worktree", "add", &target, "-b", &branch, &oid],
                None,
            )?;
            Ok((
                json!({"key": key, "worktree": target, "branch": branch, "remote": remote, "head": oid}),
                format!("{target} on {branch} at {oid} (from {remote})\n"),
            ))
        }
        Provider::Codex => {
            let argv = ["codex", "cloud", "diff", record.id.as_str()].map(OsString::from);
            let output = exec(&argv, project, Some(&[]), 300)?;
            if !output.ok || output.stdout.trim().is_empty() {
                return Err(format!(
                    "no diff for this session yet; open {link}: {}",
                    excerpt(&output.stderr)
                )
                .into());
            }
            git(
                project,
                &["worktree", "add", &target, "-b", &branch, &record.head],
                None,
            )?;
            git(
                &worktree,
                &["apply", "--index"],
                Some(output.stdout.as_bytes()),
            )?;
            Ok((
                json!({"key": key, "worktree": target, "branch": branch, "head": record.head, "applied": true}),
                format!("{target} on {branch}: session diff applied and staged, not committed\n"),
            ))
        }
    }
}

/// The named branch on `origin`, else a `claude/*` branch whose head carries
/// this launch's trailer. Returns the remote ref and its head.
fn find_branch(
    project: &Path,
    branch: &str,
    nonce: &str,
) -> Result<Option<(String, String)>, Error> {
    let named = format!("refs/heads/{branch}");
    let listed = git(
        project,
        &["ls-remote", "origin", &named, "refs/heads/claude/*"],
        None,
    )?;
    let refs: Vec<(String, String)> = listed
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(oid, name)| (name.to_owned(), oid.to_owned()))
        .collect();
    if let Some(found) = refs.iter().find(|(name, _)| *name == named) {
        return Ok(Some(found.clone()));
    }
    let trailer = cloud::trailer(nonce);
    for (name, oid) in refs.into_iter().take(64) {
        git(
            project,
            &[
                "-c",
                "fetch.fsckObjects=true",
                "fetch",
                "--no-tags",
                "origin",
                &name,
            ],
            None,
        )?;
        if git(project, &["log", "-1", "--format=%B", &oid], None)?
            .lines()
            .any(|line| line.trim() == trailer)
        {
            return Ok(Some((name, oid)));
        }
    }
    Ok(None)
}
