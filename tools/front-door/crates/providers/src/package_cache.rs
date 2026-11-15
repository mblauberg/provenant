//! Bind native package-manager caches to host locations when an adapter uses
//! private HOME/XDG roots. Only package cache/store paths, including Corepack's
//! package-manager download cache, cross this boundary.
//!
//! Recognised configuration is deliberately narrow: npm `cache` from its user
//! config or the first PATH-selected Node installation's `etc/npmrc`, and pnpm
//! `cacheDir`/`storeDir` from project config or global `config.yaml`. pnpm's
//! selected direct package or Corepack project/LKG metadata chooses one
//! environment selector family. npm cache paths may interpolate captured host
//! environment values. Legacy global pnpm `.npmrc` paths are ambiguous across
//! pnpm versions, so they warn and remain under the package manager's own
//! configuration handling.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Setting {
    Missing,
    Present(Option<PathBuf>),
}

#[derive(Default)]
struct ProjectSettings {
    npm_cache: bool,
    pnpm_cache: bool,
    pnpm_store: bool,
    legacy_pnpm_cache: bool,
    legacy_pnpm_store: bool,
    modern_pnpm_cache: bool,
    modern_pnpm_store: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PnpmSelectorFamily {
    Legacy,
    Current,
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

    // Corepack derives its package-manager download cache from HOME or
    // XDG_CACHE_HOME. Bind only COREPACK_HOME, keeping provider XDG state intact.
    if (home_changed || cache_changed) && !preserve_corepack_home(host_var, env) {
        match corepack_home(home, host_var) {
            Setting::Present(Some(path)) => set_env(env, "COREPACK_HOME", path),
            Setting::Missing | Setting::Present(None) => warn_unsupported(),
        }
    }

    let project = match project_settings(cwd, home) {
        Ok(settings) => settings,
        Err(()) => {
            warn_unsupported();
            return;
        }
    };
    if (project.legacy_pnpm_cache && !project.modern_pnpm_cache)
        || (project.legacy_pnpm_store && !project.modern_pnpm_store)
    {
        warn_legacy_project_pnpm();
    }

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

    let bind_cache = (home_changed || cache_changed || config_changed) && !project.pnpm_cache;
    let requested_store_binding =
        (home_changed || data_changed || config_changed) && !project.pnpm_store;
    let pnpm_home_explicit = explicit_host_value(host_var, &["PNPM_HOME"]).is_some()
        || env_value(env, "PNPM_HOME").is_some();
    let family = (bind_cache || requested_store_binding)
        .then(|| pnpm_selector_family(cwd, home, host_var))
        .flatten();
    let (cache_explicit, cache_selector_conflict) = if bind_cache {
        preserve_pnpm_selectors(
            family,
            "npm_config_cache_dir",
            "pnpm_config_cache_dir",
            host_var,
            env,
        )
    } else {
        (false, false)
    };
    let (store_explicit, store_selector_conflict) = if requested_store_binding {
        preserve_pnpm_selectors(
            family,
            "npm_config_store_dir",
            "pnpm_config_store_dir",
            host_var,
            env,
        )
    } else {
        (false, false)
    };
    let bind_store = requested_store_binding && (store_explicit || !pnpm_home_explicit);
    let warn_pnpm = cache_selector_conflict || store_selector_conflict;
    let mut legacy_selector_injected = false;

    if bind_cache && !cache_explicit {
        if let Some(family) = family {
            match pnpm_global_path(home, host_var, "cacheDir", "cache-dir") {
                Setting::Missing => {
                    set_pnpm_selector(
                        env,
                        family,
                        "npm_config_cache_dir",
                        "pnpm_config_cache_dir",
                        pnpm_default_cache(home, host_var),
                    );
                    legacy_selector_injected |= family == PnpmSelectorFamily::Legacy;
                }
                Setting::Present(Some(path)) => {
                    set_pnpm_selector(
                        env,
                        family,
                        "npm_config_cache_dir",
                        "pnpm_config_cache_dir",
                        path,
                    );
                    legacy_selector_injected |= family == PnpmSelectorFamily::Legacy;
                }
                Setting::Present(None) => warn_unsupported(),
            }
        }
    }

    if bind_store {
        if !store_explicit {
            if let Some(family) = family {
                match pnpm_global_path(home, host_var, "storeDir", "store-dir") {
                    Setting::Missing => {
                        set_pnpm_selector(
                            env,
                            family,
                            "npm_config_store_dir",
                            "pnpm_config_store_dir",
                            pnpm_default_store(home, host_var),
                        );
                        legacy_selector_injected |= family == PnpmSelectorFamily::Legacy;
                    }
                    Setting::Present(Some(path)) => {
                        set_pnpm_selector(
                            env,
                            family,
                            "npm_config_store_dir",
                            "pnpm_config_store_dir",
                            path,
                        );
                        legacy_selector_injected |= family == PnpmSelectorFamily::Legacy;
                    }
                    Setting::Present(None) => warn_unsupported(),
                }
            }
        }
    }

    if family.is_none() && (bind_cache || bind_store) {
        warn_pnpm_family_unresolved();
    } else if warn_pnpm {
        warn_pnpm_selector_conflict();
    }
    if legacy_selector_injected {
        warn_legacy_pnpm_environment_selector();
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
            settings.legacy_pnpm_cache |= ini_has_key(&contents, "cache-dir");
            settings.legacy_pnpm_store |= ini_has_key(&contents, "store-dir");
            settings.pnpm_cache |= settings.legacy_pnpm_cache;
            settings.pnpm_store |= settings.legacy_pnpm_store;
        }
        if let Some(contents) = read_optional(&current.join("pnpm-workspace.yaml"))? {
            settings.modern_pnpm_cache |= yaml_has_key(&contents, "cacheDir");
            settings.modern_pnpm_store |= yaml_has_key(&contents, "storeDir");
            settings.pnpm_cache |= settings.modern_pnpm_cache;
            settings.pnpm_store |= settings.modern_pnpm_store;
        }
        if current.join(".git").exists() {
            break;
        }
        directory = current.parent();
    }
    Ok(settings)
}

