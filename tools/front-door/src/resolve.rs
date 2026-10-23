//! Instance root and product root resolution.
//!
//! One resolver serves both former layers. Precedence for the product root:
//! `AGENT_FABRIC_PRODUCT_ROOT`, the pointer under the instance root,
//! `AGENTS_HOME`, then `~/.agents`. A pointer that exists but is bad is an
//! error, never a fallback.

use crate::exec::{REPAIR, is_executable};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub const FABRIC_MCP: &str = "runtime/fabric/bin/fabric-mcp";
const POINTER: &str = ".agent-fabric/product-root.json";

/// The four variables resolution reads, captured once.
#[derive(Default)]
pub struct Settings {
    /// `$HOME` as set (even if empty); the passwd entry is used only when unset.
    pub home: Option<OsString>,
    pub instance_root: Option<OsString>,
    pub product_root: Option<OsString>,
    pub agents_home: Option<OsString>,
}

impl Settings {
    pub fn from_process() -> Self {
        let set = |name| env::var_os(name).filter(|value| !value.is_empty());
        Settings {
            home: env::var_os("HOME"),
            instance_root: set("AGENT_FABRIC_INSTANCE_ROOT"),
            product_root: set("AGENT_FABRIC_PRODUCT_ROOT"),
            agents_home: set("AGENTS_HOME"),
        }
    }

    fn home_dir(&self) -> Option<OsString> {
        self.home.clone().or_else(|| passwd_home(Account::Current))
    }

    /// Python's `expanduser`: `~` and `~name` prefixes; anything else is unchanged.
    fn expand_user(&self, path: &OsStr) -> OsString {
        let bytes = path.as_bytes();
        if bytes.first() != Some(&b'~') {
            return path.to_os_string();
        }
        let end = bytes.iter().position(|&b| b == b'/').unwrap_or(bytes.len());
        let name = &bytes[1..end];
        let home = if name.is_empty() {
            self.home_dir()
        } else {
            passwd_home(Account::Named(name))
        };
        let Some(home) = home else {
            return path.to_os_string();
        };
        let mut expanded = home.into_vec();
        while expanded.last() == Some(&b'/') {
            expanded.pop();
        }
        expanded.extend_from_slice(&bytes[end..]);
        if expanded.is_empty() {
            expanded.push(b'/');
        }
        OsString::from_vec(expanded)
    }

    fn default_agents_dir(&self) -> PathBuf {
        PathBuf::from(self.expand_user(OsStr::new("~/.agents")))
    }
}

enum Account<'a> {
    Current,
    Named(&'a [u8]),
}

fn passwd_home(account: Account) -> Option<OsString> {
    let account = match account {
        Account::Current => provenant_supervisor::sys::Account::Current,
        Account::Named(name) => provenant_supervisor::sys::Account::Named(OsStr::from_bytes(name)),
    };
    provenant_supervisor::sys::account(account)
        .ok()
        .flatten()
        .map(|entry| entry.home)
}

/// `Path.resolve()`: symlinks resolved, missing tail components kept
/// lexically with `..` collapsed.
pub fn realpath(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();
    let mut resolved = loop {
        if let Ok(real) = fs::canonicalize(&existing) {
            break real;
        }
        // `file_name` is `None` for a trailing `..`, which the lexical pass keeps.
        let name = existing
            .file_name()
            .map_or_else(|| OsString::from(".."), OsStr::to_os_string);
        if !existing.pop() {
            break existing;
        }
        tail.push(name);
    };
    for name in tail.into_iter().rev() {
        if name == ".." {
            resolved.pop();
        } else {
            resolved.push(name);
        }
    }
    resolved
}

fn display(path: &OsStr) -> String {
    path.to_string_lossy().into_owned()
}

fn instance_root(settings: &Settings) -> Result<PathBuf, String> {
    let candidate = match &settings.instance_root {
        Some(configured) => PathBuf::from(settings.expand_user(configured)),
        None => settings.default_agents_dir(),
    };
    if !candidate.is_absolute() {
        let shown = settings
            .instance_root
            .as_deref()
            .unwrap_or(candidate.as_os_str());
        return Err(format!(
            "instance root must be an absolute path, got {}; {REPAIR}",
            display(shown)
        ));
    }
    Ok(realpath(&candidate))
}

