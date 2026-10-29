//! Irreversible git and shell actions, as a compiled rule table over the
//! simple commands that `shell_command` finds. Config chooses which rules are
//! on; there are no user patterns. A rule only ever reads argv and the
//! filesystem layout, so an unrecognised shape allows rather than denies.
use crate::shell_command;
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// A rule id and the one-line alternative shown when it denies.
pub const RULES: [(&str, &str); 3] = [
    (
        "force-push",
        "Force push to the default branch is refused. Accepted: `git push origin my-topic` then open a PR; the pre-push hook refuses non-fast-forward updates on every branch, so integrate with `git fetch origin && git merge origin/my-topic` rather than forcing.",
    ),
    (
        "rm-root",
        "Recursively removing the home directory, the repository root or a parent of either is refused. Accepted: name the subdirectory, for example `rm -r ./target` or `rm -r ~/old-project/build`.",
    ),
    (
        "rm-state",
        "Deleting Provenant state is refused. Accepted: `provenant worktree remove my-lane`, or `rm -r` on one named subdirectory below the state directory.",
    ),
];
const DEFAULT_DENY: &str = r#"deny = ["force-push", "rm-root", "rm-state"]
"#;

/// `[domains.guard]`: the rule ids that deny. Omit an id to turn its rule off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardConfig {
    pub deny: Vec<String>,
}
impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            deny: RULES.iter().map(|(id, _)| (*id).to_owned()).collect(),
        }
    }
}
impl DomainConfig for GuardConfig {
    fn validate(&self) -> Vec<SectionError> {
        let unknown = self
            .deny
            .iter()
            .any(|id| !RULES.iter().any(|(rule, _)| rule == id));
        if unknown {
            vec![SectionError {
                field: "deny".into(),
                code: "unknown-rule",
            }]
        } else {
            Vec::new()
        }
    }
}
pub const FIELD_METADATA: &[FieldMetadata] = &[FieldMetadata {
    values: &["force-push", "rm-root", "rm-state"],
    ..FieldMetadata::new("deny", "string_list")
}];
pub const SECTION: DomainSection = DomainSection::new::<GuardConfig>("guard", |_| DEFAULT_DENY)
    .with_boundary_checks()
    .with_fields(FIELD_METADATA);

/// Where a command runs and the directories it must not remove. A rule whose
/// directory is unknown allows.
pub struct Context<'a> {
    pub cwd: &'a Path,
    pub home: Option<&'a Path>,
    pub state: Option<&'a Path>,
}

/// The alternative of the first enabled rule that any command in `command`
/// breaks. Unparseable input allows.
pub fn violation(command: &str, ctx: &Context, config: &GuardConfig) -> Option<&'static str> {
    let commands = shell_command::commands(command)?;
    commands.iter().find_map(|argv| {
        RULES
            .iter()
            .filter(|(id, _)| config.deny.iter().any(|on| on == id))
            .find(|(id, _)| breaks(id, argv, ctx))
            .map(|(_, remedy)| *remedy)
    })
}
fn breaks(rule: &str, argv: &[String], ctx: &Context) -> bool {
    let Some((name, args)) = argv.split_first() else {
        return false;
    };
    match (rule, name.as_str()) {
        ("force-push", "git") => {
            let git = Git::parse(args);
            git.sub == "push" && forces_default_branch(&git, ctx.cwd)
        }
        ("rm-root", "rm") => {
            let repo = Repo::at(ctx.cwd).map(|repo| repo.root);
            let protected: Vec<&Path> = ctx.home.into_iter().chain(repo.as_deref()).collect();
            removes_any(args, ctx, &protected)
        }
        ("rm-state", "rm") => removes_any(args, ctx, ctx.state.as_slice()),
        _ => false,
    }
}

