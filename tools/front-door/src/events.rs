//! Read-only JSON-lines edge for project observations.
use provenant_cli::Globals;
use provenant_core::CancelToken;
use provenant_fabric_store::{Request, Store};
use provenant_os::{changes::watch::Watcher, clock::SystemClock};
use provenant_records::Outcome;
use std::{
    io::Write,
    time::{Duration, Instant},
};

pub(crate) fn run(from: i64, follow: bool, globals: &Globals) -> u8 {
    match stream(from, follow, globals) {
        Ok(()) => 0,
        Err(error) => crate::fail(
            "events",
            Outcome::Error,
            "EVENTS",
            &error.to_string(),
            globals,
        ),
    }
}

fn stream(
    mut cursor: i64,
    follow: bool,
    globals: &Globals,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    let context = provenant_cli::fabric::Context::open_inspection(
        resolved.roots.clone(),
        &resolved.home,
        &resolved.project,
    )?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    // Subscribe before the first replay so an append cannot fall between the
    // initial read and registration. Hints are advisory; timeout rereads heal gaps.
    let mut watcher = if follow {
        Some(Watcher::subscribe(&[resolved
            .roots
            .state
            .as_path()
            .join(provenant_core::changes::FILE_NAME)])?)
    } else {
        None
    };
    let store = Store::open_read_only(
        &resolved.roots.state,
        Instant::now() + Duration::from_secs(5),
    )?
    .ok_or("project event store does not exist")?
    .live_read_only()?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    loop {
        let page = store.observations(context.identity.project, cursor, 100, &request)?;
        let count = page.items.len();
        for event in page.items {
            serde_json::to_writer(&mut output, &event)?;
            writeln!(output)?;
            output.flush()?;
            cursor = event.cursor.seq;
        }
        if page.truncated || count == 100 {
            continue;
        }
        match &mut watcher {
            None => return Ok(()),
            Some(watcher) => {
                watcher.wait(Duration::from_secs(60))?;
            }
        }
    }
}