fn pointer_product_root(settings: &Settings, instance: &Path) -> Result<Option<PathBuf>, String> {
    let pointer = instance.join(POINTER);
    let at = pointer.display();
    if fs::metadata(&pointer).is_err() && fs::symlink_metadata(&pointer).is_err() {
        return Ok(None);
    }
    let unreadable = |why: &dyn std::fmt::Display| {
        format!("product-root pointer is unreadable at {at}: {why}; {REPAIR}")
    };
    let bytes = fs::read(&pointer).map_err(|error| unreadable(&crate::exec::describe(&error)))?;
    let text = String::from_utf8(bytes).map_err(|_| unreadable(&"not valid UTF-8"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| unreadable(&error))?;
    let (Some(1), Some(root)) = (
        value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64),
        value
            .get("product_root")
            .and_then(serde_json::Value::as_str),
    ) else {
        return Err(format!("product-root pointer is invalid at {at}; {REPAIR}"));
    };
    let candidate = PathBuf::from(settings.expand_user(OsStr::new(root)));
    if !candidate.is_absolute() || fs::metadata(&candidate).is_err() {
        return Err(format!(
            "product-root pointer is invalid or stale at {at}; {REPAIR}"
        ));
    }
    Ok(Some(candidate))
}

/// Resolve the product checkout, or say why not (message includes the repair hint).
pub fn product_root(settings: &Settings) -> Result<PathBuf, String> {
    let instance = instance_root(settings)?;
    let (candidate, label) = if let Some(configured) = &settings.product_root {
        (
            PathBuf::from(settings.expand_user(configured)),
            "AGENT_FABRIC_PRODUCT_ROOT",
        )
    } else if let Some(pointed) = pointer_product_root(settings, &instance)? {
        (pointed, "product-root pointer")
    } else if let Some(home) = &settings.agents_home {
        (PathBuf::from(settings.expand_user(home)), "AGENTS_HOME")
    } else {
        (settings.default_agents_dir(), "~/.agents")
    };
    if !candidate.is_absolute() {
        return Err(format!(
            "{label} must be an absolute path, got {}; {REPAIR}",
            candidate.display()
        ));
    }
    let root = realpath(&candidate);
    let wrapper = root.join(FABRIC_MCP);
    if !is_file(&wrapper) || !is_executable(&wrapper) {
        return Err(format!(
            "Fabric MCP launcher is missing or not executable at {}; {REPAIR}",
            wrapper.display()
        ));
    }
    Ok(root)
}

/// `Path.is_file()`: follows symlinks.
pub fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

