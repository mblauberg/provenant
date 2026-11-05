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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_identity_is_content_based_unless_fresh() {
        let make = |fresh: bool| {
            let mut args = vec![
                "review",
                "0123456789abcdef0123456789abcdef01234567",
                "--brief-file",
                "brief.md",
                "--shape",
                "quick",
                "--plan",
                "off",
            ];
            if fresh {
                args.push("--fresh");
            }
            let provenant_cli::ParseOutcome::Invocation(invocation) =
                provenant_cli::parse(args.into_iter().map(Into::into))
            else {
                panic!("review did not parse")
            };
            let provenant_cli::Command::Review(review) = invocation.command else {
                panic!("not a review")
            };
            lower(review).unwrap()
        };
        let directory = tempfile::tempdir().unwrap();
        let policy = provenant_fabric_runtime::read_roots::ReadRoots::new(
            vec![directory.path().to_owned()],
            &[],
        );
        let id = |request: &provenant_cli::Dispatch| {
            let mut expansion = crate::preset::expand(
                &crate::preset::Layers::empty(),
                "review",
                "brief",
                &request.preset_set,
            )
            .unwrap();
            crate::preset::bind_run_context(
                &mut expansion,
                request,
                directory.path(),
                &policy,
                "explicit",
                None,
            )
            .unwrap();
            expansion.document["graph_id"].as_str().unwrap().to_owned()
        };
        assert_eq!(id(&make(false)), id(&make(false)));
        assert_ne!(id(&make(false)), id(&make(true)));
    }

    #[test]
    fn review_arguments_reach_the_shared_preset() {
        let args = [
            "review",
            "topic",
            "--brief-file",
            "brief.md",
            "--shape",
            "quick",
            "--plan",
            "off",
            "--fresh",
            "--risk-tag",
            "security",
            "--range",
            "main...topic",
            "--exclude",
            "fixture",
        ];
        let provenant_cli::ParseOutcome::Invocation(invocation) =
            provenant_cli::parse(args.into_iter().map(Into::into))
        else {
            panic!("review did not parse")
        };
        let provenant_cli::Command::Review(review) = invocation.command else {
            panic!("not a review")
        };
        let dispatch = lower(review).unwrap();
        assert_eq!(dispatch.preset.as_deref(), Some("review"));
        assert!(dispatch.fresh);
        assert_eq!(dispatch.preset_plan.as_deref(), Some("off"));
        assert!(dispatch.preset_set.iter().any(|value| value.contains("parameters.subject.commit") && value.contains("topic")));
        assert!(
            dispatch
                .preset_set
                .iter()
                .any(|value| value.contains("parameters.base") && value.contains("main"))
        );
        assert!(
            dispatch
                .preset_set
                .iter()
                .any(|value| value.contains("security"))
        );
        assert_eq!(dispatch.route.exclude, ["fixture"]);
    }
}
