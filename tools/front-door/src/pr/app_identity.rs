//! GitHub App identity for Provenant's own automation (landing, CI observation,
//! status reads). With an App configured for the repository, those `gh` calls
//! carry a short-lived installation token and so draw on the App's own
//! rate-limit bucket. Without one they use the user's `gh` login and one
//! warning per process says so.
//!
//! The private key is read only at mint time, from the macOS Keychain or from
//! a mode-0600 file outside any repository. It is handed to `openssl` on
//! stdin, never written to disk, logged, placed in an environment or given to
//! a lane. The App identity never acts as a reviewer: see [`app_may_run`].
use provenant_config::GithubAppConfig;
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

type Result<T> = std::result::Result<T, String>;

/// A token is reused until this long before it expires.
const REFRESH_MARGIN_SECONDS: i64 = 300;
/// Keychain service name the owner stores the key under.
pub(crate) const KEYCHAIN_SERVICE: &str = "provenant-github-app";
/// Permissions requested for an installation token: what landing needs.
const PERMISSIONS: [(&str, &str); 5] = [
    ("contents", "write"),
    ("pull_requests", "write"),
    ("issues", "read"),
    ("actions", "read"),
    ("statuses", "read"),
];

pub(crate) struct Minted {
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// The effects identity selection needs, so tests run against a fake forge
/// and a generated key.
pub(crate) trait Backend: Send + Sync {
    fn now(&self) -> i64;
    fn read_key(&self, config: &GithubAppConfig) -> Result<Vec<u8>>;
    fn sign(&self, key: &[u8], message: &[u8]) -> Result<Vec<u8>>;
    /// Installation id for the project's repository, from an App JWT.
    fn find_installation(&self, jwt: &str) -> Result<u64>;
    fn mint(&self, jwt: &str, installation: u64) -> Result<Minted>;
    fn warn(&self, message: &str);
}

struct Cached {
    token: String,
    expires_at: i64,
}

pub(crate) struct Identity<B: Backend> {
    config: Option<GithubAppConfig>,
    backend: B,
    cache: Mutex<Option<Cached>>,
    warned: Mutex<bool>,
}

impl<B: Backend> Identity<B> {
    pub(crate) fn new(config: Option<GithubAppConfig>, backend: B) -> Self {
        Self {
            config,
            backend,
            cache: Mutex::new(None),
            warned: Mutex::new(false),
        }
    }

    fn warn_once(&self, message: &str) {
        if let Ok(mut warned) = self.warned.lock()
            && !*warned
        {
            *warned = true;
            self.backend.warn(message);
        }
    }

    /// The token to run `argv` with, or `None` for the user's login.
    pub(crate) fn token_for(&self, argv: &[&str]) -> Option<String> {
        if argv.first() != Some(&"gh") {
            return None;
        }
        let Some(config) = &self.config else {
            self.warn_once(
                "no GitHub App configured; Provenant automation shares the user's GitHub request quota",
            );
            return None;
        };
        if !app_may_run(argv) {
            return None;
        }
        match self.installation_token(config) {
            Ok(token) => Some(token),
            Err(cause) => {
                self.warn_once(&format!(
                    "GitHub App token unavailable ({cause}); using the user's GitHub login"
                ));
                None
            }
        }
    }

