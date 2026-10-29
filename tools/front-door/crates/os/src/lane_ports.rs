//! Per-lane port blocks: disjoint TCP/UDP port ranges for parallel writer
//! lanes, so dev servers and tests on one host do not collide.
//!
//! A block is an advisory lock on a stable file under `<state>/lane-ports/`,
//! taken by the lane's guardian before the provider starts. The provider and
//! its descendants inherit the descriptor, so the block is held until the
//! whole process tree has exited, even if the guardian dies first. The kernel
//! drops the lock when the last holder exits, even after `kill -9`, so a block
//! is released at settlement and a leaked one cannot outlive its processes.
//! Lock files are never unlinked.
//!
//! The ranges are a convention shared by cooperating lanes, not a firewall: a
//! lane can still bind outside its block, and another program can use a port
//! inside it.
use crate::fs::{Directory, FsError, LockGuard, LockMode, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::PathBuf;

/// Directory below the host state root that holds the block lock files.
pub const DIRECTORY: &str = "lane-ports";
/// Environment variable naming the first port of a lane's block.
pub const BASE_ENV: &str = "PROVENANT_LANE_PORT_BASE";
/// Environment variable naming how many consecutive ports the block holds.
pub const COUNT_ENV: &str = "PROVENANT_LANE_PORT_COUNT";

/// The configured port space: `blocks` consecutive blocks of `size` ports
/// from `base`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub state: PathBuf,
    pub base: u32,
    pub size: u32,
    pub blocks: u32,
}

/// One held block. Dropping it, or the process exiting, releases it.
#[derive(Debug)]
pub struct Lease {
    #[allow(dead_code, reason = "the open descriptor is the held lock")]
    guard: LockGuard,
    pub base: u32,
    pub size: u32,
}
impl Lease {
    /// The held lock, for a process tree to inherit so the block lasts as long
    /// as any of its processes does, not only as long as the guardian.
    pub fn guard(&self) -> &LockGuard {
        &self.guard
    }
    /// The environment that tells the lane which ports are its own.
    pub fn environment(&self) -> [(&'static str, String); 2] {
        [
            (BASE_ENV, self.base.to_string()),
            (COUNT_ENV, self.size.to_string()),
        ]
    }
}

impl Plan {
    /// Whether the space is non-empty and fits in the unprivileged range.
    pub fn is_valid(&self) -> bool {
        self.state.is_absolute()
            && self.base >= 1024
            && self.size >= 1
            && self.blocks >= 1
            && u64::from(self.base) + u64::from(self.size) * u64::from(self.blocks) <= 65536
    }

    /// Take the first free block, or none when every block is held.
    ///
    /// A block's lock file is named by the ports it covers, so a configuration
    /// change cannot give two live lanes the same ports under different
    /// names: a candidate is skipped while any held lock file of another
    /// plan overlaps it. A short gate lock serialises the scan.
    pub fn try_acquire(&self) -> Result<Option<Lease>> {
        if !self.is_valid() {
            return Err(FsError::InvalidComponent {
                name: "lane port plan".into(),
            });
        }
        let root = Directory::open_anchor(&self.state)?;
        match root.mkdir(OsStr::new(DIRECTORY), 0o700) {
            Ok(()) => root.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error),
        }
        let dir = root.open_verified_directory(OsStr::new(DIRECTORY))?;
        let _gate = (0..GATE_TRIES)
            .find_map(
                |attempt| match dir.try_lock(OsStr::new("gate.lock"), LockMode::Exclusive) {
                    Err(FsError::LockTimeout { .. }) => {
                        if attempt + 1 < GATE_TRIES {
                            crate::sys::fs::wait(GATE_POLL);
                        }
                        None
                    }
                    other => Some(other),
                },
            )
            .ok_or_else(|| FsError::LockTimeout {
                path: dir.anchored_path(),
                timeout: GATE_POLL * GATE_TRIES,
            })??;
        let mut others = Vec::new();
        for name in dir.entries()? {
            let name = name?;
            if let Some(range) = name.to_str().and_then(parse_name) {
                others.push((name, range));
            }
        }
        for index in 0..self.blocks {
            let first = self.base + index * self.size;
            let own = block_name(first, self.size);
            let overlaps_held = others.iter().any(|(name, (other_first, other_size))| {
                *name != *own
                    && first < other_first + other_size
                    && *other_first < first + self.size
                    && matches!(
                        dir.try_lock_existing(name, LockMode::Shared),
                        Err(FsError::LockTimeout { .. })
                    )
            });
            if overlaps_held {
                continue;
            }
            match dir.try_lock(&own, LockMode::Exclusive) {
                Ok(guard) => {
                    return Ok(Some(Lease {
                        guard,
                        base: first,
                        size: self.size,
                    }));
                }
                Err(FsError::LockTimeout { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }
}

const GATE_TRIES: u32 = 100;
const GATE_POLL: std::time::Duration = std::time::Duration::from_millis(20);

fn block_name(first: u32, size: u32) -> std::ffi::OsString {
    format!("block-{first}-{size}.lock").into()
}
fn parse_name(name: &str) -> Option<(u32, u32)> {
    let (first, size) = name
        .strip_prefix("block-")?
        .strip_suffix(".lock")?
        .split_once('-')?;
    Some((first.parse().ok()?, size.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn plan(state: &Path, blocks: u32) -> Plan {
        Plan {
            state: state.to_owned(),
            base: 30000,
            size: 16,
            blocks,
        }
    }

    #[test]
    fn concurrent_leases_are_disjoint_and_release_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let plan = plan(temp.path(), 3);
        let first = plan.try_acquire().unwrap().unwrap();
        let second = plan.try_acquire().unwrap().unwrap();
        let third = plan.try_acquire().unwrap().unwrap();
        let mut starts = [first.base, second.base, third.base];
        starts.sort_unstable();
        assert_eq!(starts, [30000, 30016, 30032]);
        assert!(plan.try_acquire().unwrap().is_none());
        let freed = second.base;
        drop(second);
        let again = plan.try_acquire().unwrap().unwrap();
        assert_eq!(again.base, freed);
        assert_eq!(
            again.environment(),
            [(BASE_ENV, freed.to_string()), (COUNT_ENV, "16".into())]
        );
        drop((first, third));
    }

    #[test]
    fn leases_from_different_plans_never_share_a_port() {
        let temp = tempfile::tempdir().unwrap();
        let old = plan(temp.path(), 2);
        let held = old.try_acquire().unwrap().unwrap();
        assert_eq!(held.base, 30000);
        // Base moved down one block: its block 1 is the held 30000-30015.
        let moved = Plan {
            base: 29984,
            ..plan(temp.path(), 2)
        };
        let next = moved.try_acquire().unwrap().unwrap();
        assert_eq!((next.base, next.size), (29984, 16));
        assert!(moved.try_acquire().unwrap().is_none());
        // A half-block shift overlaps both held blocks.
        let shifted = Plan {
            base: 30008,
            ..plan(temp.path(), 1)
        };
        assert!(shifted.try_acquire().unwrap().is_none());
        drop(held);
        assert_eq!(shifted.try_acquire().unwrap().unwrap().base, 30008);
    }

    #[test]
    fn a_plan_outside_the_port_space_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = plan(temp.path(), 4);
        plan.base = 65500;
        assert!(plan.try_acquire().is_err());
        plan.base = 80;
        assert!(plan.try_acquire().is_err());
    }
}
