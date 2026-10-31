//! Host-owned cleanup for exactly journalled test allocations, including denied lanes.
#![cfg(target_os = "macos")]

use super::*;
type ObjectIds = BTreeSet<(String, u32)>;

const INTERPOSER: &str = r#"
#include <sys/types.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/sem.h>
#include <fcntl.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

static int journal(void) {
    const char *path = getenv("PROVENANT_TEST_IPC_JOURNAL");
    int fd = path ? open(path, O_WRONLY | O_APPEND) : -1;
    if (fd < 0) _exit(125); /* Refuse allocation unless ownership can be recorded. */
    return fd;
}
static void record(int fd, char kind, int id, key_t key) {
    char line[128];
    int n = snprintf(line, sizeof(line), "%c %d %d %d\n", kind, id, getpid(), key);
    ssize_t written;
    do { written = write(fd, line, (size_t)n); } while (written < 0 && errno == EINTR);
    if (written != n) _exit(126);
}
__attribute__((constructor)) static void loaded(void) {
    int fd = journal();
    record(fd, 'p', 0, 0);
    close(fd);
}
static int creates(key_t key, int flags) {
    return key == IPC_PRIVATE || (flags & (IPC_CREAT | IPC_EXCL)) == (IPC_CREAT | IPC_EXCL);
}
static int audit_shmget(key_t key, size_t size, int flags) {
    int tracked = creates(key, flags);
    int fd = tracked ? journal() : -1;
    int id = shmget(key, size, flags);
    int saved_errno = errno;
    if (tracked && id >= 0) record(fd, 'm', id, key);
    if (fd >= 0) close(fd);
    errno = saved_errno;
    return id;
}
static int audit_semget(key_t key, int count, int flags) {
    int tracked = creates(key, flags);
    int fd = tracked ? journal() : -1;
    int id = semget(key, count, flags);
    int saved_errno = errno;
    if (tracked && id >= 0) record(fd, 's', id, key);
    if (fd >= 0) close(fd);
    errno = saved_errno;
    return id;
}
/* dyld does not interpose references within the replacement's own image. */
__attribute__((used)) static struct { const void *replacement, *original; }
    interpose[] __attribute__((section("__DATA,__interpose"))) = {
        {(const void *)audit_shmget, (const void *)shmget},
        {(const void *)audit_semget, (const void *)semget}
    };
"#;

pub(super) const PERL_JOURNAL: &str = r#"
my $audit_path = shift @ARGV;
sub own {
    my ($kind, $id) = @_;
    open my $journal, ">>", $audit_path or die "open ownership journal: $!\n";
    printf $journal "%s %d %d 0\n", $kind, $id, $$;
    close $journal or die "close ownership journal: $!\n";
}
"#;

pub(super) struct SysvAudit<'a> {
    executor: &'a SystemExecutor,
    root: PathBuf,
    journal: PathBuf,
    before: ObjectIds,
    finished: bool,
}

impl<'a> SysvAudit<'a> {
    pub(super) fn new(
        executor: &'a SystemExecutor,
        root: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(root)?;
        let journal = root.join("sysv-owned.txt");
        std::fs::write(&journal, b"")?;
        let (before, inventory) = Self::inventory(executor)?;
        std::fs::write(root.join("ipcs-before.txt"), inventory)?;
        Ok(Self {
            executor,
            root: root.to_owned(),
            journal,
            before,
            finished: false,
        })
    }

    pub(super) fn journal(&self) -> &Path {
        &self.journal
    }

    pub(super) fn allocation_count(&self) -> Result<usize, Box<dyn std::error::Error>> {
        Ok(self.owned()?.len())
    }

