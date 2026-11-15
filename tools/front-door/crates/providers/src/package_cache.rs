//! Bind native package-manager caches to host locations when an adapter uses
//! private HOME/XDG roots. Only cache and store paths cross this boundary.
//!
//! Recognized configuration is deliberately narrow: npm `cache` from its user
//! config or an explicit/trusted PREFIX config, and pnpm `cacheDir`/`storeDir`
//! from its project/global config. Only absolute literal global paths are
//! copied into environment selectors; unsupported or ambiguous paths warn and
//! remain under the package manager's own configuration handling.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Setting {
    Missing,
    Present(Option<PathBuf>),
}

#[derive(Default)]
struct ProjectSettings {
    npm_cache: bool,
    pnpm_cache: bool,
    pnpm_store: bool,
}

/// Repair native cache defaults without changing provider HOME/XDG state or
/// sandbox grants. Explicit environment and project settings retain priority.
pub(crate) fn bind_native(
    home: &Path,
    cwd: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
    env: &mut Vec<(OsString, OsString)>,
) {
    let selected_home = env_path(env, "HOME").unwrap_or_else(|| home.to_owned());
    let home_changed = selected_home != home;
    let cache_changed = xdg_path(env, "XDG_CACHE_HOME", &selected_home, ".cache")
        != xdg_path_host(host_var, "XDG_CACHE_HOME", home, ".cache");
    let data_changed =
        xdg_optional(env, "XDG_DATA_HOME") != xdg_optional_host(host_var, "XDG_DATA_HOME");
    let config_changed =
        pnpm_config_dir(env, &selected_home) != pnpm_config_dir_host(host_var, home);

    if !home_changed && !cache_changed && !data_changed && !config_changed {
        return;
    }

    let project = match project_settings(cwd, home) {
        Ok(settings) => settings,
        Err(()) => {
            warn_unsupported();
            return;
        }
    };

    if home_changed
        && !project.npm_cache
        && !preserve_explicit(&["npm_config_cache"], host_var, env)
    {
        match npm_global_cache(home, host_var) {
            Setting::Missing => set_env(env, "npm_config_cache", npm_default_cache(home)),
            Setting::Present(Some(path)) => set_env(env, "npm_config_cache", path),
            Setting::Present(None) => warn_unsupported(),
        }
    }

    if (home_changed || cache_changed || config_changed)
        && !project.pnpm_cache
        && !preserve_explicit(
            &["pnpm_config_cache_dir", "npm_config_cache_dir"],
            host_var,
            env,
        )
    {
        match pnpm_global_path(home, host_var, "cacheDir", "cache-dir") {
            Setting::Missing => set_env(
                env,
                "pnpm_config_cache_dir",
                pnpm_default_cache(home, host_var),
            ),
            Setting::Present(Some(path)) => set_env(env, "pnpm_config_cache_dir", path),
            Setting::Present(None) => warn_unsupported(),
        }
    }

    if (home_changed || data_changed || config_changed)
        && !project.pnpm_store
        && !preserve_explicit(
            &["pnpm_config_store_dir", "npm_config_store_dir"],
            host_var,
            env,
        )
    {
        match pnpm_global_path(home, host_var, "storeDir", "store-dir") {
            Setting::Missing
                if explicit_host_value(host_var, &["PNPM_HOME"]).is_some()
                    || env_value(env, "PNPM_HOME").is_some() => {}
            Setting::Missing => set_env(
                env,
                "pnpm_config_store_dir",
                pnpm_default_store(home, host_var),
            ),
            Setting::Present(Some(path)) => set_env(env, "pnpm_config_store_dir", path),
            Setting::Present(None) => warn_unsupported(),
        }
    }
}