fn pnpm_selector_family(
    cwd: &Path,
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PnpmSelectorFamily> {
    let path = host_var("PATH")?;
    let selected = crate::launch::first_path_executable(&path, "pnpm")?;
    let selected = std::fs::canonicalize(selected).ok()?;
    if let Some(version) = direct_pnpm_version(&selected) {
        return pnpm_family_from_version(&version);
    }
    if !is_corepack_pnpm_shim(&selected) {
        return None;
    }

    match corepack_project_spec(cwd, host_var) {
        CorepackProjectSpec::Pnpm(reference) => pnpm_family_from_version(&reference),
        CorepackProjectSpec::Unknown => None,
        CorepackProjectSpec::NoPin => corepack_last_known_good(home, host_var)
            .and_then(|version| pnpm_family_from_version(&version)),
    }
}

fn direct_pnpm_version(executable: &Path) -> Option<String> {
    let bin = executable.parent()?;
    if bin.file_name() != Some(OsStr::new("bin")) {
        return None;
    }
    let package = bin.parent()?;
    let manifest = package_json(&package.join("package.json"))?;
    if manifest.get("name")?.as_str()? != "pnpm" {
        return None;
    }
    manifest.get("version")?.as_str().map(str::to_owned)
}

fn is_corepack_pnpm_shim(executable: &Path) -> bool {
    if executable.file_stem() != Some(OsStr::new("pnpm"))
        || executable.parent().and_then(Path::file_name) != Some(OsStr::new("dist"))
    {
        return false;
    }
    executable
        .parent()
        .and_then(Path::parent)
        .and_then(|package| package_json(&package.join("package.json")))
        .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_owned))
        .is_some_and(|name| name == "corepack")
}

fn package_json(path: &Path) -> Option<serde_json::Value> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn pnpm_family_from_version(version: &str) -> Option<PnpmSelectorFamily> {
    let version = version.split('+').next()?.trim();
    let major = version
        .split(|character| character == '.' || character == '-')
        .next()?;
    if major.is_empty() || !major.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let major = major.parse::<u64>().ok()?;
    Some(if major < 11 {
        PnpmSelectorFamily::Legacy
    } else {
        PnpmSelectorFamily::Current
    })
}

enum CorepackProjectSpec {
    Pnpm(String),
    NoPin,
    Unknown,
}

