//! Host-only capture and retained contribution reader. Never consult peer stores.
use super::{Result, execute, successful};
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_core::{CancelToken, Clock, GitOid, ProjectId, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_records::{CommandResult, Outcome};
use provenant_run::{Contributions, PullRequest, Request};
use serde_json::json;
use std::{path::Path, time::Duration};

fn repository(project: &Path) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Repository {
        #[serde(rename = "nameWithOwner")]
        name: String,
    }
    let bytes = successful(execute(
        project,
        &["gh", "repo", "view", "--json", "nameWithOwner"],
        false,
    )?)?;
    Ok(serde_json::from_slice::<Repository>(&bytes)?
        .name
        .to_ascii_lowercase())
}

fn project_id(resolved: &roots::Resolved) -> Result<Option<ProjectId>> {
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
    let location = runtime
        .project_location(
            &provenant_os::exec::SystemExecutor::new(1)?,
            &resolved.project,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &CancelToken::new(),
        )?
        .location;
    Ok(
        provenant_config::lookup_project(&resolved.roots, &location)?
            .map(|binding| binding.project_id()),
    )
}

pub(super) fn bind(
    resolved: &roots::Resolved,
    branch: &str,
    number: u64,
    head: GitOid,
) -> Result<()> {
    let Some(project) = project_id(resolved)? else {
        return Ok(());
    };
    let pr = PullRequest {
        repository: repository(&resolved.project)?,
        number,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
        budget: WaitBudget::cli(clock.mono_now(), 30)?,
    };
    Contributions::open(resolved.roots.state.as_path(), &call)?
        .bind(project, branch, &pr, head, &call)?;
    Ok(())
}

pub(super) fn run(number: u64, globals: &Globals) -> u8 {
    const LEAF: &str = "pr contributions";
    let result = (|| -> Result<_> {
        let resolved = roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Optional,
        )?;
        let pr = PullRequest {
            repository: repository(&resolved.project)?,
            number,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = Request {
            clock: &clock,
            cancel: &cancel,
            budget: WaitBudget::cli(clock.mono_now(), 30)?,
        };
        let attempts = Contributions::open_read_only(resolved.roots.state.as_path(), &call)?
            .map(|db| db.for_pr(&pr, &call))
            .transpose()?
            .unwrap_or_default();
        let config = provenant_config::read_with_domains(
            &resolved.roots,
            &provenant_cli::domains::SECTIONS,
        )?;
        let local = provenant_config::lookup_host(&resolved.roots)?;
        let peers: Vec<_> = config
            .domains
            .fabric
            .peers
            .keys()
            .cloned()
            .chain(
                config
                    .domains
                    .fabric
                    .home_hosts
                    .iter()
                    .filter(|(_, host)| Some(**host) != local)
                    .map(|(name, _)| name.clone()),
            )
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut text = format!(
            "{} #{}: {} local attempts\n",
            pr.repository,
            number,
            attempts.len()
        );
        for attempt in &attempts {
            let weight = attempt
                .weight_millionths
                .map_or_else(|| "unknown".into(), |value| format!("{value}/1000000"));
            text.push_str(&format!(
                "{} role={} model={} account={} weight={}\n",
                attempt.attempt,
                attempt.role.as_deref().unwrap_or("unknown"),
                attempt.model.as_deref().unwrap_or("unknown"),
                attempt.account.as_deref().unwrap_or("unknown"),
                weight
            ));
        }
        text.push_str(&format!(
            "peer_attempts: unknown (known hosts: {})\n",
            if peers.is_empty() {
                "unknown".into()
            } else {
                peers.join(", ")
            }
        ));
        Ok((
            json!({"pull_request":pr,"attempts":attempts,
            "coverage":{"local_attempts":"captured", "peer_attempts":"unknown","known_peer_hosts":peers}}),
            text,
        ))
    })();
    match result {
        Ok((data, text)) => emit(
            &CommandResult::new(LEAF, Outcome::Ok, data),
            globals,
            &text,
            true,
        ),
        Err(error) => fail(
            LEAF,
            Outcome::Error,
            "PR_CONTRIBUTIONS",
            &error.to_string(),
            globals,
        ),
    }
}