/// Whether a forced `git push` can move the default branch: by a flag or a
/// `+refspec` naming it, or naming nothing while the default branch is checked out.
fn forces_default_branch(git: &Git, cwd: &Path) -> bool {
    let scan = Scan::parse(
        git.rest,
        "o",
        &["repo", "receive-pack", "exec", "push-option"],
    );
    let forced = scan.long("force") || scan.long("force-with-lease") || scan.short.contains(&'f');
    let repo = Repo::at(&git.dir(cwd));
    let is_default = |branch: &str| {
        matches!(branch, "main" | "master")
            || repo
                .as_ref()
                .and_then(Repo::default_branch)
                .is_some_and(|default| default == branch)
    };
    let current = || repo.as_ref().and_then(Repo::current_branch);
    // The first operand is the remote.
    let refspecs = scan.positional.get(1..).unwrap_or_default();
    if scan.long("all") || scan.long("mirror") {
        return forced;
    }
    if refspecs.is_empty() {
        return forced && current().is_some_and(|branch| is_default(&branch));
    }
    refspecs.iter().any(|spec| {
        let plus = spec.starts_with('+');
        let spec = spec.trim_start_matches('+');
        let destination = spec.rsplit(':').next().unwrap_or(spec);
        let branch = destination
            .strip_prefix("refs/heads/")
            .unwrap_or(destination);
        let branch = if branch == "HEAD" {
            current().unwrap_or_default()
        } else {
            branch.to_owned()
        };
        (forced || plus) && is_default(&branch)
    })
}

/// Whether `rm` removes recursively a path that is, or contains, a protected one.
fn removes_any(args: &[String], ctx: &Context, protected: &[&Path]) -> bool {
    let scan = Scan::parse(args, "", &[]);
    if !(scan.short.contains(&'r') || scan.short.contains(&'R') || scan.long("recursive")) {
        return false;
    }
    let protected: Vec<PathBuf> = protected.iter().map(|path| normal(path)).collect();
    scan.positional.iter().any(|target| {
        let target = resolve(target, ctx);
        protected.iter().any(|path| path.starts_with(&target))
    })
}
/// The absolute, normalised path a `rm` operand names; `~` and `$HOME` are the
/// home directory.
fn resolve(target: &str, ctx: &Context) -> PathBuf {
    let home_relative = ["~", "$HOME", "${HOME}"].iter().find_map(|prefix| {
        target
            .strip_prefix(prefix)
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
    });
    match (home_relative, ctx.home) {
        (Some(rest), Some(home)) => normal(&home.join(rest.trim_start_matches('/'))),
        _ => normal(&ctx.cwd.join(target)),
    }
}
/// `path` without `.` and `..` components, with symlinks resolved when it exists.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    std::fs::canonicalize(&out).unwrap_or(out)
}

/// The checkout holding a directory: its root, and the Git directories that
/// name its branch and its default branch.
struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}
impl Repo {
    /// No repository, or any read failure, answers `None`.
    fn at(dir: &Path) -> Option<Self> {
        let dir = std::fs::canonicalize(dir).ok()?;
        let (root, dot) = dir.ancestors().find_map(|root| {
            let dot = root.join(".git");
            std::fs::symlink_metadata(&dot)
                .ok()
                .map(|meta| (root, (dot, meta)))
        })?;
        let (dot, meta) = dot;
        let (git_dir, common_dir) = if meta.is_dir() {
            (dot.clone(), dot)
        } else {
            // A linked worktree's `.git` is a file naming its own Git directory.
            let text = std::fs::read_to_string(&dot).ok()?;
            let git_dir = root.join(text.strip_prefix("gitdir:")?.trim());
            let common = std::fs::read_to_string(git_dir.join("commondir"))
                .map_or_else(|_| git_dir.clone(), |name| git_dir.join(name.trim()));
            (git_dir, common)
        };
        Some(Self {
            root: root.to_owned(),
            git_dir,
            common_dir,
        })
    }
    fn current_branch(&self) -> Option<String> {
        let head = std::fs::read_to_string(self.git_dir.join("HEAD")).ok()?;
        Some(head.trim().strip_prefix("ref: refs/heads/")?.to_owned())
    }
    /// The branch `origin/HEAD` names, when the clone recorded one.
    fn default_branch(&self) -> Option<String> {
        let head =
            std::fs::read_to_string(self.common_dir.join("refs/remotes/origin/HEAD")).ok()?;
        Some(
            head.trim()
                .strip_prefix("ref: refs/remotes/origin/")?
                .to_owned(),
        )
    }
}

