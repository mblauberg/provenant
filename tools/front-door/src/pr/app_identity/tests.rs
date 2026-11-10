use super::*;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

const FAKE_KEY: &str =
    "-----BEGIN PRIVATE KEY-----\nFAKEKEYCONTENTDONOTLEAK\n-----END PRIVATE KEY-----\n";

#[derive(Default)]
struct Fake {
    now: AtomicI64,
    mints: AtomicUsize,
    keys_read: AtomicUsize,
    warnings: Mutex<Vec<String>>,
    seen: Mutex<Vec<String>>,
    repositories: Mutex<Vec<String>>,
    lane: std::sync::atomic::AtomicBool,
}

impl Backend for &Fake {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn read_key(&self, _: &GithubAppConfig) -> Result<Vec<u8>> {
        self.keys_read.fetch_add(1, Ordering::SeqCst);
        Ok(FAKE_KEY.as_bytes().to_vec())
    }
    fn sign(&self, key: &[u8], message: &[u8]) -> Result<Vec<u8>> {
        assert_eq!(key, FAKE_KEY.as_bytes());
        Ok(message.iter().rev().copied().collect())
    }
    fn find_installation(&self, _: &str) -> Result<u64> {
        Ok(7)
    }
    fn in_lane(&self) -> bool {
        self.lane.load(Ordering::SeqCst)
    }
    fn mint(&self, jwt: &str, installation: u64, repository: &str) -> Result<Minted> {
        self.repositories
            .lock()
            .unwrap()
            .push(repository.to_owned());
        self.seen.lock().unwrap().push(jwt.to_owned());
        let n = self.mints.fetch_add(1, Ordering::SeqCst);
        Ok(Minted {
            token: format!("ghs_install_{installation}_{n}"),
            expires_at: self.now() + 3600,
        })
    }
    fn warn(&self, message: &str) {
        self.warnings.lock().unwrap().push(message.to_owned());
    }
}

#[allow(dead_code, reason = "the worktree driver strips #[test] items")]
fn app() -> Option<GithubAppConfig> {
    Some(GithubAppConfig {
        app_id: 42,
        installation_id: None,
        private_key_path: None,
        keychain_account: Some("owner".into()),
        keychain_path: Some("/fixture/provenant-app.keychain-db".into()),
    })
}

#[allow(dead_code, reason = "the worktree driver strips #[test] items")]
const READ: &[&str] = &["gh", "api", "repos/{owner}/{repo}/pulls/1"];

#[test]
fn app_configured_calls_carry_installation_token() {
    let fake = Fake::default();
    let identity = Identity::new(app(), &fake).in_repository("repo");
    assert_eq!(identity.token_for(READ).as_deref(), Some("ghs_install_7_0"));
    assert!(fake.warnings.lock().unwrap().is_empty());
}

#[test]
fn no_app_uses_user_login_and_warns_once() {
    let fake = Fake::default();
    let identity = Identity::new(None, &fake).in_repository("repo");
    assert_eq!(identity.token_for(READ), None);
    assert_eq!(identity.token_for(READ), None);
    let warnings = fake.warnings.lock().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("shares the user's"));
    assert_eq!(fake.mints.load(Ordering::SeqCst), 0);
}

#[test]
fn token_is_reused_until_five_minutes_before_expiry() {
    let fake = Fake::default();
    let identity = Identity::new(app(), &fake).in_repository("repo");
    assert_eq!(identity.token_for(READ).as_deref(), Some("ghs_install_7_0"));
    fake.now.store(3600 - 301, Ordering::SeqCst);
    assert_eq!(identity.token_for(READ).as_deref(), Some("ghs_install_7_0"));
    assert_eq!(fake.mints.load(Ordering::SeqCst), 1);
    fake.now.store(3600 - 300, Ordering::SeqCst);
    assert_eq!(identity.token_for(READ).as_deref(), Some("ghs_install_7_1"));
    assert_eq!(fake.mints.load(Ordering::SeqCst), 2);
    // The key is read only when minting.
    assert_eq!(fake.keys_read.load(Ordering::SeqCst), 2);
}