    pub(super) fn interposer(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let source = self.root.join("sysv-audit.c");
        let library = self.root.join("sysv-audit.dylib");
        std::fs::write(&source, INTERPOSER)?;
        let argv = [
            OsString::from("/usr/bin/cc"),
            "-dynamiclib".into(),
            source.into_os_string(),
            "-o".into(),
            library.clone().into_os_string(),
        ];
        let result = self.executor.execute(ExecOptions::new(
            &argv,
            Path::new("/"),
            Duration::from_secs(30),
        ))?;
        if !successful_probe(&result) {
            return Err(format!(
                "compile SysV allocation journal: {}",
                String::from_utf8_lossy(&result.stderr.bytes)
            )
            .into());
        }
        Ok(library)
    }

    fn inventory(
        executor: &SystemExecutor,
    ) -> Result<(ObjectIds, Vec<u8>), Box<dyn std::error::Error>> {
        let argv = ["/usr/bin/ipcs", "-m", "-s"].map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
        options.stdout = StreamPolicy::Capture { limit: 256 * 1024 };
        let env = [("LC_ALL".into(), "C".into())];
        options.env = Some(&env);
        let result = executor.execute(options)?;
        if !successful_probe(&result) {
            return Err("host ipcs -m -s failed".into());
        }
        let mut ids = ObjectIds::new();
        let mut headers = 0;
        for line in std::str::from_utf8(&result.stdout.bytes)?.lines() {
            let words = line.split_whitespace().collect::<Vec<_>>();
            if words.starts_with(&["T", "ID", "KEY"]) {
                headers += 1;
            }
            if matches!(words.first(), Some(&"m" | &"s")) {
                let id = words.get(1).ok_or("missing ipcs object ID")?.parse()?;
                ids.insert((words[0].to_owned(), id));
            }
        }
        if headers != 2 {
            return Err("unrecognised ipcs -m -s inventory format".into());
        }
        Ok((ids, result.stdout.bytes))
    }

    fn owned(&self) -> Result<BTreeSet<(String, u32)>, Box<dyn std::error::Error>> {
        std::fs::read_to_string(&self.journal)?
            .lines()
            .filter_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                if fields.first() == Some(&"p") {
                    return None;
                }
                Some((|| {
                    if fields.len() != 4 || !matches!(fields[0], "m" | "s") {
                        return Err("invalid test allocation journal".into());
                    }
                    // The journal is written only by trusted test code immediately
                    // after IPC_PRIVATE or successful IPC_CREAT|IPC_EXCL allocation.
                    let id = fields[1].parse::<u32>()?;
                    let _creator = fields[2].parse::<u32>()?;
                    let _key = fields[3].parse::<i32>()?;
                    let object = (fields[0].to_owned(), id);
                    if self.before.contains(&object) {
                        return Err("refusing cleanup of a pre-existing SysV object".into());
                    }
                    Ok(object)
                })())
            })
            .collect()
    }

    pub(super) fn finish(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.finished {
            return Ok(());
        }
        let owned = self.owned()?;
        let (present, _) = Self::inventory(self.executor)?;
        let mut errors = Vec::new();
        for (kind, id) in owned.intersection(&present) {
            let argv = [
                OsString::from("/usr/bin/ipcrm"),
                if kind == "m" { "-m" } else { "-s" }.into(),
                id.to_string().into(),
            ];
            let result = self.executor.execute(ExecOptions::new(
                &argv,
                Path::new("/"),
                Duration::from_secs(3),
            ))?;
            if !successful_probe(&result) {
                errors.push(format!("host removal failed for test-owned {kind} {id}"));
            }
        }
        let (after, inventory) = Self::inventory(self.executor)?;
        std::fs::write(self.root.join("ipcs-after.txt"), inventory)?;
        let leaked = owned.intersection(&after).collect::<Vec<_>>();
        if !leaked.is_empty() || !errors.is_empty() {
            return Err(format!("test SysV cleanup: {errors:?}; leaked {leaked:?}").into());
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for SysvAudit<'_> {
    fn drop(&mut self) {
        // The supervisor still owns cleanup if a sandbox errors or an assertion
        // panics. Explicit finish reports errors; Drop supplies the unwind path.
        let _cleanup = self.finish();
    }
}
