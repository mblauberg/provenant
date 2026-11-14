use super::{LandingError, model::EligiblePr};
/// Plan ordered prefixes to isolate a red pull request.
pub fn red_prefixes(prs: &[EligiblePr]) -> Result<Vec<Vec<EligiblePr>>, LandingError> {
    if prs.len() < 2 {
        return Err(LandingError::Failed(
            "red split requires at least two pull requests".into(),
        ));
    }
    let middle = prs.len() / 2;
    Ok(vec![prs[..middle].to_vec(), prs[middle..].to_vec()])
}
/// Split at a conflicting pull request, preserving the clean prefix.
pub fn conflict_prefix(
    prs: &[EligiblePr],
    conflict: u64,
) -> Result<(Vec<EligiblePr>, Vec<EligiblePr>), LandingError> {
    let index = prs
        .iter()
        .position(|pr| pr.number == conflict)
        .ok_or_else(|| LandingError::Failed("conflicting pull request is not in batch".into()))?;
    Ok((prs[..index].to_vec(), prs[index + 1..].to_vec()))
}
/// Account for a target movement against the configured rebuild budget.
pub fn rebuild_allowed(used: u64, maximum: u64) -> Result<bool, LandingError> {
    Ok(used < maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prs(n: usize) -> Vec<EligiblePr> {
        (0..n)
            .map(|i| EligiblePr {
                number: i as u64,
                head: format!("head-{i}"),
                approval: "review".into(),
                approval_evidence: Vec::new(),
                accepted: None,
                base_branch: "main".into(),
            })
            .collect()
    }
    #[test]
    fn single_red_gate_bound_for_every_position() -> Result<(), LandingError> {
        for n in 1..=8 {
            for red in 0..n {
                let mut queue = std::collections::VecDeque::from([prs(n)]);
                let mut gates = 0;
                while let Some(batch) = queue.pop_front() {
                    gates += 1;
                    if batch.iter().any(|pr| pr.number == red as u64) && batch.len() > 1 {
                        queue.extend(red_prefixes(&batch)?);
                    }
                }
                assert!(gates <= 7, "{n} PRs, red at {red}: {gates} gates");
            }
        }
        Ok(())
    }
    #[test]
    fn conflict_excludes_only_conflicting_pr_and_rebuild_budget_is_exact()
    -> Result<(), LandingError> {
        let all = prs(4);
        let (before, after) = conflict_prefix(&all, 2)?;
        assert_eq!(before, all[..2]);
        assert_eq!(after, all[3..]);
        assert!(conflict_prefix(&all, 9).is_err());
        assert!(rebuild_allowed(1, 2)?);
        assert!(!rebuild_allowed(2, 2)?);
        assert!(!rebuild_allowed(0, 0)?);
        Ok(())
    }
}
