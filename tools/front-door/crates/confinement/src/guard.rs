//! Workflow-breaking git and shell patterns, as a compiled rule table over the
//! simple commands that `shell_command` finds. Config chooses which rules are
//! on; there are no user patterns. A rule only ever reads argv and the
//! filesystem layout, so an unrecognised shape allows rather than denies.
use crate::shell_command;
use provenant_config::{DomainConfig, DomainSection, SectionError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A rule id and the one-line remedy shown when it denies.
pub const RULES: [(&str, &str); 7] = [
    (
        "stash",
        "Bare `git stash` is refused. Commit a WIP commit, or use `git stash push -m TAG`.",
    ),
    (
        "reset-hard",
        "`git reset --hard` is refused. Commit or `git stash push -m TAG` first, or reset in a disposable linked worktree.",
    ),
    (
        "clean-force",
        "`git clean -f` is refused. Preview with `git clean -n` and delete the named files yourself.",
    ),
    (
        "force-push",
        "Force push is refused. Use `git push --force-with-lease` on your own branch.",
    ),
    (
        "no-verify",
        "`--no-verify` is refused. Fix what the hook reports instead of skipping it.",
    ),
    (
        "primary-checkout",
        "Branch changes in the primary checkout are refused. Use a linked worktree: `provenant worktree create`.",
    ),
    (
        "sleep-poll",
        "Foreground `sleep` polling is refused. Use a background wait or `provenant lanes --wait`.",
    ),
];
const DEFAULT_DENY: &str = r#"deny = ["stash", "reset-hard", "clean-force", "force-push", "no-verify", "primary-checkout", "sleep-poll"]
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
pub const SECTION: DomainSection = DomainSection::new::<GuardConfig>("guard", |_| DEFAULT_DENY);

/// The remedy of the first enabled rule that any command in `command` breaks.
/// `cwd` locates the primary-checkout rule. Unparseable input is not this
/// module's concern: `shell_command::commands` already refuses it.
pub fn violation(command: &str, cwd: &Path, config: &GuardConfig) -> Option<&'static str> {
    let commands = shell_command::commands(command).ok()?;
    commands.iter().find_map(|argv| {
        RULES
            .iter()
            .filter(|(id, _)| config.deny.iter().any(|on| on == id))
            .find(|(id, _)| breaks(id, argv, cwd))
            .map(|(_, remedy)| *remedy)
    })
}
fn breaks(rule: &str, argv: &[String], cwd: &Path) -> bool {
    let Some((name, args)) = argv.split_first() else {
        return false;
    };
    if name == "sleep" {
        return rule == "sleep-poll" && sleeps_long(args);
    }
    if name != "git" {
        return false;
    }
    let git = Git::parse(args);
    let has = |flag: &str| git.rest.iter().any(|a| a == flag);
    let cluster = |letter: char| {
        git.rest
            .iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains(letter))
    };
    match (rule, git.sub) {
        ("no-verify", _) => has("--no-verify") || (git.sub == "commit" && cluster('n')),
        ("stash", "stash") => match git.rest.first().map(String::as_str) {
            None | Some("pop" | "save") => true,
            Some("push") => !(has("-m") || has("--message")),
            Some(first) => first.starts_with('-'),
        },
        ("reset-hard", "reset") => has("--hard"),
        ("clean-force", "clean") => {
            (has("--force") || cluster('f')) && !(has("--dry-run") || cluster('n'))
        }
        ("force-push", "push") => {
            has("--force")
                || cluster('f')
                || git.rest.iter().any(|a| a.len() > 1 && a.starts_with('+'))
        }
        ("primary-checkout", "switch") => git.dir(cwd).is_some_and(|d| is_primary(&d)),
        ("primary-checkout", "checkout") => {
            moves_branch(git.rest) && git.dir(cwd).is_some_and(|d| is_primary(&d))
        }
        _ => false,
    }
}
struct Git<'a> {
    dir: Option<&'a str>,
    sub: &'a str,
    rest: &'a [String],
}
impl<'a> Git<'a> {
    /// Skip git's global options to the subcommand, keeping the last `-C`.
    fn parse(args: &'a [String]) -> Self {
        let mut dir = None;
        let mut at = 0;
        while let Some(arg) = args.get(at) {
            at += 1;
            match arg.as_str() {
                "-C" => {
                    dir = args.get(at).map(String::as_str);
                    at += 1;
                }
                "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => at += 1,
                option if option.starts_with('-') => {}
                _ => {
                    at -= 1;
                    break;
                }
            }
        }
        let sub = args.get(at).map_or("", String::as_str);
        Self {
            dir,
            sub,
            rest: args.get(at + 1..).unwrap_or_default(),
        }
    }
    fn dir(&self, cwd: &Path) -> Option<PathBuf> {
        Some(self.dir.map_or_else(|| cwd.to_path_buf(), |d| cwd.join(d)))
    }
}
/// A checkout that moves HEAD: it names a branch or revision, or creates one.
/// `git checkout -- path`, `-p` and a bare `git checkout` leave HEAD alone.
fn moves_branch(rest: &[String]) -> bool {
    let creates = ["-b", "-B", "--orphan", "--detach", "-d"];
    let before_paths = rest.split(|a| a == "--").next().unwrap_or_default();
    if before_paths.iter().any(|a| creates.contains(&a.as_str())) {
        return true;
    }
    rest.iter().all(|a| a != "--") && rest.iter().any(|a| !a.starts_with('-') && a != ".")
}
/// Whether `dir` is inside a repository whose `.git` is a directory, as in a
/// primary checkout; a linked worktree's `.git` is a file. No repository, or
/// any read failure, answers false.
fn is_primary(dir: &Path) -> bool {
    dir.ancestors()
        .map(|d| d.join(".git"))
        .find_map(|git| std::fs::symlink_metadata(git).ok())
        .is_some_and(|meta| meta.is_dir())
}
/// `sleep 5`, `sleep 1m` and `sleep infinity` poll; short pauses do not.
fn sleeps_long(args: &[String]) -> bool {
    let seconds: f64 = args
        .iter()
        .map(|arg| {
            if arg == "infinity" {
                return f64::INFINITY;
            }
            let (number, unit) =
                arg.split_at(arg.find(|c: char| c.is_alphabetic()).unwrap_or(arg.len()));
            let scale = match unit {
                "" | "s" => 1.0,
                "m" => 60.0,
                "h" => 3600.0,
                "d" => 86400.0,
                _ => 0.0,
            };
            number.parse::<f64>().unwrap_or(0.0) * scale
        })
        .sum();
    seconds >= 5.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denies(command: &str, cwd: &Path) -> bool {
        violation(command, cwd, &GuardConfig::default()).is_some()
    }
    fn repo(linked: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        if linked {
            std::fs::write(git, "gitdir: /elsewhere\n").unwrap();
        } else {
            std::fs::create_dir(git).unwrap();
        }
        dir
    }

    #[test]
    fn destructive_forms_are_denied_and_safe_forms_allowed() {
        let primary = repo(false);
        let cwd = primary.path();
        for denied in [
            "git stash",
            "git stash pop",
            "git stash -u",
            "git stash push",
            "git -C x stash",
            "sudo git stash && echo ok",
            "bash -c 'git stash'",
            "git reset --hard",
            "git reset --hard HEAD~1",
            "git clean -fd",
            "git clean --force",
            "git push --force",
            "git push -f origin main",
            "git push origin +main",
            "git commit --no-verify -m x",
            "git commit -nm x",
            "git push --no-verify",
            "git checkout main",
            "git checkout -b topic",
            "git switch main",
            "git switch -c topic",
            "sleep 5",
            "sleep 30",
            "sleep 1m",
            "while true; do sleep 10; done",
        ] {
            assert!(denies(denied, cwd), "{denied}");
        }
        for allowed in [
            "git stash push -m tag",
            "git stash list",
            "git stash apply abc123",
            "git stash drop",
            "git reset --soft HEAD~1",
            "git reset HEAD file",
            "git clean -n",
            "git clean -nfd",
            "git push --force-with-lease",
            "git push origin main",
            "git commit -m x",
            "git checkout -- path/file",
            "git checkout HEAD -- file",
            "git checkout",
            "git status",
            "sleep 1",
            "sleep 0.5",
            "rg 'git stash' docs",
            "echo git reset --hard",
        ] {
            assert!(!denies(allowed, cwd), "{allowed}");
        }
    }

    #[test]
    fn branch_changes_are_refused_only_in_a_primary_checkout() {
        let linked = repo(true);
        assert!(!denies("git checkout main", linked.path()));
        assert!(!denies("git switch -c topic", linked.path()));
        let primary = repo(false);
        assert!(denies("git checkout main", primary.path()));
        let nested = primary.path().join("a");
        std::fs::create_dir(&nested).unwrap();
        assert!(denies("git switch main", &nested));
        assert!(denies("git -C a checkout main", primary.path()));
        let outside = tempfile::tempdir().unwrap();
        assert!(!denies("git checkout main", outside.path()));
    }

    #[test]
    fn config_turns_rules_off_and_rejects_unknown_ids() {
        let cwd = Path::new("/");
        let only_stash = GuardConfig {
            deny: vec!["stash".into()],
        };
        assert!(violation("git stash", cwd, &only_stash).is_some());
        assert!(violation("git reset --hard", cwd, &only_stash).is_none());
        assert!(violation("sleep 9", cwd, &GuardConfig { deny: vec![] }).is_none());
        let bad = GuardConfig {
            deny: vec!["nope".into()],
        };
        assert_eq!(bad.validate().len(), 1);
        assert!(GuardConfig::default().validate().is_empty());
    }

    #[test]
    fn unparseable_input_allows_here_and_is_left_to_the_shell_check() {
        assert!(violation("git stash $(", Path::new("/"), &GuardConfig::default()).is_none());
    }
}