#[test]
fn private_key_never_reaches_jwt_token_or_warnings() {
    let fake = Fake::default();
    let identity = Identity::new(app(), &fake).in_repository("repo");
    let token = identity.token_for(READ).unwrap_or_default();
    let mut seen = fake.seen.lock().unwrap().clone();
    seen.push(token);
    seen.extend(fake.warnings.lock().unwrap().iter().cloned());
    for text in seen {
        assert!(!text.contains("FAKEKEYCONTENT"));
        assert!(!text.contains("BEGIN PRIVATE KEY"));
    }
    // The JWT is three dot-separated base64url parts carrying the app id.
    let jwt = fake.seen.lock().unwrap()[0].clone();
    let parts: Vec<_> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(base64url(br#"{"alg":"RS256","typ":"JWT"}"#), parts[0]);
}

#[test]
fn base64url_matches_known_vectors() {
    assert_eq!(base64url(b""), "");
    assert_eq!(base64url(b"f"), "Zg");
    assert_eq!(base64url(b"ab"), "YWI");
    assert_eq!(base64url(b"foo"), "Zm9v");
    assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
}

#[test]
fn app_never_posts_verdicts_approvals_or_comments() {
    let denied: &[&[&str]] = &[
        &["gh", "pr", "review", "1", "--approve"],
        &["gh", "pr", "comment", "1", "-b", "x"],
        &["gh", "pr", "create"],
        &["gh", "issue", "comment", "1"],
        &[
            "gh",
            "api",
            "--method",
            "POST",
            "repos/{owner}/{repo}/pulls/1/reviews",
        ],
        &[
            "gh",
            "api",
            "repos/{owner}/{repo}/issues/1/comments",
            "-f",
            "body=x",
        ],
        &[
            "gh",
            "api",
            "-X",
            "POST",
            "repos/{owner}/{repo}/pulls/1/comments",
        ],
        &[
            "gh",
            "api",
            "--method=PUT",
            "repos/{owner}/{repo}/pulls/1/reviews/2/dismissals",
        ],
        &[
            "gh",
            "api",
            "graphql",
            "-f",
            "query=mutation { addPullRequestReview(input:{}) { clientMutationId } }",
        ],
        &[
            "gh",
            "api",
            "graphql",
            "-f",
            "query=mutation { addComment(input:{}) { clientMutationId } }",
        ],
        &[
            "gh",
            "api",
            "--method",
            "POST",
            "repos/{owner}/{repo}/pulls/1/reviews",
            "-f",
            "event=APPROVE",
            "-f",
            "note=graphql",
        ],
        &[
            "gh",
            "api",
            "repos/{owner}/{repo}/issues/1/comments?x=graphql",
            "-f",
            "body=x",
        ],
        &["gh", "api", "graphql", "--input", "mutation.json"],
        &["gh", "api", "graphql", "-F", "query=@mutation.graphql"],
        &[
            "gh",
            "api",
            "graphql",
            "-f",
            "query=mutation { mergePullRequest(input:{}) { clientMutationId } }",
        ],
        &[
            "gh",
            "api",
            "graphql",
            "-f",
            "query=query { a } mutation { b }",
        ],
        &[
            "gh",
            "api",
            "--method",
            "PUT",
            "repos/{owner}/{repo}/pulls/1/merge/../reviews",
        ],
        &[
            "gh",
            "api",
            "--method",
            "PUT",
            "repos/{owner}/{repo}/pulls/x/merge",
        ],
    ];
    for argv in denied {
        assert!(!app_may_run(argv), "{argv:?}");
        let fake = Fake::default();
        let identity = Identity::new(app(), &fake).in_repository("repo");
        assert_eq!(identity.token_for(argv), None, "{argv:?}");
        assert_eq!(fake.mints.load(Ordering::SeqCst), 0, "{argv:?}");
    }
    let allowed: &[&[&str]] = &[
        READ,
        &["gh", "pr", "view", "1", "--json", "state"],
        &["gh", "pr", "checks", "1"],
        &["gh", "repo", "view", "--json", "nameWithOwner"],
        &[
            "gh",
            "api",
            "--paginate",
            "--slurp",
            "repos/{owner}/{repo}/pulls/1/reviews",
        ],
        &[
            "gh",
            "api",
            "--method",
            "PUT",
            "repos/{owner}/{repo}/pulls/1/merge-async",
            "-f",
            "sha=abc",
        ],
        &[
            "gh",
            "api",
            "graphql",
            "-f",
            "query=query { viewer { login } }",
        ],
    ];
    for argv in allowed {
        assert!(app_may_run(argv), "{argv:?}");
    }
}

#[test]
fn key_file_must_be_0600_and_outside_repositories() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("app.pem");
    std::fs::write(&key, FAKE_KEY).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_key_file(&key).is_err());
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_key_file(&key).unwrap(), FAKE_KEY.as_bytes());
    assert!(read_key_file(Path::new("relative.pem")).is_err());

    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let inside = repo.join("app.pem");
    std::fs::write(&inside, FAKE_KEY).unwrap();
    std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = read_key_file(&inside).unwrap_err();
    assert!(error.contains("inside a repository"));
    assert!(!error.contains("FAKEKEY"));
}

#[test]
fn keychain_output_decodes_plain_and_hex() {
    assert_eq!(
        decode_keychain_output(FAKE_KEY.as_bytes()),
        FAKE_KEY.trim().as_bytes()
    );
    let hex: String = FAKE_KEY.bytes().map(|b| format!("{b:02x}")).collect();
    assert_eq!(decode_keychain_output(hex.as_bytes()), FAKE_KEY.as_bytes());
}