struct Git<'a> {
    dirs: Vec<&'a str>,
    sub: &'a str,
    rest: &'a [String],
}
impl<'a> Git<'a> {
    /// Skip git's global options to the subcommand, collecting every `-C`.
    fn parse(args: &'a [String]) -> Self {
        let mut dirs = Vec::new();
        let mut at = 0;
        while let Some(arg) = args.get(at) {
            at += 1;
            let value = |attached: &'a str, at: &mut usize| {
                if attached.is_empty() {
                    *at += 1;
                    args.get(*at - 1).map_or("", String::as_str)
                } else {
                    attached
                }
            };
            if let Some(attached) = arg.strip_prefix("-C") {
                dirs.push(value(attached, &mut at));
            } else if let Some(attached) = arg.strip_prefix("-c") {
                value(attached, &mut at);
            } else if [
                "--git-dir",
                "--work-tree",
                "--namespace",
                "--super-prefix",
                "--config-env",
            ]
            .contains(&arg.as_str())
            {
                at += 1;
            } else if !arg.starts_with('-') {
                at -= 1;
                break;
            }
        }
        Self {
            dirs,
            sub: args.get(at).map_or("", String::as_str),
            rest: args.get(at + 1..).unwrap_or_default(),
        }
    }
    /// The directory git runs in.
    fn dir(&self, cwd: &Path) -> PathBuf {
        self.dirs.iter().fold(cwd.to_path_buf(), |d, c| d.join(c))
    }
}
/// Options and operands of one command, up to and after `--`.
#[derive(Default)]
struct Scan<'a> {
    short: Vec<char>,
    longs: Vec<&'a str>,
    positional: Vec<&'a String>,
}
impl<'a> Scan<'a> {
    /// `short_values` and `long_values` name the options that take a separate value.
    fn parse(args: &'a [String], short_values: &str, long_values: &[&str]) -> Self {
        let mut scan = Scan::default();
        let mut dashes = false;
        let mut tokens = args.iter();
        while let Some(token) = tokens.next() {
            if dashes {
                scan.positional.push(token);
            } else if token == "--" {
                dashes = true;
            } else if let Some(long) = token.strip_prefix("--") {
                let (name, attached) = long
                    .split_once('=')
                    .map_or((long, false), |(n, _)| (n, true));
                scan.longs.push(name);
                if !attached && long_values.contains(&name) {
                    tokens.next();
                }
            } else if let Some(cluster) = token.strip_prefix('-').filter(|c| !c.is_empty()) {
                for (at, letter) in cluster.char_indices() {
                    scan.short.push(letter);
                    if short_values.contains(letter) {
                        if at + letter.len_utf8() == cluster.len() {
                            tokens.next();
                        }
                        break;
                    }
                }
            } else {
                scan.positional.push(token);
            }
        }
        scan
    }
    /// Whether `full` was given, in full or as an abbreviation git would accept
    /// (any prefix of at least two characters).
    fn long(&self, full: &str) -> bool {
        self.longs
            .iter()
            .any(|given| given.len() >= 2 && full.starts_with(given))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        std::fs::canonicalize(std::env::temp_dir()).unwrap()
    }

