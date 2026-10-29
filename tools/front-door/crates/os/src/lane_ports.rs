//! Per-lane port blocks: disjoint TCP/UDP port ranges for parallel writer
//! lanes, so dev servers and tests on one host do not collide.
//!
//! A block is an advisory lock on a stable file under `<state>/lane-ports/`,
//! taken by the lane's guardian before the provider starts and held until the
//! guardian exits. The kernel drops the lock when the holder exits, even after
//! a crash or `kill -9`, so a block is released at settlement and a leaked one
//! cannot outlive its guardian. Lock files are never unlinked. The descriptor
//! is close-on-exec, so a lane's own daemons never keep a block.
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
        for index in 0..self.blocks {
            match dir.try_lock(
                OsStr::new(&format!("block-{index}.lock")),
                LockMode::Exclusive,
            ) {
                Ok(guard) => {
                    return Ok(Some(Lease {
                        guard,
                        base: self.base + index * self.size,
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
    fn a_plan_outside_the_port_space_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = plan(temp.path(), 4);
        plan.base = 65500;
        assert!(plan.try_acquire().is_err());
        plan.base = 80;
        assert!(plan.try_acquire().is_err());
    }
}