fn project_settings(cwd: &Path, home: &Path) -> Result<ProjectSettings, ()> {
    let mut settings = ProjectSettings::default();
    let mut directory = Some(cwd);
    let mut depth = 0;
    while let Some(current) = directory {
        depth += 1;
        if depth > 64 || current == home {
            break;
        }
        if let Some(contents) = read_optional(&current.join(".npmrc"))? {
            settings.npm_cache |= ini_has_key(&contents, "cache");
            settings.pnpm_cache |= ini_has_key(&contents, "cache-dir");
            settings.pnpm_store |= ini_has_key(&contents, "store-dir");
        }
        if let Some(contents) = read_optional(&current.join("pnpm-workspace.yaml"))? {
            settings.pnpm_cache |= yaml_has_key(&contents, "cacheDir");
            settings.pnpm_store |= yaml_has_key(&contents, "storeDir");
        }
        if current.join(".git").exists() {
            break;
        }
        directory = current.parent();
    }
    Ok(settings)
}

fn npm_global_cache(home: &Path, host_var: &dyn Fn(&str) -> Option<OsString>) -> Setting {
    let user = match host_config_file(host_var, "npm_config_userconfig", home.join(".npmrc")) {
        Ok(path) => path,
        Err(()) => return Setting::Present(None),
    };
    match ini_setting(&user, "cache") {
        Ok(Setting::Missing) => {}
        Ok(setting) => return setting,
        Err(()) => return Setting::Present(None),
    }
    let global = if let Some(path) = explicit_host_value(host_var, &["npm_config_globalconfig"]) {
        Some(absolute_config_path(path))
    } else {
        match ini_setting(&user, "globalconfig") {
            Ok(Setting::Missing) => None,
            Ok(Setting::Present(Some(path))) => Some(Ok(path)),
            Ok(Setting::Present(None)) | Err(()) => Some(Err(())),
        }
    };
    let global = match global {
        Some(Ok(path)) => Some(path),
        Some(Err(())) => return Setting::Present(None),
        None => None,
    };
    let prefix = if let Some(prefix) = explicit_host_value(host_var, &["npm_config_prefix"]) {
        Some(absolute_config_path(prefix))
    } else {
        match ini_setting(&user, "prefix") {
            Ok(Setting::Missing) => None,
            Ok(Setting::Present(Some(path))) => Some(Ok(path)),
            Ok(Setting::Present(None)) | Err(()) => Some(Err(())),
        }
    };
    let prefix = match prefix {
        Some(Ok(path)) => Some(path),
        Some(Err(())) => return Setting::Present(None),
        None => None,
    };
    let global = global
        .or_else(|| prefix.map(|prefix| prefix.join("etc/npmrc")))
        .or_else(|| {
            crate::launch::homebrew_node_prefix(host_var).map(|prefix| prefix.join("etc/npmrc"))
        });
    if let Some(path) = global {
        return if path.is_absolute() {
            ini_setting(&path, "cache").unwrap_or(Setting::Present(None))
        } else {
            Setting::Present(None)
        };
    }
    // Unknown npm installations may have a configured PREFIX/etc/npmrc. Do not
    // run npm or silently install a higher-priority cache selector over it.
    Setting::Present(None)
}

fn absolute_config_path(value: OsString) -> Result<PathBuf, ()> {
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path).ok_or(())
}

fn pnpm_global_path(
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
    yaml_key: &str,
    legacy_ini_key: &str,
) -> Setting {
    let global = pnpm_config_dir_host(host_var, home).join("config.yaml");
    match yaml_setting(&global, yaml_key) {
        Ok(Setting::Missing) => {}
        Ok(setting) => return setting,
        Err(()) => return Setting::Present(None),
    }
    let user = match host_config_file(host_var, "npm_config_userconfig", home.join(".npmrc")) {
        Ok(path) => path,
        Err(()) => return Setting::Present(None),
    };
    ini_setting(&user, legacy_ini_key).unwrap_or(Setting::Present(None))
}

