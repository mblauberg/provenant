//! Guard config readers against losing the cause of a failed read.
#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use syn::parse::Parser;
    use syn::visit::{self, Visit};
    use syn::{Expr, ExprClosure, ExprMethodCall, ImplItemFn, ItemFn, Pat};

    #[derive(Default)]
    struct Facts {
        refs: BTreeSet<String>,
        violations: Vec<String>,
        returns_transport_error: bool,
    }
    impl<'ast> Visit<'ast> for Facts {
        fn visit_macro(&mut self, node: &'ast syn::Macro) {
            for word in node
                .tokens
                .to_string()
                .split(|c: char| !c.is_alphanumeric() && c != '_')
            {
                self.refs.insert(word.to_owned());
            }
            if let Ok(expressions) =
                syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated
                    .parse2(node.tokens.clone())
            {
                for expression in &expressions {
                    let mut nested = Facts::default();
                    nested.visit_expr(expression);
                    self.refs.extend(nested.refs);
                    self.violations.extend(nested.violations);
                }
            }
        }
        fn visit_lit_str(&mut self, value: &'ast syn::LitStr) {
            self.refs.insert(format!("literal:{}", value.value()));
        }
        fn visit_path(&mut self, path: &'ast syn::Path) {
            if let Some(last) = path.segments.last() {
                self.refs.insert(last.ident.to_string());
            }
            visit::visit_path(self, path);
        }
        fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
            let method = node.method.to_string();
            self.refs.insert(method.clone());
            if matches!(method.as_str(), "ok" | "unwrap_or_default" | "unwrap_or")
                || (matches!(method.as_str(), "unwrap_or_else" | "or_else")
                    && matches!(node.args.first(), Some(Expr::Closure(closure)) if !closure.inputs.is_empty()))
            {
                self.violations
                    .push(format!(".{method}() discards an error"));
            }
            if method == "map_err"
                && let Some(Expr::Closure(ExprClosure { inputs, body, .. })) = node.args.first()
                && let Some(pattern) = inputs.first()
            {
                let pattern = match pattern {
                    Pat::Type(typed) => typed.pat.as_ref(),
                    other => other,
                };
                let ignored = match pattern {
                    Pat::Wild(_) => true,
                    Pat::Ident(ident) => {
                        let mut used = Facts::default();
                        used.visit_expr(body);
                        !used.refs.contains(&ident.ident.to_string())
                            && !used.refs.iter().any(|s| {
                                s.starts_with("literal:")
                                    && s.contains(&format!("{{{}", ident.ident))
                            })
                    }
                    _ => false,
                };
                if ignored {
                    self.violations.push(format!(
                        "map_err discards {}",
                        match pattern {
                            Pat::Ident(p) => p.ident.to_string(),
                            _ => "wildcard".into(),
                        }
                    ));
                } else {
                    let mut mapped = Facts::default();
                    mapped.visit_expr(body);
                    if ![
                        "unreadable",
                        "unreadable_config",
                        "cause",
                        "refusal",
                        "refuse",
                    ]
                    .iter()
                    .any(|name| mapped.refs.contains(*name))
                    {
                        self.violations
                            .push("map_err does not name a config-read cause".into());
                    }
                }
            }
            visit::visit_expr_method_call(self, node);
        }
        fn visit_arm(&mut self, arm: &'ast syn::Arm) {
            if let Pat::TupleStruct(pattern) = &arm.pat
                && pattern
                    .path
                    .segments
                    .last()
                    .is_some_and(|part| part.ident == "Err")
                && let Some(Pat::Wild(_)) = pattern.elems.first()
            {
                self.violations.push("match discards an error".into());
            }
            visit::visit_arm(self, arm);
        }
        fn visit_local(&mut self, local: &'ast syn::Local) {
            if matches!(local.pat, Pat::Wild(_)) && local.init.is_some() {
                self.violations.push("let _ discards a result".into());
            }
            visit::visit_local(self, local);
        }
    }
    #[derive(Default)]
    struct Functions {
        functions: BTreeMap<String, Facts>,
    }
    impl<'ast> Visit<'ast> for Functions {
        fn visit_item_fn(&mut self, node: &'ast ItemFn) {
            let mut facts = Facts::default();
            facts.visit_signature(&node.sig);
            facts.visit_block(&node.block);
            let mut signature = Facts::default();
            signature.visit_return_type(&node.sig.output);
            facts.returns_transport_error = signature.refs.contains("TransportError");
            self.functions.insert(
                format!("{}:{}", self.functions.len(), node.sig.ident),
                facts,
            );
        }
        fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
            let mut facts = Facts::default();
            facts.visit_signature(&node.sig);
            facts.visit_block(&node.block);
            let mut signature = Facts::default();
            signature.visit_return_type(&node.sig.output);
            facts.returns_transport_error = signature.refs.contains("TransportError");
            self.functions.insert(
                format!("{}:{}", self.functions.len(), node.sig.ident),
                facts,
            );
        }
        fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
            if !node.attrs.iter().any(|a| {
                a.path().is_ident("cfg")
                    && a.meta
                        .require_list()
                        .is_ok_and(|l| l.tokens.to_string() == "test")
            }) {
                visit::visit_item_mod(self, node);
            }
        }
    }
    fn sources(dir: &Path, output: &mut Vec<PathBuf>) {
        for entry in
            std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read source directory: {error}"))
        {
            let path = entry
                .unwrap_or_else(|error| panic!("source directory entry: {error}"))
                .path();
            if path.is_dir() {
                sources(&path, output);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                output.push(path);
            }
        }
    }
    fn readers(source: &str) -> Vec<(String, Vec<String>)> {
        let file = syn::parse_file(source).unwrap_or_else(|error| panic!("parse source: {error}"));
        let mut functions = Functions::default();
        functions.visit_file(&file);
        // Seed from calls into the config read core; then include typed transport
        // helpers invoked by those readers, such as root resolution. Discovery is
        // based on the production call graph, not declarations of reader names.
        let mut selected = BTreeSet::new();
        for (name, facts) in &functions.functions {
            if name.ends_with(":parse_listing")
                || [
                    "parse_listing",
                    "LISTING_ARGS",
                    "DIRECTIVE_ARGS",
                    "file_listing_args",
                    "LocalBareOrigins",
                    "ConfigValue",
                ]
                .iter()
                .any(|core| facts.refs.contains(*core))
                || (facts.refs.contains("literal:config")
                    && ["--get-all"]
                        .iter()
                        .any(|arg| facts.refs.contains(&format!("literal:{arg}"))))
            {
                selected.insert(name.clone());
            }
        }
        loop {
            let mut calls = BTreeSet::new();
            for name in &selected {
                calls.extend(functions.functions[name].refs.iter().cloned());
            }
            let before = selected.len();
            for (name, facts) in &functions.functions {
                let ident = name
                    .split_once(':')
                    .unwrap_or_else(|| panic!("function identifier"))
                    .1;
                if facts.returns_transport_error && calls.contains(ident) {
                    selected.insert(name.clone());
                }
            }
            if selected.len() == before {
                break;
            }
        }
        functions
            .functions
            .into_iter()
            .filter_map(|(name, facts)| {
                selected.contains(&name).then_some((name, facts.violations))
            })
            .collect()
    }
    #[test]
    fn config_readers_preserve_errors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or_else(|| panic!("crate directory"));
        let mut files = Vec::new();
        for krate in ["confinement", "run", "cli"] {
            sources(&root.join(krate).join("src"), &mut files);
        }
        let mut count = 0;
        for file in files {
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("read source: {error}"));
            if ![
                "parse_listing",
                "LISTING_ARGS",
                "DIRECTIVE_ARGS",
                "file_listing_args",
                "LocalBareOrigins",
                "ConfigValue",
                "--get-all",
            ]
            .iter()
            .any(|marker| text.contains(marker))
            {
                continue;
            }
            syn::parse_file(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            for (name, violations) in readers(&text) {
                count += 1;
                assert!(
                    violations.is_empty(),
                    "{} {name}: {violations:?}",
                    file.display()
                );
            }
        }
        assert!(count > 0, "no readers discovered");
    }
    #[test]
    fn synthetic_called_transport_helper_is_discovered() {
        let source = "fn reader() { parse_listing(); roots(); } fn roots() -> Result<(), TransportError> { let _ = fallible(); Ok(()) }";
        let found = readers(source);
        assert!(
            found
                .iter()
                .any(|(name, failures)| name.ends_with(":roots") && !failures.is_empty())
        );
    }

    #[test]
    fn synthetic_new_readers_cannot_discard_errors() {
        for expression in [
            "x.map_err(|_| failure())",
            "x.map_err(|_error| failure())",
            "x.map_err(|_error: E| failure())",
            "x.map_err(|error| failure())",
            "x.map_err(|error| Other(error))",
            "x.ok()",
            "dbg!(x.ok())",
            "x.unwrap_or_default()",
            "x.unwrap_or(fallback)",
            "x.unwrap_or_else(|err| fallback)",
            "x.or_else(|err| fallback)",
            "{ let _ = x; } ",
            "match x { Ok(v) => v, Err(_) => fallback }",
        ] {
            let source = format!("fn new_reader() {{ parse_listing(); {expression}; }}");
            let found = readers(&source);
            assert_eq!(found.len(), 1, "new reader was not discovered");
            assert!(!found[0].1.is_empty(), "missed {expression}");
        }
    }
}
