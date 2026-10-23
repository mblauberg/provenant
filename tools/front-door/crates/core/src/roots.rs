//! Product-root precedence. The install supplies its fallback root as a value.
use crate::paths::{expand_user, Homes, PathError};
use std::path::{Path, PathBuf};

pub const ENVIRONMENT_VARIABLE: &str = "AGENT_FABRIC_PRODUCT_ROOT";

/// Configured roots are expanded without resolving symlinks. The fallback is
/// the install's resolved product root (the Python owner's file location).
pub fn product_root(
    configured: Option<&Path>,
    installed_root: &Path,
    homes: &Homes,
) -> Result<PathBuf, PathError> {
    match configured.filter(|v| !v.as_os_str().is_empty()) {
        Some(path) => expand_user(path, homes),
        None => Ok(installed_root.into()),
    }
}
pub fn skills_root(
    configured: Option<&Path>,
    installed_root: &Path,
    homes: &Homes,
) -> Result<PathBuf, PathError> {
    Ok(product_root(configured, installed_root, homes)?.join("skills"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_root_precedes_install_and_empty_is_unset() {
        let homes = Homes {
            current: Some("/home/user".into()),
            ..Homes::default()
        };
        assert_eq!(
            product_root(
                Some(Path::new("~/product")),
                Path::new("/installed"),
                &homes
            )
            .unwrap(),
            Path::new("/home/user/product")
        );
        assert_eq!(
            skills_root(
                Some(Path::new("relative/product")),
                Path::new("/installed"),
                &homes
            )
            .unwrap(),
            Path::new("relative/product/skills")
        );
        for configured in [None, Some(Path::new(""))] {
            assert_eq!(
                product_root(configured, Path::new("/installed"), &homes).unwrap(),
                Path::new("/installed")
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn configured_symlink_keeps_the_callers_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        assert_eq!(
            product_root(Some(&alias), Path::new("/installed"), &Homes::default()).unwrap(),
            alias
        );
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    #[test]
    fn python_root_precedence() {
        let Some(expected) = crate::test_support::python(
            "skills/_shared/roots.py",
            r#"
results=[]
for value in ['', 'relative/tree', '~/tree', './a//b/', '//server//tree/']:
    os.environ['HOME'] = '/home/fixture'
    os.environ['AGENT_FABRIC_PRODUCT_ROOT'] = value
    results.append([str(m.product_root()),str(m.skills_root())])
print(json.dumps(results))
"#,
            &[],
        ) else {
            return;
        };
        let installed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap();
        let installed = std::fs::canonicalize(installed).unwrap();
        let homes = Homes {
            current: Some("/home/fixture".into()),
            ..Homes::default()
        };
        let actual: Vec<_> = ["", "relative/tree", "~/tree", "./a//b/", "//server//tree/"]
            .iter()
            .map(|v| {
                serde_json::json!([
                    product_root(Some(Path::new(v)), &installed, &homes).unwrap(),
                    skills_root(Some(Path::new(v)), &installed, &homes).unwrap()
                ])
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
