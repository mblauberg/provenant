//! Keep project-owned Git config command construction in one module.
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use syn::visit::{self, Visit};
    use syn::{Expr, ImplItemFn, ItemFn, ItemMod};

    fn has_config(expr: &Expr) -> bool {
        struct Literal(bool);
        impl<'ast> Visit<'ast> for Literal {
            fn visit_lit_str(&mut self, value: &'ast syn::LitStr) {
                self.0 |= value.value() == "config";
            }
        }
        let mut literal = Literal(false);
        literal.visit_expr(expr);
        literal.0
    }

    fn git_command(expr: &Expr) -> bool {
        match expr {
            Expr::Path(path) => path.path.segments.last().is_some_and(|part| {
                matches!(
                    part.ident.to_string().as_str(),
                    "Command" | "command" | "cmd" | "git"
                )
            }),
            Expr::MethodCall(call) => {
                (call.method == "new" && git_command(&call.receiver)) || git_command(&call.receiver)
            }
            Expr::Call(call) => matches!(call.func.as_ref(), Expr::Path(path)
                if path.path.segments.iter().any(|part| part.ident == "Command")
                    && path.path.segments.last().is_some_and(|part| part.ident == "new")
                    || path.path.segments.last().is_some_and(|part| matches!(part.ident.to_string().as_str(), "git" | "command" | "cmd"))),
            _ => false,
        }
    }

    fn exec_options(expr: &Expr) -> bool {
        matches!(expr, Expr::Call(call) if matches!(call.func.as_ref(), Expr::Path(path)
            if path.path.segments.iter().any(|part| part.ident == "ExecOptions")))
    }

    #[derive(Default)]
    struct Calls(Vec<String>);
    impl<'ast> Visit<'ast> for Calls {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            if matches!(node.method.to_string().as_str(), "arg" | "args")
                && node.args.iter().any(has_config)
                && git_command(&node.receiver)
            {
                self.0.push("std::process command".into());
            }
            if node.method == "execute"
                && node.args.iter().any(has_config)
                && node.args.iter().any(exec_options)
            {
                self.0.push("Executor argv".into());
            }
            if matches!(node.method.to_string().as_str(), "full" | "run")
                && node.args.iter().any(has_config)
            {
                self.0.push(format!("Git::{} argv", node.method));
            }
            visit::visit_expr_method_call(self, node);
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if matches!(node.func.as_ref(), Expr::Path(path)
            if path.path.segments.last().is_some_and(|part| {
                let name = part.ident.to_string();
                    name.starts_with("host_git")
                        || matches!(name.as_str(), "run" | "execute_for" | "git")
            })) && node.args.iter().any(has_config)
            {
                self.0.push("Git helper argv".into());
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

    fn check(source: &str) -> Vec<String> {
        let file = syn::parse_file(source).expect("Rust source parses");
        let mut calls = Calls::default();
        calls.visit_file(&file);
        calls.0
    }

    #[test]
    fn only_the_core_constructs_git_config_commands() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates root");
        let workspace = crates.parent().expect("workspace root");
        let core = crates.join("os/src/git_config.rs");
        let mut files = Vec::new();
        for source_root in [workspace.join("src"), workspace.join("examples")] {
            if source_root.is_dir() {
                sources(&source_root, &mut files);
            }
        }
        for entry in std::fs::read_dir(crates).expect("crate directories") {
            let crate_dir = entry.expect("crate entry").path();
            for tree in ["src", "examples"] {
                let source_root = crate_dir.join(tree);
                if source_root.is_dir() {
                    sources(&source_root, &mut files);
                }
            }
        }

        let mut violations = Vec::new();
        for path in files {
            if path == core || test_only_source(&path) {
                continue;
            }
            // These .rs files are schema DSL passed to macros, not Rust modules.
            if path.file_name().is_some_and(|name| {
                name == "schema.rs" || name.to_string_lossy().ends_with("_schema.rs")
            }) {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let file = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let mut calls = Calls::default();
            calls.visit_file(&file);
            if !calls.0.is_empty() {
                violations.push(format!("{}: {:?}", path.display(), calls.0));
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
        let git_full =
            r#"fn f(git: &Git) { git.full(repo, &["config", "--get", "core.worktree"]); }"#;
        let git_run = r#"fn f(git: &Git) { git.run(repo, &["config", "--list"]); }"#;
        let execute_for = r#"fn f() { execute_for(repo, &["git", "config"], timeout, false); }"#;
        let wrapped_host_git = r#"fn f() { let run = |args| host_git_with_cancel(repo, executor, args, timeout, false, None); run(&["config", "--get", "remote.origin.url"]); }"#;
        assert!(!check(process).is_empty());
        assert!(!check(executor).is_empty());
        assert!(!check(host_git).is_empty());
        assert!(!check(git_helper).is_empty());
        assert!(!check(git_full).is_empty());
        assert!(!check(git_run).is_empty());
        assert!(!check(execute_for).is_empty());
        assert!(!check(wrapped_host_git).is_empty());
    }

    #[test]
    fn unrelated_literals_and_test_only_calls_are_ignored() {
        let source = r#"const LABEL: &str = "config"; #[cfg(test)] fn fixture(git: &str) { std::process::Command::new(git).args(["config"]).output(); }"#;
        assert!(check(source).is_empty());
    }

    // The syntax guard sees literal arguments at process construction sites. It
    // does not resolve argv assembled through arbitrary helpers or shell text.
}
