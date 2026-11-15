use std::collections::BTreeSet;
use std::path::Path;
use syn::visit::Visit;

pub fn join_path(parts: &[String]) -> String {
    let kept: Vec<String> = parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect();
    kept.join("/")
}

pub fn module_base(file: &str) -> String {
    let path = Path::new(file);
    let dir = path.parent().unwrap().to_string_lossy().replace('\\', "/");
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let owns_dir = !matches!(stem.as_str(), "lib" | "main" | "mod");
    join_path(&[dir, if owns_dir { stem } else { String::new() }])
}

fn is_exact_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .map(|meta| matches!(&meta, syn::Meta::Path(path) if path.is_ident("test")))
                .unwrap_or(false)
    })
}

pub fn is_exact_test_file_module(module: &syn::ItemMod) -> bool {
    let exact = module.content.is_none() && is_exact_cfg_test(&module.attrs);
    exact && !module.attrs.iter().any(|attr| attr.path().is_ident("path"))
}

pub fn cfg_test_file_modules(file: &str, source: &str) -> Vec<String> {
    let base = module_base(file);
    let mut skipped = BTreeSet::new();
    let mut kept = BTreeSet::new();
    for item in syn::parse_file(source).unwrap().items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let name = module.ident.to_string();
        if is_exact_test_file_module(&module) {
            skipped.insert(name);
        } else {
            kept.insert(name);
        }
    }
    let mut files = Vec::new();
    for name in skipped.difference(&kept) {
        files.push(join_path(&[base.clone(), format!("{name}.rs")]));
        files.push(join_path(&[base.clone(), format!("{name}/mod.rs")]));
    }
    files
}

#[derive(Default)]
struct PathLiterals {
    found: Vec<String>,
}

impl<'ast> Visit<'ast> for PathLiterals {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        syn::visit::visit_item_mod(self, node);
        for attr in &node.attrs {
            let value = attr.meta.require_name_value().ok().map(|pair| &pair.value);
            if let (true, Some(syn::Expr::Lit(lit))) = (attr.path().is_ident("path"), value) {
                if let syn::Lit::Str(text) = &lit.lit {
                    self.found.push(text.value());
                }
            }
        }
    }
}

pub fn path_targets(file: &str, source: &str) -> Vec<String> {
    let mut literals = PathLiterals::default();
    literals.visit_file(&syn::parse_file(source).unwrap());
    let dir = Path::new(file).parent().unwrap().to_string_lossy();
    let dir = dir.replace('\\', "/");
    let base = module_base(file);
    let mut targets = Vec::new();
    for literal in literals.found {
        let literal = literal.trim_start_matches("./").to_string();
        targets.push(join_path(&[dir.clone(), literal.clone()]));
        targets.push(join_path(&[base.clone(), literal]));
    }
    targets
}

pub fn skipped_files(sources: &[(String, String)]) -> BTreeSet<String> {
    let skipped: BTreeSet<String> = sources
        .iter()
        .flat_map(|(file, source)| cfg_test_file_modules(file, source))
        .collect();
    let targets: BTreeSet<String> = sources
        .iter()
        .flat_map(|(file, source)| path_targets(file, source))
        .collect();
    skipped.difference(&targets).cloned().collect()
}
