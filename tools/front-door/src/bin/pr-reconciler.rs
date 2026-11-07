//! Standalone observe-only PR worker. It never installs or controls services.
use provenant::{
    event_intake::{Intake, gh::Gh, http, reconcile::Reconciler},
    pr_service::{observer::Observer, settings::Settings, supervisor},
};
use std::{
    env, fs, io,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[allow(clippy::disallowed_methods, reason = "worker wall-clock boundary")]
fn now() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(io::Error::other)
}
#[allow(clippy::print_stderr, reason = "command error boundary")]
fn main() {
    if let Err(error) = run() {
        eprintln!("pr-reconciler: {error}");
        std::process::exit(1);
    }
}

/// Owner-only state is required before opening locks, webhook bodies or reports.
#[cfg(unix)]
fn private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != provenant_os::fs::effective_uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "service state directory must be private and owned by this user",
        ));
    }
    Ok(())
}
#[cfg(not(unix))]
fn private_directory(_: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "PR worker requires Unix private-directory support",
    ))
}

#[allow(
    clippy::disallowed_methods,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "worker command, scheduling and environment boundary"
)]
fn run() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--version", "--json"] {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "command": "version",
                "status": "ok",
                "data": {
                    "package_version": env!("CARGO_PKG_VERSION"),
                    "source_digest": env!("PROVENANT_SOURCE_DIGEST"),
                    "asset_digest": env!("PROVENANT_ASSET_DIGEST"),
                    "target": env!("PROVENANT_TARGET_TRIPLE"),
                    "toolchain": env!("PROVENANT_TOOLCHAIN"),
                    "record_schema_version": 1,
                    "config_schema_version": provenant_config::CONFIG_SCHEMA_VERSION
                },
                "error": null
            })
        );
        return Ok(());
    }
    if args == ["--help"] {
        println!(
            "pr-reconciler --config ABS [--once|--status|--launchd]\npr-reconciler --generate --repository OWNER/NAME --directory ABS --checkout ABS --executable ABS --worker ABS --path PATH --policy-digest DIGEST\nObserve-only; automatic actions remain parked. --launchd only prints a plist."
        );
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--generate") {
        return generate(&args[1..]);
    }
    if !(args.len() == 2 || args.len() == 3)
        || args[0] != "--config"
        || (args.len() == 3 && !matches!(args[2].as_str(), "--once" | "--status" | "--launchd"))
    {
        return Err(io::Error::other(
            "usage: pr-reconciler --config ABS [--once|--status|--launchd]",
        ));
    }
    let config = PathBuf::from(&args[1]);
    provenant::pr_service::settings::absolute(&config)?;
    let settings = Settings::read(&config)?;
    let option = args.get(2).map(String::as_str);
    if option == Some("--launchd") {
        if settings.bind.is_some() {
            return Err(io::Error::other(
                "generated supervisor supports polling only; configure a secret with an operator-owned receiver supervisor",
            ));
        }
        println!("{}", supervisor::render(&settings, &config)?);
        return Ok(());
    }
    if option == Some("--status") {
        // Do not create state, increment boots, take the writer lock or call gh.
        let bytes = fs::read(settings.directory.join("report.json"))?;
        let report: provenant::pr_service::observer::Report =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if report.repository != settings.repository
            || report.policy_digest != settings.policy_digest
            || report.schema_version != 1
        {
            return Err(io::Error::other(
                "report belongs to different service settings",
            ));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(io::Error::other)?
        );
        return Ok(());
    }
    let secret = if settings.bind.is_some() {
        env::var("PROVENANT_INTAKE_SECRET")
            .ok()
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| {
                io::Error::other("PROVENANT_INTAKE_SECRET is required for the receiver")
            })?
            .into_bytes()
    } else {
        vec![]
    };
    private_directory(&settings.directory)?;
    let directory =
        provenant_os::fs::Directory::open_anchor(&settings.directory).map_err(io::Error::other)?;
    let _guard = directory
        .try_lock(
            "worker.lock".as_ref(),
            provenant_os::fs::LockMode::Exclusive,
        )
        .map_err(io::Error::other)?;
    env::set_current_dir(&settings.checkout)?;
    // Explicit PATH is also applied to foreground runs. No secrets are written
    // into generated settings, reports or the plist.
    let mut observer = Observer::open(
        &settings.directory,
        &settings.repository,
        &settings.policy_digest,
    )?;
    let intake = Arc::new(Mutex::new(Intake::open(
        settings.directory.join("deliveries.json"),
        settings.repository.clone(),
        secret,
    )?));
    let mut reconciler = Reconciler::open(
        settings.directory.join("state.json"),
        settings.repository.clone(),
        settings.sweep_seconds,
    )?;
    if let Some(bind) = settings.bind {
        let listener = TcpListener::bind(bind)?;
        let admission = Arc::clone(&intake);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                if let (Ok(mut intake), Ok(timestamp)) = (admission.lock(), now()) {
                    let _ = http::handle(&mut stream, &mut intake, timestamp);
                }
            }
        });
    }
    // Gh accepts an explicit process environment; do not change process-global
    // environment after starting the admission thread.
    let mut forge = Gh::with_path(settings.path.clone());
    let mut startup = true;
    loop {
        let (targets, ids) = {
            let intake = intake
                .lock()
                .map_err(|_| io::Error::other("intake lock poisoned"))?;
            Reconciler::pending(&intake)
        };
        match reconciler.run_snapshot(targets, &mut forge, now()?, startup) {
            Ok(true) => {
                let mut intake = intake
                    .lock()
                    .map_err(|_| io::Error::other("intake lock poisoned"))?;
                // Persist decisions before marking hints complete. A crash can
                // replay this observation but cannot lose the decision.
                observer.observe(&reconciler, &intake, &settings.base_branch, now()?);
                observer.persist(&settings.directory)?;
                intake.complete_at(&ids, Some(now()?))?;
                observer.observe_completion(&intake, &reconciler);
                observer.persist(&settings.directory)?;
                startup = false;
                if option == Some("--once") {
                    return Ok(());
                }
            }
            Ok(false) => {
                if option == Some("--once") {
                    return Err(io::Error::other(
                        "intake is backing off; no fresh observation was made",
                    ));
                }
            }
            Err(error) => {
                observer.error(now()?);
                observer.persist(&settings.directory)?;
                eprintln!("pr-reconciler: intake reconciliation failed; see report.json");
                if option == Some("--once") {
                    return Err(error);
                }
            }
        }
        thread::sleep(Duration::from_secs(settings.poll_seconds));
    }
}

#[allow(clippy::print_stdout, reason = "offline settings output")]
fn generate(args: &[String]) -> io::Result<()> {
    let mut values = serde_json::json!({"schema_version":1,"mode":"observe"});
    if !args.len().is_multiple_of(2) {
        return Err(io::Error::other("generation options require values"));
    }
    for pair in args.chunks_exact(2) {
        let name = match pair[0].as_str() {
            "--repository" => "repository",
            "--directory" => "directory",
            "--checkout" => "checkout",
            "--executable" => "executable",
            "--worker" => "worker",
            "--path" => "path",
            "--policy-digest" => "policy_digest",
            _ => return Err(io::Error::other("unknown generation option")),
        };
        if !values[name].is_null() {
            return Err(io::Error::other("duplicate generation option"));
        }
        values[name] = pair[1].clone().into();
    }
    let settings: Settings = serde_json::from_value(values).map_err(io::Error::other)?;
    settings.validate()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&settings).map_err(io::Error::other)?
    );
    Ok(())
}
