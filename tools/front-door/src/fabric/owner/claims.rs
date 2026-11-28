//! Advisory overlap diagnostics for live writers of a shared checkout.
use super::*;

pub(super) fn overlap_warnings(
    store: &provenant_run::RunStore,
    call: &provenant_run::Request<'_>,
    subject: &CustodySubject,
    worktree: &Path,
    claims: &[String],
) -> Result<Vec<Warning>, FacadeError> {
    let mut result = Vec::new();
    let mut after = 0;
    loop {
        let page = store
            .lanes(
                &provenant_run::LaneQuery {
                    targets: Vec::new(),
                    all: false,
                    limit: 100,
                    after,
                    newest_first: false,
                    owner: None,
                    unseen_after: None,
                },
                call,
            )
            .map_err(run_error)?;
        for live in page.lanes {
            let request = store.request(live.attempt_id, call).map_err(run_error)?;
            let Some(writer) = request.task.writer.as_ref() else {
                continue;
            };
            if writer.worktree != worktree
                || request
                    .task
                    .access
                    .shared_worktree
                    .as_ref()
                    .is_some_and(|other| other != subject)
            {
                continue;
            }
            for claim in effective_claims(claims) {
                let claim_path = Path::new(claim);
                if claim != "." && provenant_core::RelativePath::parse(claim_path).is_err() {
                    continue;
                }
                for other in effective_claims(&request.task.profile.write_scope) {
                    let other_path = Path::new(other);
                    if other != "." && provenant_core::RelativePath::parse(other_path).is_err() {
                        continue;
                    }
                    if claim == "."
                        || other == "."
                        || claim_path.starts_with(other_path)
                        || other_path.starts_with(claim_path)
                    {
                        result.push(overlap_warning(claim, other, live.attempt_id));
                    }
                }
            }
        }
        match page.next {
            Some(next) => after = next,
            None => break,
        }
    }
    Ok(result)
}

fn effective_claims(claims: &[String]) -> impl Iterator<Item = &str> {
    claims
        .iter()
        .map(String::as_str)
        .chain(claims.is_empty().then_some("."))
}

fn overlap_warning(claim: &str, other: &str, holder: provenant_core::AttemptId) -> Warning {
    let code = WarningCode(provenant_records::policy::PATH_CLAIM_OVERLAP);
    Warning::new(
        code,
        format!(
            "{}: claim {:?} overlaps claim {:?} by attempt {}",
            code.as_str(),
            claim,
            other,
            holder
        ),
        provenant_records::warning::detected_source(code),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scope_claims_the_whole_worktree() {
        assert_eq!(effective_claims(&[]).collect::<Vec<_>>(), ["."]);
        assert_eq!(
            effective_claims(&["src".into()]).collect::<Vec<_>>(),
            ["src"]
        );
    }

    #[test]
    fn overlap_warning_names_both_claims_and_holder() {
        let holder = provenant_core::AttemptId::from_bytes([9; 16]);
        let warning = overlap_warning("src/lib.rs", "src", holder);
        assert_eq!(
            warning.code.as_ref(),
            provenant_records::policy::PATH_CLAIM_OVERLAP
        );
        assert!(warning.message.contains("src/lib.rs"));
        assert!(warning.message.contains("\"src\""));
        assert!(warning.message.contains(&holder.to_string()));
    }
}
