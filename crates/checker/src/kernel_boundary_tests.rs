use std::{collections::BTreeSet, fs, path::Path};

use syn::visit::{self, Visit};

const ALLOWED_DEPENDENCIES: &[&str] =
    &["alloy_primitives", "alloy_sol_types", "serde", "thiserror"];

struct BoundaryVisitor<'a> {
    file: &'a Path,
    dependencies: &'a BTreeSet<String>,
    failures: Vec<String>,
}

impl BoundaryVisitor<'_> {
    fn reject(&mut self, path: &syn::Path, reason: &str) {
        let segments = path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>();
        self.reject_segments(&segments, reason);
    }

    fn reject_segments(&mut self, segments: &[String], reason: &str) {
        let rendered = segments.join("::");
        self.failures
            .push(format!("{}: `{rendered}` {reason}", self.file.display()));
    }

    fn check_segments(&mut self, segments: &[String], check_single_dependency: bool) {
        if segments.first().is_some_and(|root| root == "crate")
            && (segments.get(1).is_some_and(|module| module != "kernel")
                || (segments.len() == 1 && check_single_dependency))
        {
            self.reject_segments(segments, "escapes the kernel module");
        }
        if segments.first().is_some_and(|root| root == "super")
            && self.file.ends_with("src/kernel/mod.rs")
            && (segments.len() > 1 || check_single_dependency)
        {
            self.reject_segments(segments, "escapes the root kernel module");
        }
        if segments.windows(2).any(|pair| pair == ["super", "super"]) {
            self.reject_segments(segments, "escapes with super::super");
        }
        if let Some(root) = segments.first()
            && (check_single_dependency || segments.len() > 1)
            && self.dependencies.contains(root)
            && !ALLOWED_DEPENDENCIES.contains(&root.as_str())
        {
            self.reject_segments(segments, "uses a forbidden checker dependency");
        }
    }

    fn check_use_tree(&mut self, tree: &syn::UseTree, mut prefix: Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.check_use_tree(&path.tree, prefix);
            }
            syn::UseTree::Name(name) => {
                prefix.push(name.ident.to_string());
                self.check_segments(&prefix, true);
            }
            syn::UseTree::Rename(rename) => {
                prefix.push(rename.ident.to_string());
                self.check_segments(&prefix, true);
            }
            syn::UseTree::Glob(_) => self.check_segments(&prefix, true),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.check_use_tree(item, prefix.clone());
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for BoundaryVisitor<'_> {
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        if attribute.path().is_ident("path") {
            self.reject(attribute.path(), "uses forbidden #[path]");
        }
        visit::visit_attribute(self, attribute);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.is_ident("include") {
            self.reject(&mac.path, "uses forbidden include!");
        }
        visit::visit_macro(self, mac);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        let name = item.ident.to_string();
        if !matches!(name.as_str(), "std" | "core" | "alloc") {
            self.failures.push(format!(
                "{}: `extern crate {name}` can bypass the kernel boundary",
                self.file.display()
            ));
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.check_use_tree(&item.tree, Vec::new());
        visit::visit_item_use(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let segments = path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>();
        self.check_segments(&segments, false);
        visit::visit_path(self, path);
    }
}

fn collect_dependencies(value: &toml::Value, dependencies: &mut BTreeSet<String>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (name, value) in table {
        if name.ends_with("dependencies") {
            if let Some(entries) = value.as_table() {
                dependencies.extend(entries.keys().map(|name| name.replace('-', "_")));
            }
        } else {
            collect_dependencies(value, dependencies);
        }
    }
}

fn rust_files(root: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn kernel_has_a_mechanical_dependency_boundary() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_source = fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&manifest_source).unwrap();
    let mut dependencies = BTreeSet::new();
    collect_dependencies(&manifest, &mut dependencies);
    let mut files = Vec::new();
    rust_files(&manifest_dir.join("src/kernel"), &mut files);
    let mut failures = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).unwrap();
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("failed to parse {}: {error}", file.display()));
        let mut visitor = BoundaryVisitor {
            file: &file,
            dependencies: &dependencies,
            failures: Vec::new(),
        };
        visitor.visit_file(&syntax);
        failures.extend(visitor.failures);
    }
    assert!(
        failures.is_empty(),
        "kernel boundary violations:\n{}",
        failures.join("\n")
    );
}