#[test]
#[allow(clippy::disallowed_methods, reason = "drives the system openssl")]
fn openssl_signature_verifies_for_a_generated_key() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("k.pem");
    let pubkey = dir.path().join("p.pem");
    let ok = |args: &[&str]| {
        std::process::Command::new("openssl")
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !ok(&["genrsa", "-out", key.to_str().unwrap(), "2048"])
        || !ok(&[
            "rsa",
            "-in",
            key.to_str().unwrap(),
            "-pubout",
            "-out",
            pubkey.to_str().unwrap(),
        ])
    {
        return; // no usable openssl on this host
    }
    let backend = SystemBackend {
        project: dir.path().to_owned(),
    };
    let bytes = std::fs::read(&key).unwrap();
    let message = b"header.claims";
    let signature = backend.sign(&bytes, message).unwrap();
    assert_eq!(signature.len(), 256);
    let (m, s) = (dir.path().join("m"), dir.path().join("s"));
    std::fs::write(&m, message).unwrap();
    std::fs::write(&s, &signature).unwrap();
    assert!(ok(&[
        "dgst",
        "-sha256",
        "-verify",
        pubkey.to_str().unwrap(),
        "-signature",
        s.to_str().unwrap(),
        m.to_str().unwrap()
    ]));
}

#[test]
fn installation_tokens_are_limited_to_the_target_repository() {
    let fake = Fake::default();
    let identity = Identity::new(app(), &fake).in_repository("repo");
    assert!(identity.token_for(READ).is_some());
    assert_eq!(*fake.repositories.lock().unwrap(), ["repo"]);
    // Without a known repository no token is minted.
    let fake = Fake::default();
    let identity = Identity::new(app(), &fake);
    assert_eq!(identity.token_for(READ), None);
    assert_eq!(fake.mints.load(Ordering::SeqCst), 0);
}

#[test]
fn a_lane_never_mints() {
    let fake = Fake::default();
    fake.lane.store(true, Ordering::SeqCst);
    let identity = Identity::new(app(), &fake).in_repository("repo");
    assert_eq!(identity.token_for(READ), None);
    assert_eq!(fake.keys_read.load(Ordering::SeqCst), 0);
    assert_eq!(fake.mints.load(Ordering::SeqCst), 0);
}

#[test]
fn remote_urls_name_owner_and_repository() {
    for url in [
        "https://github.com/acme/widget.git\n",
        "git@github.com:acme/widget.git",
        "ssh://git@github.com/acme/widget/",
    ] {
        assert_eq!(
            remote_slug(url),
            Some(("acme".into(), "widget".into())),
            "{url}"
        );
    }
    assert_eq!(remote_slug("widget"), None);
}

#[test]
fn an_owner_app_serves_its_repositories_and_a_project_app_overrides_it() {
    let app = |id| GithubAppConfig {
        app_id: id,
        installation_id: None,
        private_key_path: None,
        keychain_account: Some("owner".into()),
        keychain_path: Some("/fixture/provenant-app.keychain-db".into()),
    };
    let mut policy = provenant_config::GithubPolicy::default();
    policy.apps.insert("Acme".into(), app(1));
    assert_eq!(policy.app_for(Some("acme")).map(|a| a.app_id), Some(1));
    assert_eq!(policy.app_for(Some("other")), None);
    assert_eq!(policy.app_for(None), None);
    policy.app = Some(app(2));
    assert_eq!(policy.app_for(Some("acme")).map(|a| a.app_id), Some(2));
}

#[test]
fn api_flags_fail_closed_in_every_spelling() {
    let denied: &[&[&str]] = &[
        &["repos/o/r/pulls/1", "-XDELETE"],
        &["repos/o/r/pulls/1", "-X=DELETE"],
        &["repos/o/r/pulls/1", "--method=DELETE"],
        &["repos/o/r/pulls/1/reviews", "-fevent=APPROVE"],
        &["repos/o/r/pulls/1/reviews", "-Fevent=APPROVE"],
        &["repos/o/r/pulls/1/reviews", "--field=event=APPROVE"],
        &["repos/o/r/pulls/1", "--unknown-flag"],
        &["repos/o/r/pulls/1", "-Z"],
    ];
    for args in denied {
        assert!(!api_may_run(args), "{args:?}");
    }
    assert!(api_may_run(&["repos/o/r/pulls/1", "-XGET"]));
    assert!(api_may_run(&[
        "repos/o/r/pulls/1",
        "--paginate",
        "-HAccept:x"
    ]));
}

#[test]
fn login_keychain_is_never_a_key_store() {
    assert!(!dedicated_keychain(Path::new(
        "/Users/u/Library/Keychains/login.keychain-db"
    )));
    assert!(!dedicated_keychain(Path::new(
        "/Users/u/Library/Keychains/app.keychain-db"
    )));
    assert!(!dedicated_keychain(Path::new("/x/login.keychain")));
    assert!(dedicated_keychain(Path::new(
        "/Users/u/.provenant/app.keychain-db"
    )));
}