/// A present setting with no parsed path is unsupported or ambiguous, so the
/// caller leaves the package manager's configuration alone.
fn ini_setting(path: &Path, key: &str) -> Result<Setting, ()> {
    let Some(contents) = read_optional(path)? else {
        return Ok(Setting::Missing);
    };
    let mut found = None;
    for line in without_bom(&contents).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(key) {
            continue;
        }
        if found.is_some() {
            return Ok(Setting::Present(None));
        }
        found = Some(absolute_literal(value.trim()));
    }
    Ok(found.map_or(Setting::Missing, Setting::Present))
}

fn yaml_setting(path: &Path, key: &str) -> Result<Setting, ()> {
    let Some(contents) = read_optional(path)? else {
        return Ok(Setting::Missing);
    };
    let mut found = None;
    for line in without_bom(&contents).lines() {
        if line.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let line = strip_yaml_comment(line);
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name
            .trim()
            .trim_matches(['\'', '"'])
            .eq_ignore_ascii_case(key)
        {
            continue;
        }
        if found.is_some() {
            return Ok(Setting::Present(None));
        }
        found = Some(absolute_literal(value.trim()));
    }
    Ok(found.map_or(Setting::Missing, Setting::Present))
}

fn read_optional(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

fn without_bom(contents: &str) -> &str {
    contents.strip_prefix('\u{feff}').unwrap_or(contents)
}

fn absolute_literal(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if value.is_empty() || value.contains('$') || value.contains('\\') || value.contains('\n') {
        return None;
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match (quote, character) {
            (Some(current), found) if current == found => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '#') if index == 0 || line[..index].ends_with(' ') => return &line[..index],
            _ => {}
        }
    }
    line.trim_end()
}

fn host_config_file(
    host_var: &dyn Fn(&str) -> Option<OsString>,
    name: &str,
    default: PathBuf,
) -> Result<PathBuf, ()> {
    let Some(value) = explicit_host_value(host_var, &[name]) else {
        return Ok(default);
    };
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path).ok_or(())
}

fn preserve_explicit(
    selectors: &[&str],
    host_var: &dyn Fn(&str) -> Option<OsString>,
    env: &mut Vec<(OsString, OsString)>,
) -> bool {
    if selectors.iter().any(|key| env_value(env, key).is_some()) {
        return true;
    }
    if let Some(value) = explicit_host_value(host_var, selectors) {
        set_env(env, selectors[0], value);
        return true;
    }
    false
}

fn explicit_host_value(
    host_var: &dyn Fn(&str) -> Option<OsString>,
    names: &[&str],
) -> Option<OsString> {
    names.iter().find_map(|name| {
        host_var(name)
            .or_else(|| host_var(&name.to_ascii_uppercase()))
            .filter(|value| !value.is_empty())
    })
}

fn npm_default_cache(home: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        home.join("AppData/Local/npm-cache")
    } else {
        home.join(".npm")
    }
}

fn pnpm_default_cache(home: &Path, host_var: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(cache_home) = xdg_optional_host(host_var, "XDG_CACHE_HOME") {
        return cache_home.join("pnpm");
    }
    if cfg!(target_os = "windows") {
        home.join("AppData/Local/pnpm-cache")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Caches/pnpm")
    } else {
        home.join(".cache/pnpm")
    }
}

fn pnpm_default_store(home: &Path, host_var: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    if let Some(pnpm_home) = explicit_host_value(host_var, &["PNPM_HOME"]) {
        let path = PathBuf::from(pnpm_home);
        if path.is_absolute() {
            return path.join("store");
        }
    }
    if let Some(data_home) = xdg_optional_host(host_var, "XDG_DATA_HOME") {
        return data_home.join("pnpm/store");
    }
    if cfg!(target_os = "windows") {
        home.join("AppData/Local/pnpm/store")
    } else if cfg!(target_os = "macos") {
        home.join("Library/pnpm/store")
    } else {
        home.join(".local/share/pnpm/store")
    }
}