    fn installation_token(&self, config: &GithubAppConfig) -> Result<String> {
        let mut cache = self.cache.lock().map_err(|_| "token cache poisoned")?;
        let now = self.backend.now();
        if let Some(cached) = cache.as_ref()
            && cached.expires_at - REFRESH_MARGIN_SECONDS > now
        {
            return Ok(cached.token.clone());
        }
        let key = self.backend.read_key(config)?;
        let jwt = app_jwt(&self.backend, &key, config.app_id, now);
        drop(key);
        let jwt = jwt?;
        let installation = match config.installation_id {
            Some(id) => id,
            None => self.backend.find_installation(&jwt)?,
        };
        let minted = self.backend.mint(&jwt, installation)?;
        *cache = Some(Cached {
            token: minted.token.clone(),
            expires_at: minted.expires_at,
        });
        Ok(minted.token)
    }
}

fn app_jwt(backend: &dyn Backend, key: &[u8], app_id: u64, now: i64) -> Result<String> {
    let header = base64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = base64url(
        format!(
            r#"{{"iat":{},"exp":{},"iss":"{app_id}"}}"#,
            now - 60,
            now + 540
        )
        .as_bytes(),
    );
    let message = format!("{header}.{claims}");
    let signature = backend.sign(key, message.as_bytes())?;
    Ok(format!("{message}.{}", base64url(&signature)))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// Whether the App identity may run this `gh` command. The App reads and
/// merges; it never posts a review verdict, approval or comment, so anything
/// not listed stays on the user's identity.
pub(crate) fn app_may_run(argv: &[&str]) -> bool {
    let args = argv.get(1..).unwrap_or_default();
    let Some(&first) = args.first() else {
        return false;
    };
    let rest = &args[1..];
    match first {
        "api" => api_may_run(rest),
        "pr" => matches!(
            rest.first().copied(),
            Some("view" | "list" | "checks" | "status" | "diff" | "merge")
        ),
        "repo" => rest.first() == Some(&"view"),
        "run" => matches!(rest.first().copied(), Some("view" | "list")),
        _ => false,
    }
}

fn api_may_run(args: &[&str]) -> bool {
    let joined = args.join(" ").to_ascii_lowercase();
    if joined.contains("graphql") {
        if !joined.contains("mutation") {
            return true;
        }
        return joined.contains("mergepullrequest")
            && !["review", "comment", "approve"]
                .iter()
                .any(|word| joined.contains(word));
    }
    let mut method = None;
    let mut body = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match *arg {
            "-X" | "--method" => method = iter.next().map(|m| m.to_ascii_uppercase()),
            "-f" | "-F" | "--field" | "--raw-field" | "--input" => body = true,
            other => {
                if let Some(value) = other.strip_prefix("--method=") {
                    method = Some(value.to_ascii_uppercase());
                } else if other.starts_with("--field=")
                    || other.starts_with("--raw-field=")
                    || other.starts_with("--input=")
                {
                    body = true;
                }
            }
        }
    }
    let method = method.unwrap_or_else(|| if body { "POST" } else { "GET" }.to_owned());
    match method.as_str() {
        "GET" | "HEAD" => true,
        "PUT" => args.iter().any(|arg| {
            arg.contains("/pulls/") && (arg.ends_with("/merge") || arg.ends_with("/merge-async"))
        }),
        _ => false,
    }
}

/// The key bytes from a file the owner keeps outside every repository, mode
/// 0600 exactly.
pub(crate) fn read_key_file(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    if !path.is_absolute() {
        return Err("private_key_path must be absolute".into());
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| "private key file unreadable")?;
    if canonical
        .ancestors()
        .skip(1)
        .any(|dir| dir.join(".git").exists())
    {
        return Err("private key file is inside a repository".into());
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| "private key file unreadable")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("private key file must be a regular file with mode 0600".into());
    }
    std::fs::read(&canonical).map_err(|_| "private key file unreadable".into())
}

/// `security -w` prints a multi-line secret as hex; accept either form.
pub(crate) fn decode_keychain_output(output: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(output);
    let trimmed = text.trim();
    if !trimmed.contains("-----BEGIN")
        && trimmed.len() % 2 == 0
        && !trimmed.is_empty()
        && trimmed.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let bytes = trimmed.as_bytes();
        let decoded: Option<Vec<u8>> = bytes
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
            .collect();
        if let Some(decoded) = decoded {
            return decoded;
        }
    }
    trimmed.as_bytes().to_vec()
}

fn gh_env(token: &str) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| name != "GH_TOKEN" && name != "GITHUB_TOKEN")
        .collect();
    env.push(("GH_TOKEN".into(), token.into()));
    env
}

/// Real effects: Keychain or file, `openssl`, and `gh api`.
pub(crate) struct SystemBackend {
    project: PathBuf,
}

impl SystemBackend {
    fn run(
        &self,
        argv: &[&str],
        env: Option<&[(OsString, OsString)]>,
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
        let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
        let mut options =
            ExecOptions::new(&args, &self.project, std::time::Duration::from_secs(30));
        options.env = env;
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        let output = SystemExecutor::new(1)
            .map_err(|_| "cannot start executor")?
            .execute(options)
            .map_err(|_| format!("cannot run {}", argv[0]))?;
        if output.termination != provenant_os::exec::Termination::Exit(0) || output.timed_out {
            return Err(format!("{} failed", argv[0]));
        }
        Ok(output.stdout.bytes)
    }

    fn gh_json(&self, jwt: &str, argv: &[&str]) -> Result<serde_json::Value> {
        let env = gh_env(jwt);
        let bytes = self.run(argv, Some(&env), None)?;
        serde_json::from_slice(&bytes).map_err(|_| "GitHub returned invalid JSON".into())
    }
}

impl Backend for SystemBackend {
    #[allow(clippy::disallowed_methods, reason = "token expiry needs wall time")]
    fn now(&self) -> i64 {
        jiff::Timestamp::now().as_second()
    }

