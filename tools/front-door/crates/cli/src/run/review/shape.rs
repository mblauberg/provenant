//! Inputs to panel selection: changed paths and measured size.
use super::*;

pub(super) type Git<'a> = dyn Fn(&[&str]) -> Result<String, Fail> + 'a;

#[derive(Default)]
pub(super) struct Inputs {
    pub paths: Vec<String>,
    pub test_marker: bool,
    pub size: Option<&'static str>,
    pub warnings: Vec<String>,
}

/// Read the changed paths and size that select the panel. An explicit shape
/// needs neither. Without `--range` the change is measured from the default
/// branch, so every review scales by default; if that cannot be measured the
/// size stays unknown and a warning says why.
pub(super) fn inputs(
    git: &Git<'_>,
    review: &provenant_routing::ReviewConfig,
    shape: Option<&str>,
    range: Option<&str>,
    head: &str,
    kind: Option<&str>,
) -> Result<Inputs, Fail> {
    let mut inputs = Inputs::default();
    let explicit_shape = shape.is_some();
    let selected_range = selection_range(shape, range)?;
    let range = match selected_range {
        Some(Some(range)) => range.to_owned(),
        Some(None) => match default_range(git, head) {
            Some(range) => range,
            None => {
                inputs
                    .warnings
                    .push("no default branch found; size unknown".into());
                return Ok(inputs);
            }
        },
        None => match range {
            Some(range) if !range.starts_with('-') => range.to_owned(),
            Some(_) => return Ok(inputs),
            None => match default_range(git, head) {
                Some(range) => range,
                None => return Ok(inputs),
            },
        },
    };
    match git(&["diff", "--no-renames", "--name-only", "-z", &range, "--"]) {
        Ok(out) => {
            inputs.paths = out
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
        Err(error) if selected_range.is_some() => return Err(error),
        Err(error) => inputs.warnings.push(error.message),
    }
    match git(&["diff", "--no-ext-diff", "--unified=0", &range, "--"]) {
        Ok(diff) => inputs.test_marker = provenant_routing::diff_has_test_markers(&diff),
        Err(error) if selected_range.is_some() => return Err(error),
        Err(error) => inputs.warnings.push(error.message),
    }
    if !explicit_shape {
        let (base, tip) = split_range(&range, head);
        let measured = review.size.measure(kind, base, tip, &mut |args| {
            git(args).map_err(|e| e.message)
        });
        inputs.size = measured.size;
        inputs.warnings.extend(measured.warning);
    }
    Ok(inputs)
}

/// `Ok(None)` when an explicit shape skips selection inputs, `Ok(Some(None))`
/// when the range should default.
fn selection_range<'a>(
    shape: Option<&str>,
    range: Option<&'a str>,
) -> Result<Option<Option<&'a str>>, Fail> {
    if shape.is_some() {
        return Ok(None);
    }
    if range.is_some_and(|range| range.starts_with('-')) {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "git range must not begin with -",
        ));
    }
    Ok(Some(range))
}

/// `<default branch>...<target>`: the change since the merge base.
fn default_range(git: &Git<'_>, head: &str) -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(name) = git(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ]) {
        candidates.push(name.trim().to_owned());
    }
    candidates.extend(["origin/main", "main", "origin/master", "master"].map(str::to_owned));
    candidates
        .into_iter()
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
        .find(|name| {
            git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{name}^{{commit}}"),
            ])
            .is_ok()
        })
        .map(|name| format!("{name}...{head}"))
}

fn split_range<'a>(range: &'a str, head: &'a str) -> (&'a str, &'a str) {
    let (base, tip) = range
        .split_once("...")
        .or_else(|| range.split_once(".."))
        .unwrap_or((range, ""));
    (
        if base.is_empty() { "HEAD" } else { base },
        if tip.is_empty() { head } else { tip },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail() -> Fail {
        Fail::new(Status::Invalid, ErrorKind::Invalid, "git failed: nope")
    }

    #[test]
    fn explicit_shape_skips_range_validation() {
        assert_eq!(
            selection_range(Some("quick"), Some("-invalid")).unwrap(),
            None
        );
        assert!(selection_range(None, Some("-invalid")).is_err());
        assert_eq!(
            selection_range(None, Some("main..HEAD")).unwrap(),
            Some(Some("main..HEAD"))
        );
        assert_eq!(selection_range(None, None).unwrap(), Some(None));
    }

    #[test]
    fn ranges_split_into_base_and_tip() {
        assert_eq!(split_range("main...topic", "abc"), ("main", "topic"));
        assert_eq!(split_range("main..", "abc"), ("main", "abc"));
        assert_eq!(split_range("main", "abc"), ("main", "abc"));
        assert_eq!(split_range("..topic", "abc"), ("HEAD", "topic"));
    }

    #[test]
    fn default_range_uses_the_first_resolvable_default_branch() {
        let git = |args: &[&str]| -> Result<String, Fail> {
            match args[0] {
                "symbolic-ref" => Err(fail()),
                "rev-parse" if args.last() == Some(&"main^{commit}") => Ok(String::new()),
                _ => Err(fail()),
            }
        };
        assert_eq!(default_range(&git, "abc").as_deref(), Some("main...abc"));
        let none = |_: &[&str]| -> Result<String, Fail> { Err(fail()) };
        assert_eq!(default_range(&none, "abc"), None);
    }

    #[test]
    fn unmeasurable_defaults_degrade_to_a_warning() {
        let review = provenant_routing::ReviewConfig::default();
        let none = |_: &[&str]| -> Result<String, Fail> { Err(fail()) };
        let inputs = inputs(&none, &review, None, None, "abc", None).unwrap();
        assert_eq!(inputs.size, None);
        assert!(inputs.warnings[0].contains("no default branch"));
        // An explicit shape needs no git at all.
        let inputs = super::inputs(&none, &review, Some("deep"), None, "abc", None).unwrap();
        assert!(inputs.warnings.is_empty());
        // A bad explicit range is the caller's error.
        assert!(super::inputs(&none, &review, None, Some("a..b"), "abc", None).is_err());
    }

    #[test]
    fn default_branch_diff_sets_size_and_paths() {
        let review = provenant_routing::ReviewConfig::default();
        let git = |args: &[&str]| -> Result<String, Fail> {
            Ok(match args[0] {
                "symbolic-ref" => "origin/main\n".into(),
                "merge-base" => "base\n".into(),
                "diff" if args.contains(&"--name-only") => "src/a.rs\0".into(),
                "diff" if args.contains(&"--unified=0") => "@@ -1,0 +2 @@\n+#[test]\n".into(),
                "diff" => "400\t0\tsrc/a.rs\0".into(),
                _ => String::new(),
            })
        };
        let inputs = inputs(&git, &review, None, None, "abc", None).unwrap();
        assert_eq!(inputs.size, Some("medium"));
        assert_eq!(inputs.paths, ["src/a.rs"]);
        assert!(inputs.test_marker);
        assert!(inputs.warnings.is_empty());
    }
}