fn corepack_project_spec(
    cwd: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> CorepackProjectSpec {
    match host_var("COREPACK_ENABLE_PROJECT_SPEC").as_deref() {
        Some(value) if value == OsStr::new("0") => return CorepackProjectSpec::NoPin,
        Some(value) if value != OsStr::new("1") => return CorepackProjectSpec::Unknown,
        _ => {}
    }

    if host_var("COREPACK_ENV_FILE").is_some_and(|value| value.as_os_str() != OsStr::new("0")) {
        return CorepackProjectSpec::Unknown;
    }

    let mut current = Some(cwd);
    let mut depth = 0;
    while let Some(directory) = current {
        depth += 1;
        if depth > 64 {
            return CorepackProjectSpec::Unknown;
        }
        if corepack_env_file_present(directory, host_var) {
            return CorepackProjectSpec::Unknown;
        }
        if !directory
            .components()
            .any(|component| component.as_os_str() == OsStr::new("node_modules"))
        {
            match read_optional(&directory.join("package.json")) {
                Ok(Some(contents)) => {
                    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else {
                        return CorepackProjectSpec::Unknown;
                    };
                    if let Some(package_manager) = manifest.get("packageManager") {
                        let Some(package_manager) = package_manager.as_str() else {
                            return CorepackProjectSpec::Unknown;
                        };
                        return package_manager
                            .strip_prefix("pnpm@")
                            .map(|reference| CorepackProjectSpec::Pnpm(reference.to_owned()))
                            .unwrap_or(CorepackProjectSpec::Unknown);
                    }

                    if manifest
                        .get("devEngines")
                        .and_then(|value| value.get("packageManager"))
                        .is_some()
                    {
                        return CorepackProjectSpec::Unknown;
                    }
                }
                Ok(None) => {}
                Err(()) => return CorepackProjectSpec::Unknown,
            }
        }
        current = directory.parent();
    }

    CorepackProjectSpec::NoPin
}

fn corepack_env_file_present(
    project_dir: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> bool {
    let path = match host_var("COREPACK_ENV_FILE") {
        Some(value) if value.as_os_str() == OsStr::new("0") => return false,
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(".corepack.env"),
    };
    let path = if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    };
    match std::fs::metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

fn corepack_last_known_good(
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> Option<String> {
    let root = if let Some(root) = host_var("COREPACK_HOME") {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return None;
        }
        root
    } else {
        match corepack_home(home, host_var) {
            Setting::Present(Some(root)) => root,
            Setting::Missing | Setting::Present(None) => return None,
        }
    };
    let contents = read_optional(&root.join("lastKnownGood.json")).ok()??;
    serde_json::from_str::<serde_json::Value>(&contents)
        .ok()?
        .get("pnpm")?
        .as_str()
        .map(str::to_owned)
}

pub(super) fn npm_global_cache(
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> Setting {
    let user = match host_config_file(host_var, "npm_config_userconfig", home.join(".npmrc")) {
        Ok(path) => path,
        Err(()) => return Setting::Present(None),
    };
    match npm_cache_setting(&user, host_var) {
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
            crate::launch::selected_node_prefix(host_var).map(|prefix| prefix.join("etc/npmrc"))
        });
    if let Some(path) = global {
        return if path.is_absolute() {
            npm_cache_setting(&path, host_var).unwrap_or(Setting::Present(None))
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

pub(super) fn pnpm_global_path(
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
    // pnpm versions differ in whether they consume legacy `.npmrc` cache/store
    // keys. Do not promote one into a higher-priority environment selector
    // unless its applicability is established; leave pnpm to resolve it.
    match ini_setting(&user, legacy_ini_key) {
        Ok(Setting::Missing) => Setting::Missing,
        Ok(Setting::Present(_)) | Err(()) => Setting::Present(None),
    }
}

/// A present setting with no parsed path is unsupported or ambiguous, so the
/// caller leaves the package manager's configuration alone.
fn ini_setting(path: &Path, key: &str) -> Result<Setting, ()> {
    ini_setting_with(path, key, absolute_literal)
}

fn npm_cache_setting(
    path: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Setting, ()> {
    ini_setting_with(path, "cache", |value| npm_cache_path(value, host_var))
}

fn ini_setting_with(
    path: &Path,
    key: &str,
    parse_path: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Setting, ()> {
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
        found = Some(parse_path(value.trim()));
    }
    Ok(found.map_or(Setting::Missing, Setting::Present))
}

fn npm_cache_path(value: &str, host_var: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let value = value.trim();
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    let mut expanded = String::with_capacity(value.len());
    let mut remainder = value;
    loop {
        let Some(marker) = remainder.find('$') else {
            expanded.push_str(remainder);
            break;
        };
        if !remainder[marker..].starts_with("${") {
            return None;
        }
        expanded.push_str(&remainder[..marker]);
        let variable = &remainder[marker + 2..];
        let end = variable.find('}')?;
        let name = &variable[..end];
        if name.is_empty()
            || !name
                .chars()
                .enumerate()
                .all(|(index, character)| match character {
                    '_' => true,
                    character if character.is_ascii_alphabetic() => true,
                    character if index > 0 && character.is_ascii_digit() => true,
                    _ => false,
                })
        {
            return None;
        }
        let value = host_var(name)
            .or_else(|| host_var(&name.to_ascii_uppercase()))?
            .into_string()
            .ok()?;
        expanded.push_str(&value);
        remainder = &variable[end + 1..];
    }
    absolute_literal(&expanded)
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

/// Preserve configured pnpm selector values without moving them between
/// package-manager generations.
fn preserve_pnpm_selectors(
    family: Option<PnpmSelectorFamily>,
    legacy_selector: &str,
    current_selector: &str,
    host_var: &dyn Fn(&str) -> Option<OsString>,
    env: &mut Vec<(OsString, OsString)>,
) -> (bool, bool) {
    let legacy = selector_value(legacy_selector, host_var, env);
    let current = selector_value(current_selector, host_var, env);
    if env_value(env, legacy_selector).is_none() {
        if let Some(value) = host_var_value(host_var, legacy_selector) {
            set_env(env, legacy_selector, value);
        }
    }
    if env_value(env, current_selector).is_none() {
        if let Some(value) = host_var_value(host_var, current_selector) {
            set_env(env, current_selector, value);
        }
    }

    let conflict =
        match family {
            Some(PnpmSelectorFamily::Legacy) => current
                .is_some_and(|current| legacy.as_ref().is_none_or(|legacy| legacy != &current)),
            Some(PnpmSelectorFamily::Current) => legacy
                .is_some_and(|legacy| current.as_ref().is_none_or(|current| current != &legacy)),
            None => false,
        };
    (legacy.is_some() || current.is_some(), conflict)
}

fn selector_value(
    name: &str,
    host_var: &dyn Fn(&str) -> Option<OsString>,
    env: &[(OsString, OsString)],
) -> Option<OsString> {
    env_value(env, name)
        .map(OsStr::to_os_string)
        .or_else(|| host_var_value(host_var, name))
}

fn host_var_value(host_var: &dyn Fn(&str) -> Option<OsString>, name: &str) -> Option<OsString> {
    host_var(name).or_else(|| host_var(&name.to_ascii_uppercase()))
}

fn set_pnpm_selector(
    env: &mut Vec<(OsString, OsString)>,
    family: PnpmSelectorFamily,
    legacy_selector: &str,
    current_selector: &str,
    path: PathBuf,
) {
    let selector = match family {
        PnpmSelectorFamily::Legacy => legacy_selector,
        PnpmSelectorFamily::Current => current_selector,
    };
    set_env(env, selector, path);
}

fn preserve_corepack_home(
    host_var: &dyn Fn(&str) -> Option<OsString>,
    env: &mut Vec<(OsString, OsString)>,
) -> bool {
    if env_value(env, "COREPACK_HOME").is_some() {
        return true;
    }
    if let Some(value) = host_var("COREPACK_HOME") {
        // Corepack uses nullish precedence, so even an empty value is explicit.
        set_env(env, "COREPACK_HOME", value);
        return true;
    }
    false
}

pub(super) fn explicit_host_value(
    host_var: &dyn Fn(&str) -> Option<OsString>,
    names: &[&str],
) -> Option<OsString> {
    names.iter().find_map(|name| {
        host_var(name)
            .or_else(|| host_var(&name.to_ascii_uppercase()))
            .filter(|value| !value.is_empty())
    })
}

pub(super) fn npm_default_cache(home: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        home.join("AppData/Local/npm-cache")
    } else {
        home.join(".npm")
    }
}

fn corepack_home(home: &Path, host_var: &dyn Fn(&str) -> Option<OsString>) -> Setting {
    let base = if let Some(path) = host_var("XDG_CACHE_HOME") {
        PathBuf::from(path)
    } else if let Some(path) = host_var("LOCALAPPDATA") {
        PathBuf::from(path)
    } else {
        if !home.is_absolute() {
            return Setting::Present(None);
        }
        return Setting::Present(Some(home.join(".cache/node/corepack")));
    };
    if base.is_absolute() {
        Setting::Present(Some(base.join("node/corepack")))
    } else {
        Setting::Present(None)
    }
}

pub(super) fn pnpm_default_cache(
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> PathBuf {
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

pub(super) fn pnpm_default_store(
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> PathBuf {
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

fn warn_legacy_project_pnpm() {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: project .npmrc contains legacy pnpm cache settings; shared cache binding was skipped because support varies by pnpm version. A version that ignores these keys may use private HOME/XDG caches. Use cacheDir/storeDir in pnpm-workspace.yaml or supported global config.yaml for portable shared paths"
    );
}

fn warn_pnpm_family_unresolved() {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: the first PATH-selected pnpm version could not be identified safely; configured selectors were preserved and shared cache binding was skipped. Set the cache/store selector family supported by that pnpm version, or use cacheDir/storeDir in pnpm-workspace.yaml or supported global config.yaml"
    );
}

fn warn_pnpm_selector_conflict() {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: pnpm cache selectors disagree with the selected pnpm version; preserving configured values and skipping shared cache binding. Set the cache/store selector family supported by the selected pnpm version"
    );
}

fn warn_legacy_pnpm_environment_selector() {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "warning: shared cache binding for pnpm before version 11 uses npm_config_cache_dir/npm_config_store_dir; npm invoked in the same provider session may report these as unknown settings. Set cacheDir/storeDir in pnpm-workspace.yaml or supported global config.yaml to avoid environment selectors"
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

    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(path.parent().expect("executable parent"))
            .expect("executable directory");
        std::fs::write(path, b"fixture executable").expect("fixture executable file");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("fixture executable mode");
    }

    fn direct_pnpm_bin(root: &Path, version: &str) -> PathBuf {
        let package = root.join("pnpm-package");
        let bin = package.join("bin");
        std::fs::create_dir_all(&bin).expect("pnpm package bin");
        std::fs::write(
            package.join("package.json"),
            format!(r#"{{"name":"pnpm","version":"{version}"}}"#),
        )
        .expect("pnpm package metadata");
        executable(&bin.join("pnpm"));
        bin
    }

    fn corepack_pnpm_bin(root: &Path) -> PathBuf {
        use std::os::unix::fs::symlink;

        let package = root.join("node_modules/corepack");
        let dist = package.join("dist");
        let bin = root.join("shims");
        std::fs::create_dir_all(&dist).expect("Corepack package dist");
        std::fs::create_dir_all(&bin).expect("Corepack shim directory");
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"corepack","version":"0.34.6"}"#,
        )
        .expect("Corepack package metadata");
        executable(&dist.join("pnpm.js"));
        symlink(dist.join("pnpm.js"), bin.join("pnpm")).expect("Corepack pnpm shim");
        bin
    }

    // Spec: docs/specs/supervisor.md#native-environment-and-sandbox-tool-stores
    #[test]
    fn corepack_home_uses_host_cache_precedence() {
        let home = Path::new("/fixture/host-home");
        let empty = |_: &str| None;
        assert_eq!(
            corepack_home(home, &empty),
            Setting::Present(Some(home.join(".cache/node/corepack")))
        );

        let cache = Path::new("/fixture/xdg/cache");
        let local_app_data = Path::new("/fixture/local-app-data");
        let host = host_vars(&[("XDG_CACHE_HOME", cache), ("LOCALAPPDATA", local_app_data)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            corepack_home(home, &host_var),
            Setting::Present(Some(cache.join("node/corepack")))
        );

        let host = host_vars(&[("LOCALAPPDATA", local_app_data)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            corepack_home(home, &host_var),
            Setting::Present(Some(local_app_data.join("node/corepack")))
        );

        let explicit_empty = |name: &str| (name == "COREPACK_HOME").then(OsString::new);
        let mut env = Vec::new();
        assert!(preserve_corepack_home(&explicit_empty, &mut env));
        assert_eq!(
            var(&env, "COREPACK_HOME"),
            Some(OsStr::new("")),
            "Corepack treats an explicitly empty home as a value"
        );
    }

    #[test]
    fn pnpm_selector_family_uses_selected_package_and_corepack_metadata() {
        let root = tempfile::tempdir().expect("fixture root");
        let home = root.path().join("host-home");
        let project = root.path().join("repo");
        std::fs::create_dir_all(project.join(".git")).expect("project root");

        let legacy_bin = direct_pnpm_bin(&root.path().join("legacy"), "10.33.2");
        let current_bin = direct_pnpm_bin(&root.path().join("current"), "12.16.1");
        let path = std::env::join_paths([&legacy_bin, &current_bin]).expect("pnpm fixture PATH");
        let host = HashMap::from([("PATH".to_owned(), path)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            pnpm_selector_family(&project, &home, &host_var),
            Some(PnpmSelectorFamily::Legacy),
            "the first PATH-selected direct pnpm package determines selector family"
        );

        let path = std::env::join_paths([&current_bin, &legacy_bin]).expect("reversed PATH");
        let host = HashMap::from([("PATH".to_owned(), path)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            pnpm_selector_family(&project, &home, &host_var),
            Some(PnpmSelectorFamily::Current)
        );

        let corepack_bin = corepack_pnpm_bin(root.path());
        let project_package = project.join("package.json");
        std::fs::write(
            &project_package,
            r#"{"packageManager":"pnpm@10.33.2+sha512.fixture"}"#,
        )
        .expect("project package manager pin");
        let lkg = home.join(".cache/node/corepack/lastKnownGood.json");
        std::fs::create_dir_all(lkg.parent().expect("Corepack home")).expect("Corepack cache");
        std::fs::write(&lkg, r#"{"pnpm":"12.16.1+sha512.fixture"}"#).expect("Corepack LKG");
        let path = std::env::join_paths([&corepack_bin]).expect("Corepack PATH");
        let host = HashMap::from([("PATH".to_owned(), path)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            pnpm_selector_family(&project, &home, &host_var),
            Some(PnpmSelectorFamily::Legacy),
            "Corepack's project packageManager pin takes precedence over LKG"
        );

        let host = HashMap::from([
            (
                "PATH".to_owned(),
                std::env::join_paths([&corepack_bin]).unwrap(),
            ),
            ("COREPACK_ENABLE_PROJECT_SPEC".to_owned(), "0".into()),
        ]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            pnpm_selector_family(&project, &home, &host_var),
            Some(PnpmSelectorFamily::Current),
            "Corepack's explicit project-spec disable uses the host LKG"
        );

        std::fs::write(&project_package, r#"{"name":"fixture"}"#).expect("remove project pin");
        let path = std::env::join_paths([&corepack_bin]).expect("Corepack PATH");
        let host = HashMap::from([("PATH".to_owned(), path)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            pnpm_selector_family(&project, &home, &host_var),
            Some(PnpmSelectorFamily::Current),
            "an unpinned Corepack shim follows host lastKnownGood.json"
        );

        let unknown_bin = root.path().join("unsupported-shim/bin");
        executable(&unknown_bin.join("pnpm"));
        let path = std::env::join_paths([&unknown_bin]).expect("unsupported shim PATH");
        let host = HashMap::from([("PATH".to_owned(), path)]);
        let host_var = |name: &str| host.get(name).cloned();
        assert_eq!(
            pnpm_selector_family(&project, &home, &host_var),
            None,
            "unknown shims do not select a cache environment family"
        );
        let mut env = vec![("HOME".into(), root.path().join("account").into_os_string())];
        bind_native(&home, &project, &host_var, &mut env);
        for selector in [
            "npm_config_cache_dir",
            "pnpm_config_cache_dir",
            "npm_config_store_dir",
            "pnpm_config_store_dir",
        ] {
            assert!(
                var(&env, selector).is_none(),
                "unknown pnpm shim must not receive guessed selector {selector}"
            );
        }
        let mut explicit = vec![
            ("HOME".into(), root.path().join("account").into_os_string()),
            ("pnpm_config_cache_dir".into(), "/configured/cache".into()),
        ];
        bind_native(&home, &project, &host_var, &mut explicit);
        assert_eq!(
            var(&explicit, "pnpm_config_cache_dir"),
            Some(OsStr::new("/configured/cache"))
        );
        assert!(var(&explicit, "npm_config_cache_dir").is_none());
    }

    // Spec: docs/specs/supervisor.md#native-environment-and-sandbox-tool-stores
    #[test]
    fn npm_cache_binding_follows_first_path_selected_node_installation() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("fixture root");
        let base = root.path().canonicalize().expect("canonical fixture root");
        let home = base.join("host-home");
        let account = base.join("account-home");
        let project = base.join("repo");
        let selected = base.join("nvm/versions/node/v24.0.1");
        let later = base.join("homebrew");
        let selected_bin = selected.join("bin");
        let later_cellar = later.join("Cellar/node@22/22.18.0");
        let later_cellar_bin = later_cellar.join("bin");
        let later_bin = later.join("opt/node@22/bin");
        std::fs::create_dir_all(&home).expect("host home");
        std::fs::create_dir_all(&account).expect("account home");
        std::fs::create_dir_all(project.join(".git")).expect("project root");
        executable(&selected_bin.join("node"));
        executable(&selected_bin.join("npm"));
        executable(&later_cellar_bin.join("node"));
        executable(&later_cellar_bin.join("npm"));
        std::fs::create_dir_all(later.join("opt")).expect("Homebrew opt directory");
        symlink(&later_cellar, later.join("opt/node@22")).expect("Homebrew opt formula link");
        std::fs::create_dir_all(later.join("etc")).expect("later global config");
        std::fs::write(later.join("etc/npmrc"), "cache=/later-private-cache\n")
            .expect("later package config");
        let pnpm_bin = direct_pnpm_bin(&base.join("pnpm10"), "10.33.2");
        let path =
            std::env::join_paths([&selected_bin, &pnpm_bin, &later_bin]).expect("fixture PATH");
        let host = HashMap::from([("PATH".to_owned(), path)]);
        let host_var = |name: &str| host.get(name).cloned();

        assert_eq!(
            crate::launch::selected_node_prefix(&host_var).as_deref(),
            Some(selected.as_path()),
            "later Node installations must not supply npm config"
        );
        let mut env = vec![("HOME".into(), account.clone().into_os_string())];
        bind_native(&home, &project, &host_var, &mut env);
        let shared_default = home.join(".npm");
        assert_eq!(
            var(&env, "npm_config_cache"),
            Some(shared_default.as_os_str()),
            "an unconfigured NVM install uses the shared host default, not account HOME"
        );
        let shared_pnpm_cache = pnpm_default_cache(&home, &host_var);
        let shared_pnpm_store = pnpm_default_store(&home, &host_var);
        assert_eq!(
            var(&env, "npm_config_cache_dir"),
            Some(shared_pnpm_cache.as_os_str()),
            "pnpm 10 receives its legacy cache selector"
        );
        assert!(var(&env, "pnpm_config_cache_dir").is_none());
        assert_eq!(
            var(&env, "npm_config_store_dir"),
            Some(shared_pnpm_store.as_os_str()),
            "pnpm 10 receives its legacy store selector"
        );
        assert!(var(&env, "pnpm_config_store_dir").is_none());

        std::fs::create_dir_all(selected.join("etc")).expect("selected global config");
        std::fs::write(selected.join("etc/npmrc"), "cache=/selected-cache\n")
            .expect("selected package config");
        let mut configured = vec![("HOME".into(), account.into_os_string())];
        bind_native(&home, &project, &host_var, &mut configured);
        assert_eq!(
            var(&configured, "npm_config_cache"),
            Some(OsStr::new("/selected-cache")),
            "the selected NVM installation's global cache remains authoritative"
        );

        let mut explicit_current = vec![
            ("HOME".into(), home.into_os_string()),
            (
                "pnpm_config_cache_dir".into(),
                "/explicit/current-cache".into(),
            ),
        ];
        bind_native(
            &base.join("host-home"),
            &project,
            &host_var,
            &mut explicit_current,
        );
        assert_eq!(
            var(&explicit_current, "pnpm_config_cache_dir"),
            Some(OsStr::new("/explicit/current-cache"))
        );
        assert!(var(&explicit_current, "npm_config_cache_dir").is_none());
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
        let expected_corepack = cache.join("node/corepack");
        std::fs::create_dir_all(project.join(".git")).expect("project root");
        std::fs::create_dir_all(&home).expect("home");
        let mut host = host_vars(&[
            ("XDG_CACHE_HOME", &cache),
            ("XDG_DATA_HOME", &data),
            ("XDG_CONFIG_HOME", &config),
            ("NPM_CONFIG_PREFIX", &root.path().join("npm-prefix")),
        ]);
        let pnpm_bin = direct_pnpm_bin(&root.path().join("pnpm10"), "10.33.2");
        host.insert(
            "PATH".to_owned(),
            std::env::join_paths([&pnpm_bin]).expect("pnpm fixture PATH"),
        );
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
                "agy" => {
                    assert_eq!(
                        env_path(&env, "npm_config_cache").as_ref(),
                        Some(&expected[0]),
                        "Agy must bind npm cache while its account HOME differs"
                    );
                    assert_eq!(
                        env_path(&env, "npm_config_cache_dir").as_ref(),
                        Some(&expected[1]),
                        "Agy must bind pnpm 10's cache selector"
                    );
                    assert_eq!(
                        env_path(&env, "npm_config_store_dir").as_ref(),
                        Some(&expected[2]),
                        "Agy must bind pnpm 10's store selector"
                    );
                    assert!(env_path(&env, "pnpm_config_cache_dir").is_none());
                    assert!(env_path(&env, "pnpm_config_store_dir").is_none());
                    assert_eq!(
                        env_path(&env, "COREPACK_HOME").as_ref(),
                        Some(&expected_corepack),
                        "Agy must keep Corepack downloads in the shared host XDG cache"
                    );
                }
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
                    assert_eq!(
                        env_path(&env, "npm_config_cache_dir").as_ref(),
                        Some(&expected[1]),
                        "OpenCode must bind pnpm 10's cache selector"
                    );
                    assert_eq!(
                        env_path(&env, "npm_config_store_dir").as_ref(),
                        Some(&expected[2]),
                        "OpenCode must bind pnpm 10's store selector"
                    );
                    assert!(env_path(&env, "pnpm_config_cache_dir").is_none());
                    assert!(env_path(&env, "pnpm_config_store_dir").is_none());
                    assert_eq!(
                        env_path(&env, "COREPACK_HOME").as_ref(),
                        Some(&expected_corepack),
                        "OpenCode keeps private XDG state while Corepack uses the host cache"
                    );
                }
                "codex" | "cursor" => {
                    for key in [
                        "npm_config_cache",
                        "pnpm_config_cache_dir",
                        "npm_config_cache_dir",
                        "pnpm_config_store_dir",
                        "npm_config_store_dir",
                    ] {
                        assert!(env_path(&env, key).is_none(), "{adapter} injected {key}");
                    }
                    assert!(
                        env_path(&env, "COREPACK_HOME").is_none(),
                        "{adapter} must retain Corepack's host default"
                    );
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
        let explicit_corepack = root.path().join("host/corepack-cache");
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
        let mut host = host_vars(&[
            ("HOME", &home),
            ("XDG_CONFIG_HOME", &xdg_config),
            ("COREPACK_HOME", &explicit_corepack),
        ]);
        let pnpm_bin = direct_pnpm_bin(&root.path().join("pnpm12"), "12.16.1");
        host.insert(
            "PATH".to_owned(),
            std::env::join_paths([&pnpm_bin]).expect("pnpm fixture PATH"),
        );
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
        assert!(var(&env, "npm_config_cache_dir").is_none());
        assert!(var(&env, "npm_config_store_dir").is_none());
        assert_eq!(
            var(&env, "COREPACK_HOME"),
            Some(explicit_corepack.as_os_str()),
            "explicit host Corepack cache takes precedence"
        );
        assert!(env.iter().all(|(_, value)| value != "secret-value"));

        std::fs::write(
            home.join(".npmrc"),
            "cache=${HOME}/npm-cache\n//registry.invalid/:_authToken=secret-value\n",
        )
        .expect("interpolated npm cache config");
        let mut interpolated = vec![("HOME".into(), account.clone().into_os_string())];
        bind_native(&home, &project, &host_var, &mut interpolated);
        assert_eq!(
            var(&interpolated, "npm_config_cache"),
            Some(home.join("npm-cache").as_os_str()),
            "npm interpolation resolves with host HOME while provider HOME is private"
        );
        assert!(
            interpolated
                .iter()
                .all(|(_, value)| value != "secret-value")
        );

        std::fs::remove_file(xdg_config.join("pnpm/config.yaml")).expect("remove pnpm config");
        std::fs::write(
            home.join(".npmrc"),
            "cache = /host/npm-cache\ncache-dir = /legacy/pnpm-cache\nstore-dir = /legacy/pnpm-store\n",
        )
        .expect("legacy pnpm config");
        let mut legacy_env = vec![("HOME".into(), account.clone().into_os_string())];
        bind_native(&home, &project, &host_var, &mut legacy_env);
        assert_eq!(
            var(&legacy_env, "npm_config_cache"),
            Some(OsStr::new("/host/npm-cache")),
            "npm's own user cache remains supported"
        );
        assert!(var(&legacy_env, "pnpm_config_cache_dir").is_none());
        assert!(var(&legacy_env, "pnpm_config_store_dir").is_none());
        assert!(var(&legacy_env, "npm_config_cache_dir").is_none());
        assert!(var(&legacy_env, "npm_config_store_dir").is_none());

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
            "npm_config_cache_dir",
            "pnpm_config_store_dir",
            "npm_config_store_dir",
        ] {
            assert!(var(&project_env, key).is_none(), "{key}");
        }
        assert_eq!(
            var(&project_env, "COREPACK_HOME"),
            Some(explicit_corepack.as_os_str()),
            "project XDG drift cannot replace an explicit host Corepack cache"
        );
        assert!(
            project_env
                .iter()
                .all(|(_, value)| value != "project-secret")
        );

        std::fs::remove_file(project.join("pnpm-workspace.yaml"))
            .expect("remove modern project pnpm config");
        std::fs::write(
            project.join(".npmrc"),
            "cache=${HOME}/project-cache\ncache-dir=/legacy/project-cache\nstore-dir=/legacy/project-store\n",
        )
        .expect("legacy project pnpm config");
        let mut legacy_project_env = project_env.clone();
        bind_native(&home, &project, &host_var, &mut legacy_project_env);
        for key in [
            "pnpm_config_cache_dir",
            "npm_config_cache_dir",
            "pnpm_config_store_dir",
            "npm_config_store_dir",
        ] {
            assert!(var(&legacy_project_env, key).is_none(), "{key}");
        }

        let mut explicit_project = vec![
            ("HOME".into(), account.clone().into_os_string()),
            (
                "pnpm_config_cache_dir".into(),
                "/explicit/project-cache-override".into(),
            ),
        ];
        bind_native(&home, &project, &host_var, &mut explicit_project);
        assert_eq!(
            var(&explicit_project, "pnpm_config_cache_dir"),
            Some(OsStr::new("/explicit/project-cache-override"))
        );
        assert!(var(&explicit_project, "npm_config_cache_dir").is_none());

        std::fs::remove_file(project.join(".npmrc")).expect("remove project npm config");
        let mut explicit = vec![
            ("HOME".into(), account.clone().into_os_string()),
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
        assert!(var(&explicit, "npm_config_cache_dir").is_none());
        assert!(var(&explicit, "npm_config_store_dir").is_none());

        let mut old_prefix = vec![
            ("HOME".into(), account.clone().into_os_string()),
            ("npm_config_cache_dir".into(), "/explicit/old-cache".into()),
            ("npm_config_store_dir".into(), "/explicit/old-store".into()),
        ];
        bind_native(&home, &project, &host_var, &mut old_prefix);
        assert!(var(&old_prefix, "pnpm_config_cache_dir").is_none());
        assert!(var(&old_prefix, "pnpm_config_store_dir").is_none());
        assert_eq!(
            var(&old_prefix, "npm_config_cache_dir"),
            Some(OsStr::new("/explicit/old-cache"))
        );
        assert_eq!(
            var(&old_prefix, "npm_config_store_dir"),
            Some(OsStr::new("/explicit/old-store"))
        );

        let mut conflicting_prefixes = vec![
            ("HOME".into(), account.into_os_string()),
            ("pnpm_config_cache_dir".into(), "/configured/current".into()),
            ("npm_config_cache_dir".into(), "/configured/legacy".into()),
        ];
        bind_native(&home, &project, &host_var, &mut conflicting_prefixes);
        assert_eq!(
            var(&conflicting_prefixes, "pnpm_config_cache_dir"),
            Some(OsStr::new("/configured/current"))
        );
        assert_eq!(
            var(&conflicting_prefixes, "npm_config_cache_dir"),
            Some(OsStr::new("/configured/legacy"))
        );
    }
}
