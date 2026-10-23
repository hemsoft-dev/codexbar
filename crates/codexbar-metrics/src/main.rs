//! Build-time quality tooling, never linked into or packaged with the app.
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, process::ExitCode};
use syn::{Attribute, Meta, Token, punctuated::Punctuated, spanned::Spanned, visit::Visit};

// Evaluate only whether a cfg can be true in production. OS/features remain unknown.
// This excludes all(test, ..), while retaining any(test, windows) production code.
fn production_cfg(meta: &Meta) -> Option<bool> {
    match meta {
        Meta::Path(path) if path.is_ident("test") => Some(false),
        Meta::NameValue(value) if value.path.is_ident("feature") => {
            if let syn::Expr::Lit(expr) = &value.value
                && let syn::Lit::Str(name) = &expr.lit
                && name.value() == "capture"
            {
                Some(false)
            } else {
                None
            }
        }
        Meta::List(list) => {
            let args = list
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .ok()?;
            let values: Vec<_> = args.iter().map(production_cfg).collect();
            if list.path.is_ident("not") && values.len() == 1 {
                values[0].map(|value| !value)
            } else if list.path.is_ident("all") {
                if values.contains(&Some(false)) {
                    Some(false)
                } else if values.iter().all(|value| *value == Some(true)) {
                    Some(true)
                } else {
                    None
                }
            } else if list.path.is_ident("any") {
                if values.contains(&Some(true)) {
                    Some(true)
                } else if values.iter().all(|value| *value == Some(false)) {
                    Some(false)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
            || (attr.path().is_ident("cfg")
                && attr.parse_args::<Meta>().ok().as_ref().and_then(production_cfg) == Some(false))
    })
}

#[derive(Default)]
struct TestSpans(Vec<(usize, usize)>);

macro_rules! exclude_test_item {
    ($method:ident, $kind:ty) => {
        fn $method(&mut self, item: &'ast $kind) {
            if test_only(&item.attrs) {
                self.0.push((item.span().start().line, item.span().end().line));
            } else {
                syn::visit::$method(self, item);
            }
        }
    };
}

impl<'ast> Visit<'ast> for TestSpans {
    exclude_test_item!(visit_item_fn, syn::ItemFn);
    exclude_test_item!(visit_item_impl, syn::ItemImpl);
    exclude_test_item!(visit_item_trait, syn::ItemTrait);
    exclude_test_item!(visit_item_mod, syn::ItemMod);
    exclude_test_item!(visit_impl_item_fn, syn::ImplItemFn);
    exclude_test_item!(visit_trait_item_fn, syn::TraitItemFn);
}

fn analyze(path: &Path, source: &str) -> Result<Value> {
    let mut excluded = TestSpans::default();
    excluded.visit_file(&syn::parse_file(source)?);
    let functions = cargo_crap::complexity::analyze_source(path, source, 1.0)?;
    let functions: Vec<_> = functions
        .into_iter()
        .filter(|function| {
            !excluded
                .0
                .iter()
                .any(|(start, end)| *start <= function.start_line && function.end_line <= *end)
        })
        .map(|function| {
            json!({
                "name": function.name, "start_line": function.start_line,
                "end_line": function.end_line, "cyclomatic": function.cyclomatic
            })
        })
        .collect();
    Ok(json!({"path": path, "sha256": format!("{:x}", Sha256::digest(source.as_bytes())), "functions": functions}))
}

fn execute() -> Result<()> {
    let files = std::env::args_os()
        .skip(1)
        .map(|path| {
            let path = Path::new(&path);
            analyze(path, &std::fs::read_to_string(path)?)
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!files.is_empty(), "Provide source files to analyze");
    println!(
        "{}",
        json!({"schema": 1, "complexity_producer": "cargo-crap 0.9.0", "files": files})
    );
    Ok(())
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cfg_is_excluded_without_removing_platform_production_code() {
        let report = analyze(
            Path::new("fixture.rs"),
            r#"
            #[cfg(test)] fn helper() {}
            #[cfg(all(test, windows))] mod tests { fn hidden() {} }
            #[cfg(test)] impl Foo { fn hidden_method() {} }
            #[gpui::test] fn integration() {}
            #[cfg(any(test, feature = "capture"))] fn screenshot_fixture() {}
            #[cfg(not(test))] fn production_only() {}
            #[cfg(any(test, windows))] fn windows_production() {}
            fn decisions(value: i32) -> i32 { if value > 0 { value } else { -value } }
        "#,
        )
        .unwrap();
        let names: Vec<_> = report["functions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|function| function["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["production_only", "windows_production", "decisions"]);
        assert_eq!(report["functions"][2]["cyclomatic"], 2.0);
    }

    #[test]
    fn syntax_errors_cannot_produce_partial_complexity_evidence() {
        assert!(analyze(Path::new("invalid.rs"), "fn broken( {").is_err());
    }
}
