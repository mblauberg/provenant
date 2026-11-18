//! Process edge: acknowledge a delta only after its complete output succeeds.
use provenant_cli::{Globals, notes::Notes};
use provenant_core::{AttemptId, CancelToken};
use provenant_fabric_runtime::notes::{Host, Input};
use provenant_records::{CommandResult, Outcome};
use serde_json::Value;

pub fn run(command: Notes, globals: &Globals) -> u8 {
    let cancel = CancelToken::new();
    let lane = std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID).ok();
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let attempt = lane.as_deref().map(AttemptId::parse).transpose()?;
        // Never fall back to chair authority when a lane relay fails.
        let ctx = if attempt.is_none() {
            let resolved = crate::roots::resolve(
                globals,
                crate::roots::Workspace::Project,
                crate::roots::Product::Optional,
            )?;
            Some(provenant_cli::fabric::Context::open(
                resolved.roots,
                &resolved.home,
                &resolved.project,
            )?)
        } else {
            None
        };
        let host = ctx.as_ref().map(|ctx| {
            Host::chair(
                ctx.identity.project,
                if matches!(&command, Notes::Add { .. }) {
                    provenant_cli::notes::current_commit(
                        ctx.roots.state.as_path(),
                        &ctx.project_root,
                        &cancel,
                    )
                } else {
                    None
                },
            )
        });
        let clock = provenant_os::clock::SystemClock::new();
        let execute = |input| -> Result<Value, Box<dyn std::error::Error>> {
            Ok(if let Some(attempt) = attempt {
                provenant_cli::notes::from_lane(attempt, input, &cancel)?
            } else {
                provenant_cli::notes::execute_host(
                    ctx.as_ref().ok_or("notes chair context is missing")?,
                    host.as_ref().ok_or("notes chair identity is missing")?,
                    input,
                    &clock,
                    &cancel,
                )?
            })
        };
        let (leaf, input, ack) = match command {
            Notes::Add {
                text,
                tags,
                scope,
                resolves,
            } => (
                "notes add",
                Input::Add {
                    text: text.join(" "),
                    tags,
                    scope: scope.resolve(),
                    resolves,
                },
                None,
            ),
            Notes::Read {
                scope,
                since,
                replay,
                reader,
                limit,
            } => {
                let scope = scope.resolve();
                let reader =
                    reader.unwrap_or_else(|| lane.clone().unwrap_or_else(|| "chair".into()));
                let after = if replay {
                    Some(0)
                } else if since == "last" {
                    None
                } else {
                    Some(since.parse::<i64>()?)
                };
                (
                    "notes read",
                    Input::Read {
                        reader: reader.clone(),
                        scope: scope.clone(),
                        after,
                        limit,
                    },
                    (!replay).then_some((reader, scope)),
                )
            }
        };
        let data = execute(input)?;
        let next = data["next"].as_i64();
        let human = serde_json::to_string_pretty(&data)?;
        let result = CommandResult::new(leaf, Outcome::Ok, data);
        let status = crate::emit_unpublished(&result, globals, &human, false, None);
        if status == 0
            && let Some(((reader, scope), seq)) = ack.zip(next)
            && let Err(error) = execute(Input::Ack { reader, scope, seq })
        {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "notes: output succeeded but cursor acknowledgement failed; the page may replay: {error}"
            );
            return Ok(3);
        }
        Ok(status)
    })();
    match result {
        Ok(status) => status,
        Err(e) => crate::fail(
            "notes",
            Outcome::Error,
            "notes_error",
            &e.to_string(),
            globals,
        ),
    }
}
