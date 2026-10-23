//! Explicit-PID selection policy; signalling stays with the supervisor owner.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("{0} is not a literal PID")]
    LiteralPid(String),
    #[error(transparent)]
    Integer(#[from] crate::IntegerError),
    #[error("cannot convert non-finite age to integer")]
    NonFiniteAge,
}
pub fn parse_pid(value: &str) -> Result<u64, Error> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::LiteralPid(provenant_core::py::python_repr(value)));
    }
    Ok(crate::pending::decimal_integer(value)?)
}
pub fn dead_state(state: &str) -> bool {
    state.starts_with(['Z', '?'])
}

pub fn descendants(pid: u64, ps: &str) -> Result<Vec<u64>, Error> {
    let mut children: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for line in crate::pending::python_splitlines(ps) {
        let parts: Vec<_> = line
            .split(provenant_core::py::python_whitespace)
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() == 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(crate::pending::python_digit))
        {
            let child = crate::pending::decimal_integer(parts[0])?;
            let parent = crate::pending::decimal_integer(parts[1])?;
            children.entry(parent).or_default().push(child);
        }
    }
    let mut ordered = Vec::new();
    let mut queue = VecDeque::from([pid]);
    while let Some(parent) = queue.pop_front() {
        for &child in children.get(&parent).into_iter().flatten() {
            if !ordered.contains(&child) {
                ordered.push(child);
                queue.push_back(child);
            }
        }
    }
    ordered.reverse();
    Ok(ordered)
}
pub fn format_age(seconds: f64) -> Result<String, Error> {
    if !seconds.is_finite() {
        return Err(Error::NonFiniteAge);
    }
    if seconds >= 18_446_744_073_709_551_616.0 {
        return Err(crate::IntegerError::NumberOutOfRange.into());
    }
    let seconds = seconds.max(0.0).trunc() as u64;
    let (days, hours, minutes, secs) = (
        seconds / 86400,
        seconds % 86400 / 3600,
        seconds % 3600 / 60,
        seconds % 60,
    );
    Ok(if days > 0 {
        format!("{days}d{hours:02}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m{secs:02}s")
    })
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProcessInfo {
    pub pid: u64,
    pub ppid: u64,
    pub state: String,
    pub started: f64,
    pub command: String,
    pub cwd: String,
}
pub fn describe(info: &ProcessInfo, now: f64) -> Result<String, Error> {
    Ok(format!(
        "pid {} age {} parent {} cwd {} cmd {}",
        info.pid,
        format_age(now - info.started)?,
        info.ppid,
        info.cwd,
        info.command
    ))
}
/// Observation seam: source varies by ps/procfs/lsof platform and process time.
pub trait Processes {
    fn info(&mut self, pid: u64) -> Option<ProcessInfo>;
    fn descendants(&mut self, pid: u64) -> Vec<u64>;
}
pub fn protected_pids(own_pid: u64, processes: &mut impl Processes) -> BTreeSet<u64> {
    let mut protected = BTreeSet::from([0, 1]);
    let mut pid = own_pid;
    while pid > 1 && protected.insert(pid) {
        let Some(info) = processes.info(pid) else {
            break;
        };
        pid = info.ppid;
    }
    protected
}
#[derive(Debug, Default, PartialEq)]
pub struct Selection {
    pub stdout: String,
    pub exit_code: u8,
    pub victims: Vec<u64>,
    pub identity: BTreeMap<u64, f64>,
}
pub fn select(
    pids: &[u64],
    protected: &BTreeSet<u64>,
    tree: bool,
    started_before: Option<f64>,
    now: f64,
    processes: &mut impl Processes,
) -> Result<Selection, Error> {
    let mut result = Selection::default();
    let mut seen = BTreeSet::new();
    for &pid in pids {
        if !seen.insert(pid) {
            continue;
        }
        if protected.contains(&pid) {
            result.stdout.push_str(&format!(
                "refused pid {pid}: pid 1, the caller or one of its ancestors\n"
            ));
            result.exit_code = 1;
            continue;
        }
        let Some(info) = processes.info(pid).filter(|p| !dead_state(&p.state)) else {
            result.stdout.push_str(&format!("pid {pid} already gone\n"));
            continue;
        };
        result.stdout.push_str(&describe(&info, now)?);
        result.stdout.push('\n');
        result.identity.insert(pid, info.started);
        if started_before.is_some_and(|before| info.started > before) {
            result.stdout.push_str(&format!(
                "refused pid {pid}: started after --started-before; PID may be reused\n"
            ));
            result.exit_code = 1;
            continue;
        }
        if tree {
            for child in processes.descendants(pid) {
                if !protected.contains(&child) {
                    if let Some(info) = processes.info(child) {
                        result.stdout.push_str("  ");
                        result.stdout.push_str(&describe(&info, now)?);
                        result.stdout.push('\n');
                        result.identity.insert(child, info.started);
                        result.victims.push(child);
                    }
                }
            }
        }
        result.victims.push(pid);
    }
    let mut seen = BTreeSet::new();
    result.victims.retain(|pid| seen.insert(*pid));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn literal_pid_and_age_policy_match_source() {
        let values = [
            "", "0", "0002", "42", "+2", "-2", "2.0", "１２", " 2", "\n", "42x",
        ];
        let ages = [
            -100.0, -0.1, 0.0, 0.9, 59.9, 60.0, 3599.99, 3600.0, 86399.0, 86400.9, 90061.0,
        ];
        let Some(expected):Option<(Vec<serde_json::Value>,Vec<String>)>=crate::oracle::python(
            "import json,sys; from scripts.reap import format_age; x=json.load(sys.stdin); print(json.dumps([[int(v) if v.isascii() and v.isdigit() else None for v in x['pids']],[format_age(v) for v in x['ages']]]))",&json!({"pids":values,"ages":ages})
        ) else {return;};
        for (value, expected) in values.iter().zip(expected.0) {
            assert_eq!(json!(parse_pid(value).ok()), expected);
        }
        for (age, expected) in ages.into_iter().zip(expected.1) {
            assert_eq!(format_age(age).unwrap(), expected);
        }
        assert!(format_age(f64::NAN).is_err());
        assert!(format_age(f64::INFINITY).is_err());
    }
    #[test]
    fn descendants_are_deepest_first_and_cycles_are_bounded() {
        assert_eq!(
            descendants(10, "11 10\n12 10\n13 11\n14 13\n12 10\nbad row\n99 -1\n").unwrap(),
            [14, 13, 12, 11]
        );
        assert_eq!(descendants(10, "11 10\n10 11\n").unwrap(), [10, 11]);
        assert_eq!(descendants(10, "١١ ١٠\n").unwrap(), [11]);
        for state in ["Z", "Z+", "?", "?E"] {
            assert!(dead_state(state));
        }
        for state in ["", "S", "R", "UZ"] {
            assert!(!dead_state(state));
        }
    }
    struct Fixture(BTreeMap<u64, ProcessInfo>);
    impl Processes for Fixture {
        fn info(&mut self, pid: u64) -> Option<ProcessInfo> {
            self.0.get(&pid).cloned()
        }
        fn descendants(&mut self, _: u64) -> Vec<u64> {
            vec![13, 1, 12]
        }
    }
    #[test]
    fn selection_refuses_protected_and_reused_pids_before_signalling() {
        let info = |pid, ppid, state: &str, started| ProcessInfo {
            pid,
            ppid,
            state: state.into(),
            started,
            command: "fixture".into(),
            cwd: "/fixture".into(),
        };
        let mut processes = Fixture(BTreeMap::from([
            (10, info(10, 2, "S", 50.0)),
            (11, info(11, 2, "Z", 50.0)),
            (12, info(12, 10, "S", 120.0)),
            (13, info(13, 12, "Z", 60.0)),
            (20, info(20, 1, "S", 150.0)),
        ]));
        let protected = protected_pids(2, &mut processes);
        assert_eq!(protected, BTreeSet::from([0, 1, 2]));
        let result = select(
            &[1, 2, 10, 10, 11, 20, 99],
            &protected,
            true,
            Some(100.0),
            200.0,
            &mut processes,
        )
        .unwrap();
        assert_eq!(result.victims, [13, 12, 10]);
        assert_eq!(result.exit_code, 1);
        assert!(result.stdout.contains("pid 11 already gone\n"));
        assert!(result
            .stdout
            .contains("refused pid 20: started after --started-before"));
        // Source includes zombie descendants and applies cutoff to named PIDs;
        // do not silently add a different child-selection policy.
        assert_eq!(result.identity[&12], 120.0);
    }
}
