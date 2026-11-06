//! Lower the review verb through the same preset graph as nested reviews.

pub(crate) fn run(
    command: provenant_cli::Review,
    globals: &provenant_cli::Globals,
    workers: &'static dyn provenant_confinement::worktrees::WorkerObserver,
) -> u8 {
    match lower(command) {
        Ok(dispatch) => crate::preset::run_with_review(dispatch, globals, workers, true),
        Err(error) => crate::fail(
            "review",
            provenant_records::Outcome::Invalid,
            "review",
            &error,
            globals,
        ),
    }
}

fn lower(command: provenant_cli::Review) -> Result<provenant_cli::Dispatch, String> {
    use std::ffi::OsString;
    let args = ["run", "--preset", "review", "--prompt-file"]
        .into_iter()
        .map(OsString::from)
        .chain(std::iter::once(command.brief_file.into_os_string()));
    let provenant_cli::ParseOutcome::Invocation(invocation) = provenant_cli::parse(args) else {
        return Err("could not prepare review dispatch".into());
    };
    let provenant_cli::Command::Run(dispatch) = invocation.command else {
        return Err("could not prepare review dispatch".into());
    };
    let mut dispatch = *dispatch;
    let quoted = |value: &str| serde_json::to_string(value).map_err(|error| error.to_string());
    dispatch.preset_set.push(format!(
        "parameters.subject.commit={}",
        quoted(&command.target)?
    ));
    if let Some(shape) = command.shape {
        dispatch
            .preset_set
            .push(format!("parameters.review_shape={}", quoted(&shape)?));
    }
    if let Some(range) = command.range {
        dispatch
            .preset_set
            .push(format!("parameters.selection_range={}", quoted(&range)?));
        let base = range
            .split_once("...")
            .or_else(|| range.split_once(".."))
            .map_or(range.as_str(), |(base, _)| base);
        dispatch.preset_set.push(format!(
            "parameters.base={}",
            quoted(if base.is_empty() { "HEAD" } else { base })?
        ));
    }
    dispatch.preset_set.push(format!(
        "parameters.risk_tags={}",
        serde_json::to_string(&command.risk_tags).map_err(|error| error.to_string())?
    ));
    dispatch.risk_tier = command.risk;
    dispatch.task_kind = command.kind;
    dispatch.route.exclude = command.exclude;
    dispatch.explain = command.explain;
    dispatch.wait = Some(command.wait);
    dispatch.preset_plan = command.plan;
    dispatch.fresh = command.fresh;
    Ok(dispatch)
}

