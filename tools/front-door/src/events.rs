//! JSON-lines edge for project observations and durable consumer cursors.
use provenant_cli::Globals;
use provenant_core::{ActorId, CancelToken};
use provenant_fabric_store::{Request, Store};
use provenant_os::{changes::watch::Watcher, clock::SystemClock};
use provenant_records::Outcome;
use sha2::{Digest, Sha256};
use std::{io::Write, time::Duration};

pub(crate) fn run(from: i64, follow: bool, consumer: Option<&str>, globals: &Globals) -> u8 {
    match stream(from, follow, consumer, globals) {
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
    consumer: Option<&str>,
    globals: &Globals,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::Project,
        crate::roots::Product::Required,
    )?;
    // Following registers the project so the existing host tick can discover
    // its source. Snapshot reads need no registration.
    let context = if follow {
        provenant_cli::fabric::Context::open(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )?
    } else {
        provenant_cli::fabric::Context::open_inspection(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )?
    };
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
    let store = Store::open(&resolved.roots.state, &request)?;
    let consumer = consumer.map(consumer_id).transpose()?;
    if let Some(consumer) = consumer {
        cursor = store.event_cursor(context.identity.project, consumer, &request)?;
    }
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
            if let Some(consumer) = consumer {
                store.mark_events_read(context.identity.project, consumer, cursor, &request)?;
            }
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

// Namespace the stable consumer identity separately from ordinary actors.
fn consumer_id(name: &str) -> Result<ActorId, &'static str> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("consumer name must be 1-128 bytes without control characters");
    }
    let mut hash = Sha256::new();
    hash.update(b"provenant observation consumer\0");
    hash.update(name.as_bytes());
    let digest = hash.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&digest[..16]);
    Ok(ActorId::from_bytes(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn named_consumers_have_stable_separate_cursors() {
        assert_eq!(
            consumer_id("chair-session").unwrap(),
            consumer_id("chair-session").unwrap()
        );
        assert_ne!(
            consumer_id("chair-session").unwrap(),
            consumer_id("other-session").unwrap()
        );
        assert!(consumer_id("").is_err());
        assert!(consumer_id("bad\nname").is_err());
    }
}
