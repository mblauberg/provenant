//! Validation of the result file returned in a textual Git diff.
use crate::cloud::CloudResult;

pub const RESULT_FILE: &str = ".provenant-cloud-result.json";

/// Extract a newly added result file from a diff that changes no other files.
/// Reject unsupported diff forms rather than accepting incomplete evidence.
pub fn result_from_diff(diff: &str) -> Result<CloudResult, String> {
    let mut files = 0;
    let mut added_header = false;
    let mut hunk = false;
    let mut contents = String::new();
    let mut expected_lines = None;
    let mut actual_lines = 0usize;
    let mut regular_file = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files += 1;
            if line != format!("diff --git a/{RESULT_FILE} b/{RESULT_FILE}") {
                return Err("cloud diff changes a file other than the result".into());
            }
        } else if line == "new file mode 100644" || line == "new file mode 100755" {
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
    serde_json::from_str(&contents).map_err(|error| format!("invalid cloud result JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(result_from_diff(&(diff.to_owned() + "diff --git a/other b/other\n")).is_err());
        assert!(
            result_from_diff(&diff.replace("--- /dev/null", "--- a/.provenant-cloud-result.json"))
                .is_err()
        );
    }
}
