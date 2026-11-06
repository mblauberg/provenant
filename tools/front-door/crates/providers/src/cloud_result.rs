//! Validation of the result file returned in a textual Git diff.
use crate::cloud::CloudResult;

pub const RESULT_FILE: &str = ".provenant-cloud-result.json";

/// Extract a newly added result file and flag any other changed files.
/// Reject unsupported diff forms rather than accepting incomplete evidence.
pub fn result_from_diff(diff: &str) -> Result<CloudResult, String> {
    let mut files = 0;
    let mut in_result = false;
    let mut other_files = false;
    let mut added_header = false;
    let mut hunk = false;
    let mut contents = String::new();
    let mut expected_lines = None;
    let mut actual_lines = 0usize;
    let mut regular_file = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_result = line == format!("diff --git a/{RESULT_FILE} b/{RESULT_FILE}");
            if in_result {
                files += 1;
            } else {
                other_files = true;
            }
            continue;
        }
        if !in_result {
            continue;
        }
        if line == "new file mode 100644" || line == "new file mode 100755" {
            regular_file = true;
        } else if line.starts_with("--- ") {
            if line != "--- /dev/null" {
                return Err("cloud result must be a new file".into());
            }
        } else if line.starts_with("+++ ") {
            if line != format!("+++ b/{RESULT_FILE}") {
                return Err("unexpected cloud result path".into());
            }
            added_header = true;
        } else if line.starts_with("@@ ") {
            if hunk || !added_header {
                return Err("unsupported cloud result diff hunks".into());
            }
            let range = line
                .strip_prefix("@@ -0,0 +1")
                .and_then(|s| s.split_once(" @@"))
                .map(|(range, _)| range)
                .ok_or("unsupported cloud result diff range")?;
            expected_lines = Some(if range.is_empty() {
                1
            } else {
                range
                    .strip_prefix(',')
                    .ok_or("invalid cloud result diff range")?
                    .parse::<usize>()
                    .map_err(|_| "invalid cloud result diff count")?
            });
            hunk = true;
        } else if hunk {
            if let Some(added) = line.strip_prefix('+') {
                actual_lines += 1;
                contents.push_str(added);
                contents.push('\n');
            } else if line != "\\ No newline at end of file" {
                return Err("cloud result diff contains non-added content".into());
            }
        }
    }
    if files != 1 || !regular_file || !added_header || !hunk || expected_lines != Some(actual_lines)
    {
        return Err("cloud diff has no complete result file".into());
    }
    let mut result: CloudResult = serde_json::from_str(&contents)
        .map_err(|error| format!("invalid cloud result JSON: {error}"))?;
    result.tracked_files_changed |= other_files;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_diff_with_tracked_change_cannot_pass() {
        let temp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = provenant_os::fixture::git_command()
                .current_dir(temp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            output.stdout
        };
        git(&["init", "-q"]);
        git(&["commit", "--allow-empty", "-qm", "base"]);
        std::fs::write(temp.path().join("tracked"), "before\n").unwrap();
        git(&["add", "tracked"]);
        git(&["commit", "-qm", "tracked"]);
        std::fs::write(temp.path().join("tracked"), "after\n").unwrap();
        std::fs::write(temp.path().join(RESULT_FILE), format!(r#"{{"job_id":"job","nonce":"nonce","commit":"{}","exit_status":0,"tests_run":["test"],"tests_failed":[],"log_location":"log","tracked_files_changed":false,"observed_model":null}}"#, "a".repeat(40))).unwrap();
        git(&["add", "-N", RESULT_FILE]);
        let diff = git(&["diff"]);
        let result = result_from_diff(std::str::from_utf8(&diff).unwrap()).unwrap();
        assert!(result.tracked_files_changed);
        assert!(
            !result
                .check("job", "nonce", &"a".repeat(40), "model")
                .unwrap()
                .passed
        );
    }
    #[test]
    fn cloud_diff_accepts_only_result_file() {
        let diff = "diff --git a/.provenant-cloud-result.json b/.provenant-cloud-result.json\nnew file mode 100644\n--- /dev/null\n+++ b/.provenant-cloud-result.json\n@@ -0,0 +1 @@\n+{\"job_id\":\"job\",\"nonce\":\"nonce\",\"commit\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"exit_status\":0,\"tests_run\":[\"test\"],\"tests_failed\":[],\"log_location\":\"log\",\"tracked_files_changed\":false,\"observed_model\":null}\n";
        let result = result_from_diff(diff).unwrap();
        assert!(
            result
                .check("job", "nonce", &"a".repeat(40), "model")
                .unwrap()
                .passed
        );
        let changed = result_from_diff(&(diff.to_owned() + "diff --git a/other b/other\nindex 1234567..7654321 100644\n--- a/other\n+++ b/other\n@@ -1 +1 @@\n-old\n+new\n")).unwrap();
        assert!(changed.tracked_files_changed);
        assert!(
            !changed
                .check("job", "nonce", &"a".repeat(40), "model")
                .unwrap()
                .passed
        );
        assert!(
            result_from_diff(&diff.replace("--- /dev/null", "--- a/.provenant-cloud-result.json"))
                .is_err()
        );
    }
}