    fn read_key(&self, config: &GithubAppConfig) -> Result<Vec<u8>> {
        if let Some(path) = &config.private_key_path {
            return read_key_file(path);
        }
        let account = config
            .keychain_account
            .as_deref()
            .ok_or("neither private_key_path nor keychain_account is configured")?;
        let output = self
            .run(
                &[
                    "/usr/bin/security",
                    "find-generic-password",
                    "-s",
                    KEYCHAIN_SERVICE,
                    "-a",
                    account,
                    "-w",
                ],
                None,
                None,
            )
            .map_err(|_| "Keychain item unavailable")?;
        Ok(decode_keychain_output(&output))
    }

    fn sign(&self, key: &[u8], message: &[u8]) -> Result<Vec<u8>> {
        // The key goes in on stdin; only the (public) message touches disk.
        let path = std::env::temp_dir().join(format!(
            "provenant-jwt-{}-{}",
            std::process::id(),
            self.now()
        ));
        std::fs::write(&path, message).map_err(|_| "cannot stage JWT message")?;
        let text = path.to_string_lossy().into_owned();
        let signature = self.run(
            &[
                "/usr/bin/openssl",
                "dgst",
                "-sha256",
                "-sign",
                "/dev/stdin",
                "-binary",
                &text,
            ],
            None,
            Some(key),
        );
        let _ = std::fs::remove_file(&path);
        signature.map_err(|_| "JWT signing failed".into())
    }

    fn find_installation(&self, jwt: &str) -> Result<u64> {
        let value = self.gh_json(jwt, &["gh", "api", "repos/{owner}/{repo}/installation"])?;
        value["id"]
            .as_u64()
            .ok_or_else(|| "App is not installed on this repository".into())
    }

    fn mint(&self, jwt: &str, installation: u64) -> Result<Minted> {
        let endpoint = format!("app/installations/{installation}/access_tokens");
        let fields: Vec<String> = PERMISSIONS
            .iter()
            .map(|(name, level)| format!("permissions[{name}]={level}"))
            .collect();
        let mut argv = vec!["gh", "api", "--method", "POST", endpoint.as_str()];
        for field in &fields {
            argv.extend(["-f", field.as_str()]);
        }
        let value = self.gh_json(jwt, &argv)?;
        let token = value["token"].as_str().ok_or("no token in response")?;
        let expires_at = value["expires_at"]
            .as_str()
            .and_then(|text| text.parse::<jiff::Timestamp>().ok())
            .ok_or("no expiry in response")?
            .as_second();
        Ok(Minted {
            token: token.to_owned(),
            expires_at,
        })
    }

    fn warn(&self, message: &str) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
    }
}

fn load_config(project: &Path) -> Option<GithubAppConfig> {
    let globals = provenant_cli::Globals::default();
    let resolved = crate::roots::resolve(
        &globals,
        crate::roots::Workspace::At(project),
        crate::roots::Product::Optional,
    )
    .ok()?;
    let git = provenant_os::git::git_executable()?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(git).ok()?;
    let location = runtime
        .project_location(
            &provenant_os::exec::SystemExecutor::new(1).ok()?,
            &resolved.project,
            provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))
                .ok()?,
            &provenant_core::CancelToken::new(),
        )
        .ok()?
        .location;
    let binding = provenant_config::lookup_project(&resolved.roots, &location).ok()??;
    provenant_config::GithubPolicy::read(&resolved.roots.state, binding.project_id())
        .ok()?
        .app
}

type Shared = Arc<Identity<SystemBackend>>;

fn identity_for(project: &Path) -> Shared {
    static IDENTITIES: OnceLock<Mutex<HashMap<PathBuf, Shared>>> = OnceLock::new();
    let map = IDENTITIES.get_or_init(Default::default);
    let Ok(mut map) = map.lock() else {
        return Arc::new(Identity::new(
            None,
            SystemBackend {
                project: project.to_owned(),
            },
        ));
    };
    map.entry(project.to_owned())
        .or_insert_with(|| {
            Arc::new(Identity::new(
                load_config(project),
                SystemBackend {
                    project: project.to_owned(),
                },
            ))
        })
        .clone()
}

/// Environment for one external command: the user's own environment, with
/// `GH_TOKEN` replaced by an installation token when the App applies.
pub(crate) fn environment(project: &Path, argv: &[&str]) -> Option<Vec<(OsString, OsString)>> {
    if argv.first() != Some(&"gh") {
        return None;
    }
    identity_for(project)
        .token_for(argv)
        .map(|token| gh_env(&token))
}

#[cfg(test)]
mod tests;
