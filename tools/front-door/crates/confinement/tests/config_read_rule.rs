//! Keep project-owned Git config command construction in one module.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use syn::visit::{self, Visit};
    use syn::{Expr, ImplItemFn, ItemFn, ItemMod};

    /// The type names whose methods carry a raw Git argv. The scanner resolves
    /// receivers through declared types, not through local variable names.
    const GIT_ARGV_TYPES: &[&str] = &["Git", "HostGit", "Command", "Executor"];

    fn literal(expr: &Expr, expected: &str) -> bool {
        struct Match<'a>(&'a str, bool);
        impl<'ast> Visit<'ast> for Match<'_> {
            fn visit_lit_str(&mut self, value: &'ast syn::LitStr) {
                self.1 |= value.value() == self.0;
            }
        }
        let mut found = Match(expected, false);
        found.visit_expr(expr);
        found.1
    }

    fn has_config(expr: &Expr) -> bool {
        literal(expr, "config")
    }

    /// Any of these flags with a `config` subcommand outside the core is a
    /// mutation: reads are only ever constructed inside the core.
    fn has_mutation_flag(expr: &Expr) -> bool {
        [
            "--local",
            "--replace-all",
            "--remove-section",
            "--unset",
            "--unset-all",
            "--add",
            "--set",
        ]
        .iter()
        .any(|flag| literal(expr, flag))
    }

    fn path_ident(path: &syn::Path) -> Option<String> {
        path.segments.last().map(|part| part.ident.to_string())
    }

    fn path_is_git_constructor(path: &syn::Path) -> bool {
        let parts: Vec<String> = path
            .segments
            .iter()
            .map(|part| part.ident.to_string())
            .collect();
        // `std::process::Command::new(...)` is identified by its type path;
        // a variable merely named `command` or `git` is not a constructor.
        parts.last().is_some_and(|last| last == "new")
            && (parts.iter().any(|part| part == "Command")
                || parts
                    .first()
                    .is_some_and(|first| GIT_ARGV_TYPES.contains(&first.as_str())))
    }

    /// Resolve a receiver to a Git/process type rather than to a variable name.
    fn git_command(expr: &Expr, types: &HashMap<String, String>) -> bool {
        match expr {
            Expr::Path(path) => path_ident(&path.path).is_some_and(|name| {
                types
                    .get(&name)
                    .is_some_and(|ty| GIT_ARGV_TYPES.contains(&ty.as_str()))
            }),
            Expr::Call(call) => match call.func.as_ref() {
                Expr::Path(path) => path_is_git_constructor(&path.path),
                _ => false,
            },
            Expr::MethodCall(call) => git_command(&call.receiver, types),
            Expr::Field(field) => git_command(&field.base, types),
            Expr::Reference(reference) => git_command(&reference.expr, types),
            Expr::Group(group) => git_command(&group.expr, types),
            Expr::Paren(paren) => git_command(&paren.expr, types),
            _ => false,
        }
    }

    fn exec_options(expr: &Expr) -> bool {
        matches!(expr, Expr::Call(call) if matches!(call.func.as_ref(), Expr::Path(path)
            if path.path.segments.iter().any(|part| part.ident == "ExecOptions")))
    }

    fn type_name(ty: &syn::Type) -> Option<String> {
        match ty {
            syn::Type::Reference(reference) => type_name(&reference.elem),
            syn::Type::Path(path) => path_ident(&path.path),
            _ => None,
        }
    }

    /// Collect declared receiver types from function arguments and typed lets.
    #[derive(Default)]
    struct TypeNames(HashMap<String, String>);
    impl<'ast> Visit<'ast> for TypeNames {
        fn visit_pat_type(&mut self, node: &'ast syn::PatType) {
            if let syn::Pat::Ident(ident) = &*node.pat
                && let Some(ty) = type_name(&node.ty)
            {
                self.0.insert(ident.ident.to_string(), ty);
            }
            visit::visit_pat_type(self, node);
        }
    }

    #[derive(Default)]
    struct Calls {
        findings: Vec<String>,
        types: HashMap<String, String>,
    }
    impl<'ast> Visit<'ast> for Calls {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            if matches!(node.method.to_string().as_str(), "arg" | "args")
                && node.args.iter().any(has_config)
                && git_command(&node.receiver, &self.types)
            {
                self.findings.push("std::process command".into());
            }
            if node.method == "execute"
                && node.args.iter().any(has_config)
                && node.args.iter().any(exec_options)
            {
                self.findings.push("Executor argv".into());
            }
            if matches!(node.method.to_string().as_str(), "full" | "run")
                && node.args.iter().any(has_config)
            {
                self.findings.push(format!("Git::{} argv", node.method));
            }
            visit::visit_expr_method_call(self, node);
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if let Expr::Path(path) = node.func.as_ref() {
                let name = path_ident(&path.path).unwrap_or_default();
                let git_argv_function = name.starts_with("host_git")
                    || matches!(name.as_str(), "run" | "execute_for" | "git");
                if git_argv_function && node.args.iter().any(has_config) {
                    self.findings.push("Git helper argv".into());
                }
                // A raw argv tail passed to git_config::write bypasses the
                // core's typed mutation enum and is a write-path violation.
                if name == "write"
                    && path
                        .path
                        .segments
                        .iter()
                        .any(|part| part.ident == "git_config")
                    && !node.args.first().is_some_and(is_context_arg)
                {
                    self.findings.push("raw git_config::write argv".into());
                }
            }
            visit::visit_expr_call(self, node);
        }

        fn visit_item_fn(&mut self, node: &'ast ItemFn) {
            if !test_only(&node.attrs) {
                visit::visit_item_fn(self, node);
            }
        }
        fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
            if !test_only(&node.attrs) {
                visit::visit_impl_item_fn(self, node);
            }
        }
        fn visit_item_mod(&mut self, node: &'ast ItemMod) {
            if !test_only(&node.attrs) {
                visit::visit_item_mod(self, node);
            }
        }
    }

    /// A typed write starts with a `ConfigContext` value or constructor. A
    /// raw argv tail (`&tail`, `&[OsString::from(..)]`) is a reference or array
    /// and is caught here.
    fn is_context_arg(expr: &Expr) -> bool {
        matches!(expr, Expr::Path(_) | Expr::Call(_))
    }

    fn test_only(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .meta
                    .require_list()
                    .is_ok_and(|list| list.tokens.to_string() == "test")
        })
    }

    fn sources(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {dir:?}: {error}"))
        {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                sources(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }

    fn test_only_source(path: &Path) -> bool {
        path.components().any(|part| {
            let name = part.as_os_str().to_string_lossy();
            name == "tests" || name.ends_with("_tests") || name.ends_with("_test")
        }) || path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name == "tests.rs"
                || name == "test_support.rs"
                || name.starts_with("tests_")
                || name.ends_with("_test.rs")
                || name.ends_with("_tests.rs")
        })
    }

    #[derive(Default)]
    struct Findings(Vec<String>);
    impl Findings {
        fn of(source: &str) -> Self {
            let file = syn::parse_file(source).expect("Rust source parses");
            let mut names = TypeNames::default();
            names.visit_file(&file);
            let mut calls = Calls {
                types: names.0,
                ..Calls::default()
            };
            calls.visit_file(&file);
            Self(calls.findings)
        }
    }

    fn workspace_files() -> (PathBuf, PathBuf, Vec<PathBuf>) {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates root")
            .to_owned();
        let workspace = crates.parent().expect("workspace root").to_owned();
        let mut files = Vec::new();
        for source_root in [workspace.join("src"), workspace.join("examples")] {
            if source_root.is_dir() {
                sources(&source_root, &mut files);
            }
        }
        for entry in std::fs::read_dir(&crates).expect("crate directories") {
            let crate_dir = entry.expect("crate entry").path();
            for tree in ["src", "examples"] {
                let source_root = crate_dir.join(tree);
                if source_root.is_dir() {
                    sources(&source_root, &mut files);
                }
            }
        }
        (workspace, crates, files)
    }

    #[test]
    fn only_the_core_constructs_git_config_commands() {
        let (_workspace, crates, files) = workspace_files();
        let core = crates.join("os/src/git_config.rs");
        let mut violations = Vec::new();
        for path in files {
            if path == core || test_only_source(&path) {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let findings = Findings::of(&source);
            if !findings.0.is_empty() {
                violations.push(format!("{}: {:?}", path.display(), findings.0));
            }
        }
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[test]
    fn no_code_outside_the_core_can_issue_a_config_write() {
        let (_workspace, crates, files) = workspace_files();
        let core = crates.join("os/src/git_config.rs");
        let mut violations = Vec::new();
        for path in files {
            if path == core || test_only_source(&path) {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let mut calls = Calls::default();
            calls.visit_file(&file);
            for finding in calls.findings {
                if finding == "raw git_config::write argv" {
                    violations.push(format!("{}: {finding}", path.display()));
                }
            }
            // A literal config argv carrying a mutation flag outside the core
            // is a second path around the typed mutation enum.
            #[derive(Default)]
            struct RawWrite(Vec<String>);
            impl<'ast> Visit<'ast> for RawWrite {
                fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
                    if node.args.iter().any(has_config) && node.args.iter().any(has_mutation_flag) {
                        self.0.push("raw config mutation argv".into());
                    }
                    visit::visit_expr_call(self, node);
                }
                fn visit_item_fn(&mut self, node: &'ast ItemFn) {
                    if !test_only(&node.attrs) {
                        visit::visit_item_fn(self, node);
                    }
                }
                fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
                    if !test_only(&node.attrs) {
                        visit::visit_impl_item_fn(self, node);
                    }
                }
                fn visit_item_mod(&mut self, node: &'ast ItemMod) {
                    if !test_only(&node.attrs) {
                        visit::visit_item_mod(self, node);
                    }
                }
            }
            let mut raw = RawWrite(Vec::new());
            raw.visit_file(&file);
            for finding in raw.0 {
                violations.push(format!("{}: {finding}", path.display()));
            }
        }
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }

    #[test]
    fn synthetic_process_and_executor_bypasses_are_caught() {
        let process = r#"fn f(git: &str) { std::process::Command::new(git).args(["config", "--list"]).output(); }"#;
        let executor = r#"fn f(executor: &dyn Executor) { executor.execute(ExecOptions::new(&["git", "config"], cwd, timeout)); }"#;
        let host_git = r#"fn f() { host_git(repo, executor, &["config", "--list"], timeout); }"#;
        let git_helper = r#"fn f() { git(repo, home, &["config", "user.name", "Probe"]); }"#;
        let git_full = r#"fn f(repo: &Path, git: &Git) { git.full(repo, &["config", "--get", "core.worktree"]); }"#;
        let git_run = r#"fn f(repo: &Path, git: &Git) { git.run(repo, &["config", "--list"]); }"#;
        let execute_for = r#"fn f() { execute_for(repo, &["git", "config"], timeout, false); }"#;
        let wrapped_host_git = r#"fn f() { let run = |args| host_git_with_cancel(repo, executor, args, timeout, false, None); run(&["config", "--get", "remote.origin.url"]); }"#;
        for source in [
            process,
            executor,
            host_git,
            git_helper,
            git_full,
            git_run,
            execute_for,
            wrapped_host_git,
        ] {
            assert!(!Findings::of(source).0.is_empty(), "missed: {source}");
        }
    }

    #[test]
    fn synthetic_raw_config_writes_are_caught_by_type() {
        let process = r#"fn f(git: &str) { std::process::Command::new(git).args(["config", "--local", "branch.lane.remote", "origin"]).output(); }"#;
        let helper = r#"fn f(repo: &Path, git: &Git) { git.full(repo, &["config", "--replace-all", "branch.lane.merge", "refs/heads/main"]); }"#;
        let raw_tail = r#"fn f(tail: &[OsString]) { provenant_os::git_config::write(&tail, |args| run(args)); }"#;
        let typed = r#"fn f() { provenant_os::git_config::write(context, ConfigMutation::SetStore { setting, value }, |args| run(args)); }"#;
        assert!(has_raw_write(process));
        assert!(has_raw_write(helper));
        assert!(has_raw_write(raw_tail));
        assert!(!has_raw_write(typed));
    }

    fn has_raw_write(source: &str) -> bool {
        !Findings::of(source).0.is_empty()
    }

    #[test]
    fn unrelated_literals_and_test_only_calls_are_ignored() {
        let source = r#"const LABEL: &str = "config"; #[cfg(test)] fn fixture(git: &str) { std::process::Command::new(git).args(["config"]).output(); }"#;
        assert!(Findings::of(source).0.is_empty());
    }

    // The syntax guard sees literal arguments at process construction sites. It
    // does not resolve argv assembled through arbitrary helpers or shell text.
}