/// The checkout this binary lives in: parent of the parent of the real
/// executable path. An installed copy in `~/.local/bin` yields `~/.local`.
pub fn local_root() -> Option<PathBuf> {
    let exe = fs::canonicalize(env::current_exe().ok()?).ok()?;
    Some(exe.parent()?.parent()?.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir = fs::canonicalize(env::temp_dir())
                .unwrap()
                .join(format!("fd-resolve-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn checkout(&self, name: &str) -> PathBuf {
            let root = self.0.join(name);
            let wrapper = root.join(FABRIC_MCP);
            fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
            fs::write(&wrapper, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
            root
        }

        fn pointer(&self, instance: &Path, body: &str) {
            let file = instance.join(POINTER);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, body).unwrap();
        }

        fn settings(&self) -> Settings {
            Settings {
                home: Some(self.0.join("home").into_os_string()),
                ..Settings::default()
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn err(settings: &Settings) -> String {
        product_root(settings).unwrap_err()
    }

    #[test]
    fn precedence_is_explicit_then_pointer_then_agents_home_then_default() {
        let s = Scratch::new("precedence");
        let (explicit, pointed, agents, default) = (
            s.checkout("explicit"),
            s.checkout("pointed"),
            s.checkout("agents"),
            s.checkout("home/.agents"),
        );
        let instance = s.0.join("instance");
        s.pointer(
            &instance,
            &format!(
                "{{\"schema_version\":1,\"product_root\":\"{}\"}}",
                pointed.display()
            ),
        );
        let mut settings = s.settings();
        settings.instance_root = Some(instance.clone().into_os_string());
        settings.agents_home = Some(agents.clone().into_os_string());
        settings.product_root = Some(explicit.clone().into_os_string());
        assert_eq!(product_root(&settings).unwrap(), explicit);
        settings.product_root = None;
        assert_eq!(product_root(&settings).unwrap(), pointed);
        fs::remove_file(instance.join(POINTER)).unwrap();
        assert_eq!(product_root(&settings).unwrap(), agents);
        settings.agents_home = None;
        assert_eq!(product_root(&settings).unwrap(), default);
    }

    #[test]
    fn agents_home_is_never_the_instance_root() {
        let s = Scratch::new("agents-home");
        let (pointed, agents) = (s.checkout("pointed"), s.checkout("agents"));
        s.pointer(
            &s.0.join("home/.agents"),
            &format!(
                "{{\"schema_version\":1,\"product_root\":\"{}\"}}",
                pointed.display()
            ),
        );
        let mut settings = s.settings();
        settings.agents_home = Some(agents.into_os_string());
        assert_eq!(product_root(&settings).unwrap(), pointed);
    }

    #[test]
    fn bad_pointers_fail_loudly_without_falling_back() {
        let s = Scratch::new("pointers");
        let fallback = s.checkout("agents");
        let instance = s.0.join("instance");
        let mut settings = s.settings();
        settings.instance_root = Some(instance.clone().into_os_string());
        settings.agents_home = Some(fallback.into_os_string());
        let cases: [(&str, &str); 14] = [
            (
                "{\"schema_version\":1,\"product_root\":\"/does/not/exist\"}",
                "invalid or stale",
            ),
            (
                "{\"schema_version\":1,\"product_root\":\"relative\"}",
                "invalid or stale",
            ),
            (
                "{\"schema_version\":1,\"product_root\":\"\"}",
                "invalid or stale",
            ),
            (
                "{\"schema_version\":2,\"product_root\":\"/\"}",
                "is invalid at",
            ),
            (
                "{\"schema_version\":\"1\",\"product_root\":\"/\"}",
                "is invalid at",
            ),
            (
                "{\"schema_version\":1.0,\"product_root\":\"/\"}",
                "is invalid at",
            ),
            (
                "{\"schema_version\":true,\"product_root\":\"/\"}",
                "is invalid at",
            ),
            (
                "{\"schema_version\":null,\"product_root\":\"/\"}",
                "is invalid at",
            ),
            ("{\"schema_version\":1}", "is invalid at"),
            ("{\"schema_version\":1,\"product_root\":7}", "is invalid at"),
            ("[1]", "is invalid at"),
            ("not json", "unreadable"),
            ("", "unreadable"),
            ("{\"schema_version\":NaN}", "unreadable"),
        ];
        for (body, wanted) in cases {
            s.pointer(&instance, body);
            let message = err(&settings);
            assert!(
                message.contains("product-root pointer") && message.contains(wanted),
                "{body}: {message}"
            );
            assert!(message.ends_with(REPAIR), "{message}");
        }
    }

    #[test]
    fn unreadable_pointer_shapes_are_errors() {
        let s = Scratch::new("shapes");
        let instance = s.0.join("instance");
        let mut settings = s.settings();
        settings.instance_root = Some(instance.clone().into_os_string());
        let file = instance.join(POINTER);
        fs::create_dir_all(&file).unwrap();
        assert!(err(&settings).contains("unreadable"));
        fs::remove_dir(&file).unwrap();
        symlink(s.0.join("nowhere"), &file).unwrap();
        assert!(err(&settings).contains("unreadable"));
        fs::remove_file(&file).unwrap();
        fs::write(&file, [0xff, 0xfe, b'{']).unwrap();
        assert!(err(&settings).contains("product-root pointer is unreadable"));
    }

    #[test]
    fn pointer_with_extra_keys_and_tilde_works() {
        let s = Scratch::new("extras");
        let target = s.checkout("home/checkout");
        let instance = s.0.join("instance");
        s.pointer(
            &instance,
            "{\"extra\":[1,2],\"schema_version\":1,\"product_root\":\"~/checkout\"}",
        );
        let mut settings = s.settings();
        settings.instance_root = Some(instance.into_os_string());
        assert_eq!(product_root(&settings).unwrap(), target);
    }

    #[test]
    fn relative_values_name_their_label() {
        let s = Scratch::new("relative");
        let mut settings = s.settings();
        settings.product_root = Some("rel/path".into());
        assert!(
            err(&settings)
                .starts_with("AGENT_FABRIC_PRODUCT_ROOT must be an absolute path, got rel/path;")
        );
        settings.product_root = None;
        settings.agents_home = Some("rel".into());
        assert!(err(&settings).starts_with("AGENTS_HOME must be an absolute path"));
        settings.agents_home = None;
        settings.instance_root = Some("inst".into());
        assert!(err(&settings).starts_with("instance root must be an absolute path, got inst;"));
    }

    #[test]
    fn launcher_must_be_an_executable_regular_file() {
        let s = Scratch::new("launcher");
        let mut settings = s.settings();
        let root = s.0.join("root");
        settings.product_root = Some(root.clone().into_os_string());
        let wrapper = root.join(FABRIC_MCP);
        assert!(err(&settings).contains("Fabric MCP launcher is missing or not executable at"));
        fs::create_dir_all(&wrapper).unwrap();
        assert!(err(&settings).contains("Fabric MCP launcher"));
        fs::remove_dir(&wrapper).unwrap();
        fs::write(&wrapper, "x").unwrap();
        assert!(err(&settings).contains("Fabric MCP launcher"));
        fs::remove_file(&wrapper).unwrap();
        symlink(s.0.join("gone"), &wrapper).unwrap();
        assert!(err(&settings).contains("Fabric MCP launcher"));
    }

    #[test]
    fn product_root_is_printed_resolved() {
        let s = Scratch::new("resolved");
        let real = s.checkout("real");
        symlink(&real, s.0.join("link")).unwrap();
        let mut settings = s.settings();
        settings.product_root = Some(format!("{}/link/../real/", s.0.display()).into());
        assert_eq!(product_root(&settings).unwrap(), real);
        settings.product_root = Some(s.0.join("link").into_os_string());
        assert_eq!(product_root(&settings).unwrap(), real);
    }

    #[test]
    fn realpath_keeps_missing_tail_lexically() {
        let s = Scratch::new("realpath");
        assert_eq!(realpath(&s.0.join("a/./b/../c")), s.0.join("a/c"));
        assert_eq!(realpath(Path::new("/")), Path::new("/"));
    }

    #[test]
    fn tilde_expansion_follows_home_first() {
        let settings = Settings {
            home: Some("/h/".into()),
            ..Settings::default()
        };
        assert_eq!(settings.expand_user(OsStr::new("~")), OsString::from("/h"));
        assert_eq!(
            settings.expand_user(OsStr::new("~/x")),
            OsString::from("/h/x")
        );
        assert_eq!(
            settings.expand_user(OsStr::new("a~/x")),
            OsString::from("a~/x")
        );
        let root = Settings {
            home: Some("/".into()),
            ..Settings::default()
        };
        assert_eq!(root.expand_user(OsStr::new("~")), OsString::from("/"));
        assert_eq!(
            root.expand_user(OsStr::new("~nosuchuser_zz/x")),
            OsString::from("~nosuchuser_zz/x")
        );
    }

    #[test]
    fn home_unset_falls_back_to_the_passwd_entry() {
        let settings = Settings::default();
        assert!(Path::new(&settings.expand_user(OsStr::new("~"))).is_absolute());
    }

    #[test]
    fn awkward_root_paths_resolve_without_creating_instance_state() {
        let s = Scratch::new("awkward paths");
        let root = s.checkout("home/checkout λ\nline");
        let mut settings = s.settings();
        settings.product_root = Some(root.clone().into_os_string());
        assert_eq!(product_root(&settings).unwrap(), root);

        let mut missing = s.settings();
        missing.home = Some(s.0.join("empty-home").into_os_string());
        assert!(product_root(&missing).is_err());
        assert!(!s.0.join("empty-home/.agents").exists());
    }
}
