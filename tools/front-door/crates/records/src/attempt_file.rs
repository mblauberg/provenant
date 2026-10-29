/// How an attempt-directory entry is treated once its attempt is over. Disk
/// reports and retention classify entries by this, never by file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptFileClass {
    /// Removed once the attempt is terminal and unreserved.
    Temporary,
    /// A provider's private home; retention prunes credentials and caches.
    ProviderState,
    /// Captures, scratch space and history kept with the attempt.
    Retained,
    /// A sealed record or lock that custody and the run store rely on.
    Record,
}

/// Every file or directory an attempt directory may hold. Writers and readers
/// name them through this list, so a new entry has one place to be declared.
/// Records that custody keeps with a writer's worktree reservation are not
/// attempt-directory entries and are not listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttemptFile {
    Prompt,
    Result,
    Stdout,
    Stderr,
    Launch,
    ProcessExit,
    Process,
    Reservation,
    CustodyLock,
    WriterLock,
    TerminalPending,
    TerminalCommit,
    Release,
    Cancel,
    Delivery,
    DeliveryStarted,
    ProviderStateExpired,
    PolicyRetry,
    Tmp,
    Scratch,
    ProviderState,
    ProviderHistory,
    Quarantine,
}

impl AttemptFile {
    pub const ALL: [Self; 23] = [
        Self::Prompt,
        Self::Result,
        Self::Stdout,
        Self::Stderr,
        Self::Launch,
        Self::ProcessExit,
        Self::Process,
        Self::Reservation,
        Self::CustodyLock,
        Self::WriterLock,
        Self::TerminalPending,
        Self::TerminalCommit,
        Self::Release,
        Self::Cancel,
        Self::Delivery,
        Self::DeliveryStarted,
        Self::ProviderStateExpired,
        Self::PolicyRetry,
        Self::Tmp,
        Self::Scratch,
        Self::ProviderState,
        Self::ProviderHistory,
        Self::Quarantine,
    ];

    /// The entry's name in the attempt directory.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Prompt => "prompt.txt",
            Self::Result => "result.md",
            Self::Stdout => "stdout.log",
            Self::Stderr => "stderr.log",
            Self::Launch => "launch.json",
            Self::ProcessExit => "process-exit.json",
            Self::Process => "process.json",
            Self::Reservation => "reservation.json",
            Self::CustodyLock => "custody.lock",
            Self::WriterLock => "writer.lock",
            Self::TerminalPending => "terminal-pending.json",
            Self::TerminalCommit => "terminal-commit.json",
            Self::Release => "release.json",
            Self::Cancel => "cancel.json",
            Self::Delivery => "delivery.json",
            Self::DeliveryStarted => "delivery-started.json",
            Self::ProviderStateExpired => "provider-state-expired.json",
            Self::PolicyRetry => "policy-retry.json",
            Self::Tmp => "tmp",
            Self::Scratch => "scratch",
            Self::ProviderState => "provider-state",
            Self::ProviderHistory => "provider-history",
            Self::Quarantine => "quarantine",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|file| file.name() == name)
    }

    /// Whether the entry is a directory rather than a file.
    pub const fn is_directory(self) -> bool {
        matches!(
            self,
            Self::Tmp
                | Self::Scratch
                | Self::ProviderState
                | Self::ProviderHistory
                | Self::Quarantine
        )
    }

    pub const fn class(self) -> AttemptFileClass {
        match self {
            Self::Tmp => AttemptFileClass::Temporary,
            Self::ProviderState => AttemptFileClass::ProviderState,
            Self::Stdout
            | Self::Stderr
            | Self::Scratch
            | Self::ProviderHistory
            | Self::Quarantine => AttemptFileClass::Retained,
            _ => AttemptFileClass::Record,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_round_trip() {
        for file in AttemptFile::ALL {
            assert_eq!(AttemptFile::from_name(file.name()), Some(file));
        }
        let mut names: Vec<_> = AttemptFile::ALL.iter().map(|f| f.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), AttemptFile::ALL.len());
        assert_eq!(AttemptFile::from_name("unlisted.json"), None);
    }

    #[test]
    fn directories_and_classes_match_the_layout() {
        let directories: Vec<_> = AttemptFile::ALL
            .into_iter()
            .filter(|f| f.is_directory())
            .map(|f| f.name())
            .collect();
        assert_eq!(
            directories,
            [
                "tmp",
                "scratch",
                "provider-state",
                "provider-history",
                "quarantine"
            ]
        );
        assert_eq!(AttemptFile::Tmp.class(), AttemptFileClass::Temporary);
        assert_eq!(
            AttemptFile::ProviderState.class(),
            AttemptFileClass::ProviderState
        );
        assert_eq!(AttemptFile::Stderr.class(), AttemptFileClass::Retained);
        assert_eq!(AttemptFile::Launch.class(), AttemptFileClass::Record);
    }
}
