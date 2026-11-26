//! Validate and bundle the built-in WESL package and generate its lit feature keys.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use wesl::pass::{Visit, flatten_imports, imported_item_path};
use wesl::syntax::{
    Attribute, Attributes, Expression, ModulePath, PathOrigin, TranslationUnit, TypeExpression,
};

#[path = "src/compose/modules.rs"]
mod modules;

fn main() {
    println!("cargo:rerun-if-changed=shaders");
    println!("cargo:rerun-if-changed=src/compose/modules.rs");

    let package = wesl::PackageBuilder::new("unlit_wgpu")
        .scan_root("shaders")
        .expect("failed to scan WESL files")
        .validate()
        .unwrap_or_else(|error| panic!("WESL validation failed: {error}"));

    for name in [
        modules::LIT_MODULE,
        modules::LINE_MODULE,
        modules::FULLSCREEN_VERTEX_MODULE,
        modules::BLIT_FRAGMENT_MODULE,
    ] {
        assert!(
            package
                .root
                .submodules
                .iter()
                .any(|module| module.name == name),
            "entry module {name} is missing from the WESL package"
        );
    }
    let mut flags = BTreeSet::new();
    collect_flags(
        &package.root,
        &ModulePath::new(
            PathOrigin::Package(package.root.name.clone()),
            vec![modules::LIT_MODULE.to_owned()],
        ),
        &mut BTreeSet::new(),
        &mut flags,
    );
    let mut generated = String::from("#[derive(Clone, Copy)]\npub(super) enum LitFeature {\n");
    for name in &flags {
        writeln!(generated, "    {},", rust_variant(name)).unwrap();
    }
    writeln!(
        generated,
        "}}\nimpl LitFeature {{\n    pub(super) const ALL: [Self; {}] = [",
        flags.len()
    )
    .unwrap();
    for name in &flags {
        writeln!(generated, "        Self::{},", rust_variant(name)).unwrap();
    }
    generated.push_str(
        "    ];\n    pub(super) const fn name(self) -> &'static str {\n        match self {\n",
    );
    for name in &flags {
        writeln!(
            generated,
            "            Self::{} => {name:?},",
            rust_variant(name)
        )
        .unwrap();
    }
    generated.push_str("        }\n    }\n}\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    std::fs::write(out.join("lit_features.rs"), generated)
        .expect("failed to write WESL feature keys");

    package
        .build_artifact()
        .expect("failed to build the WESL package artifact");
}

fn module_at<'a>(
    root: &'a wesl::package::PackageModule,
    path: &ModulePath,
) -> Option<&'a wesl::package::PackageModule> {
    if !matches!(&path.origin, PathOrigin::Package(name) if name == &root.name) {
        return None;
    }
    path.components.iter().try_fold(root, |module, name| {
        module.submodules.iter().find(|child| &child.name == name)
    })
}

fn collect_flags(
    root: &wesl::package::PackageModule,
    path: &ModulePath,
    seen: &mut BTreeSet<Vec<String>>,
    flags: &mut BTreeSet<String>,
) {
    let Some(module) = module_at(root, path) else {
        return;
    };
    if !seen.insert(path.components.clone()) {
        return;
    }
    let syntax: TranslationUnit = module
        .source
        .parse()
        .expect("the WESL module was validated");
    for attributes in Visit::<Attributes>::visit(&syntax) {
        for attribute in attributes {
            if let Attribute::If(expr) | Attribute::Elif(expr) = &**attribute {
                collect_names(expr, flags);
            }
        }
    }

    // Include inline imports and imports of either the whole module or an item.
    #[expect(
        clippy::mutable_key_type,
        reason = "WESL imports have identity keys, not mutable spelling keys"
    )]
    let imports = flatten_imports(&syntax.imports, path);
    for item in imports.values() {
        collect_flags(root, &item.path, seen, flags);
        collect_flags(
            root,
            &item.path.clone().join([item.ident.name().to_string()]),
            seen,
            flags,
        );
    }
    for expression in Visit::<TypeExpression>::visit(&syntax) {
        if let Some((module, _)) = imported_item_path(expression, path, &imports) {
            collect_flags(root, &module, seen, flags);
        }
    }
}

fn collect_names(expr: &Expression, flags: &mut BTreeSet<String>) {
    match expr {
        Expression::Literal(_) => (),
        Expression::Parenthesized(expr) => collect_names(&expr.expression, flags),
        Expression::Unary(expr) => collect_names(&expr.operand, flags),
        Expression::Binary(expr) => {
            collect_names(&expr.left, flags);
            collect_names(&expr.right, flags);
        }
        Expression::TypeOrIdentifier(expr) => {
            flags.insert(expr.ident.name().to_string());
        }
        _ => panic!("unsupported expression in a WESL feature condition: {expr:?}"),
    }
}

fn rust_variant(name: &str) -> String {
    let mut variant = String::new();
    for part in name.split('_') {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            variant.extend(first.to_uppercase());
            variant.extend(chars.flat_map(char::to_lowercase));
        }
    }
    variant
}