fn pnpm_config_dir(env: &[(OsString, OsString)], home: &Path) -> PathBuf {
    xdg_optional(env, "XDG_CONFIG_HOME")
        .map(|path| path.join("pnpm"))
        .unwrap_or_else(|| pnpm_config_default_dir(home))
}

fn pnpm_config_dir_host(host_var: &dyn Fn(&str) -> Option<OsString>, home: &Path) -> PathBuf {
    xdg_optional_host(host_var, "XDG_CONFIG_HOME")
        .map(|path| path.join("pnpm"))
        .unwrap_or_else(|| pnpm_config_default_dir(home))
}

fn pnpm_config_default_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        home.join("AppData/Local/pnpm/config")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Preferences/pnpm")
    } else {
        home.join(".config/pnpm")
    }
}

fn xdg_path(env: &[(OsString, OsString)], key: &str, home: &Path, fallback: &str) -> PathBuf {
    xdg_optional(env, key).unwrap_or_else(|| home.join(fallback))
}

fn xdg_path_host(
    host_var: &dyn Fn(&str) -> Option<OsString>,
    key: &str,
    home: &Path,
    fallback: &str,
) -> PathBuf {
    xdg_optional_host(host_var, key).unwrap_or_else(|| home.join(fallback))
}

fn xdg_optional(env: &[(OsString, OsString)], key: &str) -> Option<PathBuf> {
    env_value(env, key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn xdg_optional_host(host_var: &dyn Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    host_var(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn env_path(env: &[(OsString, OsString)], key: &str) -> Option<PathBuf> {
    env_value(env, key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_value<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsStr> {
    env.iter()
        .find(|(name, _)| {
            name.to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(key))
        })
        .map(|(_, value)| value.as_os_str())
}

fn set_env(env: &mut Vec<(OsString, OsString)>, key: &str, value: impl Into<OsString>) {
    env.retain(|(name, _)| {
        !name
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(key))
    });
    env.push((key.into(), value.into()));
}

fn ini_has_key(contents: &str, key: &str) -> bool {
    without_bom(contents).lines().any(|line| {
        let line = line.trim();
        !line.is_empty()
            && !line.starts_with('#')
            && !line.starts_with(';')
            && line
                .split_once('=')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(key))
    })
}

fn yaml_has_key(contents: &str, key: &str) -> bool {
    without_bom(contents).lines().any(|line| {
        !line.chars().next().is_some_and(char::is_whitespace)
            && strip_yaml_comment(line)
                .split_once(':')
                .is_some_and(|(name, _)| {
                    name.trim()
                        .trim_matches(['\'', '"'])
                        .eq_ignore_ascii_case(key)
                })
    })
}

fn warn_unsupported() {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: native package cache settings could not be resolved safely; configured paths were left unchanged"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn host_vars(values: &[(&str, &Path)]) -> HashMap<String, OsString> {
        values
            .iter()
            .map(|(name, path)| ((*name).into(), path.as_os_str().to_owned()))
            .collect()
    }

    fn var<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsStr> {
        env_value(env, key)
    }

    #[test]
    fn native_provider_home_and_xdg_variants_share_host_cache_paths() {
        let root = tempfile::tempdir().expect("fixture root");
        let home = root.path().join("host");
        let account = root.path().join("accounts/agy-work");
        let project = root.path().join("repo");
        let cache = root.path().join("xdg/cache");
        let data = root.path().join("xdg/data");
        let config = root.path().join("xdg/config");
        std::fs::create_dir_all(project.join(".git")).expect("project root");
        std::fs::create_dir_all(&home).expect("home");
        let host = host_vars(&[
            ("XDG_CACHE_HOME", &cache),
            ("XDG_DATA_HOME", &data),
            ("XDG_CONFIG_HOME", &config),
            ("NPM_CONFIG_PREFIX", &root.path().join("npm-prefix")),
        ]);
        let host_var = |name: &str| host.get(name).cloned();
        let expected = [
            home.join(".npm"),
            cache.join("pnpm"),
            data.join("pnpm/store"),
        ];

        for (adapter, mut env) in [
            ("agy", vec![("HOME".into(), account.into_os_string())]),
            (
                "opencode",
                vec![
                    ("HOME".into(), home.clone().into_os_string()),
                    (
                        "XDG_CACHE_HOME".into(),
                        root.path().join("attempt/cache").into(),
                    ),
                    (
                        "XDG_DATA_HOME".into(),
                        root.path().join("attempt/data").into(),
                    ),
                    (
                        "XDG_CONFIG_HOME".into(),
                        root.path().join("attempt/config").into(),
                    ),
                ],
            ),
            (
                "codex",
                vec![
                    ("HOME".into(), home.clone().into_os_string()),
                    ("XDG_CACHE_HOME".into(), cache.clone().into_os_string()),
                    ("XDG_DATA_HOME".into(), data.clone().into_os_string()),
                    ("XDG_CONFIG_HOME".into(), config.clone().into_os_string()),
                ],
            ),
            (
                "cursor",
                vec![
                    ("HOME".into(), home.clone().into_os_string()),
                    ("XDG_CACHE_HOME".into(), cache.clone().into_os_string()),
                    ("XDG_DATA_HOME".into(), data.clone().into_os_string()),
                    ("XDG_CONFIG_HOME".into(), config.clone().into_os_string()),
                ],
            ),
        ] {
            bind_native(&home, &project, &host_var, &mut env);
            let npm = env_path(&env, "npm_config_cache").unwrap_or_else(|| home.join(".npm"));
            let pnpm_cache = env_path(&env, "pnpm_config_cache_dir")
                .or_else(|| env_path(&env, "npm_config_cache_dir"))
                .unwrap_or_else(|| pnpm_default_cache(&home, &host_var));
            let pnpm_store = env_path(&env, "pnpm_config_store_dir")
                .or_else(|| env_path(&env, "npm_config_store_dir"))
                .unwrap_or_else(|| pnpm_default_store(&home, &host_var));
            assert_eq!([npm, pnpm_cache, pnpm_store], expected, "{adapter}");
            match adapter {
                "agy" => assert_eq!(
                    env_path(&env, "npm_config_cache").as_ref(),
                    Some(&expected[0]),
                    "Agy must bind npm cache while its account HOME differs"
                ),
                "opencode" => {
                    assert_eq!(
                        env_path(&env, "npm_config_cache"),
                        None,
                        "OpenCode keeps npm's host HOME default"
                    );
                    assert_eq!(
                        env_path(&env, "pnpm_config_cache_dir").as_ref(),
                        Some(&expected[1]),
                        "OpenCode must bind pnpm cache while XDG cache changes"
                    );
                    assert_eq!(
                        env_path(&env, "pnpm_config_store_dir").as_ref(),
                        Some(&expected[2]),
                        "OpenCode must bind pnpm store while XDG data changes"
                    );
                }
                "codex" | "cursor" => {
                    for key in [
                        "npm_config_cache",
                        "pnpm_config_cache_dir",
                        "pnpm_config_store_dir",
                    ] {
                        assert!(env_path(&env, key).is_none(), "{adapter} injected {key}");
                    }
                }
                _ => unreachable!("fixture adapter is covered above"),
            }
            if adapter == "opencode" {
                assert_eq!(
                    var(&env, "XDG_CONFIG_HOME"),
                    Some(root.path().join("attempt/config").as_os_str())
                );
                assert_eq!(
                    var(&env, "XDG_DATA_HOME"),
                    Some(root.path().join("attempt/data").as_os_str())
                );
            }
        }
    }

    #[test]
    fn native_cache_binding_keeps_host_and_project_settings_without_forwarding_auth() {
        let root = tempfile::tempdir().expect("fixture root");
        let home = root.path().join("host");
        let account = root.path().join("accounts/agy-work");
        let project = root.path().join("repo");
        let xdg_config = home.join("config");
        let npm_global = root.path().join("npm-global/etc/npmrc");
        std::fs::create_dir_all(project.join(".git")).expect("project root");
        std::fs::create_dir_all(xdg_config.join("pnpm")).expect("pnpm config");
        std::fs::create_dir_all(npm_global.parent().expect("npm global config parent"))
            .expect("npm global config");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(
            home.join(".npmrc"),
            format!(
                "globalconfig = {}\n//registry.invalid/:_authToken=secret-value\n",
                npm_global.display()
            ),
        )
        .expect("npm config");
        std::fs::write(&npm_global, "cache = /host/npm-cache\n_auth=secret-value\n")
            .expect("npm global config");
        std::fs::write(
            xdg_config.join("pnpm/config.yaml"),
            "cacheDir: /host/pnpm-cache\nstoreDir: /host/pnpm-store\n_auth: secret-value\n",
        )
        .expect("pnpm config");
        let host = host_vars(&[("XDG_CONFIG_HOME", &xdg_config)]);
        let host_var = |name: &str| host.get(name).cloned();

        let mut env = vec![("HOME".into(), account.clone().into_os_string())];
        bind_native(&home, &project, &host_var, &mut env);
        assert_eq!(
            var(&env, "npm_config_cache"),
            Some(OsStr::new("/host/npm-cache"))
        );
        assert_eq!(
            var(&env, "pnpm_config_cache_dir"),
            Some(OsStr::new("/host/pnpm-cache"))
        );
        assert_eq!(
            var(&env, "pnpm_config_store_dir"),
            Some(OsStr::new("/host/pnpm-store"))
        );
        assert!(env.iter().all(|(_, value)| value != "secret-value"));

        std::fs::write(
            project.join(".npmrc"),
            "cache=${HOME}/project-cache\n//registry.invalid/:_authToken=project-secret\n",
        )
        .expect("project npm config");
        std::fs::write(
            project.join("pnpm-workspace.yaml"),
            "cacheDir: ./project-cache\nstoreDir: ${STORE_DIR}\n",
        )
        .expect("project pnpm config");
        let mut project_env = vec![
            ("HOME".into(), account.clone().into_os_string()),
            (
                "XDG_CACHE_HOME".into(),
                root.path().join("attempt/cache").into(),
            ),
            (
                "XDG_DATA_HOME".into(),
                root.path().join("attempt/data").into(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                root.path().join("attempt/config").into(),
            ),
        ];
        bind_native(&home, &project, &host_var, &mut project_env);
        for key in [
            "npm_config_cache",
            "pnpm_config_cache_dir",
            "pnpm_config_store_dir",
        ] {
            assert!(var(&project_env, key).is_none(), "{key}");
        }
        assert!(
            project_env
                .iter()
                .all(|(_, value)| value != "project-secret")
        );

        let mut explicit = vec![
            ("HOME".into(), account.into_os_string()),
            ("npm_config_cache".into(), "/explicit/npm".into()),
            (
                "pnpm_config_cache_dir".into(),
                "/explicit/pnpm-cache".into(),
            ),
            (
                "pnpm_config_store_dir".into(),
                "/explicit/pnpm-store".into(),
            ),
        ];
        bind_native(&home, &project, &host_var, &mut explicit);
        assert_eq!(
            var(&explicit, "npm_config_cache"),
            Some(OsStr::new("/explicit/npm"))
        );
        assert_eq!(
            var(&explicit, "pnpm_config_cache_dir"),
            Some(OsStr::new("/explicit/pnpm-cache"))
        );
        assert_eq!(
            var(&explicit, "pnpm_config_store_dir"),
            Some(OsStr::new("/explicit/pnpm-store"))
        );
    }
}