    /// `home/project` is a checkout on `topic`; `home/state` is the state dir.
    fn tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir_in(base()).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for path in ["home/project/.git", "home/state/runs", "other"] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        std::fs::write(
            root.join("home/project/.git/HEAD"),
            "ref: refs/heads/topic\n",
        )
        .unwrap();
        (dir, root)
    }
    fn denies_in(command: &str, root: &Path, cwd: &str, config: &GuardConfig) -> bool {
        let (home, state) = (root.join("home"), root.join("home/state"));
        let cwd = root.join(cwd);
        let ctx = Context {
            cwd: &cwd,
            home: Some(&home),
            state: Some(&state),
        };
        violation(command, &ctx, config).is_some()
    }
    fn denies(command: &str, root: &Path) -> bool {
        denies_in(command, root, "home/project", &GuardConfig::default())
    }

    #[test]
    fn no_remedy_recommends_a_force_push() {
        for (id, remedy) in RULES {
            assert!(
                !remedy.contains("--force") && !remedy.contains("push -f"),
                "{id}: {remedy}"
            );
        }
    }

    #[test]
    fn every_refusal_shows_an_accepted_example() {
        let (_dir, root) = tree();
        for (id, remedy) in RULES {
            let examples: Vec<_> = remedy.split('`').skip(1).step_by(2).collect();
            assert!(!examples.is_empty(), "{id} shows no example: {remedy}");
            for example in examples {
                assert!(
                    !denies(example, &root),
                    "{id} example is refused: {example}"
                );
            }
        }
    }

    #[test]
    fn only_irreversible_forms_are_refused() {
        let (_dir, root) = tree();
        for allowed in [
            "git stash",
            "git reset --hard",
            "git clean -fd",
            "git checkout main",
            "git switch -c topic",
            "git commit --no-verify -m x",
            "git push --force origin topic",
            "git push --force-with-lease origin topic",
            "git push -f",
            "git push origin main",
            "sleep 30",
            "pkill -f cargo",
            "kill -9 $PID",
            "$CMD run",
            "echo kill 1 | sh",
            "rm -rf build",
            "rm -rf ~/scratch /tmp/x",
            "rm -rf home/state/runs/one",
            "rm -f ~",
            "rm ~",
            "echo rm -rf ~",
            "rg 'git push -f origin main' docs",
        ] {
            assert!(!denies(allowed, &root), "{allowed}");
        }
    }

    #[test]
    fn force_pushes_to_the_default_branch_are_refused() {
        let (_dir, root) = tree();
        for denied in [
            "git push --force origin main",
            "git push -f origin master",
            "git push origin +main",
            "git push --force-with-lease origin main",
            "git push --force-with-lease=main origin main",
            "git push -fu origin main",
            "git push origin +HEAD:main",
            "git push origin +topic:refs/heads/main",
            "git push -f origin :main",
            "git push --force --all",
            "git push --force --mirror origin",
            "git -C home/project push -f origin main",
            "sudo git push -f origin main",
            "bash -c 'git push -f origin main'",
            "git push --for origin main",
        ] {
            assert!(
                denies_in(denied, &root, ".", &GuardConfig::default()),
                "{denied}"
            );
        }
        for allowed in [
            "git push origin main",
            "git push origin HEAD:main",
            "git push --force origin topic",
            "git push -f origin HEAD:topic",
            "git push --force-with-lease origin +topic",
            "git push origin main:topic --force",
            "git push -f",
        ] {
            assert!(!denies(allowed, &root), "{allowed}");
        }
    }

    #[test]
    fn a_bare_force_push_follows_the_checked_out_branch() {
        let (_dir, root) = tree();
        let head = root.join("home/project/.git/HEAD");
        std::fs::write(&head, "ref: refs/heads/main\n").unwrap();
        for denied in [
            "git push -f",
            "git push --force origin",
            "git push -f origin HEAD",
        ] {
            assert!(denies(denied, &root), "{denied}");
        }
        assert!(!denies("git push origin", &root));
        // The default branch is whatever `origin/HEAD` names, besides main and master.
        std::fs::write(&head, "ref: refs/heads/trunk\n").unwrap();
        assert!(!denies("git push -f", &root));
        let origin = root.join("home/project/.git/refs/remotes/origin");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::write(origin.join("HEAD"), "ref: refs/remotes/origin/trunk\n").unwrap();
        assert!(denies("git push -f", &root));
        assert!(denies("git push -f origin trunk", &root));
    }

    #[test]
    fn a_linked_worktree_reads_its_own_head_and_the_common_default_branch() {
        let (_dir, root) = tree();
        let common = root.join("home/project/.git");
        let git_dir = common.join("worktrees/lane");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/trunk\n").unwrap();
        std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
        let lane = root.join("home/lane");
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::write(
            lane.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        let config = GuardConfig::default();
        assert!(!denies_in("git push -f", &root, "home/lane", &config));
        let origin = common.join("refs/remotes/origin");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::write(origin.join("HEAD"), "ref: refs/remotes/origin/trunk\n").unwrap();
        assert!(denies_in("git push -f", &root, "home/lane", &config));
    }

    #[test]
    fn recursive_removal_of_home_or_the_repository_root_is_refused() {
        let (_dir, root) = tree();
        let home = root.join("home");
        let project = home.join("project");
        for denied in [
            "rm -rf ~".to_owned(),
            "rm -rf ~/".to_owned(),
            "rm -rf $HOME".to_owned(),
            "rm -rf \"$HOME\"".to_owned(),
            "rm -rf ${HOME}/".to_owned(),
            "rm -Rf ~".to_owned(),
            "rm -r -f ~".to_owned(),
            "rm --recursive --force ~".to_owned(),
            "rm -rf -- ~".to_owned(),
            "rm -rf /".to_owned(),
            "rm -rf .".to_owned(),
            "rm -rf ..".to_owned(),
            "rm -rf ../project".to_owned(),
            "rm -rf ./".to_owned(),
            "rm -rf x ~".to_owned(),
            "sudo rm -rf ~".to_owned(),
            "bash -c 'rm -rf ~'".to_owned(),
            format!("rm -rf {}", home.display()),
            format!("rm -rf {}", project.display()),
            format!("rm -rf {}/..", project.display()),
            format!("rm -rf {}", root.display()),
        ] {
            assert!(denies(&denied, &root), "{denied}");
        }
        // From a subdirectory the repository root is still the nearest `.git`.
        std::fs::create_dir(project.join("src")).unwrap();
        let config = GuardConfig::default();
        assert!(denies_in("rm -rf ..", &root, "home/project/src", &config));
        assert!(!denies_in("rm -rf .", &root, "home/project/src", &config));
        assert!(!denies_in("rm -rf src", &root, "home/project", &config));
        assert!(!denies_in(
            "rm -rf ../other",
            &root,
            "home/project",
            &config
        ));
    }

    #[test]
    fn deleting_provenant_state_is_refused() {
        let (_dir, root) = tree();
        let state = root.join("home/state");
        for denied in [
            format!("rm -rf {}", state.display()),
            "rm -rf ~/state".to_owned(),
            "rm -rf ../state".to_owned(),
            "rm -rf $HOME/state/".to_owned(),
        ] {
            let only_state = GuardConfig {
                deny: vec!["rm-state".into()],
            };
            assert!(
                denies_in(&denied, &root, "home/project", &only_state),
                "{denied}"
            );
        }
        assert!(!denies("rm -rf ~/state/runs", &root));
        assert!(!denies("rm -rf ../state/runs/one", &root));
        // Without a known state directory the rule has nothing to protect.
        let ctx = Context {
            cwd: &root,
            home: None,
            state: None,
        };
        assert!(violation("rm -rf ~/state", &ctx, &GuardConfig::default()).is_none());
    }

    #[test]
    fn each_refusal_names_the_alternative() {
        for (id, remedy) in RULES {
            assert!(remedy.contains("refused"), "{id}");
            assert!(remedy.contains('`') || remedy.contains("Name"), "{id}");
        }
    }

    #[test]
    fn config_turns_rules_off_and_rejects_unknown_ids() {
        let (_dir, root) = tree();
        let only_rm = GuardConfig {
            deny: vec!["rm-root".into()],
        };
        assert!(denies_in("rm -rf ~", &root, ".", &only_rm));
        assert!(!denies_in("git push -f origin main", &root, ".", &only_rm));
        assert!(!denies_in(
            "rm -rf ~",
            &root,
            ".",
            &GuardConfig { deny: vec![] }
        ));
        let bad = GuardConfig {
            deny: vec!["stash".into()],
        };
        assert_eq!(bad.validate().len(), 1);
        assert!(GuardConfig::default().validate().is_empty());
    }

    #[test]
    fn unparseable_input_allows() {
        let ctx = Context {
            cwd: Path::new("/"),
            home: None,
            state: None,
        };
        assert!(violation("git push -f origin main $(", &ctx, &GuardConfig::default()).is_none());
    }
}
