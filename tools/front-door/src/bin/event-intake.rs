//! Standalone event intake worker. Configuration is via PROVENANT_INTAKE_*.
use provenant::event_intake::{Intake, gh::Gh, http, reconcile::Reconciler};
use std::{
    env, io,
    net::{IpAddr, SocketAddr, TcpListener},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[allow(clippy::disallowed_methods, reason = "wall-clock boundary")]
fn now() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(io::Error::other)
}
fn required(name: &str) -> io::Result<String> {
    env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| io::Error::other(format!("{name} is required")))
}
#[allow(clippy::print_stderr, reason = "command-line error output")]
fn main() {
    if let Err(e) = run() {
        eprintln!("event intake: {e}");
        std::process::exit(1);
    }
}
#[allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "service command and scheduling boundary"
)]
fn run() -> io::Result<()> {
    let repository = required("PROVENANT_INTAKE_REPOSITORY")?;
    let parts: Vec<_> = repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.')
        })
    {
        return Err(io::Error::other("invalid owner/repository"));
    }
    let directory = PathBuf::from(required("PROVENANT_INTAKE_DIR")?);
    let interval = env::var("PROVENANT_INTAKE_SWEEP_SECONDS")
        .unwrap_or_else(|_| "300".into())
        .parse::<u64>()
        .map_err(io::Error::other)?;
    let bind = env::var("PROVENANT_INTAKE_BIND")
        .ok()
        .filter(|s| !s.is_empty());
    let status_mode = env::args().nth(1).as_deref() == Some("status");
    let secret = if bind.is_some() && !status_mode {
        required("PROVENANT_INTAKE_SECRET")?.into_bytes()
    } else {
        Vec::new()
    };
    let _guard = if status_mode {
        None
    } else {
        std::fs::create_dir_all(&directory)?;
        let owner =
            provenant_os::fs::Directory::open_anchor(&directory).map_err(io::Error::other)?;
        Some(
            owner
                .try_lock(
                    "worker.lock".as_ref(),
                    provenant_os::fs::LockMode::Exclusive,
                )
                .map_err(io::Error::other)?,
        )
    };
    let intake = Arc::new(Mutex::new(Intake::open(
        directory.join("deliveries.json"),
        repository.clone(),
        secret,
    )?));
    let mut reconciler = Reconciler::open(directory.join("state.json"), repository, interval)?;
    if status_mode {
        let intake = intake
            .lock()
            .map_err(|_| io::Error::other("intake lock poisoned"))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&reconciler.status(&intake)).map_err(io::Error::other)?
        );
        return Ok(());
    }
    if let Some(bind) = bind {
        let address: SocketAddr = bind.parse().map_err(io::Error::other)?;
        if !matches!(address.ip(), IpAddr::V4(ip) if ip.is_loopback())
            && !matches!(address.ip(), IpAddr::V6(ip) if ip.is_loopback())
        {
            return Err(io::Error::other("receiver must bind to loopback"));
        }
        let listener = TcpListener::bind(address)?;
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
    let mut forge = Gh::default();
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
                intake
                    .lock()
                    .map_err(|_| io::Error::other("intake lock poisoned"))?
                    .complete_at(&ids, Some(now()?))?;
                startup = false;
            }
            Ok(false) => (),
            Err(error) => eprintln!("event intake reconciliation: {error}"),
        }
        thread::sleep(Duration::from_secs(1));
    }
}
